use super::*;
use crate::{config::Config, store::Store};
use anyhow::Result;
use clap::Subcommand;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};
#[derive(Debug, Clone, Subcommand)]
pub enum Action {
    Search {
        query: String,
        #[arg(long, default_value = "CN")]
        country: String,
        #[arg(long,default_value_t=25,value_parser=clap::value_parser!(u16).range(1..=200))]
        limit: u16,
    },
    Discover {
        url: String,
    },
    Preview {
        url: String,
    },
    Add {
        url: String,
    },
    List(crate::cli::Page),
    Episodes {
        id: String,
        #[command(flatten)]
        page: crate::cli::Page,
    },
    Refresh {
        id: Option<String>,
    },
    Unsubscribe {
        id: String,
    },
    Play {
        id: String,
        #[arg(long)]
        from_start: bool,
        #[arg(long)]
        speed: Option<f64>,
    },
    Mark {
        id: String,
        #[arg(value_parser=["played","unplayed"])]
        state: String,
    },
    Download {
        id: String,
    },
    Downloads {
        #[command(subcommand)]
        action: DownloadAction,
    },
    Queue {
        #[command(subcommand)]
        action: QueueAction,
    },
    Import {
        file: PathBuf,
        #[arg(long)]
        dry_run: bool,
    },
    Export {
        file: PathBuf,
        #[arg(long)]
        overwrite: bool,
    },
    Seek {
        time: String,
    },
    Speed {
        rate: f64,
    },
    AutoNext {
        #[arg(value_parser=["on","off"])]
        state: String,
    },
}
#[derive(Debug, Clone, Subcommand)]
pub enum DownloadAction {
    List,
    Pause { id: String },
    Resume { id: String },
    Cancel { id: String },
    Remove { id: String },
}
#[derive(Debug, Clone, Subcommand)]
pub enum QueueAction {
    List,
    Add {
        id: String,
    },
    Remove {
        id: String,
    },
    Move {
        id: String,
        position: usize,
    },
    Clear,
    Play {
        #[arg(long)]
        auto_next: bool,
    },
}
pub fn valid_speed(rate: f64) -> Result<f64> {
    if !rate.is_finite() || !(0.5..=3.).contains(&rate) {
        return Err(error(crate::t!("podcast_invalid_speed")));
    }
    Ok(rate)
}
pub fn parse_time(value: &str) -> Result<u64> {
    let mut seconds = 0f64;
    for part in value.split(':') {
        seconds = seconds * 60.
            + part
                .parse::<f64>()
                .map_err(|_| error(crate::t!("podcast_invalid_time")))?;
    }
    if !seconds.is_finite() || seconds < 0. || seconds > i64::MAX as f64 / 1000. {
        return Err(error(crate::t!("podcast_invalid_time")));
    }
    Ok((seconds * 1000.) as u64)
}
fn page<T: Serialize>(items: &[T], p: crate::cli::Page) -> Value {
    let limit = if p.all {
        items.len()
    } else {
        usize::from(p.limit)
    };
    json!({"items":items.iter().skip(p.offset).take(limit).collect::<Vec<_>>(),"total":items.len(),"offset":p.offset,"limit":if p.all {None}else{Some(p.limit)},"has_more":p.offset.saturating_add(limit)<items.len()})
}
pub fn refresh(
    config: &Config,
    store: &Store,
    id: Option<&str>,
    stop: &Arc<AtomicBool>,
) -> Result<Value> {
    let feeds = if let Some(id) = id {
        vec![store.podcast_feed(id)?]
    } else {
        store.podcast_feeds()?
    };
    let mut results = vec![];
    for f in feeds {
        match fetch(config, &f.url, Some(&f), stop) {
            Ok(p) => {
                if stop.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err(DomainError::new(ErrorCode::Cancelled).into());
                }
                store.podcast_refresh_merge(&f, p)?;
                results.push(json!({"id":f.id,"ok":true}));
            }
            Err(e) => {
                let mut f = f;
                f.error = Some(e.to_string());
                f.failures = f.failures.saturating_add(1);
                f.refreshed_at = Some(chrono::Utc::now().to_rfc3339());
                store.podcast_refresh_error(&f)?;
                results.push(json!({"id":f.id,"ok":false,"error":e.to_string()}));
            }
        }
    }
    Ok(json!(results))
}
pub fn manage(
    config: &Config,
    store: &Store,
    action: Action,
    stop: &Arc<AtomicBool>,
) -> Result<Value> {
    Ok(match action {
        Action::Search {
            query,
            country,
            limit,
        } => json!(search(config, &query, &country, limit.into(), stop)?),
        Action::Discover { url } => json!(discover(config, &url, stop)?),
        Action::Preview { url } => json!(fetch(config, &url, None, stop)?),
        Action::Add { url } => {
            let urls = discover(config, &url, stop)?;
            if urls.len() != 1 {
                return Err(error(crate::t!("podcast_choose_feed")));
            }
            let p = fetch(config, &urls[0], None, stop)?
                .ok_or_else(|| error(crate::t!("podcast_invalid_feed")))?;
            if stop.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(DomainError::new(ErrorCode::Cancelled).into());
            }
            json!(store.podcast_subscribe(&p, &url)?)
        }
        Action::List(p) => page(&store.podcast_feeds()?, p),
        Action::Episodes { id, page: p } => page(&store.podcast_episodes(&id)?, p),
        Action::Refresh { id } => refresh(config, store, id.as_deref(), stop)?,
        Action::Unsubscribe { id } => {
            store.podcast_unsubscribe(&id)?;
            json!({"id":id,"subscribed":false})
        }
        Action::Mark { id, state } => {
            let e = store.podcast_episode(&id)?;
            store.podcast_progress(
                &id,
                if state == "played" {
                    e.observed_duration_ms.unwrap_or(e.position_ms)
                } else {
                    0
                },
                e.observed_duration_ms,
                state == "played",
            )?;
            json!(store.podcast_episode(&id)?)
        }
        Action::Download { id } => json!(download(config, &id, stop)?),
        Action::Downloads { action } => match action {
            DownloadAction::List => json!(store.podcast_downloads()?),
            DownloadAction::Resume { id } => json!(download(config, &id, stop)?),
            DownloadAction::Pause { ref id }
            | DownloadAction::Cancel { ref id }
            | DownloadAction::Remove { ref id } => {
                let mut d = store
                    .podcast_download(id)?
                    .ok_or_else(|| error(crate::t!("podcast_not_found")))?;
                let remove = matches!(action, DownloadAction::Remove { .. });
                if remove {
                    if d.state == "downloading" {
                        return Err(error(crate::t!("podcast_download_busy")));
                    }
                    let path = if d.path.is_empty() {
                        download_path(config, id)
                    } else {
                        PathBuf::from(&d.path)
                    };
                    for path in [path.clone(), path.with_extension("part")] {
                        if path.exists() {
                            std::fs::remove_file(path)?;
                        }
                    }
                    d.state = "removed".into();
                    d.bytes = 0;
                } else {
                    d.state = if matches!(action, DownloadAction::Pause { .. }) {
                        "paused"
                    } else {
                        "cancelled"
                    }
                    .into();
                }
                d.generation = d.generation.saturating_add(1);
                store.podcast_save_download(&d)?;
                json!(d)
            }
        },
        Action::Queue { action } => {
            let mut ids: Vec<_> = store
                .podcast_queue()?
                .iter()
                .map(|e| e.id.clone())
                .collect();
            match action {
                QueueAction::List => {}
                QueueAction::Add { id } => {
                    store.podcast_episode(&id)?;
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
            }
            store.podcast_queue_set(&ids)?;
            json!(store.podcast_queue()?)
        }
        Action::Export { file, overwrite } => {
            use std::io::Write;
            let feeds = store.podcast_feeds()?;
            let mut text = String::from(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><opml version=\"2.0\"><head><title>radio-cli</title></head><body>",
            );
            for f in &feeds {
                text.push_str(&format!(
                    "<outline type=\"rss\" text=\"{}\" xmlUrl=\"{}\" htmlUrl=\"{}\"/>",
                    quick_xml::escape::escape(&f.title),
                    quick_xml::escape::escape(&f.url),
                    quick_xml::escape::escape(&f.website)
                ));
            }
            text.push_str("</body></opml>");
            let mut options = std::fs::OpenOptions::new();
            options.write(true);
            if overwrite {
                options.create(true).truncate(true);
            } else {
                options.create_new(true);
            }
            options.open(file)?.write_all(text.as_bytes())?;
            json!({"exported":feeds.len()})
        }
        Action::Import { file, dry_run } => {
            use std::io::Read;
            let mut data = Vec::new();
            std::fs::File::open(file)?
                .take(10 * 1024 * 1024 + 1)
                .read_to_end(&mut data)?;
            let urls = opml(&data)?;
            if dry_run {
                json!({"urls":urls})
            } else {
                let mut results = vec![];
                for u in urls {
                    match fetch(config, &u, None, stop).and_then(|p| {
                        store.podcast_subscribe(
                            &p.ok_or_else(|| error(crate::t!("podcast_invalid_feed")))?,
                            &u,
                        )
                    }) {
                        Ok(f) => results.push(json!({"id":f.id,"ok":true})),
                        Err(e) => results.push(json!({"ok":false,"error":e.to_string()})),
                    }
                }
                json!(results)
            }
        }
        Action::AutoNext { state } => {
            store.set_meta("podcast_auto_next", &state)?;
            json!({"auto_next":state=="on"})
        }
        Action::Speed { rate } => {
            valid_speed(rate)?;
            store.set_meta("podcast_speed", &rate.to_string())?;
            json!({"speed":rate})
        }
        Action::Seek { .. } | Action::Play { .. } => {
            return Err(error(crate::t!("podcast_playback_only")));
        }
    })
}
fn opml(data: &[u8]) -> Result<Vec<String>> {
    use quick_xml::{Reader, events::Event};
    if data.len() > 10 * 1024 * 1024 {
        return Err(error(crate::t!("podcast_limit")));
    }
    let mut reader = Reader::from_reader(data);
    let mut depth = 0;
    let mut root = false;
    let mut urls = vec![];
    loop {
        match reader
            .read_event()
            .map_err(|_| error(crate::t!("podcast_invalid_feed")))?
        {
            Event::Start(e) | Event::Empty(e) => {
                let empty =
                    data.get(reader.buffer_position().saturating_sub(2) as usize) == Some(&b'/');
                if depth == 0 && (root || e.local_name().as_ref() != "opml") {
                    return Err(error(crate::t!("podcast_invalid_feed")));
                }
                root = true;
                if e.local_name().as_ref() == "outline" {
                    for a in e.attributes() {
                        let a = a.map_err(|_| error(crate::t!("podcast_invalid_feed")))?;
                        if a.key.as_ref() == "xmlUrl" {
                            let value = a
                                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                                .map_err(|_| error(crate::t!("podcast_invalid_feed")))?;
                            let u = super::feed::url(&value, None)?.to_string();
                            if !urls.contains(&u) {
                                urls.push(u);
                            }
                        }
                    }
                }
                if !empty {
                    depth += 1;
                }
                if depth > 64 || urls.len() > 10000 {
                    return Err(error(crate::t!("podcast_limit")));
                }
            }
            Event::End(_) => {
                if depth == 0 {
                    return Err(error(crate::t!("podcast_invalid_feed")));
                }
                depth -= 1;
            }
            Event::DocType(_) => return Err(error(crate::t!("podcast_invalid_feed"))),
            Event::Eof => break,
            _ => {}
        }
    }
    if !root || depth != 0 {
        return Err(error(crate::t!("podcast_invalid_feed")));
    }
    Ok(urls)
}
