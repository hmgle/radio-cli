mod facets;
mod geo;
mod revisions;
pub use revisions::ListRevision;

use crate::error::{DomainError, ErrorCode};
use crate::model::{Filter, Recording, Station};
use anyhow::{Context, Result, bail};
use rusqlite::{
    Connection, OptionalExtension, TransactionBehavior, params, params_from_iter, types::Value,
};
use std::path::Path;

const RECORDING_ORDER: &str =
    "CASE WHEN json_valid(json) THEN json_extract(json,'$.started_at') END DESC,id DESC";

pub struct Store {
    pub(crate) conn: Connection,
}

impl Store {
    pub(crate) fn open_writer(path: &Path) -> Result<Self> {
        // The session owner has already migrated the database. A short lock
        // budget lets background writers retry and observe shutdown promptly.
        let conn = Connection::open(path)?;
        crate::podcast::register_functions(&conn)?;
        conn.busy_timeout(std::time::Duration::from_millis(100))?;
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=NORMAL;")?;
        Ok(Self { conn })
    }

    #[cfg(test)]
    pub(crate) fn test_connection(&self) -> &Connection {
        &self.conn
    }

    pub(crate) fn open_reader(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        crate::podcast::register_functions(&conn)?;
        conn.busy_timeout(std::time::Duration::from_millis(100))?;
        conn.execute_batch("PRAGMA cache_size=-4096;")?;
        Ok(Self { conn })
    }

    pub(crate) fn interrupt_handle(&self) -> rusqlite::InterruptHandle {
        self.conn.get_interrupt_handle()
    }

    pub(crate) fn cancel_queries_when(&self, cancel: impl FnMut() -> bool + Send + 'static) {
        self.conn.progress_handler(1000, Some(cancel));
    }

    pub(crate) fn read_snapshot<T>(&self, read: impl FnOnce(&Self) -> Result<T>) -> Result<T> {
        let tx = self.conn.unchecked_transaction()?;
        // BEGIN is deferred. Pin the WAL snapshot before reading revisions
        // so a count cached for an older snapshot cannot be reused for new rows.
        tx.query_row("SELECT 1 FROM sqlite_schema LIMIT 1", [], |_| Ok(()))?;
        let value = read(self)?;
        tx.commit()?;
        Ok(value)
    }

