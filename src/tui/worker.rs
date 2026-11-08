mod query;

use super::command::{Command, CommandReceiver};
use crate::{
    config::Config,
    directory::Directory,
    engine::{Engine, Status},
    model::{Filter, Recording, Station},
    store::{ListRevision, Store},
};
use anyhow::{Result, bail};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone)]
pub(super) struct LocatedStation {
    pub request: u64,
    pub filter: Result<Filter, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum PagePhase {
    Loading,
    /// Revalidate the current page while keeping its last successful rows
    /// visible. This avoids a blank frame for mutations such as playback
    /// history updates.
    Refreshing,
    Ready,
    Error(String),
}
#[derive(Clone)]
pub(super) struct PageState {
    // Identifies the visible request as well as Pending's latest queued query.
    pub generation: u64,
    pub requested: Filter,
    pub phase: PagePhase,
}

pub(super) enum PageStatus<'a> {
    Loading,
    Ready,
    Failed(&'a str),
}

#[derive(Clone)]
pub(super) struct View {
    pub podcast_items: Arc<Vec<super::podcast::Item>>,
    pub podcast_total: usize,
    pub podcast_browse: bool,
    pub podcast_page: Option<crate::podcast::Action>,
    pub podcast_request: Option<u64>,
    // Every browse has an identity, including results that are not polled.
    podcast_pending: Option<(Option<u64>, crate::podcast::Action)>,
    pub podcast_task: String,
    // Retries retain the submitted identity, even before the first publication.
    podcast_poll: Option<(crate::podcast::Action, Option<u64>, Instant)>,
    pub subtitles: crate::speech::Snapshot,
    pub artwork_station: Option<String>,
    pub track_artwork: Option<crate::engine::artwork::TrackArtwork>,
    published: Option<(u64, Arc<View>)>,
    queries: Option<query::Sender>,
    pub map_image: Option<Arc<super::map::Image>>,
    pub map_request: Option<super::map::Request>,
    pub map_error: Option<String>,
    pub geo_revision: u64,
    store_revision: Option<ListRevision>,
    refresh_requested: bool,
    page_retry_at: Option<Instant>,
    page_retry_attempt: u32,
    // Keep failure history across automatic retries for log deduplication and
    // backoff counting; a Loading page does not mean the failure has recovered.
    page_failure: Option<(Filter, String)>,
    pub immediate_revision: u64,
    pub recordings_revision: u64,
    pub logs_revision: u64,
    pub operation_error_id: u64,
    pub operation_error: String,
    pub facets: Arc<[Vec<crate::model::Facet>; 3]>,
    pub facets_loading: bool,
    pub facets_request: Option<(usize, String)>,
    pub facets_error: Option<String>,
    pub backups_errors: Arc<std::collections::BTreeMap<String, String>>,
    pub backups: Arc<std::collections::BTreeMap<String, Vec<String>>>,
    pub rows: Arc<Vec<Station>>,
    pub recordings: Arc<Vec<Recording>>,
    pub status: Status,
    pub catalog_note: String,
    pub task: String,
    pub task_active: bool,
    pub logs: Arc<VecDeque<Arc<str>>>,
    pub located: Option<LocatedStation>,
    pub inspected: Option<Station>,
    pub inspected_request: u64,
    pub inspected_error: Option<String>,
    pub revision: u64,
    pub rows_revision: u64,
    pub filter: Filter,
    pub page: Option<PageState>,
    pub total: usize,
    pub recordings_total: usize,
}
impl View {
    pub fn clear_map(&mut self) {
        if let Some(queries) = &self.queries {
            queries.cancel_map();
        }
        self.map_request = None;
        self.map_image = None;
        self.map_error = None;
        self.revision += 1;
    }
    pub fn request_map(&mut self, request: super::map::Request) {
        if let Some(queries) = &self.queries {
            queries.map(request.clone());
            self.map_request = Some(request);
            self.map_error = None;
        }
    }

