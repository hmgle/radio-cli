//! Local collections share a stable, filtered query before pagination.
use super::download_files::DownloadFiles;
use crate::{cli::Page, store::Store};
use anyhow::Result;
use clap::Args;
use rusqlite::params;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Debug, Clone, Args)]
pub struct LibraryQuery {
    #[arg(default_value = "all", value_parser = ["all", "inbox", "continue", "history", "saved"])]
    pub view: String,
    #[arg(long)]
    pub feed: Option<String>,
    #[arg(long, default_value = "")]
    pub query: String,
    #[arg(long, default_value = "all", value_parser = ["all", "unplayed", "progress", "played"])]
    pub status: String,
    #[arg(long, default_value = "newest", value_parser = ["newest", "oldest", "title"])]
    pub sort: String,
    #[arg(long)]
    pub downloaded: bool,
    #[command(flatten)]
    pub page: Page,
}
impl Default for LibraryQuery {
    fn default() -> Self {
        Self {
            view: "all".into(),
            feed: None,
            query: String::new(),
            status: "all".into(),
            sort: "newest".into(),
            downloaded: false,
            page: Page {
                offset: 0,
                limit: 100,
                all: false,
            },
        }
    }
}
impl Store {
    pub fn podcast_library(&self, q: &LibraryQuery) -> Result<Value> {
        self.podcast_library_with_files(q, &Arc::default())
    }

    pub(super) fn podcast_library_with_files(
        &self,
        q: &LibraryQuery,
        files: &Arc<DownloadFiles>,
    ) -> Result<Value> {
        let mut q = q.clone();
        q.feed = q
            .feed
            .as_deref()
            .map(|id| self.podcast_feed_id(id))
            .transpose()?;
        super::storage::register_download_availability(&self.conn, files.clone())?;
        let result = files.read_page(self, |store| {
            let candidates = "(?1 IS NULL OR e.feed_id=?1)
                AND (?2='' OR instr(podcast_lower(json_extract(e.json,'$.title')),?2)>0)
                AND (?3='all' OR (?3='played' AND COALESCE(p.completed,0)=1)
                  OR (?3='unplayed' AND COALESCE(p.completed,0)=0)
                  OR (?3='progress' AND COALESCE(p.completed,0)=0 AND p.position_ms>0))
                AND ((?4='all' AND f.subscribed=1) OR (?4='continue' AND COALESCE(p.completed,0)=0 AND p.position_ms>0)
                  OR (?4='history' AND h.played_at IS NOT NULL) OR (?4='saved' AND s.episode_id IS NOT NULL)
                  OR (?4='inbox' AND i.episode_id IS NOT NULL AND f.subscribed=1))";
            // CASE short-circuits even when SQLite reorders the joins.
            let filter = if q.downloaded {
                format!("CASE WHEN {candidates} THEN podcast_download_available(d.json,e.json) ELSE 0 END")
            } else {
                candidates.to_owned()
            };
            let base = format!("FROM podcast_episodes e JOIN podcast_feeds f ON f.id=e.feed_id
                LEFT JOIN podcast_progress p ON p.episode_id=e.id
                LEFT JOIN podcast_downloads d ON d.episode_id=e.id
                LEFT JOIN podcast_saved s ON s.episode_id=e.id
                LEFT JOIN podcast_inbox i ON i.episode_id=e.id
                LEFT JOIN (SELECT episode_id,MAX(played_at) AS played_at FROM podcast_history GROUP BY episode_id) h ON h.episode_id=e.id
                WHERE {filter}");
            let query = q.query.to_lowercase();
            let args = params![q.feed, query, q.status, q.view];
            let total: usize = store.conn.query_row(&format!("SELECT count(*) {base}"), args, |r| r.get(0))?;
            let order = if q.view == "history" { "h.played_at DESC,e.id" }
                else if q.view == "continue" { "p.updated_at DESC,e.id" }
                else { match q.sort.as_str() {
                    "oldest" => "json_extract(e.json,'$.published') IS NULL,json_extract(e.json,'$.published'),COALESCE(json_extract(e.json,'$.source_order'),e.legacy_order),e.id",
                    "title" => "podcast_lower(json_extract(e.json,'$.title')),e.id",
                    _ => "json_extract(e.json,'$.published') DESC,COALESCE(json_extract(e.json,'$.source_order'),e.legacy_order),e.id",
                }};
            let sql = format!("SELECT e.id {base} ORDER BY {order} LIMIT ?5 OFFSET ?6");
            let mut stmt = store.conn.prepare(&sql)?;
            let ids = stmt.query_map(params![q.feed, query, q.status, q.view,
                if q.page.all { -1 } else { i64::from(q.page.limit) }, q.page.offset as i64], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let rows = ids.iter().map(|id| super::commands::episode_value(store, &store.podcast_episode(id)?, |path| files.observed_file(path))).collect::<Result<Vec<_>>>()?;
            let mut value = super::commands::page_result(&rows, total, q.page.clone());
            if let Some(feed) = &q.feed { value["feed_title"] = json!(store.podcast_feed(feed)?.title); }
            Ok(value)
        });
        let removed = self.conn.remove_function("podcast_download_available", 2);
        let value = result?;
        removed?;
        Ok(value)
    }
    pub fn podcast_save_episode(&self, id: &str, remove: bool) -> Result<()> {
        let id = self.podcast_episode_id(id)?;
        if remove {
            self.conn
                .execute("DELETE FROM podcast_saved WHERE episode_id=?1", [id])?;
        } else {
            self.conn
                .execute("INSERT OR IGNORE INTO podcast_saved VALUES (?1)", [id])?;
        }
        Ok(())
    }
    pub fn podcast_dismiss_inbox(&self, id: &str) -> Result<()> {
        let id = self.podcast_episode_id(id)?;
        self.conn
            .execute("DELETE FROM podcast_inbox WHERE episode_id=?1", [id])?;
        Ok(())
    }
}
