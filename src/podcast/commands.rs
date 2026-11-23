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
    Chapters {
        id: String,
    },
    Bookmarks {
        id: String,
    },
    Bookmark {
        id: String,
        #[arg(long)]
        time: Option<String>,
        #[arg(long, default_value = "")]
        note: String,
    },
    BookmarkEdit {
        id: String,
        #[arg(long)]
        note: Option<String>,
    },
    Settings {
        id: String,
        key: Option<String>,
        value: Option<String>,
    },
    Jump {
        id: String,
        time: String,
    },
    Sleep {
        #[arg(default_value = "30")]
        value: String,
    },
    Library {
        #[command(flatten)]
        options: LibraryQuery,
    },
    Save {
        id: String,
        #[arg(long)]
        remove: bool,
    },
    Dismiss {
        id: String,
    },
    MarkBatch {
        #[arg(required = true)]
        ids: Vec<String>,
        #[arg(long)]
        unplayed: bool,
    },
    Resume,
    Relative {
        seconds: f64,
    },
    /// Open the discovery page without requiring a search term.
    Browse {
        #[arg(long, default_value = "")]
        country: String,
    },
    /// List categories available for regional charts.
    Categories {
        #[arg(long, default_value = "")]
        country: String,
    },
    /// Browse Apple Podcasts' regional charts, optionally in random order.
    Charts {
        #[arg(long, default_value = "")]
        country: String,
        #[arg(long, value_parser = directory::category_id)]
        category: Option<String>,
        #[arg(long, default_value_t = 25, value_parser = clap::value_parser!(u16).range(1..=100))]
        limit: u16,
        #[arg(long)]
        random: bool,
    },
    Search {
        query: String,
        #[arg(long, default_value = "")]
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
    List {
        #[command(flatten)]
        page: crate::cli::Page,
        #[arg(long, default_value = "")]
        query: String,
    },
    Episodes {
        id: String,
        #[arg(long, default_value = "")]
        query: String,
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
    /// Fetch, import, or locally generate timed episode captions.
    Subtitles {
        id: String,
        #[arg(long, conflicts_with = "transcribe")]
        file: Option<PathBuf>,
        #[arg(long)]
        transcribe: bool,
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
    Stats,
    Pause { id: String },
    Resume { id: String },
    Cancel { id: String },
    Remove { id: String },
}
#[derive(Debug, Clone, Subcommand)]
pub enum QueueAction {
    Start {
        id: String,
        #[arg(long)]
        auto_next: bool,
    },
    Next {
        id: String,
        #[arg(skip)]
        after: Option<String>,
    },
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
    #[command(skip)]
    MoveAdjacent {
        id: String,
        up: bool,
    },
    Clear,
    Play {
        #[arg(long)]
        auto_next: bool,
    },
}
impl Action {
    pub(crate) fn polls_browse(&self) -> bool {
        matches!(
            self,
            Self::List { .. }
                | Self::Bookmarks { .. }
                | Self::Library { .. }
                | Self::Episodes { .. }
                | Self::Downloads {
                    action: DownloadAction::List
                }
                | Self::Queue {
                    action: QueueAction::List
                }
        )
    }

    pub(crate) fn is_query(&self) -> bool {
        matches!(
            self,
            Self::Chapters { .. }
                | Self::Bookmarks { .. }
                | Self::Library { .. }
                | Self::List { .. }
                | Self::Episodes { .. }
                | Self::Search { .. }
                | Self::Browse { .. }
                | Self::Categories { .. }
                | Self::Charts { .. }
                | Self::Discover { .. }
                | Self::Preview { .. }
                | Self::Downloads {
                    action: DownloadAction::List
                }
                | Self::Queue {
                    action: QueueAction::List
                }
        )
    }

    /// Only actions that target a single episode may inherit the TUI player.
    pub(crate) fn episode_id_mut(&mut self) -> Option<&mut String> {
        match self {
            Self::Jump { id, .. }
            | Self::Chapters { id }
            | Self::Bookmarks { id }
            | Self::Bookmark { id, .. }
            | Self::Save { id, .. }
            | Self::Dismiss { id }
            | Self::Play { id, .. }
            | Self::Mark { id, .. }
            | Self::Subtitles { id, .. }
            | Self::Download { id } => Some(id),
            Self::Queue {
                action:
                    QueueAction::Add { id }
                    | QueueAction::Remove { id }
                    | QueueAction::Start { id, .. }
                    | QueueAction::Next { id, .. }
                    | QueueAction::Move { id, .. }
                    | QueueAction::MoveAdjacent { id, .. },
            } => Some(id),
            Self::Downloads {
                action:
                    DownloadAction::Pause { id }
                    | DownloadAction::Resume { id }
                    | DownloadAction::Cancel { id }
                    | DownloadAction::Remove { id },
            } => Some(id),
            _ => None,
        }
    }
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
    page_result(
        &items.iter().skip(p.offset).take(limit).collect::<Vec<_>>(),
        items.len(),
        p,
    )
}
pub(super) fn page_result<T: Serialize>(items: &[T], total: usize, p: crate::cli::Page) -> Value {
    json!({"items":items,"total":total,"offset":p.offset,"limit":if p.all {None}else{Some(p.limit)},"has_more":p.offset.saturating_add(items.len())<total})
}
pub fn refresh(
    config: &Config,
    store: &Store,
    id: Option<&str>,
    stop: &Arc<AtomicBool>,
) -> Result<Value> {
    if stop.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(DomainError::new(ErrorCode::Cancelled).into());
    }
    let feeds = if let Some(id) = id {
        vec![store.podcast_feed(id)?]
    } else {
        store.podcast_feeds()?
    };
    refresh_feeds(config, store, &feeds, stop)
}
pub(super) fn refresh_feeds(
    config: &Config,
    store: &Store,
    feeds: &[Feed],
    stop: &Arc<AtomicBool>,
) -> Result<Value> {
    let mut results = vec![];
    for batch in feeds.chunks(4) {
        let fetched = std::thread::scope(|scope| {
            let tasks: Vec<_> = batch
                .iter()
                .map(|f| scope.spawn(move || fetch(config, &f.url, Some(f), stop)))
                .collect();
            tasks
                .into_iter()
                .map(|t| {
                    t.join()
                        .unwrap_or_else(|_| Err(error(crate::t!("podcast_network"))))
                })
                .collect::<Vec<_>>()
        });
        for (f, fetched) in batch.iter().cloned().zip(fetched) {
            if stop.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(DomainError::new(ErrorCode::Cancelled).into());
            }
            let result = fetched.and_then(|p| {
                if stop.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err(DomainError::new(ErrorCode::Cancelled).into());
                }
                store.podcast_refresh_merge(&f, p)
            });
            match result {
                Ok(()) => {
                    if stop.load(std::sync::atomic::Ordering::Relaxed) {
                        return Err(DomainError::new(ErrorCode::Cancelled).into());
                    }
                    results.push(json!({"id":f.id,"title":f.title,"ok":true}));
                }
                Err(e) => {
                    if e.downcast_ref::<DomainError>()
                        .is_some_and(|e| e.code == ErrorCode::Cancelled)
                    {
                        return Err(e);
                    }
                    if stop.load(std::sync::atomic::Ordering::Relaxed) {
                        return Err(DomainError::new(ErrorCode::Cancelled).into());
                    }
                    let mut f = f;
                    f.error = Some(e.to_string());
                    f.failures = f.failures.saturating_add(1);
                    f.refreshed_at = Some(chrono::Utc::now().to_rfc3339());
                    store.podcast_refresh_error(&f)?;
                    results
                        .push(json!({"id":f.id,"title":f.title,"ok":false,"error":e.to_string()}));
                }
            }
        }
    }
    Ok(json!(results))
}
pub(super) fn episode_value(
    store: &Store,
    e: &Episode,
    is_file: impl FnOnce(&str) -> bool,
) -> Result<Value> {
    let mut value = json!(e);
    let feed = store.podcast_feed(&e.feed_id)?;
    value["author"] = json!(feed.author);
    value["feed_title"] = json!(feed.title);
    value["saved"] = json!(store.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM podcast_saved WHERE episode_id=?1)",
        [&e.id],
        |r| r.get::<_, bool>(0)
    )?);
    value["queued"] = json!(store.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM podcast_queue WHERE episode_id=?1)",
        [&e.id],
        |r| r.get::<_, bool>(0)
    )?);
    if let Some(d) = store.podcast_download(&e.id)? {
        value["download_state"] = json!(if d.state == "complete"
            && d.offline_path_if(e, is_file).is_none()
        {
            "stale"
        } else {
            &d.state
        });
    }
    Ok(value)
}
pub fn manage(
    config: &Config,
    store: &Store,
    action: Action,
    stop: &Arc<AtomicBool>,
) -> Result<Value> {
    let files = Arc::new(super::download_files::DownloadFiles::new(stop.clone()));
    manage_with_files(config, store, action, stop, &files)
}