    pub fn new(status: Status) -> Self {
        Self {
            subtitles: crate::speech::Snapshot::default(),
            artwork_station: None,
            track_artwork: None,
            published: None,
            queries: None,
            map_image: None,
            map_request: None,
            map_error: None,
            geo_revision: 0,
            store_revision: None,
            refresh_requested: false,
            page_retry_at: None,
            page_retry_attempt: 0,
            page_failure: None,
            immediate_revision: 1,
            podcast_items: Arc::new(vec![]),
            podcast_total: 0,
            podcast_browse: false,
            podcast_page: None,
            podcast_request: None,
            podcast_pending: None,
            podcast_task: String::new(),
            podcast_poll: None,
            recordings_revision: 0,
            logs_revision: 0,
            operation_error_id: 0,
            operation_error: String::new(),
            facets: Default::default(),
            facets_loading: false,
            facets_request: None,
            facets_error: None,
            backups_errors: Default::default(),
            backups: Default::default(),
            page: None,
            total: 0,
            recordings_total: 0,
            rows: Default::default(),
            recordings: Default::default(),
            status,
            catalog_note: crate::t!("catalog_loading").into(),
            task: String::new(),
            task_active: false,
            logs: Default::default(),
            located: None,
            inspected: None,
            inspected_request: 0,
            inspected_error: None,
            revision: 1,
            rows_revision: 0,
            filter: Filter {
                limit: 100,
                ..Filter::default()
            },
        }
    }
    pub fn page_ready(&self, filter: &Filter) -> bool {
        // "Ready" describes usable visible rows, including the last successful
        // snapshot of this exact query while its replacement is being read.
        matches!(self.page_status(filter), PageStatus::Ready)
    }
    pub fn page_status(&self, filter: &Filter) -> PageStatus<'_> {
        match self.page.as_ref() {
            Some(page)
                if self.filter == *filter
                    && (page.phase == PagePhase::Ready
                        || page.phase == PagePhase::Refreshing && page.requested == *filter) =>
            {
                PageStatus::Ready
            }
            Some(PageState {
                requested,
                phase: PagePhase::Error(error),
                ..
            }) if requested == filter => PageStatus::Failed(error),
            _ => PageStatus::Loading,
        }
    }
    pub fn page_error(&self, filter: &Filter) -> Option<&str> {
        match self.page_status(filter) {
            PageStatus::Failed(error) => Some(error),
            _ => None,
        }
    }
    pub fn stations_for(&self, filter: &Filter) -> &[Station] {
        if self.page_ready(filter) {
            &self.rows
        } else {
            &[]
        }
    }
    pub fn recordings_for(&self, filter: &Filter) -> &[Recording] {
        if self.page_ready(filter) {
            &self.recordings
        } else {
            &[]
        }
    }
    fn fail_page(&mut self, filter: &Filter, error: String) {
        if self.page_failure.as_ref().is_none_or(|(f, _)| f != filter) {
            self.page_retry_attempt = 0;
        }
        let delay = Duration::from_millis((500_u64 << self.page_retry_attempt.min(6)).min(30_000));
        self.page_retry_at = Some(Instant::now() + delay);
        self.page_retry_attempt = self.page_retry_attempt.saturating_add(1);
        // The retry deadline owns this request even if list_revision() failed.
        self.refresh_requested = false;
        let generation = self.page.as_ref().map_or(0, |p| p.generation);
        self.page = Some(PageState {
            generation,
            requested: filter.clone(),
            phase: PagePhase::Error(error.clone()),
        });
        if self
            .page_failure
            .as_ref()
            .is_none_or(|(f, e)| f != filter || e != &error)
        {
            self.log(&crate::t!("list_query_failed", "detail" => error));
        }
        self.page_failure = Some((filter.clone(), error));
        self.revision += 1;
        self.immediate_revision += 1;
    }
    fn page_succeeded(&mut self) {
        self.page_retry_at = None;
        self.page_retry_attempt = 0;
        self.page_failure = None;
    }
    pub fn snapshot(&mut self) -> Arc<Self> {
        if let Some((revision, view)) = &self.published
            && *revision == self.revision
        {
            return view.clone();
        }
        let mut view = self.clone();
        view.published = None;
        view.queries = None;
        let snapshot = Arc::new(view);
        self.published = Some((self.revision, snapshot.clone()));
        snapshot
    }
    fn fail_operation(&mut self, message: &str) {
        self.operation_error_id += 1;
        self.operation_error = super::display::diagnostic(message);
        self.log(message);
    }
    pub(super) fn update_subtitles(&mut self, snapshot: &crate::speech::Snapshot) {
        if self.subtitles.revision != snapshot.revision {
            if snapshot.failed && !self.subtitles.failed {
                self.fail_operation(&snapshot.message);
            }
            self.subtitles = snapshot.clone();
            self.revision += 1;
            self.immediate_revision += 1;
        }
    }
    fn publish_podcast(&mut self, result: Result<serde_json::Value>) {
        match result {
            Ok(value) => {
                if let Some((request, action)) = self.podcast_pending.take() {
                    self.podcast_request = request;
                    self.podcast_page = Some(action);
                    self.podcast_items = Arc::new(super::podcast::items(&value));
                    self.podcast_total = value["total"]
                        .as_u64()
                        .map_or(self.podcast_items.len(), |v| v as usize);
                    self.podcast_browse = false;
                }
                self.log(crate::t!("podcast_operation_done"));
                self.revision += 1;
            }
            Err(error) => {
                self.podcast_pending = None;
                self.podcast_browse = false;
                self.fail_operation(&error.to_string());
            }
        }
    }
    fn log(&mut self, message: &str) {
        let message = crate::clean_text(&super::display::diagnostic(message));
        Arc::make_mut(&mut self.logs)
            .push_back(format!("{} {message}", chrono::Local::now().format("%H:%M:%S")).into());
        while self.logs.len() > 200 {
            Arc::make_mut(&mut self.logs).pop_front();
        }
        self.revision += 1;
        self.logs_revision += 1;
        self.immediate_revision += 1;
    }
}

pub(super) struct CatalogJob {
    cancel: Arc<AtomicBool>,
    progress: Arc<AtomicUsize>,
    result: Receiver<Result<usize>>,
    handle: Option<thread::JoinHandle<()>>,
    bootstrap: bool,
}
impl CatalogJob {
    fn start(config: Config, bootstrap: bool, accept_shrink: bool) -> Result<Self> {
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicUsize::new(0));
        let job_cancel = cancel.clone();
        let job_progress = progress.clone();
        let (tx, result) = mpsc::sync_channel(1);
        let handle = thread::Builder::new()
            .name("radio-catalog".into())
            .spawn(move || {
                let result = (|| -> Result<usize> {
                    let mut store = Store::open(&config.data_dir.join("radio.db"))?;
                    let mut directory = Directory::new(&config, job_cancel)?;
                    if bootstrap {
                        directory.bootstrap(&mut store)
                    } else {
                        directory.sync_with_policy(
                            &mut store,
                            |n| {
                                job_progress.store(n, Ordering::Relaxed);
                            },
                            accept_shrink,
                        )
                    }
                })();
                let _ = tx.send(result);
            })?;
        Ok(Self {
            cancel,
            progress,
            result,
            handle: Some(handle),
            bootstrap,
        })
    }
}
impl Drop for CatalogJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        // Cancellation is checked before publication; do not join a
        // blocking HTTP request while shutting down media.
        self.handle.take();
    }
}

