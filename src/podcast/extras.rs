//! Persistent episode annotations and per-show playback preferences.
use super::*;
use crate::{config::Config, store::Store};
use anyhow::Result;
use rusqlite::params;
use serde_json::{Value, json};
use std::sync::{Arc, atomic::AtomicBool};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Chapter {
    pub title: String,
    pub position_ms: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bookmark {
    pub id: String,
    pub episode_id: String,
    pub position_ms: u64,
    pub note: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ShowSettings {
    pub speed: Option<f64>,
    pub sort: Option<String>,
    pub skip_back: Option<u32>,
    pub skip_forward: Option<u32>,
    pub skip_intro: u32,
    pub skip_outro: u32,
    pub completion_seconds: u32,
    pub auto_download: bool,
    pub auto_cleanup: bool,
}
impl Store {
    pub fn podcast_settings(&self, id: &str) -> Result<ShowSettings> {
        let id = self.podcast_feed_id(id)?;
        Ok(self
            .meta(&format!("podcast_settings:{id}"))?
            .map(|s| serde_json::from_str(&s))
            .transpose()?
            .unwrap_or_default())
    }
    pub fn podcast_set_setting(&self, id: &str, key: &str, value: &str) -> Result<ShowSettings> {
        let id = self.podcast_feed_id(id)?;
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let mut settings = serde_json::to_value(self.podcast_settings(&id)?)?;
        if settings.get(key).is_none() {
            return Err(error(crate::t!("podcast_invalid_setting")));
        }
        settings[key] = if value == "inherit"
            && matches!(key, "speed" | "sort" | "skip_back" | "skip_forward")
        {
            Value::Null
        } else if key == "sort" {
            json!(value)
        } else {
            serde_json::from_str(value).map_err(|_| error(crate::t!("podcast_invalid_setting")))?
        };
        let settings: ShowSettings = serde_json::from_value(settings)
            .map_err(|_| error(crate::t!("podcast_invalid_setting")))?;
        if let Some(speed) = settings.speed {
            valid_speed(speed)?;
        }
        if settings
            .sort
            .as_deref()
            .is_some_and(|s| !matches!(s, "newest" | "oldest" | "title"))
            || [
                settings.skip_intro,
                settings.skip_outro,
                settings.completion_seconds,
                settings.skip_back.unwrap_or(15),
                settings.skip_forward.unwrap_or(15),
            ]
            .iter()
            .any(|s| *s > 86400)
        {
            return Err(error(crate::t!("podcast_invalid_setting")));
        }
        self.set_meta(
            &format!("podcast_settings:{id}"),
            &serde_json::to_string(&settings)?,
        )?;
        self.conn.execute("UPDATE podcast_revision SET n=n+1", [])?;
        tx.commit()?;
        Ok(settings)
    }
    pub fn podcast_bookmarks(&self, episode: &str) -> Result<Vec<Bookmark>> {
        let episode = self.podcast_episode_id(episode)?;
        let mut stmt = self.conn.prepare("SELECT id,episode_id,position_ms,note FROM podcast_bookmarks WHERE episode_id=?1 ORDER BY position_ms,id")?;
        Ok(stmt
            .query_map([episode], |r| {
                Ok(Bookmark {
                    id: r.get(0)?,
                    episode_id: r.get(1)?,
                    position_ms: r.get(2)?,
                    note: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn podcast_add_bookmark(&self, id: &str, position_ms: u64, note: &str) -> Result<Bookmark> {
        let id = self.podcast_episode_id(id)?;
        let b = Bookmark {
            id: uuid::Uuid::new_v4().to_string(),
            episode_id: id,
            position_ms,
            note: crate::clean_text(note),
        };
        self.conn.execute(
            "INSERT INTO podcast_bookmarks VALUES (?1,?2,?3,?4)",
            params![
                b.id,
                b.episode_id,
                b.position_ms.min(i64::MAX as u64),
                b.note
            ],
        )?;
        Ok(b)
    }
    pub fn podcast_edit_bookmark(&self, id: &str, note: Option<&str>) -> Result<()> {
        let changed = if let Some(note) = note {
            self.conn.execute(
                "UPDATE podcast_bookmarks SET note=?2 WHERE id=?1",
                params![id, crate::clean_text(note)],
            )?
        } else {
            self.conn
                .execute("DELETE FROM podcast_bookmarks WHERE id=?1", [id])?
        };
        if changed == 0 {
            return Err(error(crate::t!("podcast_not_found")));
        }
        Ok(())
    }
}
pub(super) fn chapters(config: &Config, e: &Episode, stop: &Arc<AtomicBool>) -> Result<Value> {
    let mut chapters = e.chapters.clone();
    if let Some(url) = &e.chapters_url {
        let remote = (|| -> Result<Vec<Chapter>> {
            let response = super::feed::client(config)?
                .get(url)
                .send()
                .map_err(super::feed::network_error)?;
            let (_, data) = super::feed::body(response, stop)?;
            let value: Value = serde_json::from_slice(&data)?;
            let rows = value["chapters"]
                .as_array()
                .ok_or_else(|| error(crate::t!("podcast_invalid_feed")))?;
            if rows.len() > 10000 {
                return Err(error(crate::t!("podcast_limit")));
            }
            let mut result = Vec::new();
            for row in rows {
                let seconds = row["startTime"]
                    .as_f64()
                    .filter(|n| n.is_finite() && *n >= 0. && *n < i64::MAX as f64 / 1000.)
                    .ok_or_else(|| error(crate::t!("podcast_invalid_time")))?;
                result.push(Chapter {
                    title: crate::clean_text(row["title"].as_str().unwrap_or("")),
                    position_ms: (seconds * 1000.) as u64,
                });
            }
            Ok(result)
        })();
        match remote {
            Ok(rows) if !rows.is_empty() => chapters = rows,
            Err(err) if chapters.is_empty() => return Err(err),
            _ => {}
        }
    }
    chapters.sort_by_key(|c| c.position_ms);
    chapters.dedup_by_key(|c| c.position_ms);
    let items: Vec<_> = chapters
        .iter()
        .map(|c| json!({"id":e.id,"title":c.title,"kind":"chapter","position_ms":c.position_ms}))
        .collect();
    Ok(json!({"items":items,"total":items.len(),"episode_id":e.id}))
}
