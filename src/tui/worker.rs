use super::command::Command;
use crate::{
    config::Config,
    directory::Directory,
    engine::{Engine, Status},
    model::{Filter, Recording, Station},
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
pub(super) struct View {
    pub rows: Vec<Station>,
    pub recordings: Vec<Recording>,
    pub status: Status,
    pub catalog_note: String,
    pub task: String,
    pub task_active: bool,
    pub logs: VecDeque<String>,
    pub inspected: Option<Station>,
    pub inspected_request: u64,
    pub inspected_error: Option<String>,
    pub revision: u64,
    pub rows_revision: u64,
    pub filter: Filter,
    pub total: usize,
    pub recordings_total: usize,
}
impl View {
    pub fn new(status: Status) -> Self {
        Self {
            total: 0,
            recordings_total: 0,
            rows: vec![],
            recordings: vec![],
            status,
            catalog_note: "目录加载中…".into(),
            task: String::new(),
            task_active: false,
            logs: VecDeque::new(),
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
    fn log(&mut self, message: &str) {
        let message = crate::clean_text(message);
        self.logs.push_back(format!(
            "{} {message}",
            chrono::Local::now().format("%H:%M:%S")
        ));
        while self.logs.len() > 200 {
            self.logs.pop_front();
        }
        self.revision += 1;
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
                        directory.bootstrap(&store)
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
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn catalog_note(store: &Store) -> Result<String> {
    Ok(match store.meta("last_sync")? {
        Some(time) => match chrono::DateTime::parse_from_rfc3339(&time) {
            Ok(time) if chrono::Utc::now().signed_duration_since(time).num_days() < 7 => {
                format!("目录同步: {time}")
            }
            _ => "目录已过期；:sync 刷新".into(),
        },
        None => "热门目录 · 尚未完整同步；:sync 获取全部电台".into(),
    })
}

fn refresh(engine: &Engine, filter: &Filter, view: &Arc<Mutex<View>>) -> Result<()> {
    let rows = engine.store.search(filter)?;
    let recordings = engine.store.recordings_page(filter.limit, filter.offset)?;
    let note = catalog_note(&engine.store)?;
    let mut view = view.lock().unwrap();
    view.total = engine.store.search_count(filter)?;
    view.recordings_total = engine.store.recordings_count()?;
    view.rows = rows;
    view.recordings = recordings;
    view.catalog_note = note;
    view.rows_revision += 1;
    view.filter = filter.clone();
    view.revision += 1;
    Ok(())
}

pub(super) fn execute(
    command: Command,
    engine: &mut Engine,
    filter: &mut Filter,
    job: &mut Option<CatalogJob>,
    view: &Arc<Mutex<View>>,
) -> Result<()> {
    match command {
        Command::Search(value) => {
            *filter = value;
            view.lock().unwrap().inspected = None;
            refresh(engine, filter, view)?;
        }
        Command::Play(uuid) => engine.play(&uuid)?,
        Command::Random(filter) => engine.random(filter)?,
        Command::ToggleFavorite(uuid) => {
            let add = !engine.store.station(&uuid)?.favorite;
            return execute(Command::Favorite(uuid, add), engine, filter, job, view);
        }
        Command::AdjustVolume(delta) => engine.volume(delta)?,
        Command::Favorite(uuid, add) => {
            engine.store.favorite(&uuid, add)?;
            let mut data = view.lock().unwrap();
            data.log(if add {
                "已收藏电台"
            } else {
                "已取消收藏"
            });
            if let Some(s) = data.inspected.as_mut().filter(|s| s.stationuuid == uuid) {
                s.favorite = add;
            }
            drop(data);
            refresh(engine, filter, view)?;
        }
        Command::Pause => engine.pause()?,
        Command::Stop => {
            engine.shutdown()?;
            engine.status.playing = false;
            engine.status.paused = false;
            engine.status.message = "已停止播放".into();
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
            if name.trim().is_empty() {
                bail!("电台名称不能为空");
            }
            crate::config::validate_url(&url)?;
            for backup in &backups {
                crate::config::validate_url(backup)?;
            }
            let station = Station {
                stationuuid: uuid::Uuid::new_v4().to_string(),
                name,
                url: url.clone(),
                url_resolved: url,
                active: true,
                custom: true,
                lastcheckok: 1,
                ..Station::default()
            };
            engine.store.upsert(&station)?;
            for backup in backups {
                engine.store.add_backup(&station.stationuuid, &backup)?;
            }
            view.lock().unwrap().log(&format!(
                "已添加 {} ({})",
                station.name, station.stationuuid
            ));
            refresh(engine, filter, view)?;
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
            *filter = value;
            refresh(engine, filter, view)?;
        }
        Command::Refresh => refresh(engine, filter, view)?,
        Command::Info(uuid, request) => {
            let result = engine.store.station(&uuid);
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
            let (report, _) = crate::cli::doctor_report(&engine.config, &engine.stop)?;
            let mut data = view.lock().unwrap();
            for check in report["checks"].as_array().into_iter().flatten() {
                data.log(&format!(
                    "{} {}: {}",
                    if check["ok"] == true { "OK" } else { "FAIL" },
                    check["name"].as_str().unwrap_or(""),
                    check["detail"].as_str().unwrap_or("")
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn run(
    mut engine: Engine,
    commands: Receiver<Command>,
    view: Arc<Mutex<View>>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    let mut filter = Filter {
        limit: 100,
        ..Filter::default()
    };
    refresh(&engine, &filter, &view)?;
    let mut job = if engine.store.count()? == 0 {
        let mut data = view.lock().unwrap();
        data.task_active = true;
        data.task = "获取热门电台…".into();
        data.log("开始获取热门目录");
        Some(CatalogJob::start(engine.config.clone(), true, false)?)
    } else {
        None
    };
    let mut last_status = String::new();
    let mut last_message = String::new();
    let mut was_recording = false;
    let mut revision = engine.store.revision()?;
    let result = (|| -> Result<()> {
        while !stop.load(Ordering::Relaxed) {
            match commands.recv_timeout(Duration::from_millis(100)) {
                Ok(command) => {
                    if let Err(error) = execute(command, &mut engine, &mut filter, &mut job, &view)
                    {
                        view.lock().unwrap().log(&format!("错误: {error:#}"));
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
                refresh(&engine, &filter, &view)?;
            }
            if let Err(error) = engine.tick() {
                view.lock().unwrap().log(&format!("错误: {error:#}"));
            }
            let current_revision = engine.store.revision()?;
            if revision != current_revision || was_recording && !engine.status.recording {
                revision = current_revision;
                refresh(&engine, &filter, &view)?;
            }
            was_recording = engine.status.recording;
            let state = format!("{:?}", engine.status);
            if state != last_status {
                last_status = state;
                let mut data = view.lock().unwrap();
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
    let shutdown = engine.shutdown();
    drop(job);
    result?;
    shutdown
}