pub(super) fn auto_sync_due(store: &Store, days: u16) -> Result<bool> {
    if days == 0 {
        return Ok(false);
    }
    Ok(store.meta("last_sync")?.is_some_and(|time| {
        chrono::DateTime::parse_from_rfc3339(&time).map_or(true, |time| {
            chrono::Utc::now().signed_duration_since(time).num_days() >= i64::from(days)
        })
    }))
}

fn catalog_note(store: &Store, days: u16) -> Result<String> {
    Ok(match store.meta("last_sync")? {
        Some(time) => match chrono::DateTime::parse_from_rfc3339(&time) {
            Ok(time)
                if days == 0
                    || chrono::Utc::now().signed_duration_since(time).num_days()
                        < i64::from(days) =>
            {
                crate::t!("catalog_synced", "time" => time)
            }
            _ => crate::t!("catalog_stale").into(),
        },
        None => crate::t!("catalog_partial").into(),
    })
}

pub(super) fn refresh(engine: &Engine, filter: &Filter, view: &Arc<Mutex<View>>) -> Result<()> {
    let result = refresh_page(engine, filter, view);
    if let Err(error) = &result {
        let mut data = view.lock().unwrap();
        data.fail_page(filter, format!("{error:#}"));
    }
    result
}

fn refresh_page(engine: &Engine, filter: &Filter, view: &Arc<Mutex<View>>) -> Result<()> {
    let store_revision = engine.store().list_revision(filter)?;
    view.lock().unwrap().store_revision = Some(store_revision);
    let note = catalog_note(engine.store(), engine.config().settings.auto_sync_days)?;
    {
        let mut data = view.lock().unwrap();
        if let Some(sender) = data.queries.clone() {
            data.store_revision = Some(store_revision);
            data.refresh_requested = false;
            data.page_retry_at = None;
            let generation = sender.page(filter.clone(), store_revision, note);
            let phase = if data.page_ready(filter) {
                PagePhase::Refreshing
            } else {
                PagePhase::Loading
            };
            data.page = Some(PageState {
                generation,
                requested: filter.clone(),
                phase,
            });
            data.revision += 1;
            data.immediate_revision += 1;
            return Ok(());
        }
    }
    if filter.recordings {
        let recordings = engine
            .store()
            .recordings_page(filter.limit, filter.offset)?;
        let total = engine.store().recordings_count()?;
        let mut data = view.lock().unwrap();
        data.recordings = Arc::new(recordings);
        data.recordings_total = total;
        data.recordings_revision += 1;
    } else {
        let rows = engine.store().search(filter)?;
        let total = engine.store().search_count(filter)?;
        let mut data = view.lock().unwrap();
        data.rows = Arc::new(rows);
        data.backups = Default::default();
        data.backups_errors = Default::default();
        data.total = total;
        data.rows_revision += 1;
    }
    let mut data = view.lock().unwrap();
    data.store_revision = Some(store_revision);
    data.refresh_requested = false;
    data.page_succeeded();
    data.catalog_note = note;
    data.filter = filter.clone();
    data.page = Some(PageState {
        generation: 0,
        requested: filter.clone(),
        phase: PagePhase::Ready,
    });
    data.revision += 1;
    data.immediate_revision += 1;
    Ok(())
}

fn request_refresh(view: &Arc<Mutex<View>>) {
    let mut data = view.lock().unwrap();
    data.refresh_requested = true;
    data.page_retry_at = None;
    data.page_retry_attempt = 0;
}

pub(super) fn refresh_if_needed(
    engine: &Engine,
    filter: &Filter,
    view: &Arc<Mutex<View>>,
    recording_ended: bool,
) -> Result<()> {
    // Probe revisions on every tick so external commits bypass page backoff.
    let revision = match engine.store().list_revision(filter) {
        Ok(revision) => revision,
        Err(error) => {
            let mut data = view.lock().unwrap();
            if data.refresh_requested
                || recording_ended
                || data.page_retry_at.is_none_or(|at| Instant::now() >= at)
            {
                data.fail_page(filter, format!("{error:#}"));
            }
            return Ok(());
        }
    };
    let mut data = view.lock().unwrap();
    // A page read can pin store_revision before this first probe, so the
    // geographic revision must also be read while it is still unset. Otherwise
    // the Explore map compares every image against a stale zero and never
    // renders the density grid.
    if data.store_revision != Some(revision) || data.geo_revision == 0 {
        let geo_revision = engine.store().geo_revision()?;
        if data.geo_revision != geo_revision {
            data.geo_revision = geo_revision;
            data.revision += 1;
            data.immediate_revision += 1;
        }
    }
    let needed = data.refresh_requested
        || data.store_revision != Some(revision)
        || recording_ended
        || data.page_retry_at.is_some_and(|at| Instant::now() >= at);
    drop(data);
    if needed {
        let _ = refresh(engine, filter, view);
    }
    Ok(())
}