    pub fn open(path: &Path) -> Result<Self> {
        let mut conn = Connection::open(path)?;
        crate::podcast::register_functions(&conn)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;
            PRAGMA cache_size=-4096; PRAGMA synchronous=NORMAL;",
        )?;
        let version: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > 9 {
            bail!(DomainError::new(ErrorCode::DatabaseTooNew));
        }
        if version < 9 {
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
                2..=9 => {},
                _ => bail!(DomainError::new(ErrorCode::DatabaseTooNew)),
            }
            if version < 3 {
                tx.execute_batch(
                    "ALTER TABLE stations ADD COLUMN normalized_url TEXT;
                    CREATE INDEX station_url ON stations(normalized_url) WHERE active=1;",
                )?;
                let mut select = tx.prepare("SELECT uuid,json FROM stations")?;
                let mut update =
                    tx.prepare("UPDATE stations SET normalized_url=?2 WHERE uuid=?1")?;
                let rows = select
                    .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
                for row in rows {
                    let (uuid, json) = row?;
                    let station: Station = serde_json::from_str(&json)?;
                    update.execute(params![
                        uuid,
                        crate::service::normalized_url(station.primary_url()).ok()
                    ])?;
                }
            }
            if version < 4 {
                tx.execute_batch(
                    "CREATE TABLE facets_cache (
                    kind INTEGER NOT NULL,value TEXT NOT NULL,n INTEGER NOT NULL CHECK(n>=0),
                    PRIMARY KEY(kind,value)) WITHOUT ROWID;",
                )?;
                facets::rebuild(&tx)?;
            }
            if version < 5 {
                tx.execute_batch(
                    "CREATE INDEX station_sort ON stations(popularity DESC,name,uuid,active,custom,bitrate);
                    CREATE INDEX station_name ON stations(name COLLATE NOCASE,uuid,active,custom,bitrate);
                    CREATE INDEX station_bitrate ON stations(bitrate DESC,name,uuid,active,custom);
                    CREATE INDEX station_active ON stations(active) WHERE active=1;
                    CREATE INDEX station_directory ON stations(active) WHERE active=1 AND custom=0;
                    DROP INDEX IF EXISTS station_popularity;
                    CREATE VIRTUAL TABLE station_search USING fts5(search,
                        content='stations',content_rowid='rowid',columnsize=0,detail=none,
                        tokenize='trigram case_sensitive 1');
                    INSERT INTO station_search(station_search) VALUES ('rebuild');
                    CREATE VIRTUAL TABLE station_search_terms USING fts5vocab(station_search,'row');
                    CREATE TABLE station_search_stats(term TEXT PRIMARY KEY,n INTEGER NOT NULL) WITHOUT ROWID;
                    INSERT INTO station_search_stats SELECT term,doc FROM station_search_terms;
                    INSERT INTO station_search_stats VALUES ('',(SELECT count(*) FROM stations));
                    CREATE TRIGGER station_search_insert AFTER INSERT ON stations BEGIN
                        INSERT INTO station_search(rowid,search) VALUES (new.rowid,new.search);
                    END;
                    CREATE TRIGGER station_search_delete AFTER DELETE ON stations BEGIN
                        INSERT INTO station_search(station_search,rowid,search) VALUES ('delete',old.rowid,old.search);
                    END;
                    CREATE TRIGGER station_search_update AFTER UPDATE OF search ON stations
                    WHEN old.search IS NOT new.search BEGIN
                        INSERT INTO station_search(station_search,rowid,search) VALUES ('delete',old.rowid,old.search);
                        INSERT INTO station_search(rowid,search) VALUES (new.rowid,new.search);
                    END;
                    ANALYZE;",
                )?;
            }
            if version < 6 {
                geo::migrate(&tx)?;
            }
            if version < 7 {
                revisions::migrate(&tx)?;
            }
            if version < 8 {
                tx.execute_batch(&format!(
                    "CREATE INDEX IF NOT EXISTS recording_started ON recordings({RECORDING_ORDER});"
                ))?;
            }
            if version < 9 {
                crate::podcast::migrate(&tx)?;
            }
            tx.pragma_update(None, "user_version", 9)?;
            tx.commit()?;
        }
        Ok(Self { conn })
    }

    pub fn upsert(&self, station: &Station) -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        Self::write_station(&tx, "stations", station)?;
        geo::changed(&tx)?;
        tx.commit()?;
        Ok(())
    }

    pub fn bootstrap(
        &mut self,
        rows: &[Station],
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<usize> {
        let tx = self.conn.transaction()?;
        for row in rows {
            if cancelled() {
                bail!(DomainError::new(ErrorCode::Cancelled));
            }
            Self::write_station_with_policy(&tx, "stations", row, false, false)?;
        }
        if cancelled() {
            bail!(DomainError::new(ErrorCode::Cancelled));
        }
        if rows.is_empty() {
            bail!(DomainError::new(ErrorCode::EmptyCatalog));
        }
        facets::rebuild(&tx)?;
        if cancelled() {
            bail!(DomainError::new(ErrorCode::Cancelled));
        }
        geo::changed(&tx)?;
        tx.commit()?;
        // Advisory maintenance must not fail an already published catalog.
        let _ = self.refresh_search_stats();
        Ok(rows.len())
    }

    fn write_station(conn: &Connection, table: &str, s: &Station) -> Result<()> {
        Self::write_station_with_policy(conn, table, s, false, table == "stations")
    }

    fn write_station_with_policy(
        conn: &Connection,
        table: &str,
        s: &Station,
        custom: bool,
        maintain_facets: bool,
    ) -> Result<()> {
        if s.stationuuid.is_empty() || s.name.trim().is_empty() {
            bail!(DomainError::new(ErrorCode::InvalidStation));
        }
        uuid::Uuid::parse_str(&s.stationuuid).context(DomainError::new(ErrorCode::InvalidUuid))?;
        let before = if maintain_facets {
            facets::station_values(conn, &s.stationuuid)?
        } else {
            Default::default()
        };
        let sql = format!("INSERT INTO {table} (uuid,name,search,country,languages,tags,bitrate,popularity,custom,active,json,normalized_url,geo_x,geo_y)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)
            ON CONFLICT(uuid) DO UPDATE SET name=excluded.name,search=excluded.search,
            country=excluded.country,languages=excluded.languages,tags=excluded.tags,
            bitrate=excluded.bitrate,popularity=excluded.popularity,custom=excluded.custom,active=excluded.active,json=excluded.json,normalized_url=excluded.normalized_url,geo_x=excluded.geo_x,geo_y=excluded.geo_y
            WHERE {table}.custom=0 OR {custom}");
        // Facet queries compare normalized prefixes with binary SQL equality.
        // Keep country codes uppercase and language/tag tokens Unicode lowercase
        // here, for both catalog imports and custom stations.
        let normalize = |value: &str| {
            value
                .split(',')
                .map(|v| v.trim().to_lowercase())
                .filter(|v| !v.is_empty())
                .collect::<Vec<_>>()
                .join(",")
        };
        let languages = normalize(&format!("{},{}", s.language, s.languagecodes));
        let coordinates = crate::geo::coordinates(s.geo_lat, s.geo_long);
        conn.prepare_cached(&sql)?.execute(params![
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
            serde_json::to_string(s)?,
            crate::service::normalized_url(s.primary_url()).ok(),
            coordinates.map(|p| p.0),
            coordinates.map(|p| p.1)
        ])?;
        if maintain_facets {
            facets::update(
                conn,
                &before,
                &facets::station_values(conn, &s.stationuuid)?,
            )?;
        }
        Ok(())
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        Self::write_meta(&self.conn, key, value)
    }

    fn write_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
        conn.execute(
            "INSERT INTO meta(key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value WHERE meta.value IS NOT excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub(crate) fn save_session(
        &mut self,
        preferences: &std::collections::BTreeMap<String, String>,
        plays: &[(String, String)],
    ) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (uuid, at) in plays {
            tx.execute(
                "INSERT INTO history(uuid,played_at) VALUES (?1,?2)",
                params![uuid, at],
            )?;
            let threshold = tx.last_insert_rowid().saturating_sub(1000);
            tx.execute("DELETE FROM history WHERE id<=?1", [threshold])?;
            Self::write_meta(&tx, "pref_last_station", uuid)?;
            Self::write_meta(&tx, "pref_last_media", "station")?;
        }
        for (key, value) in preferences {
            if let Some(id) = key.strip_prefix("podcast_progress:") {
                Self::write_podcast_progress(&tx, id, &serde_json::from_str(value)?)?;
                continue;
            }
            if let Some(id) = key.strip_prefix("podcast_played:") {
                tx.execute(
                    "INSERT INTO podcast_history(episode_id,played_at) VALUES (?1,?2)",
                    params![id, value],
                )?;
                Self::write_meta(&tx, "pref_last_episode", id)?;
                Self::write_meta(&tx, "pref_last_media", "episode")?;
                continue;
            }
            Self::write_meta(&tx, key, value)?;
        }
        tx.commit()?;
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
            CREATE TEMP TABLE incoming AS SELECT uuid,name,search,country,languages,tags,bitrate,popularity,custom,active,json,normalized_url,geo_x,geo_y FROM stations WHERE 0;
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
        self.finish_sync_cancellable(|| false)
    }

    pub(crate) fn finish_sync_cancellable(
        &mut self,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<usize> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let count: usize = tx.query_row("SELECT count(*) FROM incoming", [], |r| r.get(0))?;
        if count == 0 {
            bail!(DomainError::new(ErrorCode::EmptyCatalog));
        }
        let search_changed: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM incoming i LEFT JOIN stations s ON s.uuid=i.uuid
             WHERE s.uuid IS NULL OR (s.custom=0 AND s.search IS NOT i.search))",
            [],
            |r| r.get(0),
        )?;
        let facet_delta = facets::publication_delta(&tx)?;
        if cancelled() {
            bail!(DomainError::new(ErrorCode::Cancelled));
        }
        let removed = tx.execute(
            "UPDATE stations SET active=0 WHERE custom=0 AND active=1
             AND NOT EXISTS(SELECT 1 FROM incoming i WHERE i.uuid=stations.uuid)",
            [],
        )?;
        let updated = tx.execute(
            "INSERT INTO stations (uuid,name,search,country,languages,tags,bitrate,popularity,custom,active,json,normalized_url,geo_x,geo_y) SELECT uuid,name,search,country,languages,tags,bitrate,popularity,custom,active,json,normalized_url,geo_x,geo_y FROM incoming WHERE true
            ON CONFLICT(uuid) DO UPDATE SET name=excluded.name,search=excluded.search,
            country=excluded.country,languages=excluded.languages,tags=excluded.tags,
            bitrate=excluded.bitrate,popularity=excluded.popularity,active=1,json=excluded.json,normalized_url=excluded.normalized_url,geo_x=excluded.geo_x,geo_y=excluded.geo_y
            WHERE stations.custom=0 AND (
                stations.active<>1 OR stations.json IS NOT excluded.json OR
                stations.name IS NOT excluded.name OR stations.search IS NOT excluded.search OR
                stations.country IS NOT excluded.country OR stations.languages IS NOT excluded.languages OR
                stations.tags IS NOT excluded.tags OR stations.bitrate IS NOT excluded.bitrate OR
                stations.popularity IS NOT excluded.popularity OR stations.normalized_url IS NOT excluded.normalized_url OR
                stations.geo_x IS NOT excluded.geo_x OR stations.geo_y IS NOT excluded.geo_y)", [],
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO meta(key,value) VALUES ('last_sync',?1)",
            [chrono::Utc::now().to_rfc3339()],
        )?;
        facets::apply_delta(&tx, facet_delta)?;
        if cancelled() {
            bail!(DomainError::new(ErrorCode::Cancelled));
        }
        if removed + updated > 0 {
            geo::changed(&tx)?;
        }
        tx.commit()?;
        // Planner maintenance cannot turn a successful publication into a
        // reported failure after its transaction has already committed.
        if search_changed {
            let _ = self.refresh_search_stats();
        }
        if removed + updated > 0 {
            let _ = self.conn.execute_batch("PRAGMA optimize;");
        }
        Ok(count)
    }

    /// Connection activity for diagnostics, including preferences and SQL
    /// maintenance. This is not a list cache key; use [`Self::list_revision`].
    /// Values are meaningful only for repeated reads on the same connection.
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
            .context(DomainError::new(ErrorCode::StationNotFound))?;
        let mut s: Station = serde_json::from_str(&row.0)?;
        s.active = row.1;
        s.favorite = row.2;
        Ok(s)
    }

    fn conditions(&self, filter: &Filter) -> (String, Vec<Value>, &'static str) {
        self.conditions_with_spatial(filter, true)
    }

    fn conditions_with_spatial(
        &self,
        filter: &Filter,
        spatial: bool,
    ) -> (String, Vec<Value>, &'static str) {
        let mut clauses = vec![
            "(s.active=1 OR s.custom=1 OR EXISTS(SELECT 1 FROM favorites f WHERE f.uuid=s.uuid))"
                .to_owned(),
        ];
        let mut args = vec![];
        // Token filters still use instr. Without a country index to narrow
        // them, avoid random table lookups through a sorting index.
        let mut source =
            if filter.country.is_none() && (filter.language.is_some() || filter.tag.is_some()) {
                "stations s NOT INDEXED"
            } else {
                "stations s"
            };
        if filter.favorites {
            clauses.push("EXISTS(SELECT 1 FROM favorites f WHERE f.uuid=s.uuid)".into());
        }
        if filter.custom {
            clauses.push("s.custom=1".into());
        }
        if filter.history {
            clauses[0] = "1".into();
        }
        let mut text_candidates = false;
        let query = filter.query.trim();
        if !query.is_empty() {
            // Resolve localized names at query time so existing catalogs do not
            // need a resync or a rewrite of their original station metadata.
            let codes: Vec<_> = crate::territories::matching_codes(query).collect();
            let normalized = query.to_lowercase();
            source = "stations s NOT INDEXED";
            if !filter.history
                && let Some(candidate) = self.trigram_candidate(&normalized)
            {
                text_candidates = true;
                // A literal trigram narrows candidates; instr below checks
                // the complete query without decoding positional postings.
                let mut candidates =
                    "SELECT rowid FROM station_search WHERE station_search MATCH ?".to_owned();
                args.push(candidate.into());
                if !codes.is_empty() {
                    candidates.push_str(&format!(
                        " UNION SELECT rowid FROM stations WHERE country IN ({})",
                        vec!["?"; codes.len()].join(",")
                    ));
                    args.extend(codes.iter().map(|code| Value::from((*code).to_owned())));
                }
                clauses.push(
                    if codes.is_empty()
                        && !matches!(filter.geo, Some(crate::geo::GeoFilter::Cell(_)))
                    {
                        source =
                            "station_search CROSS JOIN stations s ON s.rowid=station_search.rowid";
                        "station_search MATCH ?".to_owned()
                    } else {
                        format!("s.rowid IN ({candidates})")
                    },
                );
            }
            if codes.is_empty() {
                clauses.push("instr(s.search,?)>0".into());
            } else {
                let placeholders = vec!["?"; codes.len()].join(",");
                clauses.push(format!(
                    "(instr(s.search,?)>0 OR s.country IN ({placeholders}))"
                ));
            }
            args.push(normalized.into());
            args.extend(codes.into_iter().map(|code| Value::from(code.to_owned())));
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
        if filter.min_bitrate > 0 {
            clauses.push("s.bitrate>=?".into());
            args.push((filter.min_bitrate as i64).into());
        }
        if let Some(geo) = filter.geo {
            clauses[0] = "s.active=1".into();
            match geo {
                crate::geo::GeoFilter::Missing => clauses.push("s.geo_x IS NULL".into()),
                crate::geo::GeoFilter::Cell(bounds) if spatial => {
                    clauses.push(geo::predicate(bounds, &mut args));
                    // A full longitude scan through a non-covering spatial
                    // index turns filtered world maps into random table reads.
                    if (!text_candidates && bounds.width < crate::geo::WORLD_WIDTH)
                        || geo::plain(filter)
                    {
                        source = "stations s INDEXED BY station_geo";
                    }
                    if filter.query.trim().is_empty()
                        && let Some(country) = filter.country.as_ref().filter(|c| !c.is_empty())
                        && self
                            .conn
                            .prepare_cached("SELECT n FROM facets_cache WHERE kind=0 AND value=?1")
                            .and_then(|mut stmt| {
                                stmt.query_row([country.to_uppercase()], |r| r.get::<_, usize>(0))
                                    .optional()
                            })
                            .is_ok_and(|n| n.unwrap_or(0) <= geo::SORT_THRESHOLD)
                    {
                        // A small country is an upper bound on any of its map
                        // cells. This hint only chooses an index, never rows.
                        source = "stations s INDEXED BY station_country";
                    }
                }
                crate::geo::GeoFilter::Cell(_) => {}
            }
        }
        if filter.history {
            // History retains at most 1000 entries. Drive the query from its
            // distinct stations rather than probing history for every catalog
            // row. CROSS JOIN preserves this bounded outer loop.
            source = "(SELECT uuid,max(id) AS last_id FROM history GROUP BY uuid) recent
                      CROSS JOIN stations s ON s.uuid=recent.uuid";
        }
        (clauses.join(" AND "), args, source)
    }

    /// One rare trigram supplies candidates; instr verifies the full literal.
    /// Avoid positional phrase decoding for every row of a broad match.
    fn trigram_candidate(&self, normalized: &str) -> Option<String> {
        if normalized.chars().count() < 3 || normalized.contains('\0') {
            return None;
        }
        let chars: Vec<_> = normalized.chars().collect();
        let stride = ((chars.len() - 2) / 16).max(1);
        let mut best = (usize::MAX, chars[..3].iter().collect::<String>());
        // Bound vocabulary lookups for unusually long command-line queries.
        let terms: std::collections::BTreeSet<String> = chars
            .windows(3)
            .step_by(stride)
            .take(16)
            .map(|part| part.iter().collect())
            .collect();
        for term in terms {
            let frequency = self
                .conn
                .prepare_cached("SELECT n FROM station_search_stats WHERE term=?1")
                .and_then(|mut s| s.query_row([&term], |r| r.get::<_, usize>(0)).optional());
            // If statistics are unavailable, the literal remains a valid
            // candidate. Query errors are reported by the actual FTS query.
            let frequency = match frequency {
                Ok(Some(n)) => n,
                Ok(None) => 0,
                Err(_) => continue,
            };
            if frequency < best.0 {
                best = (frequency, term);
            }
            if frequency == 0 {
                break;
            }
        }
        let total: usize = self
            .conn
            .query_row(
                "SELECT n FROM station_search_stats WHERE term=''",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        // Statistics only choose a query strategy. Stale hints after a single
        // station mutation never change the exact substring predicate.
        if total > 0 && best.0 > total / 4 {
            return None;
        }
        Some(format!("\"{}\"", best.1.replace('"', "\"\"")))
    }

    fn refresh_search_stats(&self) -> Result<()> {
        // A single read statement pins a consistent snapshot for the vocabulary
        // and row count. Collect before taking the main database's write lock:
        // even a small vocabulary requires scanning FTS postings to count docs.
        let stats = {
            let mut stmt = self.conn.prepare(
                "SELECT term,doc FROM station_search_terms
                UNION ALL SELECT '',count(*) FROM stations",
            )?;
            stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        // Publish all hints atomically. Concurrent station changes may make
        // them stale, which only affects the choice of search strategy.
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM station_search_stats", [])?;
        {
            let mut stmt = tx.prepare("INSERT INTO station_search_stats VALUES (?1,?2)")?;
            for (term, count) in stats {
                stmt.execute(params![term, count])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn search_order(filter: &Filter) -> &'static str {
        if filter.history {
            "recent.last_id DESC"
        } else {
            match filter.sort {
                crate::model::Sort::Name => "s.name COLLATE NOCASE,s.uuid",
                crate::model::Sort::Bitrate => "s.bitrate DESC,s.name,s.uuid",
                crate::model::Sort::Votes => "json_extract(s.json,'$.votes') DESC,s.name,s.uuid",
                crate::model::Sort::Clickcount => "s.popularity DESC,s.name,s.uuid",
                crate::model::Sort::Random => "random()",
            }
        }
    }

    /// Locate a station without loading every preceding page into memory.
    pub fn station_position(&self, uuid: &str, filter: &Filter) -> Result<Option<usize>> {
        use crate::model::Sort;
        let (conditions, mut args, source) = self.conditions(filter);
        if !filter.history && matches!(filter.sort, Sort::Clickcount | Sort::Bitrate | Sort::Name) {
            let tx = self.conn.unchecked_transaction()?;
            let mut target_args = args.clone();
            target_args.push(uuid.to_owned().into());
            let target = tx
                .query_row(
                    &format!(
                        "SELECT s.popularity,s.bitrate,s.name FROM {source}
                    WHERE {conditions} AND s.uuid=?"
                    ),
                    params_from_iter(target_args),
                    |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, i64>(1)?,
                            r.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()?;
            let Some((popularity, bitrate, name)) = target else {
                return Ok(None);
            };
            let count = |predicate: &str, values: Vec<Value>| -> Result<usize> {
                let mut values_with_filter = args.clone();
                values_with_filter.extend(values);
                Ok(tx
                    .prepare_cached(&format!(
                        "SELECT count(*) FROM {source}
                    WHERE {conditions} AND ({predicate})"
                    ))?
                    .query_row(params_from_iter(values_with_filter), |r| r.get(0))?)
            };
            // Separate ranges let each count use the leading sort key. DESC
            // on the numeric key and ASC on ties cannot use one tuple '<'.
            let position = if filter.sort == Sort::Name {
                count("s.name COLLATE NOCASE < ?", vec![name.clone().into()])?
                    + count(
                        "s.name COLLATE NOCASE = ? AND s.uuid < ?",
                        vec![name.into(), uuid.to_owned().into()],
                    )?
            } else {
                let (column, value) = if filter.sort == Sort::Bitrate {
                    ("bitrate", bitrate)
                } else {
                    ("popularity", popularity)
                };
                count(&format!("s.{column}>?"), vec![value.into()])?
                    + count(
                        &format!("s.{column}=? AND (s.name,s.uuid)<(?,?)"),
                        vec![value.into(), name.into(), uuid.to_owned().into()],
                    )?
            };
            tx.commit()?;
            return Ok(Some(position));
        }
        let order = Self::search_order(filter);
        let sql = format!(
            "WITH ranked AS (
            SELECT s.uuid,row_number() OVER (ORDER BY {order})-1 AS position
            FROM {source} WHERE {conditions}
        ) SELECT position FROM ranked WHERE uuid=?"
        );
        args.push(uuid.to_owned().into());
        Ok(self
            .conn
            .query_row(&sql, params_from_iter(args), |row| row.get::<_, i64>(0))
            .optional()?
            .map(|position| position as usize))
    }

    pub fn search(&self, filter: &Filter) -> Result<Vec<Station>> {
        // The bounded geographic probe and its page must see the same rows,
        // including when called directly outside the TUI reader's snapshot.
        let _snapshot = if filter.geo.is_some() && self.conn.is_autocommit() {
            Some(self.conn.unchecked_transaction()?)
        } else {
            None
        };
        let (mut conditions, mut args, mut source) = self.conditions(filter);
        // Probe matching row IDs, not just geographic density: a city may have
        // thousands of stations but only a few matching the user's filters.
        // Small result sets reuse the probe; dense sets use a sort index and a
        // compact spatial membership set to avoid sorting random table reads.
        if let Some(crate::geo::GeoFilter::Cell(bounds)) = filter.geo
            && !filter.history
            && let Some(index) = match filter.sort {
                crate::model::Sort::Clickcount => Some("stations s INDEXED BY station_sort"),
                crate::model::Sort::Name => Some("stations s INDEXED BY station_name"),
                crate::model::Sort::Bitrate => Some("stations s INDEXED BY station_bitrate"),
                _ => None,
            }
        {
            let probe = format!(
                "SELECT s.rowid FROM {source} WHERE {conditions} LIMIT {}",
                geo::SORT_THRESHOLD + 1
            );
            // Unfiltered probes are covering index scans: count inside SQLite
            // instead of copying thousands of IDs we will immediately discard.
            let ids = if geo::plain(filter) {
                None
            } else {
                Some(
                    self.conn
                        .prepare_cached(&probe)?
                        .query_map(params_from_iter(&args), |r| r.get::<_, i64>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?,
                )
            };
            let n = if let Some(ids) = &ids {
                ids.len()
            } else {
                self.conn
                    .prepare_cached(&format!("SELECT count(*) FROM ({probe})"))?
                    .query_row(params_from_iter(&args), |r| r.get::<_, usize>(0))?
            };
            if n == 0 {
                return Ok(vec![]);
            }
            if n <= geo::SORT_THRESHOLD
                && let Some(ids) = ids
            {
                conditions = format!("s.rowid IN ({})", vec!["?"; ids.len()].join(","));
                args = ids.into_iter().map(Value::Integer).collect();
                source = "stations s";
            } else if n > geo::SORT_THRESHOLD {
                let mut spatial_args = vec![];
                let spatial = geo::predicate(bounds, &mut spatial_args);
                let candidates = format!(
                    "SELECT s.rowid FROM stations s INDEXED BY station_geo WHERE s.active=1 AND {spatial}"
                );
                // Membership already checks coordinates. Keep the remaining
                // predicates so the sort scan fetches table rows only when a
                // non-covering filter actually needs them.
                (conditions, args, _) = self.conditions_with_spatial(filter, false);
                conditions = format!("s.rowid IN ({candidates}) AND {conditions}");
                spatial_args.extend(args);
                args = spatial_args;
                source = index;
            }
        }
        let order = Self::search_order(filter);
        // The random page has already been shuffled. Other sorts still need
        // their outer order, including history which overrides filter.sort.
        let page_order = if filter.history {
            " ORDER BY page.last_id DESC".to_owned()
        } else if filter.sort == crate::model::Sort::Random {
            String::new()
        } else {
            format!(" ORDER BY {order}")
        };
        let history_column = if filter.history {
            ",recent.last_id"
        } else {
            ""
        };
        // Sort compact row references, fetching JSON only for the selected page.
        let sql = format!(
            "WITH page AS MATERIALIZED (
            SELECT s.rowid AS id{history_column} FROM {source}
            WHERE {conditions} ORDER BY {order} LIMIT ? OFFSET ?)
            SELECT s.json,s.active,EXISTS(SELECT 1 FROM favorites f WHERE f.uuid=s.uuid)
            FROM page JOIN stations s ON s.rowid=page.id{page_order}"
        );
        args.push((filter.limit.clamp(1, 1000) as i64).into());
        args.push((filter.offset.min(i64::MAX as usize) as i64).into());
        let mut stmt = self.conn.prepare_cached(&sql)?;
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
        let (mut conditions, mut args, source) = self.conditions(filter);
        if !exclude.is_empty() {
            let placeholders = vec!["?"; exclude.len()].join(",");
            conditions.push_str(&format!(" AND s.uuid NOT IN ({placeholders})"));
            args.extend(exclude.iter().cloned().map(Value::Text));
        }
        let sql = format!(
            "SELECT s.uuid FROM {source}
            WHERE {conditions} ORDER BY random() LIMIT 1"
        );
        let selected = self
            .conn
            .query_row(&sql, params_from_iter(args), |r| r.get::<_, String>(0))
            .optional()?;
        selected.map(|id| self.station(&id)).transpose()
    }

    pub fn facets(
        &self,
        kind: crate::model::FacetKind,
        prefix: &str,
    ) -> Result<Vec<crate::model::Facet>> {
        self.facets_limited(kind, prefix, usize::MAX)
    }

    pub fn facets_limited(
        &self,
        kind: crate::model::FacetKind,
        prefix: &str,
        limit: usize,
    ) -> Result<Vec<crate::model::Facet>> {
        use crate::model::{Facet, FacetKind};
        let (kind, prefix) = match kind {
            FacetKind::Country => (0, prefix.trim().to_uppercase()),
            FacetKind::Language => (1, prefix.trim().to_lowercase()),
            FacetKind::Tag => (2, prefix.trim().to_lowercase()),
        };
        let codes = if kind == 0 {
            crate::territories::matching_prefix_codes(&prefix).collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        // Match localized names before LIMIT, so countries beyond the first
        // candidate page remain reachable. Values and counts stay catalog data.
        let localized = if codes.is_empty() {
            String::new()
        } else {
            let placeholders = (4..4 + codes.len())
                .map(|index| format!("?{index}"))
                .collect::<Vec<_>>()
                .join(",");
            format!(" OR value IN ({placeholders})")
        };
        let mut args: Vec<Value> = vec![
            kind.into(),
            prefix.into(),
            (limit.min(i64::MAX as usize) as i64).into(),
        ];
        args.extend(codes.into_iter().map(|code| Value::from(code.to_owned())));
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT value,n FROM facets_cache WHERE kind=?1
             AND (substr(value,1,length(?2))=?2{localized}) ORDER BY value LIMIT ?3",
        ))?;
        let rows = stmt.query_map(params_from_iter(args), |r| {
            Ok(Facet {
                value: r.get(0)?,
                count: r.get(1)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn favorite(&self, uuid: &str, add: bool) -> Result<()> {
        self.station(uuid)?;
        if add {
            self.conn.execute(
                "INSERT OR IGNORE INTO favorites(uuid,created_at) VALUES (?1,?2)",
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
        .context(DomainError::new(ErrorCode::StationNotFound))?;
        let removed = tx.execute("DELETE FROM favorites WHERE uuid=?1", [uuid])?;
        let added = removed == 0;
        if added {
            tx.execute(
                "INSERT INTO favorites(uuid,created_at) VALUES (?1,?2)",
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
        let threshold = self.conn.last_insert_rowid().saturating_sub(1000);
        self.conn
            .execute("DELETE FROM history WHERE id <= ?1", [threshold])?;
        Ok(())
    }

    pub fn add_backup(&self, uuid: &str, url: &str) -> Result<()> {
        crate::config::validate_url(url)?;
        self.conn.execute(
            "INSERT OR IGNORE INTO streams(uuid,url) VALUES (?1,?2)",
            params![uuid, url],
        )?;
        Ok(())
    }

    pub fn save_custom(&mut self, station: &Station, backups: Option<&[String]>) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let key = crate::service::normalized_url(station.primary_url())?;
        if let Some(uuid) = tx.query_row(
            "SELECT uuid FROM stations WHERE active=1 AND normalized_url=?1 AND uuid<>?2 LIMIT 1",
            params![key, station.stationuuid], |r| r.get::<_, String>(0),
        ).optional()? {
            bail!(DomainError::new(ErrorCode::DuplicateStation).with("uuid", uuid));
        }
        Self::write_station_with_policy(&tx, "stations", station, true, true)?;
        if let Some(backups) = backups {
            tx.execute("DELETE FROM streams WHERE uuid=?1", [&station.stationuuid])?;
            for url in backups {
                crate::config::validate_url(url)?;
                tx.execute(
                    "INSERT OR IGNORE INTO streams(uuid,url) VALUES (?1,?2)",
                    params![station.stationuuid, url],
                )?;
            }
        }
        geo::changed(&tx)?;
        tx.commit()?;
        Ok(())
    }

    pub fn remove_custom(&mut self, uuid: &str, keep_references: bool) -> Result<bool> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let json: String = tx
            .query_row(
                "SELECT json FROM stations WHERE uuid=?1 AND custom=1",
                [uuid],
                |r| r.get(0),
            )
            .optional()?
            .context(DomainError::new(ErrorCode::NotCustom))?;
        let mut station: Station = serde_json::from_str(&json)?;
        let referenced: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM favorites WHERE uuid=?1) OR EXISTS(SELECT 1 FROM history WHERE uuid=?1) OR EXISTS(SELECT 1 FROM recordings WHERE json_extract(json,'$.stationuuid')=?1)", [uuid], |r| r.get(0))?;
        tx.execute("DELETE FROM streams WHERE uuid=?1", [uuid])?;
        if referenced || keep_references {
            station.custom = false;
            station.active = false;
            station.url.clear();
            station.url_resolved.clear();
            Self::write_station_with_policy(&tx, "stations", &station, true, true)?;
        } else {
            let before = facets::station_values(&tx, uuid)?;
            tx.execute("DELETE FROM stations WHERE uuid=?1", [uuid])?;
            facets::update(&tx, &before, &Default::default())?;
        }
        geo::changed(&tx)?;
        tx.commit()?;
        Ok(referenced || keep_references)
    }

    pub fn update_backups(
        &mut self,
        uuid: &str,
        add: &[String],
        remove: &[String],
    ) -> Result<Vec<String>> {
        self.station(uuid)?;
        let tx = self.conn.transaction()?;
        for url in remove {
            tx.execute(
                "DELETE FROM streams WHERE uuid=?1 AND url=?2",
                params![uuid, url],
            )?;
        }
        for url in add {
            crate::config::validate_url(url)?;
            tx.execute(
                "INSERT OR IGNORE INTO streams(uuid,url) VALUES (?1,?2)",
                params![uuid, url],
            )?;
        }
        tx.commit()?;
        self.backups(uuid)
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
        let (conditions, args, source) = self.conditions(filter);
        Ok(self
            .conn
            .prepare_cached(&format!("SELECT count(*) FROM {source} WHERE {conditions}"))?
            .query_row(params_from_iter(args), |r| r.get(0))?)
    }

    pub fn recordings(&self, limit: usize) -> Result<Vec<Recording>> {
        self.recordings_page(limit, 0)
    }

    pub fn recordings_count(&self) -> Result<usize> {
        Ok(self
            .conn
            .query_row("SELECT count(*) FROM recordings", [], |r| r.get(0))?)
    }

    pub fn recording(&self, id: &str) -> Result<Recording> {
        let row = self
            .conn
            .query_row(
                "SELECT json,recovery_pending FROM recordings WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .context(DomainError::new(ErrorCode::RecordingNotFound))?;
        Self::read_recording(row)
    }

    pub fn remove_recording_index(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM recordings WHERE id=?1", [id])?;
        Ok(())
    }

    pub fn recordings_page(&self, limit: usize, offset: usize) -> Result<Vec<Recording>> {
        let mut stmt = self.conn.prepare_cached(&format!("SELECT json,recovery_pending FROM recordings ORDER BY {RECORDING_ORDER} LIMIT ?1 OFFSET ?2"))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_order_index_migrates_without_losing_old_or_malformed_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("radio.db");
        let store = Store::open(&path).unwrap();
        for (id, at) in [
            ("a", "2026-01-02"),
            ("b", "2026-01-02"),
            ("old", "2026-01-01"),
        ] {
            store
                .save_recording(&Recording {
                    id: id.into(),
                    stationuuid: "station".into(),
                    station_name: "Station".into(),
                    track: None,
                    source_url_resolved: "http://localhost/live".into(),
                    started_at: at.into(),
                    stopped_at: None,
                    media_duration_ms: Some(1000),
                    codec: "mp3".into(),
                    container: "mp3".into(),
                    path: id.into(),
                    final_path: None,
                    size_bytes: 1,
                    status: "complete".into(),
                    error: None,
                    reconnects: 0,
                    recovery_pending: false,
                })
                .unwrap();
        }
        store
            .conn
            .execute_batch(
                "DROP INDEX recording_started; PRAGMA user_version=7;
            INSERT INTO recordings(id,status,json) VALUES ('broken','complete','invalid');",
            )
            .unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.recordings_count().unwrap(), 4);
        let page = store.recordings_page(2, 0).unwrap();
        assert_eq!(
            page.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["b", "a"]
        );
        assert_eq!(store.recordings_page(1, 2).unwrap()[0].id, "old");
        assert!(store.recordings_page(1, 3).is_err());
        let plan: Vec<String> = store.conn.prepare(&format!(
            "EXPLAIN QUERY PLAN SELECT json,recovery_pending FROM recordings ORDER BY {RECORDING_ORDER} LIMIT 100"
        )).unwrap().query_map([], |r| r.get(3)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        assert!(plan.iter().any(|line| line.contains("recording_started")));
        assert!(!plan.iter().any(|line| line.contains("TEMP B-TREE")));
    }

    #[test]
    fn territory_queries_union_indexed_country_and_trigram_candidates() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("radio.db")).unwrap();
        let rows: Vec<_> = (0..100)
            .map(|i| Station {
                stationuuid: uuid::Uuid::new_v4().to_string(),
                name: match i {
                    1 => "爱尔兰音乐".into(),
                    2 => "爱尔兰广播".into(),
                    _ => format!("catalog station {i}"),
                },
                countrycode: if i == 0 || i == 2 { "IE" } else { "FR" }.into(),
                clickcount: i,
                active: true,
                ..Station::default()
            })
            .collect();
        store.bootstrap(&rows, || false).unwrap();
        let filter = Filter {
            query: " 爱尔兰 ".into(),
            limit: 100,
            ..Filter::default()
        };
        let actual: Vec<_> = store
            .search(&filter)
            .unwrap()
            .into_iter()
            .map(|row| row.stationuuid)
            .collect();
        assert_eq!(actual, [2, 1, 0].map(|i| rows[i].stationuuid.clone()));
        assert_eq!(store.search_count(&filter).unwrap(), 3);
        let (conditions, args, source) = store.conditions(&filter);
        let plan: Vec<String> = store
            .conn
            .prepare(&format!(
                "EXPLAIN QUERY PLAN SELECT s.rowid FROM {source} WHERE {conditions}"
            ))
            .unwrap()
            .query_map(params_from_iter(args), |r| r.get(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        for index in [
            "SEARCH s USING INTEGER PRIMARY KEY (rowid=?)",
            "station_search VIRTUAL TABLE INDEX",
            "station_country (country=?)",
        ] {
            assert!(plan.iter().any(|step| step.contains(index)), "{plan:?}");
        }
    }

    #[test]
    fn search_statistics_collection_allows_a_concurrent_writer() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("radio.db");
        let mut store = Store::open(&path).unwrap();
        let rows: Vec<_> = (0..256)
            .map(|i| Station {
                stationuuid: uuid::Uuid::new_v4().to_string(),
                name: format!("station {i:05} jazz"),
                active: true,
                ..Station::default()
            })
            .collect();
        store.bootstrap(&rows, || false).unwrap();
        let writer = Connection::open(&path).unwrap();
        writer.busy_timeout(std::time::Duration::ZERO).unwrap();
        let attempted = Arc::new(AtomicBool::new(false));
        let succeeded = Arc::new(AtomicBool::new(false));
        let (attempt, success) = (attempted.clone(), succeeded.clone());
        store.conn.progress_handler(
            1000,
            Some(move || {
                if !attempt.swap(true, Ordering::SeqCst) {
                    success.store(
                        writer
                            .execute("INSERT INTO meta VALUES ('concurrent','write')", [])
                            .is_ok(),
                        Ordering::SeqCst,
                    );
                }
                false
            }),
        );
        store.refresh_search_stats().unwrap();
        store.conn.progress_handler(0, None::<fn() -> bool>);
        assert!(attempted.load(Ordering::SeqCst));
        assert!(succeeded.load(Ordering::SeqCst));
        let actual: usize = store
            .conn
            .query_row(
                "SELECT n FROM station_search_stats WHERE term=''",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(actual, rows.len());
        let differences: usize = store
            .conn
            .query_row(
                "SELECT count(*) FROM (
                    SELECT term,doc FROM station_search_terms
                    EXCEPT SELECT term,n FROM station_search_stats WHERE term<>'')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(differences, 0);
    }

    #[test]
    fn failed_statistics_refresh_preserves_hints_and_successful_publication() {
        for sync in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut store = Store::open(&dir.path().join("radio.db")).unwrap();
            let row = |name: &str| Station {
                stationuuid: uuid::Uuid::new_v4().to_string(),
                name: name.into(),
                tags: name.into(),
                active: true,
                ..Station::default()
            };
            store.bootstrap(&[row("originalneedle")], || false).unwrap();
            let stats = || {
                store
                    .conn
                    .prepare("SELECT term,n FROM station_search_stats ORDER BY term")
                    .unwrap()
                    .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
                    .unwrap()
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .unwrap()
            };
            let before = stats();
            store
                .conn
                .execute_batch(
                    "CREATE TRIGGER reject_stats BEFORE INSERT ON station_search_stats
                BEGIN SELECT RAISE(ABORT,'statistics unavailable'); END;",
                )
                .unwrap();
            let replacement = row("replacementneedle");
            if sync {
                store.begin_sync().unwrap();
                store.stage(&[replacement]).unwrap();
                assert_eq!(store.finish_sync().unwrap(), 1);
                assert!(store.meta("last_sync").unwrap().is_some());
            } else {
                assert_eq!(store.bootstrap(&[replacement], || false).unwrap(), 1);
            }
            assert_eq!(
                store
                    .search_count(&Filter {
                        query: "replacementneedle".into(),
                        ..Filter::default()
                    })
                    .unwrap(),
                1
            );
            assert_eq!(
                store
                    .facets(crate::model::FacetKind::Tag, "replacement")
                    .unwrap()[0]
                    .count,
                1
            );
            let after: Vec<_> = store
                .conn
                .prepare("SELECT term,n FROM station_search_stats ORDER BY term")
                .unwrap()
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            assert_eq!(after, before);
        }
    }

    #[test]
    fn read_snapshot_pins_revision_before_a_concurrent_commit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("radio.db");
        let writer = Store::open(&path).unwrap();
        let reader = Store::open_reader(&path).unwrap();
        let revision = reader
            .read_snapshot(|store| {
                let revision = store.list_revision(&Filter::default())?;
                writer.upsert(&Station {
                    stationuuid: uuid::Uuid::new_v4().to_string(),
                    name: "new row".into(),
                    active: true,
                    ..Station::default()
                })?;
                assert_eq!(store.list_revision(&Filter::default())?, revision);
                assert_eq!(store.count()?, 0);
                Ok(revision)
            })
            .unwrap();
        assert_ne!(reader.list_revision(&Filter::default()).unwrap(), revision);
        assert_eq!(reader.count().unwrap(), 1);
    }

    #[test]
    fn cancellation_before_sync_commit_rolls_back_all_derived_data() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("radio.db")).unwrap();
        let row = |name: &str, tag: &str| Station {
            stationuuid: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            tags: tag.into(),
            active: true,
            ..Station::default()
        };
        store.upsert(&row("originalneedle", "originaltag")).unwrap();
        store.begin_sync().unwrap();
        store
            .stage(&[row("replacementneedle", "replacementtag")])
            .unwrap();
        assert!(store.finish_sync_cancellable(|| true).is_err());
        assert_eq!(store.count().unwrap(), 1);
        for (query, expected) in [("originalneedle", 1), ("replacementneedle", 0)] {
            assert_eq!(
                store
                    .search_count(&Filter {
                        query: query.into(),
                        ..Filter::default()
                    })
                    .unwrap(),
                expected
            );
        }
        assert_eq!(
            store.facets(crate::model::FacetKind::Tag, "").unwrap()[0].value,
            "originaltag"
        );
        assert!(store.meta("last_sync").unwrap().is_none());
    }

    #[test]
    fn sync_writes_only_changed_stations_and_facet_memberships() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("radio.db")).unwrap();
        let row = |name: &str| Station {
            stationuuid: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            tags: "jazz,jazz".into(),
            language: "english,en".into(),
            countrycode: "US".into(),
            active: true,
            ..Station::default()
        };
        let unchanged = row("unchanged");
        let mut changed = row("changed");
        let removed = row("removed");
        let mut revived = row("revived");
        revived.active = false;
        let mut custom = row("custom");
        custom.custom = true;
        for station in [&unchanged, &changed, &removed, &revived, &custom] {
            store.upsert(station).unwrap();
        }
        store
            .conn
            .execute_batch(
                "CREATE TABLE station_writes(uuid TEXT);
             CREATE TRIGGER audit_station_writes AFTER UPDATE ON stations
             BEGIN INSERT INTO station_writes VALUES(new.uuid); END;",
            )
            .unwrap();
        changed.tags = "rock,rock".into();
        changed.countrycode = "FR".into();
        revived.active = true;
        let mut collision = custom.clone();
        collision.custom = false;
        collision.tags = "wrong".into();
        let incoming = [unchanged.clone(), changed, revived, collision, row("new")];
        store.begin_sync().unwrap();
        store.stage(&incoming).unwrap();
        store.finish_sync().unwrap();
        let writes: Vec<String> = store
            .conn
            .prepare("SELECT uuid FROM station_writes")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(writes.len(), 3);
        assert!(!writes.contains(&unchanged.stationuuid));
        assert!(!writes.contains(&custom.stationuuid));
        assert!(!store.station(&removed.stationuuid).unwrap().active);
        assert_eq!(
            store.station(&custom.stationuuid).unwrap().tags,
            "jazz,jazz"
        );
        let facets = || {
            store
                .conn
                .prepare("SELECT kind,value,n FROM facets_cache ORDER BY kind,value")
                .unwrap()
                .query_map([], |r| {
                    Ok((
                        r.get::<_, u8>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        let delta_counts = facets();
        facets::rebuild(&store.conn).unwrap();
        assert_eq!(facets(), delta_counts);
        let geo_revision = store.geo_revision().unwrap();
        store.begin_sync().unwrap();
        store.stage(&incoming).unwrap();
        let before = store.conn.total_changes();
        store.finish_sync().unwrap();
        // Only last_sync changes: stations, indexes, facets, geographic and
        // search statistics remain untouched for an identical snapshot.
        assert_eq!(store.conn.total_changes() - before, 1);
        assert_eq!(store.geo_revision().unwrap(), geo_revision);
    }
}
