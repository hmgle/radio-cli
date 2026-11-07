use super::*;
use crate::store::Store;
use anyhow::Result;
use rusqlite::{OptionalExtension, TransactionBehavior, params};

pub(crate) fn migrate(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS podcast_feeds(id TEXT PRIMARY KEY,url TEXT NOT NULL UNIQUE,subscribed INTEGER NOT NULL,json TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS podcast_aliases(url TEXT PRIMARY KEY,feed_id TEXT NOT NULL REFERENCES podcast_feeds(id));
    CREATE TABLE IF NOT EXISTS podcast_episodes(id TEXT PRIMARY KEY,feed_id TEXT NOT NULL REFERENCES podcast_feeds(id),identity TEXT NOT NULL,json TEXT NOT NULL,UNIQUE(feed_id,identity));
    CREATE TABLE IF NOT EXISTS podcast_progress(episode_id TEXT PRIMARY KEY REFERENCES podcast_episodes(id),position_ms INTEGER NOT NULL,duration_ms INTEGER,completed INTEGER NOT NULL,updated_at TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS podcast_history(id INTEGER PRIMARY KEY,episode_id TEXT NOT NULL REFERENCES podcast_episodes(id),played_at TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS podcast_downloads(episode_id TEXT PRIMARY KEY REFERENCES podcast_episodes(id),json TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS podcast_queue(episode_id TEXT PRIMARY KEY REFERENCES podcast_episodes(id),position INTEGER NOT NULL);
    CREATE INDEX IF NOT EXISTS podcast_feed_episodes ON podcast_episodes(feed_id);")?;
    Ok(())
}
impl Store {
    pub fn podcast_feeds(&self) -> Result<Vec<Feed>> {
        let mut stmt = self.conn.prepare("SELECT json FROM podcast_feeds WHERE subscribed=1 ORDER BY json_extract(json,'$.title'),id")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }
    pub fn podcast_feed(&self, id: &str) -> Result<Feed> {
        let json: Option<String> = self
            .conn
            .query_row("SELECT json FROM podcast_feeds WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(serde_json::from_str(
            &json.ok_or_else(|| error(crate::t!("podcast_not_found")))?,
        )?)
    }
    pub fn podcast_by_url(&self, url: &str) -> Result<Option<Feed>> {
        let id: Option<String> = self.conn.query_row("SELECT id FROM podcast_feeds WHERE url=?1 UNION SELECT feed_id FROM podcast_aliases WHERE url=?1 LIMIT 1",[url],|r|r.get(0)).optional()?;
        id.map(|id| self.podcast_feed(&id)).transpose()
    }
    pub fn podcast_subscribe(&self, preview: &Preview, requested: &str) -> Result<Feed> {
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let mut feed = self
            .podcast_by_url(requested)?
            .or(self.podcast_by_url(&preview.feed.url)?)
            .unwrap_or_else(|| Feed {
                id: uuid::Uuid::new_v4().to_string(),
                ..Default::default()
            });
        let id = feed.id.clone();
        let revision = feed.revision.saturating_add(1);
        feed = preview.feed.clone();
        feed.revision = revision;
        feed.id = id;
        feed.subscribed = true;
        self.podcast_write(&feed)?;
        for url in [requested, &feed.url] {
            self.conn.execute(
                "INSERT INTO podcast_aliases VALUES (?1,?2) ON CONFLICT(url) DO NOTHING",
                params![url, feed.id],
            )?;
        }
        self.podcast_merge(&feed.id, &preview.episodes)?;
        tx.commit()?;
        Ok(feed)
    }
    pub(crate) fn podcast_write(&self, feed: &Feed) -> Result<()> {
        self.conn.execute("INSERT INTO podcast_feeds VALUES (?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET url=excluded.url,subscribed=excluded.subscribed,json=excluded.json",params![feed.id,feed.url,feed.subscribed,serde_json::to_string(feed)?])?;
        Ok(())
    }
    pub(crate) fn podcast_refresh_error(&self, f: &Feed) -> Result<()> {
        self.conn.execute("UPDATE podcast_feeds SET json=?2 WHERE id=?1 AND subscribed=1 AND json_extract(json,'$.revision')=?3",params![f.id,serde_json::to_string(f)?,f.revision])?;
        Ok(())
    }
    pub fn podcast_refresh_merge(&self, old: &Feed, preview: Option<Preview>) -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let current = self.podcast_feed(&old.id)?;
        if !current.subscribed || current.revision != old.revision {
            return Ok(());
        }
        let mut feed = preview.as_ref().map(|p| p.feed.clone()).unwrap_or(current);
        feed.id = old.id.clone();
        feed.subscribed = true;
        feed.error = None;
        feed.failures = 0;
        feed.revision = old.revision.saturating_add(1);
        feed.refreshed_at = Some(chrono::Utc::now().to_rfc3339());
        self.podcast_write(&feed)?;
        self.conn.execute(
            "INSERT OR IGNORE INTO podcast_aliases VALUES (?1,?2)",
            params![feed.url, feed.id],
        )?;
        if let Some(p) = preview {
            self.podcast_merge(&feed.id, &p.episodes)?;
        }
        tx.commit()?;
        Ok(())
    }
    fn podcast_merge(&self, feed_id: &str, episodes: &[Episode]) -> Result<()> {
        for item in episodes {
            let identity = item
                .guid
                .as_ref()
                .map(|g| format!("guid:{g}"))
                .unwrap_or_else(|| format!("url:{}", item.url));
            let old: Option<String> = self
                .conn
                .query_row(
                    "SELECT json FROM podcast_episodes WHERE feed_id=?1 AND identity=?2",
                    params![feed_id, identity],
                    |r| r.get(0),
                )
                .optional()?;
            let mut e = item.clone();
            e.feed_id = feed_id.into();
            e.id = if let Some(old) = old {
                let old: Episode = serde_json::from_str(&old)?;
                if (old.bytes.is_some() && e.bytes.is_some() && old.bytes != e.bytes)
                    || (old.duration_ms.is_some()
                        && e.duration_ms.is_some()
                        && old.duration_ms != e.duration_ms)
                {
                    self.conn.execute(
                        "UPDATE podcast_progress SET completed=0 WHERE episode_id=?1",
                        [&old.id],
                    )?;
                }
                old.id
            } else {
                uuid::Uuid::new_v4().to_string()
            };
            self.conn.execute("INSERT INTO podcast_episodes VALUES (?1,?2,?3,?4) ON CONFLICT(feed_id,identity) DO UPDATE SET json=excluded.json",params![e.id,feed_id,identity,serde_json::to_string(&e)?])?;
        }
        Ok(())
    }
    pub fn podcast_unsubscribe(&self, id: &str) -> Result<()> {
        let mut f = self.podcast_feed(id)?;
        f.subscribed = false;
        f.revision = f.revision.saturating_add(1);
        self.podcast_write(&f)
    }
    pub fn podcast_episode(&self, id: &str) -> Result<Episode> {
        let json: Option<String> = self
            .conn
            .query_row("SELECT json FROM podcast_episodes WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        let mut e: Episode =
            serde_json::from_str(&json.ok_or_else(|| error(crate::t!("podcast_not_found")))?)?;
        let p=self.conn.query_row("SELECT position_ms,duration_ms,completed FROM podcast_progress WHERE episode_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        if let Some((pos, dur, done)) = p {
            e.position_ms = pos;
            e.observed_duration_ms = dur;
            e.completed = done;
        }
        Ok(e)
    }
    pub fn podcast_episodes(&self, id: &str) -> Result<Vec<Episode>> {
        self.podcast_feed(id)?;
        let mut stmt=self.conn.prepare("SELECT id FROM podcast_episodes WHERE feed_id=?1 ORDER BY json_extract(json,'$.published') DESC,rowid DESC")?;
        let ids = stmt.query_map([id], |r| r.get::<_, String>(0))?;
        ids.map(|id| self.podcast_episode(&id?)).collect()
    }
    pub fn podcast_progress(
        &self,
        id: &str,
        pos: u64,
        duration: Option<u64>,
        completed: bool,
    ) -> Result<()> {
        self.conn.execute("INSERT INTO podcast_progress VALUES (?1,?2,?3,?4,?5) ON CONFLICT(episode_id) DO UPDATE SET position_ms=excluded.position_ms,duration_ms=COALESCE(excluded.duration_ms,duration_ms),completed=excluded.completed,updated_at=excluded.updated_at",params![id,pos.min(i64::MAX as u64),duration.map(|d|d.min(i64::MAX as u64)),completed,chrono::Utc::now().to_rfc3339()])?;
        Ok(())
    }
    pub fn podcast_played(&self, id: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO podcast_history(episode_id,played_at) VALUES (?1,?2)",
            params![id, chrono::Utc::now().to_rfc3339()],
        )?;
        self.set_meta("pref_last_episode", id)?;
        self.set_meta("pref_last_media", "episode")
    }
    pub fn podcast_download(&self, id: &str) -> Result<Option<Download>> {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT json FROM podcast_downloads WHERE episode_id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        json.map(|j| Ok(serde_json::from_str(&j)?)).transpose()
    }
    pub(crate) fn podcast_download_update(&self, d: &Download) -> Result<()> {
        let changed=self.conn.execute("UPDATE podcast_downloads SET json=?2 WHERE episode_id=?1 AND COALESCE(json_extract(json,'$.generation'),0)=?3",params![d.episode_id,serde_json::to_string(d)?,d.generation])?;
        if changed == 0 {
            return Err(DomainError::new(ErrorCode::Cancelled).into());
        }
        Ok(())
    }
    pub fn podcast_save_download(&self, d: &Download) -> Result<()> {
        self.conn.execute("INSERT INTO podcast_downloads VALUES (?1,?2) ON CONFLICT(episode_id) DO UPDATE SET json=excluded.json",params![d.episode_id,serde_json::to_string(d)?])?;
        Ok(())
    }
    pub fn podcast_downloads(&self) -> Result<Vec<Download>> {
        let mut stmt = self
            .conn
            .prepare("SELECT json FROM podcast_downloads ORDER BY episode_id")?;
        stmt.query_map([], |r| r.get::<_, String>(0))?
            .map(|j| Ok(serde_json::from_str(&j?)?))
            .collect()
    }
    pub fn podcast_queue(&self) -> Result<Vec<Episode>> {
        let mut stmt = self
            .conn
            .prepare("SELECT episode_id FROM podcast_queue ORDER BY position,rowid")?;
        stmt.query_map([], |r| r.get::<_, String>(0))?
            .map(|id| self.podcast_episode(&id?))
            .collect()
    }
    pub fn podcast_queue_set(&self, ids: &[String]) -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        self.conn.execute("DELETE FROM podcast_queue", [])?;
        for (i, id) in ids.iter().enumerate() {
            self.conn.execute(
                "INSERT OR IGNORE INTO podcast_queue VALUES (?1,?2)",
                params![id, i],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}
