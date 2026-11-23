use crate::podcast::Action;
use serde_json::Value;

/// A transient directory page can only be restored faithfully from its rows.
/// Local library pages are queried again so listening data stays current.
#[derive(Clone)]
pub(super) struct Snapshot {
    pub action: Action,
    pub items: std::sync::Arc<Vec<Item>>,
    pub total: usize,
    pub title: String,
}

pub(super) fn is_transient(action: &Action) -> bool {
    matches!(
        action,
        Action::Search { .. }
            | Action::Discover { .. }
            | Action::Preview { .. }
            | Action::Browse { .. }
            | Action::Categories { .. }
            | Action::Charts { .. }
    )
}

pub(super) fn is_browse(action: &Action) -> bool {
    use crate::podcast::{DownloadAction, QueueAction};
    matches!(
        action,
        Action::Chapters { .. }
            | Action::Bookmarks { .. }
            | Action::Library { .. }
            | Action::List { .. }
            | Action::Add { .. }
            | Action::Episodes { .. }
            | Action::Search { .. }
            | Action::Browse { .. }
            | Action::Categories { .. }
            | Action::Charts { .. }
            | Action::Discover { .. }
            | Action::Preview { .. }
            | Action::Downloads {
                action: DownloadAction::List
            }
            | Action::Queue {
                action: QueueAction::List
            }
    )
}

#[derive(Clone, Default)]
pub(super) struct Item {
    pub command: Option<String>,
    pub input: bool,
    pub local_feed_id: Option<String>,
    pub id: String,
    pub title: String,
    pub description: String,
    pub url: String,
    pub kind: String,
    pub meta: String,
    pub position_ms: Option<u64>,
    pub feed_id: Option<String>,
    pub sort: Option<String>,
    pub bookmark_id: Option<String>,
    pub download_state: Option<String>,
    pub queued: bool,
    pub saved: bool,
}
pub(super) fn page_matches(
    action: &Action,
    feed: &Option<String>,
    filter: &crate::model::Filter,
) -> bool {
    let (id, query, page) = match action {
        Action::Library { options } => (options.feed.as_ref(), &options.query, &options.page),
        Action::List { query, page } => (None, query, page),
        Action::Episodes { id, query, page } => (Some(id), query, page),
        _ => return false,
    };
    id == feed.as_ref()
        && query == &filter.query
        && page.offset == filter.offset
        && usize::from(page.limit) == filter.limit
}
pub(super) fn items(value: &Value) -> Vec<Item> {
    if let Some(p) = value.get("feed") {
        return vec![Item {
            id: String::new(),
            title: p["title"].as_str().unwrap_or("").into(),
            description: format!(
                "{}\n{}",
                p["description"].as_str().unwrap_or(""),
                crate::t!("podcast_confirm")
            ),
            url: value["subscription_url"]
                .as_str()
                .or_else(|| p["url"].as_str())
                .unwrap_or("")
                .into(),
            kind: "preview".into(),
            meta: String::new(),
            position_ms: None,
            feed_id: None,
            sort: None,
            bookmark_id: None,
            ..Default::default()
        }];
    }
    let rows = value.get("items").unwrap_or(value);
    let rows = rows
        .as_array()
        .cloned()
        .unwrap_or_else(|| vec![rows.clone()]);
    rows.iter()
        .map(|v| {
            let strv = |key| v[key].as_str().unwrap_or("").to_owned();
            // Apple uses `kind: "podcast"` for a media type. Convert its show
            // results to actionable directory rows before reading local kinds.
            let directory =
                v.get("collectionName").is_some() || v.get("feedUrl").is_some() || v.is_string();
            let kind = if directory {
                "url"
            } else if let Some(kind) = v["kind"].as_str() {
                kind
            } else if v.get("episode_id").is_some() {
                "download"
            } else if v.get("feed_id").is_some() {
                "episode"
            } else if v.get("subscribed").is_some() {
                "feed"
            } else {
                "info"
            };
            let download_state = v["download_state"].as_str().or_else(|| v["state"].as_str());
            Item {
                command: v["command"].as_str().map(str::to_owned),
                input: v["input"].as_bool().unwrap_or(false),
                local_feed_id: v["local_feed_id"].as_str().map(str::to_owned),
                id: if kind == "download" {
                    strv("episode_id")
                } else {
                    strv("id")
                },
                title: if v.is_string() {
                    v.as_str().unwrap().into()
                } else if v.get("collectionName").is_some() {
                    strv("collectionName")
                } else if kind == "download" {
                    format!(
                        "{} · {} · {} B",
                        v["title"]
                            .as_str()
                            .unwrap_or(crate::t!("podcast_episode_unknown")),
                        download_label(download_state.unwrap_or("")),
                        v["bytes"]
                    )
                } else {
                    strv("title")
                },
                description: format!(
                    "{}\n{}\n{}",
                    v["artistName"]
                        .as_str()
                        .unwrap_or_else(|| v["author"].as_str().unwrap_or("")),
                    metadata(v),
                    if v.get("collectionName").is_some()
                        && v["feedUrl"].as_str().is_none()
                        && v["collectionViewUrl"].as_str().is_none()
                    {
                        crate::t!("podcast_no_public_feed").into()
                    } else {
                        strv("description")
                    }
                ),
                url: if kind == "url" {
                    v.as_str().map(str::to_owned).unwrap_or_else(|| {
                        v["feedUrl"]
                            .as_str()
                            .or_else(|| v["collectionViewUrl"].as_str())
                            .unwrap_or("")
                            .into()
                    })
                } else {
                    strv("url")
                },
                position_ms: v["position_ms"].as_u64(),
                feed_id: v["feed_id"].as_str().map(str::to_owned),
                sort: v["sort"].as_str().map(str::to_owned),
                bookmark_id: v["bookmark_id"].as_str().map(str::to_owned),
                download_state: download_state.map(str::to_owned),
                queued: v["queued"].as_bool().unwrap_or(false),
                saved: v["saved"].as_bool().unwrap_or(false),
                meta: metadata(v),
                kind: kind.into(),
            }
        })
        .collect()
}
fn download_label(state: &str) -> &str {
    match state {
        "queued" => crate::t!("podcast_download_queued"),
        "downloading" => crate::t!("podcast_download_downloading"),
        "paused" => crate::t!("podcast_download_paused"),
        "cancelled" => crate::t!("podcast_download_cancelled"),
        "failed" => crate::t!("podcast_download_failed"),
        "complete" => crate::t!("podcast_download_complete"),
        "stale" => crate::t!("podcast_download_stale"),
        "removed" => crate::t!("podcast_download_removed"),
        other => other,
    }
}