pub(super) fn manage_with_files(
    config: &Config,
    store: &Store,
    mut action: Action,
    stop: &Arc<AtomicBool>,
    files: &Arc<super::download_files::DownloadFiles>,
) -> Result<Value> {
    if let Some(id) = action.episode_id_mut() {
        *id = store.podcast_episode_id(id)?;
    }
    Ok(match action {
        Action::Chapters { id } => {
            super::extras::chapters(config, &store.podcast_episode(&id)?, stop)?
        }
        Action::Bookmarks { id } => {
            let rows: Vec<_> = store.podcast_bookmarks(&id)?.iter().map(|b| json!({"id":b.episode_id,"bookmark_id":b.id,"title":b.note,"position_ms":b.position_ms,"kind":"bookmark"})).collect();
            json!({"items":rows,"total":rows.len()})
        }
        Action::Bookmark { id, time, note } => json!(
            store.podcast_add_bookmark(
                &id,
                parse_time(
                    time.as_deref()
                        .ok_or_else(|| error(crate::t!("podcast_invalid_time")))?
                )?,
                &note
            )?
        ),
        Action::BookmarkEdit { id, note } => {
            store.podcast_edit_bookmark(&id, note.as_deref())?;
            json!({"id":id,"removed":note.is_none()})
        }
        Action::Settings { id, key, value } => match (key, value) {
            (None, None) => json!(store.podcast_settings(&id)?),
            (Some(key), Some(value)) => json!(store.podcast_set_setting(&id, &key, &value)?),
            _ => return Err(error(crate::t!("podcast_invalid_setting"))),
        },
        Action::Jump { .. } | Action::Sleep { .. } => {
            return Err(error(crate::t!("podcast_playback_only")));
        }
        Action::Library { options } => store.podcast_library_with_files(&options, files)?,
        Action::Save { id, remove } => {
            store.podcast_save_episode(&id, remove)?;
            json!({"id":id,"saved":!remove})
        }
        Action::Dismiss { id } => {
            store.podcast_dismiss_inbox(&id)?;
            json!({"id":id,"dismissed":true})
        }
        Action::MarkBatch { ids, unplayed } => {
            let tx = rusqlite::Transaction::new_unchecked(
                &store.conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            for id in &ids {
                let e = store.podcast_episode(id)?;
                let mut progress = e.progress();
                progress.position_ms = if unplayed {
                    0
                } else {
                    e.observed_duration_ms
                        .or(e.duration_ms)
                        .unwrap_or(e.position_ms)
                };
                progress.completed = !unplayed;
                Store::write_podcast_progress(&tx, &e.id, &progress)?;
            }
            tx.commit()?;
            json!({"marked":ids.len()})
        }
        Action::Resume | Action::Relative { .. } => {
            return Err(error(crate::t!("podcast_playback_only")));
        }
        Action::Browse { country } => directory::home(&directory::country(config, &country)?),
        Action::Categories { country } => {
            directory::categories(&directory::country(config, &country)?)
        }
        Action::Charts {
            country,
            category,
            limit,
            random,
        } => directory::charts(
            config,
            store,
            &directory::country(config, &country)?,
            category.as_deref(),
            limit,
            random,
            stop,
        )?,
        Action::Search {
            query,
            country,
            limit,
        } => {
            let country = directory::country(config, &country)?;
            let mut rows = search(config, &query, &country, limit.into(), stop)?;
            directory::annotate_subscriptions(store, &mut rows)?;
            json!(rows)
        }
        Action::Discover { url } => json!(discover(config, &url, stop)?),
        Action::Preview { url } => {
            let subscription_url = url.clone();
            let resolved = if feed::is_apple_url(&url) {
                let urls = discover(config, &url, stop)?;
                if urls.len() != 1 {
                    return Err(error(crate::t!("podcast_no_public_feed")));
                }
                urls[0].clone()
            } else {
                url
            };
            let mut value = json!(fetch(config, &resolved, None, stop)?);
            value["subscription_url"] = json!(subscription_url);
            value
        }
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
        Action::List { page: p, query } => {
            let query = query.to_lowercase();
            let feeds: Vec<_> = store
                .podcast_feeds()?
                .into_iter()
                .filter(|feed| feed.title.to_lowercase().contains(&query))
                .collect();
            let mut rows = Vec::new();
            for feed in feeds {
                let mut value = json!(feed);
                value["unplayed_count"] = json!(store.podcast_unplayed_count(&feed.id)?);
                value["sort"] = json!(store.podcast_settings(&feed.id)?.sort);
                rows.push(value);
            }
            page(&rows, p)
        }
        Action::Episodes { id, query, page: p } => {
            let (items, total) = store.podcast_episodes_page(&id, &query, &p)?;
            let items = items
                .iter()
                .map(|e| episode_value(store, e, |path| files.is_file(path)))
                .collect::<Result<Vec<_>>>()?;
            let mut value = page_result(&items, total, p);
            value["feed_title"] = json!(store.podcast_feed(&id)?.title);
            value
        }
        Action::Refresh { id } => refresh(config, store, id.as_deref(), stop)?,
        Action::Unsubscribe { id } => {
            store.podcast_unsubscribe(&id)?;
            json!({"id":id,"subscribed":false})
        }
        Action::Mark { id, state } => {
            let e = store.podcast_episode(&id)?;
            store.podcast_progress(
                &e,
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
        Action::Subtitles {
            id,
            file,
            transcribe,
        } => {
            super::transcript::manage(config, store, &id, file.as_deref(), transcribe, stop, None)?
        }
        Action::Download { id } => json!(super::download::foreground(config, store, &id, stop)?),
        Action::Downloads { action } => match action {
            DownloadAction::Stats => {
                let rows = store.podcast_downloads()?;
                let bytes: u64 = rows.iter().map(|d| d.bytes).sum();
                let directory = config
                    .settings
                    .podcasts
                    .directory
                    .clone()
                    .unwrap_or_else(|| config.data_dir.join("podcasts"));
                json!({"downloads":rows.len(),"bytes":bytes,"completed":rows.iter().filter(|d| d.state=="complete").count(),"available_bytes":fs2::available_space(directory).ok()})
            }
            DownloadAction::List => {
                let mut rows = Vec::new();
                for d in store.podcast_downloads()? {
                    let mut value =
                        episode_value(store, &store.podcast_episode(&d.episode_id)?, |path| {
                            files.is_file(path)
                        })?;
                    value
                        .as_object_mut()
                        .unwrap()
                        .extend(json!(d).as_object().unwrap().clone());
                    rows.push(value);
                }
                json!(rows)
            }
            DownloadAction::Resume { id } => {
                json!(super::download::foreground(config, store, &id, stop)?)
            }
            DownloadAction::Pause { ref id }
            | DownloadAction::Cancel { ref id }
            | DownloadAction::Remove { ref id } => {
                let d = store.podcast_download_transaction(id, |current| {
                    let mut d = current.ok_or_else(|| error(crate::t!("podcast_not_found")))?;
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
                    Ok(d)
                })?;
                json!(d)
            }
        },
        Action::Queue {
            action: QueueAction::Start { .. },
        } => return Err(error(crate::t!("podcast_playback_only"))),
        Action::Queue {
            action: QueueAction::List,
        } => files.read_page(store, |store| {
            let items = store
                .podcast_queue()?
                .iter()
                .map(|e| episode_value(store, e, |path| files.observed_file(path)))
                .collect::<Result<Vec<_>>>()?;
            Ok(json!(items))
        })?,
        Action::Queue { action } => {
            json!(store.podcast_queue_update(action)?)
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
            if stop.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(DomainError::new(ErrorCode::Cancelled).into());
            }
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
                for batch in urls.chunks(4) {
                    let fetched = std::thread::scope(|scope| {
                        let tasks: Vec<_> = batch
                            .iter()
                            .map(|u| scope.spawn(move || fetch(config, u, None, stop)))
                            .collect();
                        tasks
                            .into_iter()
                            .map(|t| {
                                t.join()
                                    .unwrap_or_else(|_| Err(error(crate::t!("podcast_network"))))
                            })
                            .collect::<Vec<_>>()
                    });
                    for (u, fetched) in batch.iter().zip(fetched) {
                        if stop.load(std::sync::atomic::Ordering::Relaxed) {
                            return Err(DomainError::new(ErrorCode::Cancelled).into());
                        }
                        match fetched.and_then(|p| {
                            if stop.load(std::sync::atomic::Ordering::Relaxed) {
                                return Err(DomainError::new(ErrorCode::Cancelled).into());
                            }
                            store.podcast_subscribe(
                                &p.ok_or_else(|| error(crate::t!("podcast_invalid_feed")))?,
                                u,
                            )
                        }) {
                            Ok(f) => results.push(json!({"id":f.id,"ok":true})),
                            Err(e) => {
                                if e.downcast_ref::<DomainError>()
                                    .is_some_and(|e| e.code == ErrorCode::Cancelled)
                                {
                                    return Err(e);
                                }
                                if stop.load(std::sync::atomic::Ordering::Relaxed) {
                                    return Err(DomainError::new(ErrorCode::Cancelled).into());
                                }
                                results.push(json!({"ok":false,"error":e.to_string()}));
                            }
                        }
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
