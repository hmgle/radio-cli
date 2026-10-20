mod playback;
mod recording;
mod status;
use playback::PlaybackController;
use recording::RecordingController;
pub use status::{PlaybackState, RecordingState};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub station: Option<Station>,
    pub title: String,
    pub message: String,
    pub playback_state: PlaybackState,
    pub volume: u8,
    pub muted: bool,
    pub recording_state: RecordingState,
    pub recording_ms: u64,
    pub recording_path: String,
    pub source_url: String,
}

impl Status {
    pub(crate) fn control_eq(&self, other: &Self) -> bool {
        self.station == other.station
            && self.title == other.title
            && self.message == other.message
            && self.playback_state == other.playback_state
            && self.recording_state == other.recording_state
            && self.volume == other.volume
            && self.muted == other.muted
            && self.recording_path == other.recording_path
            && self.source_url == other.source_url
    }
    pub(crate) fn ui_eq(&self, other: &Self) -> bool {
        self.control_eq(other) && self.recording_ms / 1000 == other.recording_ms / 1000
    }
    pub fn playing(&self) -> bool {
        matches!(
            self.playback_state,
            PlaybackState::Playing | PlaybackState::Paused
        )
    }
    pub fn paused(&self) -> bool {
        self.playback_state == PlaybackState::Paused
    }
    pub fn recording(&self) -> bool {
        matches!(
            self.recording_state,
            RecordingState::Probing | RecordingState::Recording
        )
    }
}

pub struct Engine {
    session: Session,
    playback: PlaybackController,
    recording: RecordingController,
}

/// Shared services and published session state owned by the engine.
pub struct Session {
    pub config: Config,
    pub store: Store,
    pub directory: Directory,
    pub status: Status,
    pub stop: Arc<AtomicBool>,
    pub(crate) _lock: Arc<MediaLock>,
    pub(crate) background_media: bool,
    pub(crate) diagnostics: Option<Background<(serde_json::Value, bool)>>,
    pub(crate) library_task: Option<Background<String>>,
    pub playback_failed: bool,
}

impl std::ops::Deref for Engine {
    type Target = Session;
    fn deref(&self) -> &Session {
        &self.session
    }
}
impl std::ops::DerefMut for Engine {
    fn deref_mut(&mut self) -> &mut Session {
        &mut self.session
    }
}