fn metadata(v: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(rank) = v["chartRank"].as_u64() {
        parts.push(format!("#{rank}"));
    }
    if v["local_feed_id"].is_string() {
        parts.push(crate::t!("podcast_subscribed").into());
    }
    for key in ["artistName", "primaryGenreName"] {
        if let Some(s) = v[key].as_str().filter(|s| !s.is_empty()) {
            parts.push(crate::clean_text(s));
        }
    }
    if matches!(v["kind"].as_str(), Some("chapter" | "bookmark")) {
        parts.push(super::display::duration(
            v["position_ms"].as_u64().unwrap_or(0),
        ));
    }
    if v.get("episode_id").is_some() {
        if let Some(total) = v["total_bytes"].as_u64().filter(|n| *n > 0) {
            parts.push(format!(
                "{:.0}%",
                v["bytes"].as_u64().unwrap_or(0) as f64 / total as f64 * 100.
            ));
        }
        let speed = v["bytes_per_second"].as_u64().unwrap_or(0);
        if speed > 0 {
            parts.push(format!("{} KiB/s", speed / 1024));
        }
        if let Some(retry) = v["retry_at"].as_str() {
            parts.push(format!("↻ {retry}"));
        }
    }
    if v.get("feed_id").is_some() {
        parts.push(if v["completed"].as_bool() == Some(true) {
            crate::t!("podcast_menu_played").to_owned()
        } else if v["position_ms"].as_u64().unwrap_or(0) > 0 {
            "◐".into()
        } else {
            "○".into()
        });
        if let Some(date) = v["published"]
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        {
            parts.push(
                date.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d")
                    .to_string(),
            );
        }
        if let Some(duration) = v["observed_duration_ms"]
            .as_u64()
            .or(v["duration_ms"].as_u64())
        {
            parts.push(format!(
                "{} / {}",
                super::display::duration(v["position_ms"].as_u64().unwrap_or(0)),
                super::display::duration(duration)
            ));
        }
    }
    if v["queued"].as_bool() == Some(true) {
        parts.push("≡".into());
    }
    if let Some(state) = v["download_state"].as_str() {
        parts.push(download_label(state).into());
    }
    if let Some(n) = v["unplayed_count"].as_u64() {
        parts.push(format!("○ {n}"));
    }
    if let Some(error) = v["error"].as_str() {
        parts.push(crate::clean_text(error));
    }
    parts.join(" · ")
}
pub(super) fn browse(
    source: super::state::Source,
    feed: &Option<String>,
    query: &str,
    offset: usize,
    limit: usize,
) -> Action {
    use super::state::Source;
    let page = crate::cli::Page {
        offset,
        limit: limit as u16,
        all: false,
    };
    match source {
        Source::PodcastDownloads => Action::Downloads {
            action: crate::podcast::DownloadAction::List,
        },
        Source::PodcastQueue => Action::Queue {
            action: crate::podcast::QueueAction::List,
        },
        _ => {
            if let Some(id) = feed {
                Action::Episodes {
                    id: id.clone(),
                    query: query.into(),
                    page,
                }
            } else {
                Action::List {
                    page,
                    query: query.into(),
                }
            }
        }
    }
}

