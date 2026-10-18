use crate::model::{Filter, Recording, Station};
use anyhow::{Context, Result, bail};
use rusqlite::{
    Connection, OptionalExtension, TransactionBehavior, params, params_from_iter, types::Value,
};
use std::path::Path;

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;
            PRAGMA cache_size=-4096; PRAGMA synchronous=NORMAL;",
        )?;
        let version: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > 2 {
            bail!("数据库版本较新，请升级 radio");
        }
        if version < 2 {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // Another process may have migrated while we waited for the lock.
            let version: u32 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
            match version {
                0 => tx.execute_batch(
                    "CREATE TABLE stations (
                        uuid TEXT PRIMARY KEY, name TEXT NOT NULL, search TEXT NOT NULL,
                        country TEXT NOT NULL, languages TEXT NOT NULL, tags TEXT NOT NULL,
                        bitrate INTEGER NOT NULL, popularity INTEGER NOT NULL,
                        custom INTEGER NOT NULL, active INTEGER NOT NULL, json TEXT NOT NULL);
                    CREATE INDEX station_country ON stations(country, bitrate);
                    CREATE INDEX station_popularity ON stations(popularity DESC);
                    CREATE TABLE favorites (
                        uuid TEXT PRIMARY KEY REFERENCES stations(uuid), created_at TEXT NOT NULL);
                    CREATE TABLE history (
                        id INTEGER PRIMARY KEY, uuid TEXT NOT NULL REFERENCES stations(uuid), played_at TEXT NOT NULL);
                    CREATE INDEX history_station ON history(uuid);
                    CREATE TABLE streams (
                        uuid TEXT NOT NULL REFERENCES stations(uuid), url TEXT NOT NULL, PRIMARY KEY(uuid,url));
                    CREATE TABLE recordings (
                        id TEXT PRIMARY KEY, status TEXT NOT NULL, json TEXT NOT NULL,
                        recovery_pending INTEGER NOT NULL DEFAULT 0 CHECK(recovery_pending IN (0,1)));
                    CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);"
                )?,
                1 => tx.execute_batch(
                    "ALTER TABLE recordings ADD COLUMN recovery_pending INTEGER NOT NULL DEFAULT 0 CHECK(recovery_pending IN (0,1));
                    -- Translate legacy diagnostics once, never during normal queries.
                    UPDATE recordings SET recovery_pending=1 WHERE status='interrupted' AND json_extract(json,'$.error') LIKE '恢复失败:%';"
                )?,
                2 => {},
                _ => bail!("数据库版本较新，请升级 radio"),
            }
            tx.pragma_update(None, "user_version", 2)?;
            tx.commit()?;
        }
        Ok(Self { conn })
    }

    pub fn upsert(&self, station: &Station) -> Result<()> {
        Self::write_station(&self.conn, "stations", station)
    }

    fn write_station(conn: &Connection, table: &str, s: &Station) -> Result<()> {
        if s.stationuuid.is_empty() || s.name.trim().is_empty() {
            bail!("电台缺少 UUID 或名称");
        }
        uuid::Uuid::parse_str(&s.stationuuid).context("电台 UUID 无效")?;
        let sql = format!("INSERT INTO {table} VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
            ON CONFLICT(uuid) DO UPDATE SET name=excluded.name,search=excluded.search,
            country=excluded.country,languages=excluded.languages,tags=excluded.tags,
            bitrate=excluded.bitrate,popularity=excluded.popularity,active=excluded.active,json=excluded.json
            WHERE {table}.custom=0");
        let normalize = |value: &str| {
            value
                .split(',')
                .map(|v| v.trim().to_lowercase())
                .filter(|v| !v.is_empty())
                .collect::<Vec<_>>()
                .join(",")
        };
        let languages = normalize(&format!("{},{}", s.language, s.languagecodes));
        conn.execute(
            &sql,
            params![
                s.stationuuid,
                s.name,
                s.search_text(),
                s.countrycode.to_uppercase(),
                format!(",{languages},"),
                format!(",{},", normalize(&s.tags)),
                s.bitrate,
                s.clickcount.min(i64::MAX as u64),
                s.custom,
                s.active,
                serde_json::to_string(s)?
            ],
        )?;
        Ok(())
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn directory_count(&self) -> Result<usize> {
        Ok(self.conn.query_row(
            "SELECT count(*) FROM stations WHERE custom=0 AND active=1",
            [],
            |r| r.get(0),
        )?)
    }

    pub fn incoming_count(&self) -> Result<usize> {
        Ok(self
            .conn
            .query_row("SELECT count(*) FROM incoming", [], |r| r.get(0))?)
    }

    pub fn begin_sync(&self) -> Result<()> {
        self.conn.execute_batch(
            "DROP TABLE IF EXISTS temp.incoming;
            CREATE TEMP TABLE incoming AS SELECT * FROM stations WHERE 0;
            CREATE UNIQUE INDEX temp.incoming_uuid ON incoming(uuid);",
        )?;
        Ok(())
    }

    pub fn stage(&mut self, rows: &[Station]) -> Result<()> {
        let tx = self.conn.transaction()?;
        for row in rows {
            let mut s = row.clone();
            s.active = true;
            s.custom = false;
            Self::write_station(&tx, "incoming", &s)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn finish_sync(&mut self) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let count: usize = tx.query_row("SELECT count(*) FROM incoming", [], |r| r.get(0))?;
        if count == 0 {
            bail!("服务器返回空目录，保留原有数据");
        }
        tx.execute("UPDATE stations SET active=0 WHERE custom=0", [])?;
        tx.execute_batch(
            "INSERT INTO stations SELECT * FROM incoming WHERE true
            ON CONFLICT(uuid) DO UPDATE SET name=excluded.name,search=excluded.search,
            country=excluded.country,languages=excluded.languages,tags=excluded.tags,
            bitrate=excluded.bitrate,popularity=excluded.popularity,active=1,json=excluded.json
            WHERE stations.custom=0;",
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO meta VALUES ('last_sync',?1)",
            [chrono::Utc::now().to_rfc3339()],
        )?;
        tx.commit()?;
        Ok(count)
    }

    pub fn revision(&self) -> Result<(u64, u64)> {
        let external = self
            .conn
            .pragma_query_value(None, "data_version", |r| r.get(0))?;
        Ok((external, self.conn.total_changes()))
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn count(&self) -> Result<usize> {
        Ok(self
            .conn
            .query_row("SELECT count(*) FROM stations WHERE active=1", [], |r| {
                r.get(0)
            })?)
    }

    pub fn station(&self, uuid: &str) -> Result<Station> {
        let row = self
            .conn
            .query_row(
                "SELECT s.json,s.active,f.uuid IS NOT NULL FROM stations s
            LEFT JOIN favorites f ON f.uuid=s.uuid WHERE s.uuid=?1",
                [uuid],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, bool>(1)?,
                        r.get::<_, bool>(2)?,
                    ))
                },
            )
            .optional()?
            .context("本地未找到电台；请先 sync 或 station add")?;
        let mut s: Station = serde_json::from_str(&row.0)?;
        s.active = row.1;
        s.favorite = row.2;
        Ok(s)
    }

    fn conditions(filter: &Filter) -> (String, Vec<Value>) {
        let mut clauses = vec!["(s.active=1 OR s.custom=1 OR f.uuid IS NOT NULL)".to_owned()];
        let mut args = vec![];
        if filter.favorites {
            clauses.push("f.uuid IS NOT NULL".into());
        }
        if filter.history {
            clauses[0] = "EXISTS(SELECT 1 FROM history h WHERE h.uuid=s.uuid)".into();
        }
        let query = filter.query.trim();
        if !query.is_empty() {
            clauses.push("instr(s.search,?)>0".into());
            args.push(query.to_lowercase().into());
        }
        if let Some(country) = &filter.country {
            clauses.push("s.country=?".into());
            args.push(country.to_uppercase().into());
        }
        for (col, value) in [("languages", &filter.language), ("tags", &filter.tag)] {
            if let Some(value) = value {
                clauses.push(format!("instr(s.{col},?)>0"));
                args.push(format!(",{},", value.trim().to_lowercase()).into());
            }
        }
        clauses.push("s.bitrate>=?".into());
        args.push((filter.min_bitrate as i64).into());
        (clauses.join(" AND "), args)
    }

    pub fn search(&self, filter: &Filter) -> Result<Vec<Station>> {
        let (conditions, mut args) = Self::conditions(filter);
        let order = if filter.history {
            "(SELECT max(id) FROM history WHERE uuid=s.uuid) DESC"
        } else {
            "s.popularity DESC,s.name,s.uuid"
        };
        let sql = format!("SELECT s.json,s.active,f.uuid IS NOT NULL FROM stations s
            LEFT JOIN favorites f ON f.uuid=s.uuid WHERE {conditions} ORDER BY {order} LIMIT ? OFFSET ?");
        args.push((filter.limit.clamp(1, 1000) as i64).into());
        args.push((filter.offset.min(i64::MAX as usize) as i64).into());
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, bool>(1)?,
                r.get::<_, bool>(2)?,
            ))
        })?;
        rows.map(|row| {
            let (json, active, favorite) = row?;
            let mut s: Station = serde_json::from_str(&json)?;
            s.active = active;
            s.favorite = favorite;
            Ok(s)
        })
        .collect()
    }

    pub fn random(&self, filter: &Filter, exclude: &[String]) -> Result<Option<Station>> {
        let (conditions, args) = Self::conditions(filter);
        let sql = format!(
            "SELECT s.uuid FROM stations s LEFT JOIN favorites f ON f.uuid=s.uuid
            WHERE {conditions}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut selected = None;
        let mut count = 0u64;
        for uuid in stmt.query_map(params_from_iter(args), |r| r.get::<_, String>(0))? {
            let uuid = uuid?;
            if exclude.contains(&uuid) {
                continue;
            }
            count += 1;
            if rand::random_range(0..count) == 0 {
                selected = Some(uuid);
            }
        }
        selected.map(|id| self.station(&id)).transpose()
    }

    pub fn favorite(&self, uuid: &str, add: bool) -> Result<()> {
        self.station(uuid)?;
        if add {
            self.conn.execute(
                "INSERT OR IGNORE INTO favorites VALUES (?1,?2)",
                params![uuid, chrono::Utc::now().to_rfc3339()],
            )?;
        } else {
            self.conn
                .execute("DELETE FROM favorites WHERE uuid=?1", [uuid])?;
        }
        Ok(())
    }

    pub fn toggle_favorite(&mut self, uuid: &str) -> Result<bool> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.query_row("SELECT 1 FROM stations WHERE uuid=?1", [uuid], |r| {
            r.get::<_, u8>(0)
        })
        .optional()?
        .context("本地未找到电台；请先 sync 或 station add")?;
        let removed = tx.execute("DELETE FROM favorites WHERE uuid=?1", [uuid])?;
        let added = removed == 0;
        if added {
            tx.execute(
                "INSERT INTO favorites VALUES (?1,?2)",
                params![uuid, chrono::Utc::now().to_rfc3339()],
            )?;
        }
        tx.commit()?;
        Ok(added)
    }

    pub fn played(&self, uuid: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO history(uuid,played_at) VALUES (?1,?2)",
            params![uuid, chrono::Utc::now().to_rfc3339()],
        )?;
        self.conn.execute("DELETE FROM history WHERE id NOT IN (SELECT id FROM history ORDER BY id DESC LIMIT 1000)", [])?;
        Ok(())
    }

    pub fn add_backup(&self, uuid: &str, url: &str) -> Result<()> {
        crate::config::validate_url(url)?;
        self.conn.execute(
            "INSERT OR IGNORE INTO streams VALUES (?1,?2)",
            params![uuid, url],
        )?;
        Ok(())
    }

    pub fn backups(&self, uuid: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT url FROM streams WHERE uuid=?1 ORDER BY rowid")?;
        Ok(stmt
            .query_map([uuid], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn save_recording(&self, recording: &Recording) -> Result<()> {
        self.conn.execute(
            "INSERT INTO recordings (id,status,json,recovery_pending) VALUES (?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET status=excluded.status,json=excluded.json,recovery_pending=excluded.recovery_pending",
            params![
                recording.id,
                recording.status,
                serde_json::to_string(recording)?,
                recording.recovery_pending
            ],
        )?;
        Ok(())
    }

    pub fn search_count(&self, filter: &Filter) -> Result<usize> {
        let (conditions, args) = Self::conditions(filter);
        Ok(self.conn.query_row(&format!("SELECT count(*) FROM stations s LEFT JOIN favorites f ON f.uuid=s.uuid WHERE {conditions}"), params_from_iter(args), |r| r.get(0))?)
    }

    pub fn recordings(&self, limit: usize) -> Result<Vec<Recording>> {
        self.recordings_page(limit, 0)
    }

    pub fn recordings_count(&self) -> Result<usize> {
        Ok(self
            .conn
            .query_row("SELECT count(*) FROM recordings", [], |r| r.get(0))?)
    }

    pub fn recordings_page(&self, limit: usize, offset: usize) -> Result<Vec<Recording>> {
        let mut stmt = self.conn.prepare("SELECT json,recovery_pending FROM recordings ORDER BY json_extract(json,'$.started_at') DESC,id DESC LIMIT ?1 OFFSET ?2")?;
        let rows = stmt.query_map(
            params![
                limit.clamp(1, 1000) as i64,
                offset.min(i64::MAX as usize) as i64
            ],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?)),
        )?;
        rows.map(|row| Self::read_recording(row?)).collect()
    }

    fn read_recording((json, recovery_pending): (String, bool)) -> Result<Recording> {
        let mut record: Recording = serde_json::from_str(&json)?;
        record.recovery_pending = recovery_pending;
        Ok(record)
    }

    pub fn unfinished_recordings(&self) -> Result<Vec<Recording>> {
        let mut stmt = self
            .conn
            .prepare("SELECT json,recovery_pending FROM recordings WHERE status IN ('recording','finalizing') OR recovery_pending=1")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?)))?;
        rows.map(|row| Self::read_recording(row?)).collect()
    }
}
