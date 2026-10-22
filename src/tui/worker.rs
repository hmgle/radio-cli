mod query;

use super::command::{Command, CommandReceiver};
use crate::{
    config::Config,
    directory::Directory,
    engine::{Engine, Status},
    model::{Filter, Recording, Station},
    process::Background,
    store::Store,
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
    time::Duration,
};

#[derive(Clone)]
pub(super) struct LocatedStation {
    pub request: u64,
    pub filter: Result<Filter, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum PagePhase {
    Loading,
    Ready,
    Error(String),
}
#[derive(Clone)]
pub(super) struct PageState {
    pub generation: u64,
    pub requested: Filter,
    pub phase: PagePhase,
}

#[derive(Clone)]
pub(super) struct View {
    pub artwork_station: Option<String>,
    pub track_artwork: Option<crate::engine::artwork::TrackArtwork>,
    published: Option<(u64, Arc<View>)>,
    queries: Option<query::Sender>,
    store_revision: Option<(u64, u64)>,
    refresh_requested: bool,
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
    pub fn new(status: Status) -> Self {
        Self {
            artwork_station: None,
            track_artwork: None,
            published: None,
            queries: None,
            store_revision: None,
            refresh_requested: false,
            immediate_revision: 1,
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
            catalog_note: "目录加载中…".into(),
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
        self.filter == *filter
            && self
                .page
                .as_ref()
                .is_none_or(|p| p.phase == PagePhase::Ready)
    }
    pub fn page_error(&self, filter: &Filter) -> Option<&str> {
        self.page
            .as_ref()
            .filter(|p| p.requested == *filter)
            .and_then(|p| {
                if let PagePhase::Error(error) = &p.phase {
                    Some(error.as_str())
                } else {
                    None
                }
            })
    }
    fn fail_page(&mut self, filter: &Filter, error: String) {
        let generation = self.page.as_ref().map_or(0, |p| p.generation);
        self.page = Some(PageState {
            generation,
            requested: filter.clone(),
            phase: PagePhase::Error(error.clone()),
        });
        self.log(&crate::t!("list_query_failed", "detail" => error));
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
        self.operation_error = message.into();
        self.log(message);
    }
    fn log(&mut self, message: &str) {
        let message = crate::clean_text(message);
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
                format!("目录同步: {time}")
            }
            _ => "目录已过期；:sync 刷新".into(),
        },
        None => "热门目录 · 尚未完整同步；:sync 获取全部电台".into(),
    })
}

fn refresh(engine: &Engine, filter: &Filter, view: &Arc<Mutex<View>>) -> Result<()> {
    let result = refresh_page(engine, filter, view);
    if let Err(error) = &result {
        let mut data = view.lock().unwrap();
        data.refresh_requested = false;
        data.fail_page(filter, format!("{error:#}"));
    }
    result
}