/// Freeze a visible target while the user chooses an action or enters a path.
pub(super) struct Menu {
    pub id: String,
    pub title: String,
    pub current: bool,
    pub selected: usize,
    pub rows: Vec<ratatui::layout::Rect>,
    pub actions: Vec<usize>,
    pub bookmark_id: Option<String>,
    pub feed_id: Option<String>,
}
impl Menu {
    pub fn labels() -> [&'static str; 49] {
        [
            crate::t!("podcast_menu_show"),
            crate::t!("podcast_menu_generate"),
            crate::t!("podcast_menu_import"),
            crate::t!("podcast_menu_download"),
            crate::t!("podcast_menu_queue"),
            crate::t!("podcast_menu_restart"),
            crate::t!("podcast_menu_cancel"),
            crate::t!("podcast_menu_locate"),
            crate::t!("podcast_menu_played"),
            crate::t!("podcast_menu_unplayed"),
            crate::t!("podcast_menu_unqueue"),
            crate::t!("podcast_download_paused"),
            crate::t!("podcast_download_resume"),
            crate::t!("podcast_save"),
            crate::t!("podcast_unsave"),
            crate::t!("podcast_all"),
            crate::t!("podcast_inbox"),
            crate::t!("podcast_continue"),
            crate::t!("podcast_history"),
            crate::t!("podcast_saved"),
            crate::t!("podcast_chapters"),
            crate::t!("podcast_bookmarks"),
            crate::t!("podcast_bookmark_add"),
            crate::t!("podcast_sleep_30"),
            crate::t!("podcast_sleep_end"),
            crate::t!("podcast_sleep_off"),
            crate::t!("podcast_queue_next"),
            crate::t!("podcast_auto_on"),
            crate::t!("podcast_auto_off"),
            crate::t!("podcast_speed_prompt"),
            crate::t!("podcast_refresh_show"),
            crate::t!("podcast_settings_show"),
            crate::t!("podcast_unsubscribe_show"),
            crate::t!("podcast_filter_prompt"),
            crate::t!("podcast_queue_up"),
            crate::t!("podcast_queue_down"),
            crate::t!("podcast_download_cancel"),
            crate::t!("podcast_download_remove"),
            crate::t!("podcast_queue_clear"),
            crate::t!("podcast_import_prompt"),
            crate::t!("podcast_export_prompt"),
            crate::t!("podcast_search_prompt"),
            crate::t!("podcast_add_prompt"),
            crate::t!("podcast_refresh_all"),
            crate::t!("podcast_bookmark_edit"),
            crate::t!("podcast_bookmark_remove"),
            crate::t!("podcast_browse"),
            crate::t!("podcast_categories"),
            crate::t!("podcast_surprise"),
        ]
    }
}
