//! Public podcast subscriptions are independent of the radio catalog.
mod commands;
pub mod directory;
mod extras;
mod library;
pub use extras::{Bookmark, Chapter, ShowSettings};
pub use library::LibraryQuery;
mod download;
mod download_files;
mod feed;
mod storage;
pub mod transcript;
use crate::error::{DomainError, ErrorCode};
pub use commands::{Action, DownloadAction, QueueAction, manage};
pub(crate) use download::set_cli_progress;
pub use download::{download, download_path};
pub use feed::{Preview, discover, fetch, search};
use serde::{Deserialize, Serialize};
pub use transcript::Transcript;

pub(crate) fn error(detail: &str) -> anyhow::Error {
    DomainError::new(ErrorCode::PodcastFailed)
        .with("detail", detail)
        .into()
}
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Feed {
    pub failures: u32,
    pub new_feed_url: Option<String>,
    pub revision: u64,
    pub id: String,
    pub url: String,
    pub title: String,
    pub author: String,
    pub description: String,
    pub image: String,
    pub website: String,
    pub subscribed: bool,
    pub etag: Option<String>,
    pub modified: Option<String>,
    pub refreshed_at: Option<String>,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Episode {
    #[serde(default)]
    pub chapters: Vec<Chapter>,
    #[serde(default)]
    pub chapters_url: Option<String>,
    #[serde(default)]
    pub source_order: Option<u64>,
    pub id: String,
    pub feed_id: String,
    pub guid: Option<String>,
    pub title: String,
    pub published: Option<String>,
    pub description: String,
    pub website: String,
    pub url: String,
    pub mime: String,
    pub bytes: Option<u64>,
    pub duration_ms: Option<u64>,
    pub position_ms: u64,
    pub observed_duration_ms: Option<u64>,
    pub completed: bool,
    #[serde(default)]
    pub transcripts: Vec<Transcript>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpisodeMedia {
    url: String,
    bytes: Option<u64>,
    duration_ms: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct EpisodeProgress {
    pub media: EpisodeMedia,
    pub position_ms: u64,
    pub observed_duration_ms: Option<u64>,
    pub completed: bool,
}

impl Episode {
    pub fn media(&self) -> EpisodeMedia {
        EpisodeMedia {
            url: self.url.clone(),
            bytes: self.bytes,
            duration_ms: self.duration_ms,
        }
    }

    pub(crate) fn progress(&self) -> EpisodeProgress {
        EpisodeProgress {
            media: self.media(),
            position_ms: self.position_ms,
            observed_duration_ms: self.observed_duration_ms,
            completed: self.completed,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Download {
    #[serde(skip)]
    pub cleanup_eligible: bool,
    pub total_bytes: Option<u64>,
    pub bytes_per_second: u64,
    pub retries: u8,
    pub retry_at: Option<String>,
    pub enqueued_at: Option<String>,
    pub generation: u64,
    pub url: String,
    pub media: Option<EpisodeMedia>,
    pub episode_id: String,
    pub state: String,
    pub bytes: u64,
    pub etag: Option<String>,
    pub modified: Option<String>,
    pub path: String,
    pub error: Option<String>,
}

impl Download {
    pub(crate) fn matches_media(&self, episode: &Episode) -> bool {
        self.url == episode.url && self.media.as_ref() == Some(&episode.media())
    }

    pub(crate) fn offline_path(&self, episode: &Episode) -> Option<&str> {
        self.offline_path_if(episode, |path| std::path::Path::new(path).is_file())
    }

    fn offline_path_if(
        &self,
        episode: &Episode,
        is_file: impl FnOnce(&str) -> bool,
    ) -> Option<&str> {
        (self.state == "complete" && self.matches_media(episode) && is_file(&self.path))
            .then_some(self.path.as_str())
    }
}

pub(crate) use storage::migrate;
pub(crate) use storage::migrate_episode_order;
pub(crate) use storage::migrate_library;
pub(crate) use storage::register_functions;

pub use commands::{parse_time, refresh, valid_speed};

pub(crate) mod progressive;
mod runtime;
pub(crate) use runtime::{BrowsePage, BrowseSnapshot, Runtime};

pub(crate) fn sanitize_output(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(s) => *s = crate::clean_text(s),
        serde_json::Value::Array(a) => a.iter_mut().for_each(sanitize_output),
        serde_json::Value::Object(o) => o.values_mut().for_each(sanitize_output),
        _ => {}
    }
}

/// Human-readable podcast output; machine callers retain the JSON contracts.
pub fn text_output(value: &serde_json::Value) -> String {
    if let Some(rows) = value.get("items").unwrap_or(value).as_array() {
        if rows.is_empty() {
            return String::new();
        }
        let first = &rows[0];
        let columns: &[&str] = if first.get("episode_id").is_some() {
            &["title", "state", "bytes", "total_bytes", "episode_id"]
        } else if first.get("feed_id").is_some() {
            &[
                "title",
                "published",
                "position_ms",
                "duration_ms",
                "completed",
                "id",
            ]
        } else if first.get("subscribed").is_some() {
            &["title", "author", "unplayed_count", "id"]
        } else if first.get("chartRank").is_some() {
            &[
                "chartRank",
                "collectionName",
                "artistName",
                "collectionViewUrl",
            ]
        } else if first.get("collectionName").is_some() {
            &["collectionName", "artistName", "feedUrl"]
        } else if first.get("command").is_some() {
            &["title", "command"]
        } else if first.get("ok").is_some() {
            &["id", "ok", "error"]
        } else {
            &[]
        };
        if !columns.is_empty() {
            let mut lines = vec![columns.join("\t")];
            for row in rows {
                lines.push(
                    columns
                        .iter()
                        .map(|key| {
                            let value = if *key == "state" && row["download_state"].is_string() {
                                &row["download_state"]
                            } else {
                                &row[*key]
                            };
                            if value.is_null() {
                                "—".into()
                            } else {
                                crate::clean_text(
                                    &value
                                        .as_str()
                                        .map(str::to_owned)
                                        .unwrap_or_else(|| value.to_string()),
                                )
                                .replace(['\n', '\t'], " ")
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\t"),
                );
            }
            return lines.join("\n");
        }
    }
    fn row(v: &serde_json::Value) -> String {
        match v {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Object(fields) => fields
                .iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(key, value)| format!("{}: {}", key, text_output(value)))
                .collect::<Vec<_>>()
                .join("\n"),
            other => other.to_string(),
        }
    }
    if let Some(rows) = value.get("items").unwrap_or(value).as_array() {
        rows.iter().map(row).collect::<Vec<_>>().join("\n")
    } else {
        row(value)
    }
}