fn refresh_page(engine: &Engine, filter: &Filter, view: &Arc<Mutex<View>>) -> Result<()> {
    let store_revision = engine.store.revision()?;
    view.lock().unwrap().store_revision = Some(store_revision);
    let note = catalog_note(&engine.store, engine.config.settings.auto_sync_days)?;
    {
        let mut data = view.lock().unwrap();
        if let Some(sender) = data.queries.clone() {
            data.store_revision = Some(store_revision);
            data.refresh_requested = false;
            let generation = sender.page(filter.clone(), store_revision, note);
            data.page = Some(PageState {
                generation,
                requested: filter.clone(),
                phase: PagePhase::Loading,
            });
            data.revision += 1;
            data.immediate_revision += 1;
            return Ok(());
        }
    }
    if filter.recordings {
        let recordings = engine.store.recordings_page(filter.limit, filter.offset)?;
        let total = engine.store.recordings_count()?;
        let mut data = view.lock().unwrap();
        data.recordings = Arc::new(recordings);
        data.recordings_total = total;
        data.recordings_revision += 1;
    } else {
        let rows = engine.store.search(filter)?;
        let total = engine.store.search_count(filter)?;
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
    view.lock().unwrap().refresh_requested = true;
}

pub(super) fn refresh_if_needed(
    engine: &Engine,
    filter: &Filter,
    view: &Arc<Mutex<View>>,
    recording_ended: bool,
) -> Result<()> {
    let revision = match engine.store.revision() {
        Ok(revision) => revision,
        Err(error) => {
            let mut data = view.lock().unwrap();
            if data.refresh_requested || data.page_error(filter).is_none() {
                data.refresh_requested = false;
                data.fail_page(filter, format!("{error:#}"));
            }
            return Ok(());
        }
    };
    let data = view.lock().unwrap();
    let needed = data.refresh_requested || data.store_revision != Some(revision) || recording_ended;
    drop(data);
    if needed {
        let _ = refresh(engine, filter, view);
    }
    Ok(())
}

fn favorite_changed(view: &Arc<Mutex<View>>, uuid: &str, add: bool) -> Result<()> {
    let mut data = view.lock().unwrap();
    data.log(if add {
        "已收藏电台"
    } else {
        "已取消收藏"
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
            let message = format!("备用地址查询失败，将重试: {error:#}");
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
    match command {
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
                    .store
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
                bail!("无效的筛选字段");
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
                    engine.store.facets_limited(kind, prefix, 100)?;
                data.facets_loading = false;
            }
        }
        Command::Search(value) => {
            let source = if value.history {
                super::state::Source::History
            } else if value.favorites {
                super::state::Source::Favorites
            } else {
                super::state::Source::Stations
            };
            if let Err(error) = engine.store.set_meta("pref_source", source.preference()) {
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
            let add = engine.store.toggle_favorite(&uuid)?;
            favorite_changed(view, &uuid, add)?;
        }
        Command::AdjustVolume(delta) => engine.volume(delta)?,
        Command::Favorite(uuid, add) => {
            engine.store.favorite(&uuid, add)?;
            favorite_changed(view, &uuid, add)?;
        }
        Command::Pause => engine.pause()?,
        Command::Mute => engine.mute()?,
        Command::SetLanguage(language) => {
            crate::messages::init(Some(&language), None);
            engine.config.settings.language = Some(language.clone());
            engine.status.message = format!("language = {language}");
        }
        Command::Stop => {
            engine.stop_playback()?;
            engine.status.message = crate::t!("playback_stopped").into();
        }
        Command::Volume(value) => {
            engine.volume(i32::from(value) - i32::from(engine.status.volume))?
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
                    .status
                    .station
                    .as_ref()
                    .is_none_or(|s| s.stationuuid != uuid)
                {
                    bail!(
                        "TUI 仅录制当前播放电台；请先播放目标电台，或退出后用 radio record UUID 独立录音"
                    );
                }
                engine.record_current(duration, dir.as_deref())?;
            } else {
                engine.record_current(duration, dir.as_deref())?;
            }
        }
        Command::Add { name, url, backups } => {
            let station = crate::service::add_station(&mut engine.store, name, url, &backups)?;
            view.lock().unwrap().log(&format!(
                "已添加 {} ({})",
                station.name, station.stationuuid
            ));
            request_refresh(view);
        }
        Command::Station(action) => {
            use crate::cli::StationAction;
            let output = match action {
                StationAction::Edit {
                    uuid,
                    name,
                    url,
                    backup_url,
                } => {
                    let station = crate::service::edit_station(
                        &mut engine.store,
                        &uuid,
                        name,
                        url,
                        &backup_url,
                    )?;
                    format!("已更新 {} ({uuid})", station.name)
                }
                StationAction::Remove {
                    uuid,
                    keep_references,
                } => {
                    let kept = engine.store.remove_custom(&uuid, keep_references)?;
                    format!(
                        "已删除自定义电台 {uuid}{}",
                        if kept { "；引用名称保留" } else { "" }
                    )
                }
                StationAction::Backups { uuid, add, remove } => {
                    let backups = engine.store.update_backups(&uuid, &add, &remove)?;
                    format!("备用地址 {uuid}: {}", backups.join(" · "))
                }
                _ => unreachable!("station list/add use their existing commands"),
            };
            view.lock().unwrap().log(&output);
            request_refresh(view);
        }
        Command::Sync(accept_shrink) => {
            if job.is_some() {
                bail!("目录任务正在运行；使用 :sync-cancel 取消");
            }
            *job = Some(CatalogJob::start(
                engine.config.clone(),
                false,
                accept_shrink,
            )?);
            let mut data = view.lock().unwrap();
            data.task_active = true;
            data.task = "同步中 · 已获取 0 台".into();
            data.log("开始后台同步目录");
        }
        Command::Cancel => {
            let Some(job) = job else {
                bail!("没有正在运行的目录任务");
            };
            job.cancel.store(true, Ordering::Relaxed);
            let mut data = view.lock().unwrap();
            data.task = "正在取消 · 等待当前请求结束…".into();
            data.log("已请求取消目录任务");
        }
        Command::Recordings(value) => {
            if let Err(error) = engine.store.set_meta("pref_source", "recordings") {
                view.lock().unwrap().fail_operation(&format!("{error:#}"));
            }
            *filter = value;
            filter.recordings = true;
            request_refresh(view);
        }
        Command::Refresh => request_refresh(view),
        Command::Backups(uuid) => load_backups(&engine.store, &uuid, view),
        Command::Info(uuid, request) => {
            let result = match engine
                .status
                .station
                .as_ref()
                .filter(|s| s.stationuuid == uuid)
            {
                Some(station) => Ok(station.clone()),
                None => engine.store.station(&uuid),
            };
            load_backups(&engine.store, &uuid, view);
            let mut data = view.lock().unwrap();
            data.inspected_request = request;
            data.inspected = None;
            data.inspected_error = None;
            match result {
                Ok(station) => data.inspected = Some(station),
                Err(error) => data.inspected_error = Some(format!("详情查询失败: {error:#}")),
            }
            data.revision += 1;
        }
        Command::Doctor => {
            if engine.diagnostics.is_some() {
                bail!("诊断任务正在运行");
            }
            let config = engine.config.clone();
            engine.diagnostics = Some(Background::start("radio-doctor", move |cancel| {
                crate::cli::doctor_report(&config, &cancel)
            })?);
            view.lock().unwrap().log("开始后台诊断");
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
    engine.enable_background_media();
    let queries = query::Worker::start(&engine.config.data_dir.join("radio.db"), &view)?;
    view.lock().unwrap().queries = Some(queries.sender.clone());
    refresh(&engine, &filter, &view)?;
    if let Err(error) = engine.resume_last_station() {
        view.lock()
            .unwrap()
            .log(&format!("恢复播放失败: {error:#}"));
    }
    let empty = engine.store.count()? == 0;
    let expired = auto_sync_due(&engine.store, engine.config.settings.auto_sync_days)?;
    let mut job = if empty || expired {
        let mut data = view.lock().unwrap();
        data.task_active = true;
        data.task = if empty {
            "获取热门电台…"
        } else {
            "目录已过期，后台同步中…"
        }
        .into();
        let message = data.task.clone();
        data.log(&message);
        Some(CatalogJob::start(engine.config.clone(), empty, false)?)
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
                    let text = format!(
                        "同步中 · 已获取 {} 台",
                        active.progress.load(Ordering::Relaxed)
                    );
                    if data.task != text {
                        data.task = text;
                        data.revision += 1;
                    }
                }
                match active.result.try_recv() {
                    Ok(result) => Some(result),
                    Err(mpsc::TryRecvError::Disconnected) => {
                        Some(Err(anyhow::anyhow!("目录任务异常退出")))
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
                    Ok(n) => format!(
                        "{}完成：{n} 台",
                        if active.bootstrap {
                            "热门目录"
                        } else {
                            "同步"
                        }
                    ),
                    Err(_) if cancelled => "目录任务已取消，保留原目录".into(),
                    Err(error) => format!("目录任务失败: {error:#}"),
                };
                let text = data.task.clone();
                data.log(&text);
                drop(data);
                drop(active);
                request_refresh(&view);
            }
            if let Some(result) = engine.diagnostics.as_ref().and_then(Background::poll) {
                engine.diagnostics = None;
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
                    Err(error) => data.log(&format!("诊断失败: {error:#}")),
                }
            }
            engine.artwork.demand = view.lock().unwrap().artwork_station.clone();
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
                was_recording && !engine.status.recording(),
            )?;
            was_recording = engine.status.recording();
            let mut data = view.lock().unwrap();
            if data.track_artwork != engine.artwork.cover {
                data.track_artwork = engine.artwork.cover.clone();
                data.revision += 1;
                data.immediate_revision += 1;
            }
            if !data.status.ui_eq(&engine.status) {
                if !data.status.control_eq(&engine.status) {
                    data.immediate_revision += 1;
                }
                data.status = engine.status.clone();
                data.revision += 1;
                if engine.status.message != last_message {
                    last_message = engine.status.message.clone();
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