fn favorite_changed(store: &Store, view: &Arc<Mutex<View>>, uuid: &str, add: bool) -> Result<()> {
    let name = super::state::target_name(
        &store.station(uuid).map(|s| s.name).unwrap_or_default(),
        uuid,
    );
    let mut data = view.lock().unwrap();
    data.log(&if add {
        crate::t!("station_favorited", "name" => name)
    } else {
        crate::t!("station_unfavorited", "name" => name)
    });
    if let Some(station) = data.inspected.as_mut().filter(|s| s.stationuuid == uuid) {
        station.favorite = add;
    }
    drop(data);
    request_refresh(view);
    Ok(())
}

fn load_backups(store: &Store, uuid: &str, view: &Arc<Mutex<View>>) {
    let result = store.backups(uuid);
    let mut data = view.lock().unwrap();
    match result {
        Ok(backups) => {
            Arc::make_mut(&mut data.backups_errors).remove(uuid);
            Arc::make_mut(&mut data.backups).insert(uuid.to_owned(), backups);
        }
        Err(error) => {
            let message = crate::t!("backups_query_failed", "detail" => format!("{error:#}"));
            if data.backups_errors.get(uuid) == Some(&message) {
                return;
            }
            data.log(&message);
            Arc::make_mut(&mut data.backups_errors).insert(uuid.to_owned(), message);
        }
    }
    data.revision += 1;
    data.immediate_revision += 1;
}

