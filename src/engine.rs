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
    pub recording_state: RecordingState,
    pub recording_ms: u64,
    pub recording_path: String,
    pub source_url: String,
}

impl Status {
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
                history_recorded: false,
                local_playback: false,
                local_recording_id: None,
                reconnect_attempts: 0,
                reconnect_due: None,
                started: Instant::now(),
                random_filter: None,
                random_tried: vec![],
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
        engine.playback.history_recorded = true;
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
        assert!(engine.playback.reconnect_due.is_none());
        engine.playback.reconnect_attempts = 0;
        engine
            .playback
            .stream_failed(&mut engine.session, &mut engine.recording, "EOF")
            .unwrap();
        engine.stop_playback().unwrap();
        assert!(engine.playback.reconnect_due.is_none());
        let generation = engine.playback.generation;
        engine.tick().unwrap();
        assert_eq!(engine.playback.generation, generation);
        assert!(engine.playback.player.is_none() && engine.playback.player_start.is_none());
    }
}
