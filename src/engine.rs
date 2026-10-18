use crate::error::{DomainError, ErrorCode};
use crate::{
    config::Config,
    directory::Directory,
    model::{Filter, Station},
    player::{Event, Player},
    process::{Background, MediaLock},
    recorder::{self, Recorder},
    store::Store,
};
use anyhow::{Context, Result, bail};
use std::{
    collections::VecDeque,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

#[derive(Debug, Clone)]
pub struct Status {
    pub station: Option<Station>,
    pub title: String,
    pub message: String,
    pub playing: bool,
    pub paused: bool,
    pub volume: u8,
    pub recording: bool,
    pub recording_ms: u64,
    pub recording_path: String,
    pub source_url: String,
}

pub struct Engine {
    pub config: Config,
    pub store: Store,
    pub directory: Directory,
    pub status: Status,
    pub stop: Arc<AtomicBool>,
    _lock: Arc<MediaLock>,
    background_media: bool,
    generation: u64,
    refresh_task: Option<(u64, String, Background<Option<Station>>)>,
    click_task: Option<Background<()>>,
    recording_probe: Option<RecordingProbe>,
    finalizing: Option<Background<crate::model::Recording>>,
    pub(crate) diagnostics: Option<Background<(serde_json::Value, bool)>>,
    player: Option<Player>,
    player_start: Option<(u64, u8, Background<Player>)>,
    retiring: Vec<Background<()>>,
    recorder: Option<Recorder>,
    url: String,
    candidates: VecDeque<String>,
    tried: Vec<String>,
    refreshed: bool,
    confirmed: bool,
    history_recorded: bool,
    reconnect_attempts: usize,
    reconnect_due: Option<Instant>,
    started: Instant,
    random_filter: Option<Filter>,
    random_tried: Vec<String>,
    pub playback_failed: bool,
}

struct RecordingProbe {
    generation: u64,
    station: Station,
    url: String,
    dir: std::path::PathBuf,
    duration: Option<Duration>,
    task: Background<serde_json::Value>,
}

impl Engine {
    pub fn new(config: Config, stop: Arc<AtomicBool>) -> Result<Self> {
        let lock = MediaLock::acquire(&config.runtime_dir)?;
        let store = Store::open(&config.data_dir.join("radio.db"))?;
        let recovery_warnings = recorder::recover(&store)?;
        let directory = Directory::new(&config, stop.clone())?;
        let status = Status {
            station: None,
            title: String::new(),
            message: if recovery_warnings.is_empty() {
                "就绪".into()
            } else {
                recovery_warnings.join("\n")
            },
            playing: false,
            paused: false,
            volume: config.settings.volume,
            recording: false,
            recording_ms: 0,
            recording_path: String::new(),
            source_url: String::new(),
        };
        Ok(Self {
            config,
            store,
            directory,
            status,
            stop,
            _lock: Arc::new(lock),
            background_media: false,
            generation: 0,
            refresh_task: None,
            click_task: None,
            recording_probe: None,
            finalizing: None,
            diagnostics: None,
            player: None,
            player_start: None,
            retiring: vec![],
            recorder: None,
            url: String::new(),
            candidates: VecDeque::new(),
            tried: vec![],
            refreshed: false,
            confirmed: false,
            history_recorded: false,
            reconnect_attempts: 0,
            reconnect_due: None,
            started: Instant::now(),
            random_filter: None,
            random_tried: vec![],
            playback_failed: false,
        })
    }

    pub fn play(&mut self, uuid: &str) -> Result<()> {
        self.random_filter = None;
        self.random_tried.clear();
        self.play_station(self.store.station(uuid)?)
    }

    fn play_station(&mut self, station: Station) -> Result<()> {
        self.generation = self.generation.wrapping_add(1);
        self.refresh_task = None;
        self.stop_recording(None)?;
        self.retire_player();
        self.tried.clear();
        self.candidates.clear();
        self.refreshed = false;
        self.confirmed = false;
        self.history_recorded = false;
        self.reconnect_attempts = 0;
        self.reconnect_due = None;
        self.playback_failed = false;
        self.status.title.clear();
        self.status.playing = false;
        self.status.paused = false;
        self.status.station = Some(station.clone());
        self.candidates
            .extend(station.stream_candidates(&self.store.backups(&station.stationuuid)?));
        self.start_candidate()
    }

    fn start_candidate(&mut self) -> Result<()> {
        self.url = self
            .candidates
            .pop_front()
            .context(DomainError::new(ErrorCode::NoStream))?;
        self.status.source_url = self.url.clone();
        self.tried.push(self.url.clone());
        self.start_url()
    }

    fn start_url(&mut self) -> Result<()> {
        self.config.settings.volume = self.status.volume;
        if self.background_media {
            let config = self.config.clone();
            let url = self.url.clone();
            self.player_start = Some((
                self.generation,
                self.status.volume,
                Background::start("radio-player-start", move |cancel| {
                    Player::start(&config, &url, &cancel)
                })?,
            ));
        } else {
            self.player = Some(Player::start(&self.config, &self.url, &self.stop)?);
        }
        self.started = Instant::now();
        self.confirmed = false;
        self.status.message = "连接中…".into();
        self.status.playing = false;
        Ok(())
    }

    fn stream_failed(&mut self, reason: &str) -> Result<()> {
        // Three bounded attempts span a ten-second outage even when the
        // unavailable source rejects each connection immediately.
        const BACKOFF: [u64; 3] = [1, 3, 7];
        if self.history_recorded && self.reconnect_attempts < BACKOFF.len() {
            let delay = BACKOFF[self.reconnect_attempts];
            self.reconnect_attempts += 1;
            self.retire_player();
            self.status.playing = false;
            self.status.paused = false;
            self.reconnect_due = Some(Instant::now() + Duration::from_secs(delay));
            self.status.message = format!(
                "重连中 · {}/{} · {delay}s: {reason}",
                self.reconnect_attempts,
                BACKOFF.len()
            );
            return Ok(());
        }
        self.fail(reason)
    }

    pub fn random(&mut self, filter: Filter) -> Result<()> {
        self.random_tried = self
            .status
            .station
            .as_ref()
            .map(|s| vec![s.stationuuid.clone()])
            .unwrap_or_default();
        self.random_filter = Some(filter);
        self.pick_random()
    }

    fn pick_random(&mut self) -> Result<()> {
        let filter = self
            .random_filter
            .as_ref()
            .context(DomainError::new(ErrorCode::RandomDisabled))?;
        let station = self
            .store
            .random(filter, &self.random_tried)?
            .context(DomainError::new(ErrorCode::NoMatchingStation))?;
        self.random_tried.push(station.stationuuid.clone());
        self.play_station(station)
    }

    fn fail(&mut self, reason: &str) -> Result<()> {
        self.retire_player();
        self.status.playing = false;
        self.status.paused = false;
        self.stop_recording(Some(reason))?;
        let station = self
            .status
            .station
            .clone()
            .context(DomainError::new(ErrorCode::NotPlaying))?;
        if !self.candidates.is_empty() {
            return self.start_candidate();
        }
        if !self.refreshed && !station.custom {
            self.refreshed = true;
            let mut config = self.config.clone();
            config.settings.request_timeout_secs = config.settings.request_timeout_secs.min(3);
            let uuid = station.stationuuid.clone();
            let task = Background::start("radio-refresh", move |cancel| {
                Directory::new(&config, cancel)?.refresh(&uuid)
            })?;
            self.refresh_task = Some((self.generation, reason.into(), task));
            self.status.message = "正在后台刷新线路…".into();
            return Ok(());
        }
        self.exhausted(reason)
    }

    fn exhausted(&mut self, reason: &str) -> Result<()> {
        if self.random_filter.is_some() && self.random_tried.len() < 6 && self.pick_random().is_ok()
        {
            return Ok(());
        }
        self.playback_failed = true;
        self.status.message = format!("播放失败: {reason}");
        Ok(())
    }

    pub fn tick(&mut self) -> Result<()> {
        self.poll_background()?;
        if self.reconnect_due.is_some_and(|due| Instant::now() >= due) {
            self.reconnect_due = None;
            if let Err(error) = self.start_url() {
                self.stream_failed(&format!("{error:#}"))?;
            }
        }
        if let Some(player) = self.player.as_mut() {
            let events = player.poll();
            match events {
                Ok(events) => {
                    for event in events {
                        match event {
                            Event::Playing => {
                                self.status.playing = true;
                                self.status.message = "播放中".into();
                                if !self.confirmed {
                                    self.confirmed = true;
                                    if let Some(s) = &self.status.station
                                        && !self.history_recorded
                                    {
                                        self.store.played(&s.stationuuid)?;
                                        self.history_recorded = true;
                                        if !s.custom
                                            && self
                                                .click_task
                                                .as_ref()
                                                .is_none_or(|t| t.poll().is_some())
                                        {
                                            let config = self.config.clone();
                                            let uuid = s.stationuuid.clone();
                                            self.click_task = Some(Background::start(
                                                "radio-click",
                                                move |cancel| {
                                                    Directory::new(&config, cancel)?.click(&uuid)
                                                },
                                            )?);
                                        }
                                    }
                                }
                            }
                            Event::Title(title) => self.status.title = title,
                            Event::Paused(paused) => self.status.paused = paused,
                            Event::Volume(volume) => self.status.volume = volume,
                            Event::End(reason) if reason == "redirect" => {}
                            Event::End(reason) if matches!(reason.as_str(), "stop" | "quit") => {
                                self.stop_playback()?;
                                self.status.message = "已停止播放".into();
                                break;
                            }
                            Event::End(reason) => {
                                if matches!(reason.as_str(), "eof" | "error") {
                                    self.stream_failed(&format!("音频流结束 ({reason})"))?;
                                } else {
                                    self.fail(&format!("音频流结束 ({reason})"))?;
                                }
                                break;
                            }
                        }
                    }
                }
                Err(e) => self.stream_failed(&e.to_string())?,
            }
            if self.player.is_some()
                && !self.confirmed
                && self.started.elapsed()
                    > Duration::from_secs(self.config.settings.playback_timeout_secs)
            {
                self.stream_failed("起播超时")?;
            }
        }
        if let Some(recorder) = self.recorder.as_mut() {
            let ended = recorder.poll()?;
            self.status.recording_ms = recorder.elapsed_ms();
            if ended {
                self.stop_recording(None)?;
            }
        }
        Ok(())
    }

    pub fn pause(&mut self) -> Result<()> {
        self.player
            .as_mut()
            .context(DomainError::new(ErrorCode::NotPlaying))?
            .toggle_pause()
    }

    pub fn volume(&mut self, delta: i32) -> Result<()> {
        let value = (i32::from(self.status.volume) + delta).clamp(0, 100) as u8;
        if let Some(player) = &mut self.player {
            player.volume(i32::from(value) - i32::from(self.status.volume))?;
        }
        self.status.volume = value;
        Ok(())
    }

    pub fn toggle_recording(&mut self) -> Result<()> {
        if self.recorder.is_some() || self.recording_probe.is_some() {
            return self.stop_recording(None);
        }
        self.record_current(None, None)
    }

    pub fn record_current(
        &mut self,
        duration: Option<Duration>,
        dir: Option<&std::path::Path>,
    ) -> Result<()> {
        if !self.status.playing {
            bail!(DomainError::new(ErrorCode::WaitForPlayback));
        }
        let station = self
            .status
            .station
            .clone()
            .context(DomainError::new(ErrorCode::NotPlaying))?;
        if self.background_media {
            if self.recorder.is_some()
                || self.recording_probe.is_some()
                || self.finalizing.is_some()
            {
                bail!(DomainError::new(ErrorCode::RecordingBusy));
            }
            let config = self.config.clone();
            let url = self.url.clone();
            let input = url.clone();
            let task = Background::start("radio-record-probe", move |cancel| {
                recorder::probe(&config, &input, &cancel)
            })?;
            self.recording_probe = Some(RecordingProbe {
                generation: self.generation,
                station,
                url,
                dir: dir.unwrap_or(&self.config.recording_dir).into(),
                duration,
                task,
            });
            self.status.recording = true;
            self.status.recording_ms = 0;
            self.status.recording_path.clear();
            self.status.message = "正在后台检测录音编码…".into();
            Ok(())
        } else {
            self.start_recording(station, self.url.clone(), duration, dir)
        }
    }

    pub fn standalone_record(
        &mut self,
        uuid: &str,
        duration: Option<Duration>,
        dir: Option<&std::path::Path>,
    ) -> Result<()> {
        if self.recorder.is_some() {
            bail!(DomainError::new(ErrorCode::RecordingBusy));
        }
        let mut station = self.store.station(uuid)?;
        let mut tried = Vec::new();
        let mut last = None;
        for refresh in [false, true] {
            if refresh {
                if station.custom {
                    break;
                }
                if let Ok(Some(updated)) = self.directory.refresh(uuid) {
                    self.store.upsert(&updated)?;
                    station = updated;
                }
            }
            for url in station.stream_candidates(&self.store.backups(uuid)?) {
                if tried.contains(&url) {
                    continue;
                }
                tried.push(url.clone());
                match recorder::probe(&self.config, &url, &self.stop).and_then(|info| {
                    let codec = info["streams"][0]["codec_name"]
                        .as_str()
                        .context(DomainError::new(ErrorCode::NoAudio))?;
                    recorder::format_for_codec(codec)?;
                    Ok(info)
                }) {
                    Ok(info) => {
                        let recorder = Recorder::start_with_info(
                            &self.config,
                            &self.store,
                            &station,
                            &url,
                            dir.unwrap_or(&self.config.recording_dir),
                            duration,
                            info,
                        )?;
                        self.attach_recorder(recorder);
                        return Ok(());
                    }
                    Err(error) => {
                        if self.stop.load(std::sync::atomic::Ordering::Relaxed)
                            || error.chain().any(|cause| {
                                cause.downcast_ref::<std::io::Error>().is_some_and(|e| {
                                    matches!(
                                        e.kind(),
                                        std::io::ErrorKind::NotFound
                                            | std::io::ErrorKind::PermissionDenied
                                    )
                                })
                            })
                        {
                            return Err(error);
                        }
                        last = Some(error);
                    }
                }
            }
        }
        Err(last.unwrap_or_else(|| anyhow::anyhow!(DomainError::new(ErrorCode::NoRecordingStream))))
            .context(DomainError::new(ErrorCode::RecordingSourcesFailed))
    }

    fn start_recording(
        &mut self,
        station: Station,
        url: String,
        duration: Option<Duration>,
        dir: Option<&std::path::Path>,
    ) -> Result<()> {
        if self.recorder.is_some() {
            bail!(DomainError::new(ErrorCode::RecordingBusy));
        }
        let dir = dir.unwrap_or(&self.config.recording_dir);
        let recorder = Recorder::start(
            &self.config,
            &self.store,
            &station,
            &url,
            dir,
            duration,
            &self.stop,
        )?;
        self.attach_recorder(recorder);
        Ok(())
    }

    fn attach_recorder(&mut self, recorder: Recorder) {
        self.status.recording_path = recorder.record.path.clone();
        self.recorder = Some(recorder);
        self.status.recording = true;
        self.status.recording_ms = 0;
        self.status.message = "正在录音".into();
    }

    pub(crate) fn enable_background_media(&mut self) {
        self.background_media = true;
    }

    fn retire_player(&mut self) {
        if let Some((_, _, task)) = self.player_start.take() {
            // A completed task may already contain a Player. Drop it away
            // from controls so its child reaping cannot block cancellation.
            let lock = self._lock.clone();
            if let Ok(cleanup) = Background::start("radio-player-cancel", move |_| {
                let _lock = lock;
                drop(task);
                Ok(())
            }) {
                self.retiring.push(cleanup);
            }
        }
        if let Some(player) = self.player.take() {
            if self.background_media {
                let lock = self._lock.clone();
                if let Ok(task) = Background::start("radio-player-stop", move |_| {
                    let _lock = lock;
                    drop(player);
                    Ok(())
                }) {
                    self.retiring.push(task);
                }
            } else {
                drop(player);
            }
        }
    }

    fn poll_background(&mut self) -> Result<()> {
        self.retiring.retain(|task| task.poll().is_none());
        if let Some(result) = self
            .player_start
            .as_ref()
            .and_then(|(_, _, task)| task.poll())
        {
            let (generation, initial_volume, _) = self.player_start.take().unwrap();
            if generation == self.generation {
                match result {
                    Ok(mut player) => {
                        // Controls accepted while IPC was starting still apply.
                        let delta = i32::from(self.status.volume) - i32::from(initial_volume);
                        if delta != 0 {
                            player.volume(delta)?;
                        }
                        self.player = Some(player);
                        self.started = Instant::now();
                    }
                    Err(error) => {
                        self.stream_failed(&format!("播放器启动失败: {error:#}"))?;
                    }
                }
            }
        }
        if let Some(result) = self
            .refresh_task
            .as_ref()
            .and_then(|(_, _, task)| task.poll())
        {
            let (generation, reason, _) = self.refresh_task.take().unwrap();
            if generation == self.generation {
                if let Ok(Some(station)) = result {
                    self.store.upsert(&station)?;
                    self.candidates.extend(
                        station.stream_candidates(&self.store.backups(&station.stationuuid)?),
                    );
                    self.candidates.retain(|url| !self.tried.contains(url));
                }
                if self.candidates.is_empty() {
                    self.exhausted(&reason)?;
                } else {
                    self.start_candidate()?;
                }
            }
        }
        if let Some(result) = self
            .recording_probe
            .as_ref()
            .and_then(|probe| probe.task.poll())
        {
            let probe = self.recording_probe.take().unwrap();
            self.status.recording = false;
            if probe.generation == self.generation {
                match result.and_then(|info| {
                    Recorder::start_with_info(
                        &self.config,
                        &self.store,
                        &probe.station,
                        &probe.url,
                        &probe.dir,
                        probe.duration,
                        info,
                    )
                }) {
                    Ok(recorder) => self.attach_recorder(recorder),
                    Err(error) => self.status.message = format!("录音启动失败: {error:#}"),
                }
            }
        }
        if let Some(result) = self.finalizing.as_ref().and_then(Background::poll) {
            self.finalizing = None;
            match result {
                Ok(record) => self.recording_finished(record),
                Err(error) => {
                    self.status.message =
                        format!("录音收尾失败，可用 recordings recover 重试: {error:#}")
                }
            }
        }
        Ok(())
    }

    fn recording_finished(&mut self, record: crate::model::Recording) {
        self.status.recording_path = record.path;
        self.status.message = format!("录音 {}: {}", record.status, self.status.recording_path);
        if record.status != "complete" {
            self.status
                .message
                .push_str(&format!(" ({})", record.error.unwrap_or_default()));
        }
    }

    pub fn stop_recording(&mut self, reason: Option<&str>) -> Result<()> {
        if self.recording_probe.take().is_some() {
            self.status.message = "录音检测已取消".into();
        }
        self.status.recording = false;
        if let Some(recorder) = self.recorder.take() {
            if self.background_media {
                let config = self.config.clone();
                let reason = reason.map(str::to_owned);
                let lock = self._lock.clone();
                self.finalizing = Some(Background::start("radio-finalize", move |cancel| {
                    let _lock = lock;
                    let store = Store::open(&config.data_dir.join("radio.db"))?;
                    recorder.finish_cancellable(&config, &store, reason.as_deref(), &cancel)
                })?);
                self.status.message = "录音正在后台收尾…".into();
            } else {
                let record = recorder.finish(&self.config, &self.store, reason)?;
                self.recording_finished(record);
            }
        }
        Ok(())
    }

    pub fn stop_playback(&mut self) -> Result<()> {
        self.reconnect_due = None;
        self.generation = self.generation.wrapping_add(1);
        self.refresh_task = None;
        self.retire_player();
        self.status.playing = false;
        self.status.paused = false;
        self.stop_recording(None)
    }

    pub fn shutdown(&mut self) -> Result<()> {
        self.stop_playback()?;
        self.click_task = None;
        self.diagnostics = None;
        // Allow normal finalization, then cancel verification if its budget
        // expires. Outstanding HTTP results never need joining.
        let deadline = Instant::now() + Duration::from_secs(6);
        while (self.finalizing.is_some() || !self.retiring.is_empty()) && Instant::now() < deadline
        {
            self.poll_background()?;
            std::thread::sleep(Duration::from_millis(10));
        }
        self.finalizing = None;
        Ok(())
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_budget_is_bounded_and_stop_cancels_it() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(None, Some(dir.path()), Some("http://127.0.0.1:1")).unwrap();
        config.prepare().unwrap();
        let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
        engine.status.station = Some(Station {
            custom: true,
            name: "Test".into(),
            ..Station::default()
        });
        engine.history_recorded = true;
        for delay in [1, 3, 7] {
            engine.stream_failed("EOF").unwrap();
            let remaining = engine
                .reconnect_due
                .unwrap()
                .saturating_duration_since(Instant::now());
            assert!(remaining <= Duration::from_secs(delay));
            assert!(remaining > Duration::from_millis(delay * 1000 - 100));
            assert!(!engine.playback_failed);
        }
        engine.reconnect_due = None;
        engine.stream_failed("EOF").unwrap();
        assert!(engine.playback_failed);
        assert!(engine.reconnect_due.is_none());
        engine.reconnect_attempts = 0;
        engine.stream_failed("EOF").unwrap();
        engine.stop_playback().unwrap();
        assert!(engine.reconnect_due.is_none());
        let generation = engine.generation;
        engine.tick().unwrap();
        assert_eq!(engine.generation, generation);
        assert!(engine.player.is_none() && engine.player_start.is_none());
    }
}