pub(super) fn execute(
    command: Command,
    engine: &mut Engine,
    filter: &mut Filter,
    job: &mut Option<CatalogJob>,
    view: &Arc<Mutex<View>>,
) -> Result<()> {
    let podcast_request = match &command {
        Command::PodcastRequest { request, .. } => Some(*request),
        _ => None,
    };
    match command {
        Command::Podcast(action) | Command::PodcastRequest { action, .. } => {
            use crate::podcast::{Action, DownloadAction, QueueAction};
            let browse = super::podcast::is_browse(&action);
            if browse {
                let periodic = matches!(
                    &action,
                    Action::List { .. }
                        | Action::Episodes { .. }
                        | Action::Downloads {
                            action: DownloadAction::List
                        }
                        | Action::Queue {
                            action: QueueAction::List
                        }
                );
                view.lock().unwrap().podcast_poll =
                    periodic.then(|| (action.clone(), podcast_request, Instant::now()));
                let source = match &action {
                    Action::Downloads { .. } => "podcast_downloads",
                    Action::Queue { .. } => "podcast_queue",
                    _ => "podcasts",
                };
                engine.save_preference("pref_source", source)?;
                let mut data = view.lock().unwrap();
                data.podcast_browse = true;
                data.podcast_pending = Some((podcast_request, action.clone()));
                data.podcast_page = None;
                data.podcast_items = Arc::new(vec![]);
                data.revision += 1;
                engine.cancel_podcast();
            }
            if let Err(error) = engine.podcast_action(action) {
                if browse {
                    let mut data = view.lock().unwrap();
                    data.podcast_pending = None;
                    data.podcast_browse = false;
                }
                return Err(error);
            }
        }
        Command::Subtitles(action) => match action.as_str() {
            "on" => engine.set_subtitles(true),
            "off" => engine.set_subtitles(false),
            "toggle" => engine.set_subtitles(!engine.subtitles().enabled),
            "clear" => engine.clear_subtitles(),
            "retry" => engine.retry_subtitles(),
            _ => unreachable!(),
        },
        Command::SubtitleModel(id) => engine.set_subtitle_model(id)?,
        Command::LocatePlaying {
            uuid,
            request,
            limit,
        } => {
            if let Some(sender) = view.lock().unwrap().queries.clone() {
                sender.locate(uuid, request, limit);
                return Ok(());
            }
            let result = (|| -> Result<_> {
                let mut target = Filter {
                    // The rank and subsequent page must use the same stable order;
                    // do not inherit a random sort from the current search.
                    limit: limit.clamp(1, 1000),
                    ..Filter::default()
                };
                let position = engine
                    .store()
                    .station_position(&uuid, &target)?
                    .ok_or_else(|| anyhow::anyhow!(crate::t!("jump_unavailable")))?;
                target.offset = position / target.limit * target.limit;
                Ok(target)
            })();
            let mut data = view.lock().unwrap();
            data.located = Some(LocatedStation {
                request,
                filter: result.map_err(|error| format!("{error:#}")),
            });
            data.revision += 1;
            data.immediate_revision += 1;
        }
        Command::Facets { selected, prefix } => {
            if selected >= 3 {
                bail!(crate::t!("invalid_filter_field"));
            }
            let mut data = view.lock().unwrap();
            data.facets_request = Some((selected, prefix));
            data.facets_error = None;
            data.revision += 1;
            data.immediate_revision += 1;
            data.facets_loading = true;
            if let Some(sender) = data.queries.clone() {
                sender.facets(selected, data.facets_request.as_ref().unwrap().1.clone());
            } else {
                // Isolated owner tests do not run the query service.
                use crate::model::FacetKind;
                let kind = [FacetKind::Country, FacetKind::Language, FacetKind::Tag][selected];
                let prefix = &data.facets_request.as_ref().unwrap().1;
                Arc::make_mut(&mut data.facets)[selected] =
                    engine.store().facets_limited(kind, prefix, 100)?;
                data.facets_loading = false;
            }
        }
        Command::Search(value) => {
            view.lock().unwrap().podcast_poll = None;
            let source = if value.geo.is_some() {
                super::state::Source::Explore
            } else if value.history {
                super::state::Source::History
            } else if value.favorites {
                super::state::Source::Favorites
            } else {
                super::state::Source::Stations
            };
            if let Err(error) = engine.save_preference("pref_source", source.preference()) {
                view.lock().unwrap().fail_operation(&format!("{error:#}"));
            }
            *filter = value;
            view.lock().unwrap().inspected = None;
            request_refresh(view);
        }
        Command::Play(uuid) => engine.play(&uuid)?,
        Command::PlayRecording(id) => engine.play_recording(&id)?,
        Command::DeleteRecording { id, index_only } => engine.delete_recording(id, index_only)?,
        Command::OpenRecordings => engine.open_recordings()?,
        Command::Random(filter) => engine.random(filter)?,
        Command::ToggleFavorite(uuid) => {
            let add = engine.toggle_favorite(&uuid)?;
            favorite_changed(engine.store(), view, &uuid, add)?;
        }
        Command::AdjustVolume(delta) => engine.volume(delta)?,
        Command::Favorite(uuid, add) => {
            engine.store().favorite(&uuid, add)?;
            favorite_changed(engine.store(), view, &uuid, add)?;
        }
        Command::Pause => engine.pause()?,
        Command::Mute => engine.mute()?,
        Command::SetLanguage(language) => {
            engine.set_language(language);
        }
        Command::Stop => {
            engine.stop()?;
        }
        Command::Volume(value) => {
            engine.volume(i32::from(value) - i32::from(engine.status().volume))?
        }
        Command::RecordToggle => engine.toggle_recording()?,
        Command::RecordStop => engine.stop_recording(None)?,
        Command::Record {
            uuid,
            duration,
            dir,
        } => {
            if let Some(uuid) = uuid {
                if engine
                    .status()
                    .station
                    .as_ref()
                    .is_none_or(|s| s.stationuuid != uuid)
                {
                    bail!(crate::t!("tui_record_current_only"));
                }
                engine.record_current(duration, dir.as_deref())?;
            } else {
                engine.record_current(duration, dir.as_deref())?;
            }
        }
        Command::Station(command) => {
            use crate::service::StationChange;
            let output = match engine.change_station(command)? {
                StationChange::Added(station) => {
                    crate::t!("station_added", "name" => station.name, "uuid" => station.stationuuid)
                }
                StationChange::Updated(station) => {
                    crate::t!("station_updated", "name" => station.name, "uuid" => station.stationuuid)
                }
                StationChange::Removed {
                    uuid,
                    placeholder_kept,
                } => {
                    crate::t!("station_removed", "uuid" => uuid, "references" => if placeholder_kept { crate::t!("station_references_kept") } else { "" })
                }
                StationChange::Backups { uuid, urls } => {
                    crate::t!("station_backups_updated", "uuid" => uuid, "urls" => urls.join(" · "))
                }
            };
            view.lock().unwrap().log(&output);
            request_refresh(view);
        }
        Command::Sync(accept_shrink) => {
            if job.is_some() {
                bail!(crate::t!("catalog_task_busy"));
            }
            *job = Some(CatalogJob::start(
                engine.config().clone(),
                false,
                accept_shrink,
            )?);
            let mut data = view.lock().unwrap();
            data.task_active = true;
            data.task = crate::t!("catalog_sync_progress", "count" => 0);
            data.log(crate::t!("catalog_sync_started"));
        }
        Command::Cancel => {
            let podcast = engine.cancel_podcast();
            let mut data = view.lock().unwrap();
            let podcast = podcast || data.podcast_browse;
            if podcast {
                data.podcast_browse = false;
                data.podcast_pending = None;
                data.podcast_poll = None;
                if data.podcast_items.is_empty() {
                    data.podcast_total = 0;
                }
                data.log(crate::t!("podcast_operation_cancelled"));
                data.revision += 1;
                data.immediate_revision += 1;
            }
            if let Some(job) = job {
                job.cancel.store(true, Ordering::Relaxed);
                data.task = crate::t!("catalog_task_cancelling").into();
                data.log(crate::t!("catalog_task_cancel_requested"));
            } else if !podcast {
                bail!(crate::t!("catalog_task_missing"));
            }
        }
        Command::Recordings(value) => {
            view.lock().unwrap().podcast_poll = None;
            if let Err(error) = engine.save_preference("pref_source", "recordings") {
                view.lock().unwrap().fail_operation(&format!("{error:#}"));
            }
            *filter = value;
            filter.recordings = true;
            request_refresh(view);
        }
        Command::Refresh => request_refresh(view),
        Command::Backups(uuid) => load_backups(engine.store(), &uuid, view),
        Command::Info(uuid, request) => {
            let result = match engine
                .status()
                .station
                .as_ref()
                .filter(|s| s.stationuuid == uuid)
            {
                Some(station) => Ok(station.clone()),
                None => engine.store().station(&uuid),
            };
            load_backups(engine.store(), &uuid, view);
            let mut data = view.lock().unwrap();
            data.inspected_request = request;
            data.inspected = None;
            data.inspected_error = None;
            match result {
                Ok(station) => data.inspected = Some(station),
                Err(error) => {
                    data.inspected_error =
                        Some(crate::t!("station_details_failed", "detail" => format!("{error:#}")))
                }
            }
            data.revision += 1;
        }
        Command::Doctor => {
            engine.start_diagnostics()?;
            view.lock().unwrap().log(crate::t!("diagnostics_started"));
        }
    }
    Ok(())
}

