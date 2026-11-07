use super::*;
use crate::store::Store;
use anyhow::Result;
use rusqlite::{OptionalExtension, TransactionBehavior, params};

pub(crate) fn register_functions(conn: &rusqlite::Connection) -> Result<()> {
    conn.create_scalar_function(
        "podcast_lower",
        1,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8
            | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
        |context| Ok(context.get::<String>(0)?.to_lowercase()),
    )?;
    Ok(())
}

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
        self.podcast_episode_rows(id, "", -1, 0)
    }
    pub(crate) fn podcast_episode_position(&self, feed: &str, id: &str) -> Result<Option<usize>> {
        Ok(self
            .conn
            .query_row(
                "SELECT position FROM (
                SELECT id,row_number() OVER (
                    ORDER BY json_extract(json,'$.published') DESC,rowid DESC
                )-1 AS position FROM podcast_episodes WHERE feed_id=?1
             ) WHERE id=?2",
                params![feed, id],
                |r| r.get(0),
            )
            .optional()?)
    }
    pub fn podcast_episodes_page(
        &self,
        id: &str,
        query: &str,
        page: &crate::cli::Page,
    ) -> Result<(Vec<Episode>, usize)> {
        let query = query.to_lowercase();
        self.read_snapshot(|store| {
            store.podcast_feed(id)?;
            let total = store.conn.query_row(
                "SELECT count(*) FROM podcast_episodes WHERE feed_id=?1
                 AND (?2='' OR instr(podcast_lower(json_extract(json,'$.title')),?2)>0)",
                params![id, query],
                |r| r.get(0),
            )?;
            let items = store.podcast_episode_rows(
                id,
                &query,
                if page.all { -1 } else { i64::from(page.limit) },
                page.offset.min(i64::MAX as usize) as i64,
            )?;
            Ok((items, total))
        })
    }
    fn podcast_episode_rows(
        &self,
        id: &str,
        query: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Episode>> {
        let mut stmt = self.conn.prepare(
            "SELECT e.json,p.position_ms,p.duration_ms,p.completed FROM podcast_episodes e
             LEFT JOIN podcast_progress p ON p.episode_id=e.id WHERE e.feed_id=?1
             AND (?2='' OR instr(podcast_lower(json_extract(e.json,'$.title')),?2)>0)
             ORDER BY json_extract(e.json,'$.published') DESC,e.rowid DESC LIMIT ?3 OFFSET ?4",
        )?;
        let rows = stmt.query_map(params![id, query, limit, offset], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<u64>>(1)?,
                r.get::<_, Option<u64>>(2)?,
                r.get::<_, Option<bool>>(3)?,
            ))
        })?;
        rows.map(|row| {
            let (json, position, duration, completed) = row?;
            let mut episode: Episode = serde_json::from_str(&json)?;
            if let Some(position) = position {
                episode.position_ms = position;
                episode.observed_duration_ms = duration;
                episode.completed = completed.unwrap_or(false);
            }
            Ok(episode)
        })
        .collect()
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
        self.podcast_queue_write(ids)?;
        tx.commit()?;
        Ok(())
    }
    pub fn podcast_queue_update(&self, action: QueueAction) -> Result<Vec<Episode>> {
        if matches!(action, QueueAction::List) {
            return self.read_snapshot(|store| store.podcast_queue());
        }
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let mut ids = self
            .conn
            .prepare("SELECT episode_id FROM podcast_queue ORDER BY position,rowid")?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        match action {
            QueueAction::Add { id } => {
                self.podcast_episode(&id)?;
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
            QueueAction::Remove { id } => ids.retain(|v| v != &id),
            QueueAction::Clear => ids.clear(),
            QueueAction::Move { id, position } => {
                if position == 0 || position > ids.len() || !ids.contains(&id) {
                    return Err(error(crate::t!("podcast_invalid_time")));
                }
                ids.retain(|v| v != &id);
                ids.insert(position - 1, id);
            }
            QueueAction::Play { .. } => return Err(error(crate::t!("podcast_playback_only"))),
            QueueAction::List => unreachable!(),
        }
        self.podcast_queue_write(&ids)?;
        let items = self.podcast_queue()?;
        tx.commit()?;
        Ok(items)
    }
    fn podcast_queue_write(&self, ids: &[String]) -> Result<()> {
        self.conn.execute("DELETE FROM podcast_queue", [])?;
        for (i, id) in ids.iter().enumerate() {
            self.conn.execute(
                "INSERT OR IGNORE INTO podcast_queue VALUES (?1,?2)",
                params![id, i],
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier, atomic::AtomicBool};

    fn queue_fixture() -> (tempfile::TempDir, crate::config::Config, Vec<Episode>) {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let store = Store::open(&config.data_dir.join("radio.db")).unwrap();
        let preview = Preview {
            feed: Feed {
                url: "https://example.test/rss".into(),
                ..Default::default()
            },
            episodes: (0..2)
                .map(|i| Episode {
                    guid: Some(i.to_string()),
                    url: format!("https://example.test/{i}.mp3"),
                    ..Default::default()
                })
                .collect(),
        };
        let feed = store
            .podcast_subscribe(&preview, &preview.feed.url)
            .unwrap();
        let episodes = store.podcast_episodes(&feed.id).unwrap();
        (dir, config, episodes)
    }

    #[test]
    fn concurrent_queue_additions_preserve_both_updates() {
        use rusqlite::hooks::{AuthAction, AuthContext, Authorization, TransactionOperation};
        let (_dir, config, episodes) = queue_fixture();
        let barrier = Arc::new(Barrier::new(2));
        let workers: Vec<_> = episodes
            .iter()
            .map(|e| {
                let config = config.clone();
                let id = e.id.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let store = Store::open(&config.data_dir.join("radio.db")).unwrap();
                    let mut synchronized = false;
                    // Align the first write transactions. Reading before BEGIN
                    // makes both workers overwrite the same old queue snapshot.
                    store.conn.authorizer(Some(move |context: AuthContext<'_>| {
                        if !synchronized
                            && matches!(
                                context.action,
                                AuthAction::Transaction {
                                    operation: TransactionOperation::Begin
                                }
                            )
                        {
                            synchronized = true;
                            barrier.wait();
                        }
                        Authorization::Allow
                    }));
                    manage(
                        &config,
                        &store,
                        Action::Queue {
                            action: QueueAction::Add { id },
                        },
                        &Arc::new(AtomicBool::new(false)),
                    )
                    .unwrap();
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        let store = Store::open(&config.data_dir.join("radio.db")).unwrap();
        let mut actual: Vec<_> = store
            .podcast_queue()
            .unwrap()
            .into_iter()
            .map(|e| e.id)
            .collect();
        let mut expected: Vec<_> = episodes.into_iter().map(|e| e.id).collect();
        actual.sort();
        expected.sort();
        assert_eq!(actual, expected);
    }

    #[test]
    fn listing_the_queue_is_read_only() {
        let (_dir, config, episodes) = queue_fixture();
        let store = Store::open(&config.data_dir.join("radio.db")).unwrap();
        store.podcast_queue_set(&[episodes[0].id.clone()]).unwrap();
        store.conn.execute_batch("PRAGMA query_only=ON").unwrap();
        let value = manage(
            &config,
            &store,
            Action::Queue {
                action: QueueAction::List,
            },
            &Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert_eq!(value[0]["id"], episodes[0].id);
    }

    #[test]
    fn queue_edits_preserve_order_and_rollback_invalid_moves() {
        let (_dir, config, episodes) = queue_fixture();
        let store = Store::open(&config.data_dir.join("radio.db")).unwrap();
        for episode in [&episodes[0], &episodes[1], &episodes[0]] {
            store
                .podcast_queue_update(QueueAction::Add {
                    id: episode.id.clone(),
                })
                .unwrap();
        }
        let reordered = store
            .podcast_queue_update(QueueAction::Move {
                id: episodes[0].id.clone(),
                position: 2,
            })
            .unwrap();
        assert_eq!(
            reordered.iter().map(|e| &e.id).collect::<Vec<_>>(),
            vec![&episodes[1].id, &episodes[0].id]
        );
        assert!(
            store
                .podcast_queue_update(QueueAction::Move {
                    id: episodes[0].id.clone(),
                    position: 3,
                })
                .is_err()
        );
        assert_eq!(store.podcast_queue().unwrap(), reordered);
        let remaining = store
            .podcast_queue_update(QueueAction::Remove {
                id: episodes[1].id.clone(),
            })
            .unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, episodes[0].id);
        assert!(
            store
                .podcast_queue_update(QueueAction::Clear)
                .unwrap()
                .is_empty()
        );
    }
}
