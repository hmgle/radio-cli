use crate::podcast::Action;
use serde_json::Value;
#[derive(Clone, Default)]
pub(super) struct Item {
    pub id: String,
    pub title: String,
    pub description: String,
    pub url: String,
    pub kind: String,
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
            url: p["url"].as_str().unwrap_or("").into(),
            kind: "preview".into(),
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
            let kind = if v.get("feed_id").is_some() {
                "episode"
            } else if v.get("subscribed").is_some() {
                "feed"
            } else if v.get("episode_id").is_some() {
                "download"
            } else if v.get("feedUrl").is_some() || v.is_string() {
                "url"
            } else {
                "info"
            };
            Item {
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
                        strv("episode_id"),
                        strv("state"),
                        v["bytes"]
                    )
                } else {
                    strv("title")
                },
                description: format!(
                    "{}\n{}\n{}",
                    strv("author"),
                    strv("published"),
                    if v.get("collectionName").is_some() && v["feedUrl"].as_str().is_none() {
                        crate::t!("podcast_no_public_feed").into()
                    } else {
                        strv("description")
                    }
                ),
                url: if kind == "url" {
                    v.as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| strv("feedUrl"))
                } else {
                    strv("url")
                },
                kind: kind.into(),
            }
        })
        .collect()
}
pub(super) fn browse(
    source: super::state::Source,
    feed: &Option<String>,
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
                    page,
                }
            } else {
                Action::List(page)
            }
        }
    }
}