pub(super) fn run(
    mut engine: Engine,
    commands: CommandReceiver,
    view: Arc<Mutex<View>>,
    stop: Arc<AtomicBool>,
    mut filter: Filter,
) -> Result<()> {
    engine.enable_background_media()?;
    let queries = query::Worker::start(&engine.config().data_dir.join("radio.db"), &view)?;
    view.lock().unwrap().queries = Some(queries.sender.clone());
    refresh(&engine, &filter, &view)?;
    if let Err(error) = engine.resume_last_station() {
        view.lock()
            .unwrap()
            .log(&crate::t!("resume_playback_failed", "detail" => format!("{error:#}")));
    }
    let empty = engine.store().count()? == 0;
    let expired = auto_sync_due(engine.store(), engine.config().settings.auto_sync_days)?;
    let mut job = if empty || expired {
        let mut data = view.lock().unwrap();
        data.task_active = true;
        data.task = if empty {
            crate::t!("catalog_bootstrap_started")
        } else {
            crate::t!("catalog_auto_sync_started")
        }
        .into();
        let message = data.task.clone();
        data.log(&message);
        Some(CatalogJob::start(engine.config().clone(), empty, false)?)
    } else {
        None
    };
    let mut last_message = String::new();
    let mut was_recording = false;
    let result = (|| -> Result<()> {
        while !stop.load(Ordering::Relaxed) {
            match commands.recv_timeout(Duration::from_millis(100)) {
                Ok(command) => {
                    if let Err(error) = execute(command, &mut engine, &mut filter, &mut job, &view)
                    {
                        view.lock().unwrap().fail_operation(&format!("{error:#}"));
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            let finished = if let Some(active) = job.as_ref() {
                let mut data = view.lock().unwrap();
                if !active.cancel.load(Ordering::Relaxed) && !active.bootstrap {
                    let text = crate::t!("catalog_sync_progress", "count" => active.progress.load(Ordering::Relaxed));
                    if data.task != text {
                        data.task = text;
                        data.revision += 1;
                    }
                }
                match active.result.try_recv() {
                    Ok(result) => Some(result),
                    Err(mpsc::TryRecvError::Disconnected) => {
                        Some(Err(anyhow::anyhow!(crate::t!("catalog_task_exited"))))
                    }
                    Err(mpsc::TryRecvError::Empty) => None,
                }
            } else {
                None
            };
            if let Some(result) = finished {
                let active = job.take().unwrap();
                let cancelled = active.cancel.load(Ordering::Relaxed);
                let mut data = view.lock().unwrap();
                data.task_active = false;
                data.task = match result {
                    Ok(n) => {
                        if active.bootstrap {
                            crate::t!("catalog_bootstrap_done", "count" => n)
                        } else {
                            crate::t!("catalog_sync_done", "count" => n)
                        }
                    }
                    Err(_) if cancelled => crate::t!("catalog_task_cancelled").into(),
                    Err(error) => {
                        crate::t!("catalog_task_failed", "detail" => format!("{error:#}"))
                    }
                };
                let text = data.task.clone();
                data.log(&text);
                drop(data);
                drop(active);
                request_refresh(&view);
            }
            let repeat = {
                let mut data = view.lock().unwrap();
                if !data.podcast_browse {
                    data.podcast_poll.as_mut().and_then(|(a, request, at)| {
                        if at.elapsed() >= Duration::from_secs(3) {
                            *at = Instant::now();
                            Some((*request, a.clone()))
                        } else {
                            None
                        }
                    })
                } else {
                    None
                }
            };
            if let Some((request, action)) = repeat
                && engine.podcast_action(action.clone()).is_ok()
            {
                let mut data = view.lock().unwrap();
                data.podcast_pending = Some((request, action));
                data.podcast_browse = true;
            }
            if let Some(result) = engine.poll_podcast() {
                view.lock().unwrap().publish_podcast(result);
            }
            if let Some(result) = engine.poll_diagnostics() {
                let mut data = view.lock().unwrap();
                match result {
                    Ok((report, _)) => {
                        for check in report["checks"].as_array().into_iter().flatten() {
                            data.log(&format!(
                                "{} {}: {}",
                                if check["ok"] == true { "OK" } else { "FAIL" },
                                check["name"].as_str().unwrap_or(""),
                                check["detail"].as_str().unwrap_or("")
                            ));
                        }
                    }
                    Err(error) => {
                        data.log(&crate::t!("diagnostics_failed", "detail" => format!("{error:#}")))
                    }
                }
            }
            {
                let (downloads, network) = engine.podcast_tasks();
                let label = if downloads == 0 && !network {
                    String::new()
                } else {
                    crate::t!("podcast_tasks","downloads"=>downloads,"network"=>network)
                };
                let mut data = view.lock().unwrap();
                if data.podcast_task != label {
                    data.podcast_task = label;
                    data.revision += 1;
                }
            }
            engine.request_artwork(view.lock().unwrap().artwork_station.clone());
            if let Err(error) = engine.tick() {
                view.lock().unwrap().fail_operation(&format!("{error:#}"));
            }
            {
                let data = view.lock().unwrap();
                if data
                    .page
                    .as_ref()
                    .is_some_and(|p| p.requested == filter && p.phase == PagePhase::Ready)
                {
                    filter = data.filter.clone();
                }
            }
            refresh_if_needed(
                &engine,
                &filter,
                &view,
                was_recording && !engine.status().recording(),
            )?;
            was_recording = engine.status().recording();
            let mut data = view.lock().unwrap();
            data.update_subtitles(engine.subtitles());
            if &data.track_artwork != engine.track_artwork() {
                data.track_artwork = engine.track_artwork().clone();
                data.revision += 1;
                data.immediate_revision += 1;
            }
            if !data.status.ui_eq(engine.status()) {
                if !data.status.control_eq(engine.status()) {
                    data.immediate_revision += 1;
                }
                data.status = engine.status().clone();
                data.revision += 1;
                if engine.status().message != last_message {
                    last_message = engine.status().message.clone();
                    data.log(&last_message);
                }
            }
        }
        Ok(())
    })();
    // Stop media before waiting for a cancelled directory request to finish.
    if let Some(active) = job.as_ref() {
        active.cancel.store(true, Ordering::Relaxed);
    }
    view.lock().unwrap().queries = None;
    drop(queries);
    let shutdown = engine.shutdown();
    drop(job);
    result?;
    shutdown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_podcast_browsing_keeps_its_request_identity_on_retry() {
        use super::super::{
            command,
            state::{App, Focus, Source},
        };
        use crate::podcast::{Episode, Feed, Preview};
        use crossterm::event::{
            KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
        };
        for previous_success in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut config = crate::test_support::config(dir.path());
            config.settings.auto_sync_days = 0;
            config.settings.podcasts.refresh_hours = 0;
            let store = Store::open(&config.data_dir.join("radio.db")).unwrap();
            store
                .upsert(&Station {
                    stationuuid: uuid::Uuid::new_v4().to_string(),
                    name: "Radio".into(),
                    active: true,
                    ..Default::default()
                })
                .unwrap();
            let preview = Preview {
                feed: Feed {
                    url: "https://example.test/feed".into(),
                    ..Default::default()
                },
                episodes: (0..2)
                    .map(|i| Episode {
                        guid: Some(i.to_string()),
                        title: format!("Episode {i}"),
                        url: format!("https://example.test/{i}.mp3"),
                        ..Default::default()
                    })
                    .collect(),
            };
            let feed = store
                .podcast_subscribe(&preview, &preview.feed.url)
                .unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let engine = Engine::new(config, stop.clone()).unwrap();
            let view = Arc::new(Mutex::new(View::new(engine.status().clone())));
            let (sender, receiver) = command::channel(8);
            let output = view.clone();
            let flag = stop.clone();
            let worker =
                thread::spawn(move || run(engine, receiver, output, flag, Filter::default()));
            let mut app = App::default();
            app.source = Source::Podcasts;
            app.focus = Focus::List;
            app.podcast_feed = Some(feed.id);
            if previous_success {
                app.flush_search(&sender);
                let deadline = Instant::now() + Duration::from_secs(3);
                while view.lock().unwrap().podcast_request.is_none() {
                    assert!(Instant::now() < deadline);
                    thread::sleep(Duration::from_millis(5));
                }
            }
            let previous = view.lock().unwrap().podcast_request;
            // Force a query error on the real podcast worker connection, then
            // repair the fixture before the scheduled retry can execute.
            let episodes = store
                .podcast_episodes(&app.podcast_feed.clone().unwrap())
                .unwrap();
            store
                .conn
                .execute("UPDATE podcast_episodes SET json='invalid json'", [])
                .unwrap();
            app.filter.query = "Episode".into();
            app.flush_search(&sender);
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                let data = view.lock().unwrap();
                if data.operation_error_id > 0 && !data.podcast_browse {
                    assert_eq!(data.podcast_request, previous);
                    break;
                }
                assert!(Instant::now() < deadline);
                drop(data);
                thread::sleep(Duration::from_millis(5));
            }
            for episode in episodes {
                store
                    .conn
                    .execute(
                        "UPDATE podcast_episodes SET json=?2 WHERE id=?1",
                        rusqlite::params![episode.id, serde_json::to_string(&episode).unwrap()],
                    )
                    .unwrap();
            }
            {
                let mut data = view.lock().unwrap();
                data.podcast_poll.as_mut().unwrap().2 = Instant::now() - Duration::from_secs(3);
            }
            let deadline = Instant::now() + Duration::from_secs(3);
            let recovered = loop {
                let data = view.lock().unwrap();
                if !data.podcast_browse && data.podcast_items.len() == 2 {
                    break data.clone();
                }
                assert!(Instant::now() < deadline);
                drop(data);
                thread::sleep(Duration::from_millis(5));
            };
            stop.store(true, Ordering::Relaxed);
            worker.join().unwrap().unwrap();
            assert_ne!(recovered.podcast_request, previous);
            let (sender, actions) = command::channel(8);
            app.key(
                KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
                &recovered,
                &sender,
            );
            assert_eq!(app.list.selected(), Some(1));
            app.key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                &recovered,
                &sender,
            );
            assert!(
                matches!(actions.try_recv(), Ok(Command::Podcast(crate::podcast::Action::Play { id, .. })) if id == recovered.podcast_items[1].id)
            );
            app.rects[Focus::List.index()] = ratatui::layout::Rect::new(0, 0, 40, 5);
            let click = MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 2,
                row: 1,
                modifiers: KeyModifiers::NONE,
            };
            let now = Instant::now();
            app.mouse_at(click, &recovered, &sender, now);
            app.mouse_at(click, &recovered, &sender, now + Duration::from_millis(100));
            assert!(
                matches!(actions.try_recv(), Ok(Command::Podcast(crate::podcast::Action::Play { id, .. })) if id == recovered.podcast_items[0].id)
            );
        }
    }

    #[test]
    fn podcast_preview_confirmation_waits_for_the_latest_request() {
        use super::super::{
            command,
            state::{App, Mode},
        };
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let server = crate::test_support::Server::new(|_, stream| {
            crate::test_support::response(stream, 200, "application/rss+xml",
                b"<rss><channel><title>Preview</title><item><title>Episode</title><enclosure url='https://example.test/audio.mp3'/></item></channel></rss>");
        });
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
        let view = Arc::new(Mutex::new(View::new(engine.status().clone())));
        let mut app = App::default();
        let (sender, receiver) = command::channel(8);
        let mut filter = Filter::default();
        let mut job = None;
        let finish = |engine: &mut Engine| {
            let deadline = Instant::now() + Duration::from_secs(3);
            let result = loop {
                if let Some(result) = engine.poll_podcast() {
                    break result;
                }
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            };
            assert!(result.is_ok(), "{result:?}");
            view.lock().unwrap().publish_podcast(result);
        };
        let old_url = format!("{}/old", server.url);
        app.mode = Mode::Command;
        app.editor.begin(&format!("podcast add {old_url}"));
        app.key(enter, &view.lock().unwrap(), &sender);
        execute(
            receiver.try_recv().unwrap(),
            &mut engine,
            &mut filter,
            &mut job,
            &view,
        )
        .unwrap();
        finish(&mut engine);
        let old = view.lock().unwrap().clone();
        assert!(
            matches!(&old.podcast_page, Some(crate::podcast::Action::Preview { url }) if url == &old_url)
        );
        assert_eq!(old.podcast_items[0].url, old_url);

        let new_url = format!("{}/new", server.url);
        let mut requests = vec![];
        for _ in 0..2 {
            app.mode = Mode::Command;
            app.editor.begin(&format!("podcast add {new_url}"));
            app.key(enter, &old, &sender);
            requests.push(receiver.try_recv().unwrap());
            // The worker has not seen the new command yet.
            app.key(enter, &old, &sender);
            assert!(receiver.try_recv().is_err());
        }
        let second = requests.pop().unwrap();
        let first = requests.pop().unwrap();
        execute(first, &mut engine, &mut filter, &mut job, &view).unwrap();
        finish(&mut engine);
        // Even the same URL needs a result from the latest submission.
        app.key(enter, &view.lock().unwrap(), &sender);
        assert!(receiver.try_recv().is_err());
        execute(second, &mut engine, &mut filter, &mut job, &view).unwrap();
        app.key(enter, &view.lock().unwrap(), &sender);
        assert!(receiver.try_recv().is_err());
        finish(&mut engine);
        assert!(engine.store().podcast_feeds().unwrap().is_empty());
        app.key(enter, &view.lock().unwrap(), &sender);
        let add = receiver.try_recv().unwrap();
        assert!(
            matches!(&add, Command::PodcastRequest { action: crate::podcast::Action::Add { url }, .. } if url == &new_url)
        );
        execute(add, &mut engine, &mut filter, &mut job, &view).unwrap();
        finish(&mut engine);
        let feeds = engine.store().podcast_feeds().unwrap();
        assert_eq!(feeds.len(), 1);
        assert_eq!(feeds[0].url, new_url);
    }

    #[test]
    fn cancelling_podcast_commands_clears_loading_and_periodic_refresh() {
        for (with_catalog, browse) in [(false, false), (false, true), (true, false), (true, true)] {
            let dir = tempfile::tempdir().unwrap();
            let config = crate::test_support::config(dir.path());
            let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
            let view = Arc::new(Mutex::new(View::new(engine.status().clone())));
            let mut filter = Filter::default();
            let (_sender, result) = mpsc::channel();
            let catalog_cancel = Arc::new(AtomicBool::new(false));
            let mut job = with_catalog.then(|| CatalogJob {
                cancel: catalog_cancel.clone(),
                progress: Arc::new(AtomicUsize::new(0)),
                result,
                handle: None,
                bootstrap: false,
            });
            execute(
                Command::Podcast(if browse {
                    crate::podcast::Action::Queue {
                        action: crate::podcast::QueueAction::List,
                    }
                } else {
                    crate::podcast::Action::Refresh { id: None }
                }),
                &mut engine,
                &mut filter,
                &mut job,
                &view,
            )
            .unwrap();
            let before = view.lock().unwrap().revision;
            assert_eq!(view.lock().unwrap().podcast_browse, browse);
            assert_eq!(view.lock().unwrap().podcast_poll.is_some(), browse);
            execute(Command::Cancel, &mut engine, &mut filter, &mut job, &view).unwrap();
            let data = view.lock().unwrap();
            assert!(!data.podcast_browse);
            assert!(data.podcast_pending.is_none());
            assert!(data.podcast_poll.is_none());
            assert!(data.revision > before);
            assert!(engine.poll_podcast().is_none());
            assert!(!engine.podcast_tasks().1);
            assert_eq!(catalog_cancel.load(Ordering::Relaxed), with_catalog);
            assert!(
                data.logs
                    .iter()
                    .any(|line| line.contains(crate::t!("podcast_operation_cancelled")))
            );
            if with_catalog {
                assert_eq!(data.task, crate::t!("catalog_task_cancelling"));
            }
            drop(data);
            job = None;
            assert!(execute(Command::Cancel, &mut engine, &mut filter, &mut job, &view).is_err());
        }
    }
}
