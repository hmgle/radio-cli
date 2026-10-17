use crate::{
    config::Config,
    directory::Directory,
    model::{Filter, Station},
    player::{Event, Player},
    process::MediaLock,
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
    _lock: MediaLock,
    player: Option<Player>,
    recorder: Option<Recorder>,
    url: String,
    candidates: VecDeque<String>,
    tried: Vec<String>,
    refreshed: bool,
    confirmed: bool,
    started: Instant,
    random_filter: Option<Filter>,
    random_tried: Vec<String>,
    pub playback_failed: bool,
}

impl Engine {
    pub fn new(config: Config, stop: Arc<AtomicBool>) -> Result<Self> {
        let lock = MediaLock::acquire(&config.runtime_dir)?;
        let store = Store::open(&config.data_dir.join("radio.db"))?;
        recorder::recover(&store)?;
        let directory = Directory::new(&config, stop.clone())?;
        let status = Status {
            station: None,
            title: String::new(),
            message: "就绪".into(),
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
            _lock: lock,
            player: None,
            recorder: None,
            url: String::new(),
            candidates: VecDeque::new(),
            tried: vec![],
            refreshed: false,
            confirmed: false,
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
        self.stop_recording(None)?;
        self.player = None;
        self.tried.clear();
        self.candidates.clear();
        self.refreshed = false;
        self.confirmed = false;
        self.playback_failed = false;
        self.status.title.clear();
        self.status.playing = false;
        self.status.paused = false;
        self.status.station = Some(station.clone());
        self.candidates.push_back(station.primary_url().to_owned());
        self.start_candidate()
    }

    fn start_candidate(&mut self) -> Result<()> {
        self.url = self.candidates.pop_front().context("无可用播放地址")?;
        self.status.source_url = self.url.clone();
        self.tried.push(self.url.clone());
        self.config.settings.volume = self.status.volume;
        self.player = Some(Player::start(&self.config, &self.url, &self.stop)?);
        self.started = Instant::now();
        self.confirmed = false;
        self.status.message = "连接中…".into();
        self.status.playing = false;
        Ok(())
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
        let filter = self.random_filter.as_ref().context("随机模式未启用")?;
        let station = self
            .store
            .random(filter, &self.random_tried)?
            .context("没有符合条件的其他电台")?;
        self.random_tried.push(station.stationuuid.clone());
        self.play_station(station)
    }

    fn fail(&mut self, reason: &str) -> Result<()> {
        self.player = None;
        self.status.playing = false;
        self.status.paused = false;
        self.stop_recording(Some(reason))?;
        let station = self.status.station.clone().context("没有当前电台")?;
        if !self.refreshed {
            self.refreshed = true;
            if !station.custom
                && let Ok(Some(updated)) = self.directory.refresh(&station.stationuuid)
            {
                self.store.upsert(&updated)?;
                self.candidates.push_back(updated.primary_url().into());
                self.candidates.push_back(updated.url);
            }
            self.candidates.push_back(station.url.clone());
            self.candidates
                .extend(self.store.backups(&station.stationuuid)?);
            self.candidates
                .retain(|url| !url.is_empty() && !self.tried.contains(url));
        }
        if !self.candidates.is_empty() {
            return self.start_candidate();
        }
        if self.random_filter.is_some() && self.random_tried.len() < 6 && self.pick_random().is_ok()
        {
            return Ok(());
        }
        self.playback_failed = true;
        self.status.message = format!("播放失败: {reason}");
        Ok(())
    }

    pub fn tick(&mut self) -> Result<()> {
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
                                    if let Some(s) = &self.status.station {
                                        self.store.played(&s.stationuuid)?;
                                        if !s.custom {
                                            let _ = self.directory.click(&s.stationuuid);
                                        }
                                    }
                                }
                            }
                            Event::Title(title) => self.status.title = title,
                            Event::Paused(paused) => self.status.paused = paused,
                            Event::Volume(volume) => self.status.volume = volume,
                            Event::End(reason) if reason == "redirect" => {}
                            Event::End(reason) => {
                                self.fail(&format!("音频流结束 ({reason})"))?;
                                break;
                            }
                        }
                    }
                }
                Err(e) => self.fail(&e.to_string())?,
            }
            if self.player.is_some()
                && !self.confirmed
                && self.started.elapsed()
                    > Duration::from_secs(self.config.settings.playback_timeout_secs)
            {
                self.fail("起播超时")?;
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
        self.player.as_mut().context("尚未播放")?.toggle_pause()
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
        if self.recorder.is_some() {
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
            bail!("请等待电台开始播放后再录音");
        }
        let station = self.status.station.clone().context("尚未播放")?;
        self.start_recording(station, self.url.clone(), duration, dir)
    }

    pub fn standalone_record(
        &mut self,
        uuid: &str,
        duration: Option<Duration>,
        dir: Option<&std::path::Path>,
    ) -> Result<()> {
        let station = self.store.station(uuid)?;
        self.start_recording(station.clone(), station.primary_url().into(), duration, dir)
    }

    fn start_recording(
        &mut self,
        station: Station,
        url: String,
        duration: Option<Duration>,
        dir: Option<&std::path::Path>,
    ) -> Result<()> {
        if self.recorder.is_some() {
            bail!("已有录音任务");
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
        self.status.recording_path = recorder.record.path.clone();
        self.recorder = Some(recorder);
        self.status.recording = true;
        self.status.recording_ms = 0;
        self.status.message = "正在录音".into();
        Ok(())
    }

    pub fn stop_recording(&mut self, reason: Option<&str>) -> Result<()> {
        if let Some(recorder) = self.recorder.take() {
            self.status.recording = false;
            let record = recorder.finish(&self.config, &self.store, reason)?;
            self.status.recording_path = record.path;
            self.status.message = format!("录音 {}: {}", record.status, self.status.recording_path);
            if record.status != "complete" {
                self.status
                    .message
                    .push_str(&format!(" ({})", record.error.unwrap_or_default()));
            }
        }
        Ok(())
    }

    pub fn shutdown(&mut self) -> Result<()> {
        self.player = None;
        self.stop_recording(None)
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}
