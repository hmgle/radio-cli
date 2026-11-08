//! Public podcast subscriptions are independent of the radio catalog.
mod commands;
mod download;
mod feed;
mod storage;
use crate::error::{DomainError, ErrorCode};
pub use commands::{Action, DownloadAction, QueueAction, manage};
pub use download::{download, download_path};
pub use feed::{Preview, discover, fetch, search};
use serde::{Deserialize, Serialize};

pub(crate) fn error(detail: &str) -> anyhow::Error {
    DomainError::new(ErrorCode::PodcastFailed)
        .with("detail", detail)
        .into()
}
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Feed {
    pub failures: u32,
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
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct EpisodeMedia {
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
    pub(crate) fn media(&self) -> EpisodeMedia {
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
    pub generation: u64,
    pub url: String,
    pub episode_id: String,
    pub state: String,
    pub bytes: u64,
    pub etag: Option<String>,
    pub modified: Option<String>,
    pub path: String,
    pub error: Option<String>,
}

pub(crate) use storage::migrate;
pub(crate) use storage::register_functions;

pub use commands::{parse_time, refresh, valid_speed};

mod runtime;
pub(crate) use runtime::Runtime;

pub(crate) fn sanitize_output(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(s) => *s = crate::clean_text(s),
        serde_json::Value::Array(a) => a.iter_mut().for_each(sanitize_output),
        serde_json::Value::Object(o) => o.values_mut().for_each(sanitize_output),
        _ => {}
    }
}