impl Engine {
    pub fn bootstrap(&mut self) -> Result<usize> {
        self.session.directory.bootstrap(&mut self.session.store)
    }
    pub fn new(mut config: Config, stop: Arc<AtomicBool>) -> Result<Self> {
        let lock = MediaLock::acquire(&config.runtime_dir)?;
        let store = Store::open(&config.data_dir.join("radio.db"))?;
        if let Some(volume) = store
            .meta("pref_volume")?
            .and_then(|v| v.parse::<u8>().ok())
            .filter(|v| *v <= 100)
        {
            config.settings.volume = volume;
        }
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
            playback_state: PlaybackState::Idle,
            volume: config.settings.volume,
            muted: false,
            recording_state: RecordingState::Idle,
            recording_ms: 0,
            recording_path: String::new(),
            source_url: String::new(),
        };
        Ok(Self {
            session: Session {
                config,
                store,
                directory,
                status,
                stop,
                _lock: Arc::new(lock),
                background_media: false,
                diagnostics: None,
                library_task: None,
                playback_failed: false,
            },
            playback: PlaybackController {
                generation: 0,
                refresh_task: None,
                click_task: None,
                player: None,
                player_start: None,
                retiring: vec![],
                url: String::new(),
                candidates: VecDeque::new(),
                tried: vec![],
                refreshed: false,
                confirmed: false,
                session_started: false,
                source_kind: playback::SourceKind::Catalog,
                local_recording_id: None,
                reconnect_attempts: 0,
                reconnect_due: None,
                reconnect_window: VecDeque::new(),
                started: Instant::now(),
                random_filter: None,
                random_tried: vec![],
                failure: None,
            },
            recording: RecordingController {
                recording_probe: None,
                finalizing: None,
                recorder: None,
            },
        })
    }

    pub fn play(&mut self, uuid: &str) -> Result<()> {
        let result = self
            .playback
            .play(&mut self.session, &mut self.recording, uuid);
        if let Err(error) = &result
            && !self.playback_active()
        {
            self.status.playback_state = PlaybackState::Failed {
                reason: format!("{error:#}"),
            };
        }
        result
    }

    pub fn resume_last_station(&mut self) -> Result<()> {
        self.playback
            .resume_last_station(&mut self.session, &mut self.recording)
    }

    pub fn play_recording(&mut self, id: &str) -> Result<()> {
        let result = self
            .playback
            .play_recording(&mut self.session, &mut self.recording, id);
        if let Err(error) = &result
            && !self.playback_active()
        {
            self.status.playback_state = PlaybackState::Failed {
                reason: format!("{error:#}"),
            };
        }
        result
    }

    pub fn random(&mut self, filter: Filter) -> Result<()> {
        let result = self
            .playback
            .random(&mut self.session, &mut self.recording, filter);
        if let Err(error) = &result
            && !self.playback_active()
        {
            self.status.playback_state = PlaybackState::Failed {
                reason: format!("{error:#}"),
            };
        }
        result
    }

    pub fn tick(&mut self) -> Result<()> {
        self.poll_background()?;
        self.playback.tick(&mut self.session, &mut self.recording)?;
        self.recording
            .recording_tick(&mut self.session, &mut self.playback)
    }

    pub fn pause(&mut self) -> Result<()> {
        self.playback.pause(&mut self.session, &mut self.recording)
    }
    pub fn mute(&mut self) -> Result<()> {
        self.playback
            .player
            .as_mut()
            .context(DomainError::new(ErrorCode::NotPlaying))?
            .toggle_mute()
    }

    pub fn volume(&mut self, delta: i32) -> Result<()> {
        self.playback
            .volume(&mut self.session, &mut self.recording, delta)
    }

    pub fn stop_playback(&mut self) -> Result<()> {
        self.playback
            .stop_playback(&mut self.session, &mut self.recording)
    }

    pub fn toggle_recording(&mut self) -> Result<()> {
        let result = self
            .recording
            .toggle_recording(&mut self.session, &mut self.playback);
        if let Err(error) = &result
            && self.recording.recorder.is_none()
            && self.recording.recording_probe.is_none()
            && self.recording.finalizing.is_none()
        {
            self.status.recording_state = RecordingState::Failed {
                reason: format!("{error:#}"),
            };
        }
        result
    }

    pub fn record_current(
        &mut self,
        duration: Option<Duration>,
        dir: Option<&std::path::Path>,
    ) -> Result<()> {
        let result =
            self.recording
                .record_current(&mut self.session, &mut self.playback, duration, dir);
        if let Err(error) = &result
            && self.recording.recorder.is_none()
            && self.recording.recording_probe.is_none()
            && self.recording.finalizing.is_none()
        {
            self.status.recording_state = RecordingState::Failed {
                reason: format!("{error:#}"),
            };
        }
        result
    }

    pub fn standalone_record(
        &mut self,
        uuid: &str,
        duration: Option<Duration>,
        dir: Option<&std::path::Path>,
    ) -> Result<()> {
        let result = self.recording.standalone_record(
            &mut self.session,
            &mut self.playback,
            uuid,
            duration,
            dir,
        );
        if let Err(error) = &result
            && self.recording.recorder.is_none()
            && self.recording.recording_probe.is_none()
            && self.recording.finalizing.is_none()
        {
            self.status.recording_state = RecordingState::Failed {
                reason: format!("{error:#}"),
            };
        }
        result
    }

    pub fn stop_recording(&mut self, reason: Option<&str>) -> Result<()> {
        self.recording
            .stop_recording(&mut self.session, &mut self.playback, reason)
    }

    pub(crate) fn enable_background_media(&mut self) {
        self.background_media = true;
    }

    pub fn delete_recording(&mut self, id: String, index_only: bool) -> Result<()> {
        if self.library_task.is_some() {
            bail!(DomainError::new(ErrorCode::RecordingBusy));
        }
        if self.playback.local_recording_id.as_deref() == Some(&id) {
            self.stop_playback()?;
        }
        let path = self.config.data_dir.join("radio.db");
        let lock = self._lock.clone();
        self.library_task = Some(Background::start(
            "radio-recording-delete",
            move |cancel| {
                let _lock = lock;
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    bail!(DomainError::new(ErrorCode::Cancelled));
                }
                crate::service::delete_recording(&Store::open(&path)?, &id, index_only)?;
                Ok(format!("已删除录音 {id}"))
            },
        )?);
        Ok(())
    }

    pub fn open_recordings(&mut self) -> Result<()> {
        if self.library_task.is_some() {
            bail!(DomainError::new(ErrorCode::RecordingBusy));
        }
        let dir = self.config.recording_dir.clone();
        self.library_task = Some(Background::start("radio-open-recordings", move |cancel| {
            crate::service::open_recordings(&dir, &cancel)?;
            Ok(format!("已打开 {}", dir.display()))
        })?);
        Ok(())
    }

    pub fn playback_active(&self) -> bool {
        self.playback.player.is_some()
            || self.playback.player_start.is_some()
            || self.playback.refresh_task.is_some()
            || self.playback.reconnect_due.is_some()
    }

    pub fn playback_error(&self) -> Option<&DomainError> {
        self.playback.failure.as_ref()
    }

    pub fn shutdown(&mut self) -> Result<()> {
        self.stop_playback()?;
        self.playback.click_task = None;
        self.diagnostics = None;
        // Allow normal finalization, then cancel verification if its budget
        // expires. Outstanding HTTP results never need joining.
        let deadline = Instant::now() + Duration::from_secs(6);
        while (self.recording.finalizing.is_some() || !self.playback.retiring.is_empty())
            && Instant::now() < deadline
        {
            self.poll_background()?;
            std::thread::sleep(Duration::from_millis(10));
        }
        self.recording.finalizing = None;
        Ok(())
    }

    fn poll_background(&mut self) -> Result<()> {
        if let Some(result) = self.library_task.as_ref().and_then(Background::poll) {
            self.library_task = None;
            self.status.message = match result {
                Ok(message) => message,
                Err(error) => format!("录音操作失败: {error:#}"),
            };
        }
        self.playback
            .poll_background(&mut self.session, &mut self.recording)?;
        self.recording
            .record_poll_background(&mut self.session, &mut self.playback)
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
    fn device_failure_skips_backups_reconnects_refresh_and_random_fallback() {
        use crate::test_support::{Server, fake_mpv};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = requests.clone();
        let server = Server::new(move |_, _| {
            counter.fetch_add(1, Ordering::Relaxed);
        });
        let dir = tempfile::tempdir().unwrap();
        let mpv = fake_mpv(dir.path());
        for background in [false, true] {
            let data = dir.path().join(background.to_string());
            let mut config = Config::load(None, Some(&data), Some(&server.url)).unwrap();
            config.prepare().unwrap();
            config.settings.mpv = mpv.to_string_lossy().into_owned();
            config.settings.audio_device = Some("test/fail".into());
            let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
            engine.session.background_media = background;
            let station = Station {
                stationuuid: uuid::Uuid::new_v4().to_string(),
                name: "Catalog station".into(),
                url: format!("{}/live", server.url),
                active: true,
                ..Station::default()
            };
            engine.store.upsert(&station).unwrap();
            engine
                .store
                .add_backup(&station.stationuuid, &format!("{}/backup", server.url))
                .unwrap();
            engine.play(&station.stationuuid).unwrap();
            engine.playback.random_filter = Some(Filter::default());
            engine.playback.session_started = true;
            let deadline = Instant::now() + Duration::from_secs(3);
            while !engine.playback_failed {
                engine.tick().unwrap();
                assert!(Instant::now() < deadline, "{}", engine.status.message);
                std::thread::sleep(Duration::from_millis(10));
            }
            let error = engine.playback_error().unwrap();
            assert_eq!(error.code, ErrorCode::AudioDeviceFailed);
            assert_eq!(error.params["device"], "test/fail");
            assert_eq!(engine.playback.tried.len(), 1);
            assert!(!engine.playback_active());
            assert!(!engine.playback.refreshed);
            assert!(engine.playback.reconnect_window.is_empty());
            engine.shutdown().unwrap();
        }
        assert_eq!(requests.load(Ordering::Relaxed), 0);
        let commands = std::fs::read_to_string(mpv.with_extension("commands")).unwrap();
        assert_eq!(
            commands
                .lines()
                .filter(|s| s.contains("\"loadfile\""))
                .count(),
            2
        );
    }

    #[test]
    fn background_player_startup_preserves_dependency_failure() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = crate::test_support::config(dir.path());
        config.settings.mpv = dir
            .path()
            .join("missing-mpv")
            .to_string_lossy()
            .into_owned();
        let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
        engine.enable_background_media();
        engine.play("https://example.com/live").unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !engine.playback_failed {
            engine.tick().unwrap();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            engine.playback_error().unwrap().code,
            ErrorCode::MissingDependency
        );
        assert!(!engine.playback_active());
    }

    #[test]
    fn failed_recording_finalization_keeps_the_previous_playback_source_kind() {
        use crate::test_support::{Server, config, fixture, response};
        let bytes = fixture("libmp3lame", "mp3");
        let server = Server::new(move |_, stream| response(stream, 200, "audio/mpeg", &bytes));
        let dir = tempfile::tempdir().unwrap();
        let mut engine = Engine::new(config(dir.path()), Arc::new(AtomicBool::new(false))).unwrap();
        let station = Station {
            stationuuid: uuid::Uuid::new_v4().to_string(),
            name: "Saved station".into(),
            url: server.url.clone(),
            active: true,
            custom: true,
            ..Station::default()
        };
        engine.store.upsert(&station).unwrap();
        engine.status.station = Some(station.clone());
        engine.status.playback_state = PlaybackState::Playing;
        engine.playback.url = server.url.clone();
        engine.record_current(None, None).unwrap();
        let conn = rusqlite::Connection::open(dir.path().join("radio.db")).unwrap();
        conn.execute_batch("CREATE TRIGGER reject_recording_update BEFORE UPDATE ON recordings BEGIN SELECT RAISE(ABORT, 'test failure'); END;").unwrap();
        assert!(engine.play("https://example.com/audition").is_err());
        assert_eq!(
            engine.status.station.as_ref().unwrap().stationuuid,
            station.stationuuid
        );
        assert_eq!(engine.playback.source_kind, playback::SourceKind::Catalog);
    }

    #[test]
    fn recording_track_is_snapshotted_before_probe_and_stays_stable() {
        use crate::test_support::{Server, config, fixture, response};
        let bytes = fixture("libmp3lame", "mp3");
        let server = Server::new(move |_, stream| response(stream, 200, "audio/mpeg", &bytes));
        for background in [false, true] {
            for title in [
                "Björk / 中文\x1b\n".to_owned(),
                String::new(),
                "曲".repeat(100),
            ] {
                let dir = tempfile::tempdir().unwrap();
                let mut engine =
                    Engine::new(config(dir.path()), Arc::new(AtomicBool::new(false))).unwrap();
                if background {
                    engine.enable_background_media();
                }
                let station = Station {
                    stationuuid: uuid::Uuid::new_v4().to_string(),
                    name: if title.starts_with('曲') {
                        "台".repeat(60)
                    } else {
                        "Station".into()
                    },
                    url: server.url.clone(),
                    custom: true,
                    active: true,
                    ..Station::default()
                };
                engine.store.upsert(&station).unwrap();
                engine.status.station = Some(station);
                engine.status.playback_state = PlaybackState::Playing;
                engine.status.title = title.clone();
                engine.playback.url = server.url.clone();
                engine
                    .record_current(Some(Duration::from_secs(1)), None)
                    .unwrap();
                engine.status.title = "Next track".into();
                let deadline = Instant::now() + Duration::from_secs(8);
                while engine.status.recording_path.is_empty() {
                    engine.tick().unwrap();
                    assert!(Instant::now() < deadline, "{}", engine.status.message);
                    std::thread::sleep(Duration::from_millis(10));
                }
                let record = engine.store.recordings(1).unwrap().remove(0);
                let expected = crate::clean_text(&title).trim().to_owned();
                assert_eq!(
                    record.track.as_deref(),
                    (!expected.is_empty()).then_some(expected.as_str())
                );
                let final_path = record.final_path.clone().unwrap();
                let name = std::path::Path::new(&record.path)
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap();
                assert!(name.len() + ".json.tmp".len() <= 255);
                if title.starts_with("Björk") {
                    assert!(name.contains("-Station-Björk中文-"));
                }
                if title.is_empty() {
                    assert!(name.contains("-Station-"));
                }
                while engine.status.recording()
                    || engine.status.recording_state == RecordingState::Finalizing
                {
                    engine.tick().unwrap();
                    assert!(Instant::now() < deadline, "{}", engine.status.message);
                    std::thread::sleep(Duration::from_millis(10));
                }
                let saved = engine.store.recording(&record.id).unwrap();
                assert_eq!(saved.status, "complete", "{:?}", saved.error);
                assert_eq!(saved.path, final_path);
                assert_eq!(saved.track, record.track);
                let sidecar: crate::model::Recording =
                    serde_json::from_slice(&std::fs::read(format!("{}.json", saved.path)).unwrap())
                        .unwrap();
                assert_eq!(sidecar.track, saved.track);
                let reopened = Store::open(&dir.path().join("radio.db")).unwrap();
                assert_eq!(reopened.recording(&record.id).unwrap().track, saved.track);
            }
        }
    }

    #[test]
    fn changing_station_clears_finished_recording_status() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(None, Some(dir.path()), Some("http://127.0.0.1:1")).unwrap();
        config.prepare().unwrap();
        let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
        for state in [
            RecordingState::Done,
            RecordingState::Failed {
                reason: "disk".into(),
            },
        ] {
            engine.status.recording_state = state;
            engine.status.recording_path = "old.mp3".into();
            engine.status.recording_ms = 1000;
            // No source reaches mpv; the new station still owns clean status.
            assert!(
                engine
                    .playback
                    .play_source(
                        &mut engine.session,
                        &mut engine.recording,
                        Station::default(),
                        vec![],
                        playback::SourceKind::Catalog
                    )
                    .is_err()
            );
            assert_eq!(engine.status.recording_state, RecordingState::Idle);
            assert!(engine.status.recording_path.is_empty());
            assert_eq!(engine.status.recording_ms, 0);
        }
    }

    #[test]
    fn flapping_reconnects_have_a_sliding_window_limit() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(None, Some(dir.path()), Some("http://127.0.0.1:1")).unwrap();
        config.prepare().unwrap();
        let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
        engine.status.station = Some(Station {
            custom: true,
            ..Station::default()
        });
        engine.playback.session_started = true;
        engine.playback.reconnect_window = std::iter::repeat_n(Instant::now(), 6).collect();
        engine
            .playback
            .stream_failed(&mut engine.session, &mut engine.recording, "EOF")
            .unwrap();
        assert!(engine.playback_failed);
        assert!(!engine.status.muted);
        assert_eq!(engine.playback.reconnect_attempts, 0);
        assert!(engine.playback.reconnect_window.is_empty());
        assert!(engine.playback.reconnect_due.is_none());
        let failed_state = engine.status.playback_state.clone();
        for _ in 0..3 {
            engine.tick().unwrap();
            assert!(engine.playback_failed);
            assert_eq!(engine.status.playback_state, failed_state);
            assert!(!engine.playback_active());
        }
        engine.playback.reconnect_window =
            std::iter::repeat_n(Instant::now() - Duration::from_secs(61), 6).collect();
        engine
            .playback
            .stream_failed(&mut engine.session, &mut engine.recording, "EOF")
            .unwrap();
        assert!(engine.playback.reconnect_due.is_some());
        assert_eq!(engine.playback.reconnect_window.len(), 1);
    }

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
        engine.status.muted = true;
        engine.playback.session_started = true;
        for delay in [1, 3, 7] {
            engine
                .playback
                .stream_failed(&mut engine.session, &mut engine.recording, "EOF")
                .unwrap();
            let remaining = engine
                .playback
                .reconnect_due
                .unwrap()
                .saturating_duration_since(Instant::now());
            assert!(remaining <= Duration::from_secs(delay));
            assert!(remaining > Duration::from_millis(delay * 1000 - 100));
            assert!(!engine.playback_failed);
        }
        engine.playback.reconnect_due = None;
        engine
            .playback
            .stream_failed(&mut engine.session, &mut engine.recording, "EOF")
            .unwrap();
        assert!(engine.playback_failed);
        assert!(!engine.status.muted);
        assert_eq!(engine.playback.reconnect_attempts, 0);
        assert!(engine.playback.reconnect_window.is_empty());
        assert!(engine.playback.reconnect_due.is_none());
        engine.playback.reconnect_attempts = 0;
        engine
            .playback
            .stream_failed(&mut engine.session, &mut engine.recording, "EOF")
            .unwrap();
        engine.status.muted = true;
        engine.stop_playback().unwrap();
        assert!(!engine.status.muted);
        assert_eq!(engine.playback.reconnect_attempts, 0);
        assert!(engine.playback.reconnect_window.is_empty());
        assert!(engine.playback.reconnect_due.is_none());
        let generation = engine.playback.generation;
        engine.tick().unwrap();
        assert_eq!(engine.playback.generation, generation);
        assert!(engine.playback.player.is_none() && engine.playback.player_start.is_none());
    }
}
