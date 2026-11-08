pub(crate) mod artwork;
mod persistence;
mod playback;
mod queue;
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

#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub episode: Option<crate::podcast::Episode>,
    pub seekable: bool,
    pub speed: f64,
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
        self.episode
            .as_ref()
            .map(|e| (&e.id, e.completed, e.observed_duration_ms))
            == other
                .episode
                .as_ref()
                .map(|e| (&e.id, e.completed, e.observed_duration_ms))
            && self.seekable == other.seekable
            && self.speed == other.speed
            && self.station == other.station
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
        self.control_eq(other)
            && self.recording_ms / 1000 == other.recording_ms / 1000
            && self.episode.as_ref().map(|e| e.position_ms / 1000)
                == other.episode.as_ref().map(|e| e.position_ms / 1000)
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
    podcast_runtime: crate::podcast::Runtime,
    session: Session,
    playback: PlaybackController,
    recording: RecordingController,
}

/// Shared services and published session state owned by the engine.
pub(crate) struct Session {
    persistence: Option<persistence::Writer>,
    pub subtitles: crate::speech::Controller,
    pub(crate) artwork: artwork::Controller,
    pub config: Config,
    pub store: Store,
    pub directory: Directory,
    pub status: Status,
    // Session preferences survive playback resets and pending database writes.
    pub podcast_speed: f64,
    pub podcast_auto_next: bool,
    pub stop: Arc<AtomicBool>,
    pub(crate) _lock: Arc<MediaLock>,
    pub(crate) background_media: bool,
    pub(crate) diagnostics: Option<Background<(serde_json::Value, bool)>>,
    pub(crate) library_task: Option<Background<String>>,
    pub playback_failed: bool,
}

impl Session {
    pub(crate) fn save_preference(&self, key: &str, value: &str) -> Result<()> {
        if let Some(writer) = &self.persistence {
            writer.preference(key, value)
        } else {
            if let Some(id) = key.strip_prefix("podcast_progress:") {
                let (pos, duration, done) = serde_json::from_str(value)?;
                self.store.podcast_progress(id, pos, duration, done)
            } else if let Some(id) = key.strip_prefix("podcast_played:") {
                self.store.podcast_played(id)
            } else {
                self.store.set_meta(key, value)
            }
        }
    }

    fn remember_play(&self, uuid: &str) -> Result<()> {
        if let Some(writer) = &self.persistence {
            writer.played(uuid)?;
            writer.preference("pref_last_media", "station")
        } else {
            self.store.played(uuid)?;
            self.store.set_meta("pref_last_media", "station")?;
            self.store.set_meta("pref_last_station", uuid)
        }
    }
}

impl Engine {
    pub fn status(&self) -> &Status {
        &self.session.status
    }

    pub fn config(&self) -> &Config {
        &self.session.config
    }

    pub fn store(&self) -> &Store {
        &self.session.store
    }

    pub fn subtitles(&self) -> &crate::speech::Snapshot {
        &self.session.subtitles.snapshot
    }

    pub fn playback_failed(&self) -> bool {
        self.session.playback_failed
    }

    /// Add, edit, remove, or change backup URLs without exposing session state.
    pub fn change_station(
        &mut self,
        command: crate::service::StationCommand,
    ) -> Result<crate::service::StationChange> {
        crate::service::change_station(&mut self.session.store, command)
    }

    /// Atomically toggle a favorite and return its new state.
    pub fn toggle_favorite(&mut self, uuid: &str) -> Result<bool> {
        self.session.store.toggle_favorite(uuid)
    }

    pub(crate) fn save_preference(&self, key: &str, value: &str) -> Result<()> {
        self.session.save_preference(key, value)
    }

    pub(crate) fn set_language(&mut self, language: String) {
        crate::messages::init(Some(&language), None);
        self.session.config.settings.language = Some(language.clone());
        self.session.status.message = crate::t!("language_set", "language" => language);
    }

    pub(crate) fn request_artwork(&mut self, station: Option<String>) {
        self.session.artwork.demand = station;
    }

    pub(crate) fn track_artwork(&self) -> &Option<artwork::TrackArtwork> {
        &self.session.artwork.cover
    }

    pub(crate) fn start_diagnostics(&mut self) -> Result<()> {
        if self.session.diagnostics.is_some() {
            bail!(crate::t!("diagnostics_busy"));
        }
        let config = self.session.config.clone();
        self.session.diagnostics = Some(Background::start("radio-doctor", move |cancel| {
            crate::service::doctor_report(&config, &cancel)
        })?);
        Ok(())
    }

    pub(crate) fn poll_diagnostics(&mut self) -> Option<Result<(serde_json::Value, bool)>> {
        let result = self.session.diagnostics.as_ref()?.poll()?;
        self.session.diagnostics = None;
        Some(result)
    }

    pub(crate) fn stop(&mut self) -> Result<()> {
        self.stop_playback()?;
        self.session.status.message = crate::t!("playback_stopped").into();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn test_session(&mut self) -> &mut Session {
        &mut self.session
    }

    pub fn bootstrap(&mut self) -> Result<usize> {
        self.session.directory.bootstrap(&mut self.session.store)
    }

    /// Fetch and publish a full catalog. This call performs blocking I/O.
    pub fn sync(&mut self, progress: impl FnMut(usize)) -> Result<usize> {
        self.sync_with_policy(progress, false)
    }

    /// Sync with explicit permission to accept a catalog shrink over 25%.
    /// This call performs blocking I/O.
    pub fn sync_with_policy(
        &mut self,
        progress: impl FnMut(usize),
        accept_shrink: bool,
    ) -> Result<usize> {
        self.session
            .directory
            .sync_with_policy(&mut self.session.store, progress, accept_shrink)
    }

    /// Fetch current directory metadata without persisting it. Performs I/O.
    pub fn refresh_station(&mut self, uuid: &str) -> Result<Option<Station>> {
        self.session.directory.refresh(uuid)
    }

    /// Report a confirmed play to the directory. This call performs blocking I/O.
    pub fn report_click(&mut self, uuid: &str) -> Result<()> {
        self.session.directory.click(uuid)
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
        let podcast_speed = store
            .meta("podcast_speed")?
            .and_then(|v| v.parse().ok())
            .unwrap_or(1.);
        let podcast_auto_next = store.meta("podcast_auto_next")?.as_deref() == Some("on");
        let recovery_warnings = recorder::recover(&store)?;
        let directory = Directory::new(&config, stop.clone())?;
        let status = Status {
            episode: None,
            seekable: false,
            speed: 1.,
            station: None,
            title: String::new(),
            message: if recovery_warnings.is_empty() {
                crate::t!("status_ready").into()
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
            podcast_runtime: Default::default(),
            session: Session {
                persistence: None,
                subtitles: crate::speech::Controller::default(),
                artwork: artwork::Controller::default(),
                config,
                store,
                directory,
                status,
                podcast_speed,
                podcast_auto_next,
                stop,
                _lock: Arc::new(lock),
                background_media: false,
                diagnostics: None,
                library_task: None,
                playback_failed: false,
            },
            playback: PlaybackController {
                episode: None,
                progress: Default::default(),
                seek_request: None,
                loaded: false,
                resume_pending: false,
                resume_blocked: false,
                last_save: Instant::now(),
                queue_mode: false,
                auto_next: false,
                queue_completions: Default::default(),
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
                retiring_probes: vec![],
                finalizing: None,
                recorder: None,
            },
        })
    }

    pub fn play_episode(&mut self, id: &str, from_start: bool, speed: Option<f64>) -> Result<()> {
        self.playback.play_episode(
            &mut self.session,
            &mut self.recording,
            id,
            from_start,
            speed,
        )
    }
    pub fn play_podcast_queue(&mut self, auto_next: bool) -> Result<()> {
        let id = self
            .store()
            .podcast_queue()?
            .first()
            .map(|e| e.id.clone())
            .context(crate::t!("podcast_empty_queue"))?;
        self.play_episode(&id, false, None)?;
        self.playback.queue_mode = true;
        self.playback.auto_next = auto_next;
        Ok(())
    }
    pub fn seek_episode(&mut self, position: u64) -> Result<()> {
        self.playback.seek_episode(&mut self.session, position)
    }
    pub fn podcast_speed(&mut self, rate: f64) -> Result<()> {
        crate::podcast::valid_speed(rate)?;
        if let Some(p) = self.playback.player.as_mut()
            && self.playback.episode.is_some()
        {
            p.request(serde_json::json!(["set_property", "speed", rate]))?;
        }
        self.session.podcast_speed = rate;
        self.session.status.speed = rate;
        self.session
            .save_preference("podcast_speed", &rate.to_string())
    }
    pub fn podcast_auto_next(&mut self, on: bool) -> Result<()> {
        self.session.podcast_auto_next = on;
        self.playback.auto_next = on;
        self.session
            .save_preference("podcast_auto_next", if on { "on" } else { "off" })
    }
    pub fn play(&mut self, uuid: &str) -> Result<()> {
        let result = self
            .playback
            .play(&mut self.session, &mut self.recording, uuid);
        if let Err(error) = &result
            && !self.playback_active()
        {
            self.session.status.playback_state = PlaybackState::Failed {
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
            self.session.status.playback_state = PlaybackState::Failed {
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
            self.session.status.playback_state = PlaybackState::Failed {
                reason: format!("{error:#}"),
            };
        }
        result
    }

    pub fn podcast_action(&mut self, action: crate::podcast::Action) -> Result<()> {
        use crate::podcast::{Action, DownloadAction, QueueAction};
        match action {
            Action::Play {
                id,
                from_start,
                speed,
            } => self.play_episode(&id, from_start, speed),
            Action::Queue {
                action: QueueAction::Play { auto_next },
            } => self.play_podcast_queue(auto_next || self.session.podcast_auto_next),
            Action::Seek { time } => self.seek_episode(crate::podcast::parse_time(&time)?),
            Action::Speed { rate } => self.podcast_speed(rate),
            Action::Mark { id, state } => {
                let mut e = self.session.store.podcast_episode(&id)?;
                if self.playback.episode.as_ref().is_some_and(|e| e.id == id) {
                    self.stop_playback()?;
                    self.playback.episode = None;
                }
                e.completed = state == "played";
                e.position_ms = if e.completed {
                    e.observed_duration_ms.unwrap_or(e.position_ms)
                } else {
                    0
                };
                self.playback.progress.insert(id.clone(), e.clone());
                if self
                    .session
                    .status
                    .episode
                    .as_ref()
                    .is_some_and(|p| p.id == id)
                {
                    self.session.status.episode = Some(e.clone());
                }
                self.session.save_preference(
                    &format!("podcast_progress:{id}"),
                    &serde_json::to_string(&(e.position_ms, e.observed_duration_ms, e.completed))?,
                )
            }
            Action::AutoNext { state } => self.podcast_auto_next(state == "on"),
            Action::Downloads {
                action: DownloadAction::Remove { ref id },
            } if self.podcast_runtime.download_active(id) => {
                Err(crate::podcast::error(crate::t!("podcast_download_busy")))
            }
            Action::Downloads {
                action: DownloadAction::Remove { ref id },
            } if self.playback_active()
                && self
                    .store()
                    .podcast_download(id)?
                    .is_some_and(|d| !d.path.is_empty() && d.path == self.playback.url) =>
            {
                Err(crate::podcast::error(crate::t!("podcast_download_in_use")))
            }
            action => self.podcast_runtime.action(
                &self.session.config,
                action,
                self.session._lock.clone(),
            ),
        }
    }
    pub fn podcast_tasks(&self) -> (usize, bool) {
        self.podcast_runtime.tasks()
    }
    pub fn poll_podcast(&mut self) -> Option<Result<serde_json::Value>> {
        self.podcast_runtime.poll()
    }
    pub fn cancel_podcast(&mut self) -> bool {
        self.podcast_runtime.cancel()
    }
    pub fn tick(&mut self) -> Result<()> {
        if self.session.background_media {
            self.podcast_runtime
                .tick(&self.session.config, self.session._lock.clone())?;
        }
        self.poll_background()?;
        self.playback.tick(&mut self.session, &mut self.recording)?;
        self.recording
            .recording_tick(&mut self.session, &mut self.playback)?;
        self.subtitle_tick();
        self.artwork_tick();
        Ok(())
    }

    fn subtitle_tick(&mut self) {
        if !self.session.background_media {
            return;
        }
        if self.session.subtitles.snapshot.enabled && !self.session.artwork.quiescent() {
            self.session.artwork.cancel();
            return;
        }
        let playing = self.session.status.playback_state == PlaybackState::Playing;
        let paused = self.session.status.paused();
        let local = self.playback.source_kind == playback::SourceKind::Local;
        let generation = self.playback.generation;
        let session = &mut self.session;
        let key = (generation, session.status.source_url.as_str());
        session
            .subtitles
            .update(&session.config, key, playing, paused, local, &session.stop);
    }

    pub fn set_subtitles(&mut self, enabled: bool) {
        self.session.subtitles.enabled(enabled);
    }
    pub fn retry_subtitles(&mut self) {
        self.session.subtitles.retry();
    }
    pub fn clear_subtitles(&mut self) {
        self.session.subtitles.clear();
    }
    pub fn set_subtitle_model(&mut self, id: String) -> Result<()> {
        crate::speech::validate_model(&id)?;
        let mut settings = self.session.config.settings.subtitles.clone();
        settings.model = id;
        settings.validate()?;
        self.session.config.settings.subtitles = settings;
        self.session.subtitles.retry();
        self.session.subtitles.clear();
        Ok(())
    }

    fn artwork_tick(&mut self) {
        let request = (self.session.background_media
            && !self.session.subtitles.snapshot.enabled
            && self.session.subtitles.quiescent()
            && self.session.config.settings.artwork == crate::config::ArtworkMode::Track
            && self.session.status.playback_state == PlaybackState::Playing
            && !matches!(
                self.session.status.recording_state,
                RecordingState::Probing | RecordingState::Recording | RecordingState::Finalizing
            )
            && self.playback.source_kind != playback::SourceKind::Local
            && self.playback.retiring.is_empty()
            && self.recording.retiring_probes.is_empty()
            && !self.session.status.title.is_empty())
        .then_some(self.session.status.station.as_ref())
        .flatten()
        .filter(|station| {
            station.hls == 0
                && self.session.artwork.demand.as_deref() == Some(station.stationuuid.as_str())
        })
        .filter(|_| crate::config::validate_url(&self.session.status.source_url).is_ok())
        .map(|station| artwork::Request {
            stationuuid: station.stationuuid.clone(),
            source_url: self.session.status.source_url.clone(),
            title: self.session.status.title.clone(),
        });
        let session = &mut self.session;
        session.artwork.update(&session.config, request);
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
            self.session.status.recording_state = RecordingState::Failed {
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
            self.session.status.recording_state = RecordingState::Failed {
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
            self.session.status.recording_state = RecordingState::Failed {
                reason: format!("{error:#}"),
            };
        }
        result
    }

    pub fn stop_recording(&mut self, reason: Option<&str>) -> Result<()> {
        self.recording
            .stop_recording(&mut self.session, &mut self.playback, reason)
    }

    pub(crate) fn enable_background_media(&mut self) -> Result<()> {
        if self.session.persistence.is_none() {
            self.session.persistence = Some(persistence::Writer::start(
                self.session.config.data_dir.join("radio.db"),
                self.session._lock.clone(),
            )?);
        }
        self.session.background_media = true;
        let enabled = self.session.config.settings.subtitles.enabled;
        self.session.subtitles.enabled(enabled);
        Ok(())
    }

    pub fn delete_recording(&mut self, id: String, index_only: bool) -> Result<()> {
        if self.session.library_task.is_some() {
            bail!(DomainError::new(ErrorCode::RecordingBusy));
        }
        if self.playback.local_recording_id.as_deref() == Some(&id) {
            self.stop_playback()?;
        }
        let path = self.session.config.data_dir.join("radio.db");
        let lock = self.session._lock.clone();
        self.session.library_task = Some(Background::start(
            "radio-recording-delete",
            move |cancel| {
                let _lock = lock;
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    bail!(DomainError::new(ErrorCode::Cancelled));
                }
                crate::service::delete_recording(&Store::open(&path)?, &id, index_only)?;
                Ok(crate::t!("recording_deleted", "id" => id))
            },
        )?);
        Ok(())
    }

    pub fn open_recordings(&mut self) -> Result<()> {
        if self.session.library_task.is_some() {
            bail!(DomainError::new(ErrorCode::RecordingBusy));
        }
        let dir = self.session.config.recording_dir.clone();
        self.session.library_task =
            Some(Background::start("radio-open-recordings", move |cancel| {
                crate::service::open_recordings(&dir, &cancel)?;
                Ok(crate::t!("recordings_opened", "path" => dir.display()))
            })?);
        Ok(())
    }

    pub fn playback_active(&self) -> bool {
        self.playback.player.is_some()
            || self.playback.player_start.is_some()
            || self.playback.refresh_task.is_some()
            || self.playback.reconnect_due.is_some()
            || self
                .playback
                .queue_completions
                .active(self.playback.generation)
    }

    pub fn playback_error(&self) -> Option<&DomainError> {
        self.playback.failure.as_ref()
    }

    pub fn shutdown(&mut self) -> Result<()> {
        self.shutdown_with_budget(Duration::from_secs(6))
    }

    fn shutdown_with_budget(&mut self, budget: Duration) -> Result<()> {
        self.podcast_runtime = Default::default();
        let mut failure = self.stop_playback().err();
        self.session.subtitles.enabled(false);
        self.playback.click_task = None;
        self.session.diagnostics = None;
        self.session.library_task = None;
        let deadline = Instant::now() + budget;
        if let Some(writer) = &self.session.persistence {
            writer.close(deadline);
        }
        // Allow normal finalization, then cancel verification if its budget
        // expires. Outstanding HTTP results never need joining.
        while (self.recording.finalizing.is_some()
            || !self.playback.queue_completions.is_empty()
            || !self.playback.retiring.is_empty()
            || !self.recording.retiring_probes.is_empty()
            || !self.session.artwork.quiescent()
            || !self.session.subtitles.quiescent()
            || self
                .session
                .persistence
                .as_ref()
                .is_some_and(|writer| !writer.finished()))
            && Instant::now() < deadline
        {
            if let Err(error) = self.poll_background() {
                failure.get_or_insert(error);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.recording.finalizing = None;
        if !self.playback.queue_completions.is_empty() {
            failure.get_or_insert_with(|| anyhow::anyhow!(crate::t!("session_save_failed")));
        }
        if let Some(writer) = self.session.persistence.take()
            && let Err(error) = writer.finish().context(crate::t!("session_save_failed"))
        {
            failure.get_or_insert(error);
        }
        failure.map_or(Ok(()), Err)
    }

    fn poll_background(&mut self) -> Result<()> {
        if let Some(writer) = &self.session.persistence {
            writer.poll().context(crate::t!("session_save_failed"))?;
        }
        self.session.subtitles.poll_retiring();
        if let Some(result) = self
            .session
            .library_task
            .as_ref()
            .and_then(Background::poll)
        {
            self.session.library_task = None;
            self.session.status.message = match result {
                Ok(message) => message,
                Err(error) => {
                    crate::t!("recording_operation_failed", "error" => format!("{error:#}"))
                }
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
    fn queue_completion_refreshes_candidates_after_concurrent_edits() {
        use crate::podcast::{Episode, Feed, Preview, QueueAction};
        for edit in ["clear", "remove", "move", "readd"] {
            let dir = tempfile::tempdir().unwrap();
            let mut config = crate::test_support::config(dir.path());
            config.settings.mpv = crate::test_support::fake_mpv(dir.path())
                .to_string_lossy()
                .into_owned();
            let path = config.data_dir.join("radio.db");
            let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
            let preview = Preview {
                feed: Feed {
                    url: "https://example.test/feed".into(),
                    ..Default::default()
                },
                episodes: ["episode-eof", "second", "third"]
                    .into_iter()
                    .map(|name| Episode {
                        guid: Some(name.into()),
                        url: format!("https://example.test/{name}"),
                        ..Default::default()
                    })
                    .collect(),
            };
            let feed = engine
                .store()
                .podcast_subscribe(&preview, &preview.feed.url)
                .unwrap();
            let episodes = engine.store().podcast_episodes(&feed.id).unwrap();
            let ids: Vec<_> = ["episode-eof", "second", "third"]
                .into_iter()
                .map(|name| {
                    episodes
                        .iter()
                        .find(|e| e.guid.as_deref() == Some(name))
                        .unwrap()
                        .id
                        .clone()
                })
                .collect();
            engine.store().podcast_queue_set(&ids).unwrap();
            engine.enable_background_media().unwrap();
            engine.play_podcast_queue(true).unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            while engine.playback.queue_completions.is_empty() {
                engine.tick().unwrap();
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
            // Launch completion, then stop polling until its transaction commits.
            engine.poll_background().unwrap();
            let external = Store::open(&path).unwrap();
            while external.podcast_queue().unwrap().len() != 2 {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
            let action = match edit {
                "clear" => QueueAction::Clear,
                "remove" => QueueAction::Remove { id: ids[1].clone() },
                "move" => QueueAction::Move {
                    id: ids[2].clone(),
                    position: 1,
                },
                "readd" => QueueAction::Add { id: ids[0].clone() },
                _ => unreachable!(),
            };
            external.podcast_queue_update(action).unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            while !engine.playback.queue_completions.is_empty() {
                engine.tick().unwrap();
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
            match edit {
                "clear" => {
                    assert!(!engine.playback_active());
                    assert_eq!(engine.status().playback_state, PlaybackState::Idle);
                }
                "remove" | "move" => {
                    assert_eq!(engine.status().episode.as_ref().unwrap().id, ids[2])
                }
                "readd" => {
                    assert_eq!(engine.status().episode.as_ref().unwrap().id, ids[1]);
                    assert!(
                        external
                            .podcast_queue()
                            .unwrap()
                            .iter()
                            .any(|e| e.id == ids[0])
                    );
                }
                _ => unreachable!(),
            }
            engine.shutdown().unwrap();
        }
    }

    #[test]
    fn queue_completion_retries_without_blocking_or_overriding_controls() {
        use crate::podcast::{Episode, Feed, Preview};
        for control in ["advance", "stop", "switch", "off", "shutdown"] {
            let dir = tempfile::tempdir().unwrap();
            let mut config = crate::test_support::config(dir.path());
            config.settings.mpv = crate::test_support::fake_mpv(dir.path())
                .to_string_lossy()
                .into_owned();
            let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
            let preview = Preview {
                feed: Feed {
                    url: "https://example.test/feed".into(),
                    ..Default::default()
                },
                episodes: ["episode-eof", "audio"]
                    .into_iter()
                    .map(|name| Episode {
                        guid: Some(name.into()),
                        url: format!("https://example.test/{name}"),
                        ..Default::default()
                    })
                    .collect(),
            };
            let feed = engine
                .store()
                .podcast_subscribe(&preview, &preview.feed.url)
                .unwrap();
            let episodes = engine.store().podcast_episodes(&feed.id).unwrap();
            let first = episodes
                .iter()
                .find(|e| e.guid.as_deref() == Some("episode-eof"))
                .unwrap();
            let next = episodes
                .iter()
                .find(|e| e.guid.as_deref() == Some("audio"))
                .unwrap();
            engine
                .store()
                .podcast_queue_set(&[first.id.clone(), next.id.clone()])
                .unwrap();
            engine.enable_background_media().unwrap();
            engine.play_podcast_queue(true).unwrap();
            let connection = rusqlite::Connection::open(dir.path().join("radio.db")).unwrap();
            connection.execute_batch("BEGIN IMMEDIATE").unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            while engine.playback.queue_completions.is_empty() {
                let started = Instant::now();
                engine.tick().unwrap();
                assert!(
                    started.elapsed() < Duration::from_millis(250),
                    "EOF blocked controls"
                );
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
            let deadline = Instant::now() + Duration::from_millis(350);
            while Instant::now() < deadline {
                let started = Instant::now();
                engine.tick().unwrap();
                engine.volume(-1).unwrap();
                assert!(
                    started.elapsed() < Duration::from_millis(250),
                    "queue retry blocked controls"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(engine.store().podcast_queue().unwrap().len(), 2);
            assert!(!engine.playback.queue_completions.is_empty());
            if control == "shutdown" {
                let release = std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(200));
                    connection.execute_batch("ROLLBACK").unwrap();
                });
                engine.shutdown().unwrap();
                release.join().unwrap();
                assert!(engine.playback.queue_completions.is_empty());
                assert_eq!(engine.status().playback_state, PlaybackState::Idle);
                assert_eq!(engine.store().podcast_queue().unwrap()[0].id, next.id);
                assert!(engine.store().podcast_episode(&first.id).unwrap().completed);
                continue;
            }
            match control {
                "stop" => {
                    engine.stop_playback().unwrap();
                    assert!(!engine.playback_active());
                }
                "switch" => {
                    engine.play("https://example.test/live").unwrap();
                }
                "off" => engine.podcast_auto_next(false).unwrap(),
                _ => assert!(engine.playback_active()),
            }
            connection.execute_batch("ROLLBACK").unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                engine.tick().unwrap();
                if engine.playback.queue_completions.is_empty()
                    && (matches!(control, "stop" | "off")
                        || engine.status().playback_state == PlaybackState::Playing)
                {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "{control}: {:?}",
                    engine.status()
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(
                engine
                    .store()
                    .podcast_queue()
                    .unwrap()
                    .iter()
                    .map(|e| &e.id)
                    .collect::<Vec<_>>(),
                vec![&next.id]
            );
            match control {
                "advance" => assert_eq!(engine.status().episode.as_ref().unwrap().id, next.id),
                "stop" | "off" => assert_eq!(engine.status().playback_state, PlaybackState::Idle),
                "switch" => {
                    assert!(engine.status().episode.is_none());
                    assert_eq!(engine.status().source_url, "https://example.test/live");
                }
                _ => unreachable!(),
            }
            engine.shutdown().unwrap();
            assert!(engine.store().podcast_episode(&first.id).unwrap().completed);
        }
    }

    #[test]
    fn podcast_preferences_apply_before_the_background_write_finishes() {
        use crate::podcast::{Action, Episode, Feed, Preview, QueueAction};
        for (old_speed, old_auto_next, speed, auto_next) in
            [(1., false, 1.5, true), (1.5, true, 1.25, false)]
        {
            let dir = tempfile::tempdir().unwrap();
            let mut config = crate::test_support::config(dir.path());
            let mpv = crate::test_support::fake_mpv(dir.path());
            config.settings.mpv = mpv.to_string_lossy().into_owned();
            let store = Store::open(&config.data_dir.join("radio.db")).unwrap();
            store
                .set_meta("podcast_speed", &old_speed.to_string())
                .unwrap();
            store
                .set_meta(
                    "podcast_auto_next",
                    if old_auto_next { "on" } else { "off" },
                )
                .unwrap();
            let preview = Preview {
                feed: Feed {
                    url: "https://example.test/feed".into(),
                    ..Default::default()
                },
                episodes: vec![Episode {
                    url: "https://example.test/audio".into(),
                    ..Default::default()
                }],
            };
            let feed = store
                .podcast_subscribe(&preview, &preview.feed.url)
                .unwrap();
            let id = store.podcast_episodes(&feed.id).unwrap().remove(0).id;
            store.podcast_queue_set(std::slice::from_ref(&id)).unwrap();
            let mut engine = Engine::new(config.clone(), Arc::new(AtomicBool::new(false))).unwrap();
            engine.enable_background_media().unwrap();
            let connection = rusqlite::Connection::open(dir.path().join("radio.db")).unwrap();
            connection.execute_batch("BEGIN IMMEDIATE").unwrap();
            engine
                .podcast_action(Action::Speed { rate: speed })
                .unwrap();
            engine
                .podcast_action(Action::AutoNext {
                    state: if auto_next { "on" } else { "off" }.into(),
                })
                .unwrap();
            assert_eq!(
                store.meta("podcast_speed").unwrap(),
                Some(old_speed.to_string())
            );
            assert_eq!(
                store.meta("podcast_auto_next").unwrap().as_deref(),
                Some(if old_auto_next { "on" } else { "off" })
            );
            engine
                .podcast_action(Action::Queue {
                    action: QueueAction::Play { auto_next: false },
                })
                .unwrap();
            assert_eq!(engine.status().speed, speed);
            assert_eq!(engine.playback.auto_next, auto_next);
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                engine.tick().unwrap();
                let commands =
                    std::fs::read_to_string(mpv.with_extension("commands")).unwrap_or_default();
                let speed_sent = commands
                    .lines()
                    .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                    .any(|value| {
                        value["command"] == serde_json::json!(["set_property", "speed", speed])
                    });
                if speed_sent && engine.status().playback_state == PlaybackState::Playing {
                    break;
                }
                assert!(Instant::now() < deadline, "{:?}", engine.status());
                std::thread::sleep(Duration::from_millis(5));
            }
            engine.play("https://example.test/live").unwrap();
            assert_eq!(engine.status().speed, 1.);
            engine.play_episode(&id, false, Some(2.)).unwrap();
            assert_eq!(engine.status().speed, 2.);
            assert!(engine.podcast_speed(f64::NAN).is_err());
            engine.play_episode(&id, false, None).unwrap();
            assert_eq!(engine.status().speed, speed);
            engine
                .podcast_action(Action::Queue {
                    action: QueueAction::Play { auto_next: true },
                })
                .unwrap();
            assert!(engine.playback.auto_next);
            connection.execute_batch("ROLLBACK").unwrap();
            engine.shutdown().unwrap();
            let lock = Arc::downgrade(&engine.session._lock);
            drop(engine);
            // Cancelled player startups finish their cleanup on detached threads.
            let deadline = Instant::now() + Duration::from_secs(3);
            while lock.strong_count() > 0 {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
            let mut restored = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
            restored.play_episode(&id, false, None).unwrap();
            assert_eq!(restored.status().speed, speed);
            assert_eq!(restored.playback.auto_next, auto_next);
            restored.shutdown().unwrap();
        }
    }

    #[test]
    fn podcast_marks_apply_before_the_background_write_finishes() {
        use crate::podcast::{Action, Episode, Feed, Preview};
        for active in [false, true] {
            for state in ["unplayed", "played"] {
                let dir = tempfile::tempdir().unwrap();
                let mut config = crate::test_support::config(dir.path());
                config.settings.mpv = crate::test_support::fake_mpv(dir.path())
                    .to_string_lossy()
                    .into_owned();
                let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
                let preview = Preview {
                    feed: Feed {
                        url: "https://example.test/feed".into(),
                        ..Default::default()
                    },
                    episodes: vec![Episode {
                        url: "https://example.test/audio".into(),
                        ..Default::default()
                    }],
                };
                let feed = engine
                    .store()
                    .podcast_subscribe(&preview, &preview.feed.url)
                    .unwrap();
                let id = engine
                    .store()
                    .podcast_episodes(&feed.id)
                    .unwrap()
                    .remove(0)
                    .id;
                engine
                    .store()
                    .podcast_progress(&id, 15000, Some(20000), false)
                    .unwrap();
                engine.enable_background_media().unwrap();
                if active {
                    engine.play_episode(&id, false, None).unwrap();
                }
                let connection = rusqlite::Connection::open(dir.path().join("radio.db")).unwrap();
                connection.execute_batch("BEGIN IMMEDIATE").unwrap();
                engine
                    .podcast_action(Action::Mark {
                        id: id.clone(),
                        state: state.into(),
                    })
                    .unwrap();
                let stored = engine.store().podcast_episode(&id).unwrap();
                assert_eq!(stored.position_ms, 15000);
                assert!(!stored.completed);
                engine.play_episode(&id, false, None).unwrap();
                let playing = engine.status().episode.as_ref().unwrap();
                assert_eq!(playing.position_ms, 0, "{state}, active={active}");
                assert!(!playing.completed);
                connection.execute_batch("ROLLBACK").unwrap();
                engine.shutdown().unwrap();
                assert_eq!(engine.store().podcast_episode(&id).unwrap().position_ms, 0);
            }
        }
    }

    #[test]
    fn replay_preserves_uncommitted_progress_for_unchanged_media() {
        use crate::podcast::{Episode, Feed, Preview};
        let dir = tempfile::tempdir().unwrap();
        let mut config = crate::test_support::config(dir.path());
        config.settings.mpv = crate::test_support::fake_mpv(dir.path())
            .to_string_lossy()
            .into_owned();
        let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
        let preview = Preview {
            feed: Feed {
                url: "https://example.test/feed".into(),
                ..Default::default()
            },
            episodes: vec![Episode {
                url: "https://example.test/audio".into(),
                duration_ms: Some(20000),
                ..Default::default()
            }],
        };
        let feed = engine
            .store()
            .podcast_subscribe(&preview, &preview.feed.url)
            .unwrap();
        let mut episode = engine.store().podcast_episodes(&feed.id).unwrap().remove(0);
        let id = episode.id.clone();
        engine.enable_background_media().unwrap();
        let connection = rusqlite::Connection::open(dir.path().join("radio.db")).unwrap();
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        episode.position_ms = 8000;
        engine.playback.episode = Some(episode);
        engine
            .playback
            .save_episode(&mut engine.session, false)
            .unwrap();
        engine.playback.episode = None;
        assert_eq!(engine.store().podcast_episode(&id).unwrap().position_ms, 0);
        engine.play_episode(&id, false, None).unwrap();
        assert_eq!(engine.status().episode.as_ref().unwrap().position_ms, 8000);
        connection.execute_batch("ROLLBACK").unwrap();
        engine.shutdown().unwrap();
        assert_eq!(
            engine.store().podcast_episode(&id).unwrap().position_ms,
            8000
        );
    }

    #[test]
    fn download_removal_checks_the_active_input_instead_of_the_saved_episode() {
        use crate::podcast::{self, Action, Episode, Feed, Preview};
        for streaming in [false, true] {
            for background in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let mut config = crate::test_support::config(dir.path());
                config.settings.mpv = crate::test_support::fake_mpv(dir.path())
                    .to_string_lossy()
                    .into_owned();
                let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
                if background {
                    engine.enable_background_media().unwrap();
                }
                let p = Preview {
                    feed: Feed {
                        url: "https://example.test/rss".into(),
                        ..Default::default()
                    },
                    episodes: vec![Episode {
                        guid: Some("episode".into()),
                        url: "https://example.test/audio".into(),
                        ..Default::default()
                    }],
                };
                let feed = engine.store().podcast_subscribe(&p, &p.feed.url).unwrap();
                let episode = engine.store().podcast_episodes(&feed.id).unwrap().remove(0);
                let path = podcast::download_path(engine.config(), &episode.id);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, b"fixture").unwrap();
                let download = podcast::Download {
                    episode_id: episode.id.clone(),
                    state: "complete".into(),
                    path: path.to_string_lossy().into_owned(),
                    bytes: 7,
                    ..Default::default()
                };
                // An online player must not reserve a download completed later.
                if !streaming {
                    engine.store().podcast_save_download(&download).unwrap();
                }
                engine.play_episode(&episode.id, false, None).unwrap();
                if streaming {
                    engine.store().podcast_save_download(&download).unwrap();
                }
                assert!(engine.playback_active());
                let remove = Action::Downloads {
                    action: podcast::DownloadAction::Remove {
                        id: episode.id.clone(),
                    },
                };
                if !streaming {
                    assert!(engine.podcast_action(remove.clone()).is_err());
                    assert!(path.exists());
                    engine.stop_playback().unwrap();
                    assert!(!engine.playback_active());
                    assert_eq!(engine.status().episode.as_ref().unwrap().id, episode.id);
                }
                engine.podcast_action(remove).unwrap();
                let deadline = Instant::now() + Duration::from_secs(3);
                let result = loop {
                    if let Some(result) = engine.poll_podcast() {
                        break result.unwrap();
                    }
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(10));
                };
                assert_eq!(result["state"], "removed");
                assert!(!path.exists());
                if streaming {
                    assert!(engine.playback_active());
                }
                engine.shutdown().unwrap();
            }
        }
    }

    #[test]
    fn background_media_preserves_the_lock_budget_for_catalog_writes() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
        let station = crate::service::add_station(
            &mut engine.session.store,
            "Custom".into(),
            "https://example.test/live".into(),
            &[],
        )
        .unwrap();
        engine.enable_background_media().unwrap();
        // Every operation waits through a short publication transaction. The
        // former 20 ms owner budget failed each one while the lock was held.
        for operation in 0..5 {
            let connection = rusqlite::Connection::open(dir.path().join("radio.db")).unwrap();
            connection.execute_batch("BEGIN IMMEDIATE").unwrap();
            let release = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(100));
                connection.execute_batch("ROLLBACK").unwrap();
            });
            let result = match operation {
                0 => engine.toggle_favorite(&station.stationuuid).map(|_| ()),
                1 => engine
                    .change_station(crate::service::StationCommand::Backups {
                        uuid: station.stationuuid.clone(),
                        add: vec!["https://example.test/backup".into()],
                        remove: vec![],
                    })
                    .map(|_| ()),
                2 => engine.store().upsert(&station),
                3 => engine.store().save_recording(
                    &serde_json::from_value(serde_json::json!({
                        "id":"record", "stationuuid":station.stationuuid,
                        "station_name":station.name, "source_url_resolved":station.url,
                        "started_at":"2026-11-01T00:00:00Z", "codec":"mp3", "container":"mp3",
                        "path":"record.mp3", "size_bytes":0, "status":"recording"
                    }))
                    .unwrap(),
                ),
                4 => engine
                    .change_station(crate::service::StationCommand::Remove {
                        uuid: station.stationuuid.clone(),
                        keep_references: true,
                    })
                    .map(|_| ()),
                _ => unreachable!(),
            };
            release.join().unwrap();
            result.unwrap();
        }
        engine.shutdown().unwrap();
    }

    #[test]
    fn background_session_writes_keep_controls_responsive_under_a_write_lock() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
        let station = Station {
            stationuuid: uuid::Uuid::new_v4().to_string(),
            name: "Remembered".into(),
            active: true,
            ..Station::default()
        };
        engine.session.store.upsert(&station).unwrap();
        engine.enable_background_media().unwrap();
        let connection = rusqlite::Connection::open(dir.path().join("radio.db")).unwrap();
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        let started = Instant::now();
        for _ in 0..10 {
            engine.volume(-1).unwrap();
        }
        engine.save_preference("pref_source", "favorites").unwrap();
        engine.session.remember_play(&station.stationuuid).unwrap();
        engine.stop_playback().unwrap();
        engine.tick().unwrap();
        assert!(started.elapsed() < Duration::from_millis(250));
        assert_eq!(engine.session.status.volume, 70);
        // Let the writer hit contention before sending the newer value.
        std::thread::sleep(Duration::from_millis(150));
        engine.volume(-5).unwrap();
        connection.execute_batch("ROLLBACK").unwrap();
        // Closing flushes the latest value even before the debounce expires.
        engine.shutdown().unwrap();
        assert_eq!(
            engine.session.store.meta("pref_volume").unwrap().as_deref(),
            Some("65")
        );
        assert_eq!(
            engine.session.store.meta("pref_source").unwrap().as_deref(),
            Some("favorites")
        );
        assert_eq!(
            engine
                .session
                .store
                .meta("pref_last_station")
                .unwrap()
                .as_deref(),
            Some(station.stationuuid.as_str())
        );
        assert_eq!(
            engine
                .session
                .store
                .search_count(&Filter {
                    history: true,
                    ..Filter::default()
                })
                .unwrap(),
            1
        );
    }

    #[test]
    fn background_session_write_failures_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
        engine
            .session
            .store
            .test_connection()
            .execute_batch(
                "CREATE TRIGGER deny_preferences BEFORE INSERT ON meta
             BEGIN SELECT RAISE(ABORT,'preference denied'); END;",
            )
            .unwrap();
        engine.enable_background_media().unwrap();
        engine.volume(-1).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Err(error) = engine.tick() {
                assert!(format!("{error:#}").contains("preference denied"));
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(engine.shutdown().is_err());
        assert!(engine.session.persistence.is_none());
        engine.shutdown().unwrap();
    }

    #[test]
    fn shutdown_drains_finalization_after_a_session_write_failure() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
        engine
            .store()
            .test_connection()
            .execute_batch(
                "CREATE TRIGGER deny_preferences BEFORE INSERT ON meta
             BEGIN SELECT RAISE(ABORT,'preference denied'); END;",
            )
            .unwrap();
        engine.enable_background_media().unwrap();
        engine.volume(-1).unwrap();
        let (completed, receive) = std::sync::mpsc::channel();
        engine.recording.finalizing = Some(
            Background::start("test-finalization", move |_| {
                std::thread::sleep(Duration::from_millis(150));
                completed.send(()).unwrap();
                bail!("finalization fixture");
            })
            .unwrap(),
        );
        let error = engine.shutdown().unwrap_err();
        assert!(format!("{error:#}").contains("preference denied"));
        receive
            .try_recv()
            .expect("finalization must finish before shutdown returns");
        assert!(engine.recording.finalizing.is_none());
        assert!(engine.session.persistence.is_none());
        assert!(matches!(
            engine.status().recording_state,
            RecordingState::Failed { .. }
        ));
        engine.shutdown().unwrap();
    }

    #[test]
    fn shutdown_stops_pending_writes_before_reporting_a_flush_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
        engine.enable_background_media().unwrap();
        let connection = rusqlite::Connection::open(dir.path().join("radio.db")).unwrap();
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        engine.volume(-1).unwrap();
        let (cancelled, receive) = std::sync::mpsc::channel();
        engine.recording.finalizing = Some(
            Background::start("test-finalization", move |cancel| {
                while !cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                cancelled.send(()).unwrap();
                bail!("cancelled finalization fixture");
            })
            .unwrap(),
        );
        assert!(
            engine
                .shutdown_with_budget(Duration::from_millis(150))
                .is_err()
        );
        assert!(engine.session.persistence.is_none());
        assert!(engine.recording.finalizing.is_none());
        receive.recv_timeout(Duration::from_secs(1)).unwrap();
        connection.execute_batch("ROLLBACK").unwrap();
        // The writer has settled, so releasing the lock cannot produce a late
        // successful write after the caller was told that flushing failed.
        assert!(engine.store().meta("pref_volume").unwrap().is_none());
        engine.shutdown().unwrap();
    }

    #[test]
    fn invalid_subtitle_model_reports_id_without_changing_settings() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
        let original = engine.session.config.settings.subtitles.model.clone();
        for id in ["unknown", "silero-vad", "../../model"] {
            let error = engine.set_subtitle_model(id.into()).unwrap_err();
            let error = error.downcast_ref::<DomainError>().unwrap();
            assert_eq!(error.code, ErrorCode::SubtitleModel);
            assert_eq!(engine.session.config.settings.subtitles.model, original);
        }
    }

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
            let mut config = crate::test_support::config_with_api(&data, &server.url);
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
            engine.session.store.upsert(&station).unwrap();
            engine
                .session
                .store
                .add_backup(&station.stationuuid, &format!("{}/backup", server.url))
                .unwrap();
            engine.play(&station.stationuuid).unwrap();
            engine.playback.random_filter = Some(Filter::default());
            engine.playback.session_started = true;
            let deadline = Instant::now() + Duration::from_secs(3);
            while !engine.session.playback_failed {
                engine.tick().unwrap();
                assert!(
                    Instant::now() < deadline,
                    "{}",
                    engine.session.status.message
                );
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
        engine.enable_background_media().unwrap();
        engine.play("https://example.com/live").unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !engine.session.playback_failed {
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
        engine.session.store.upsert(&station).unwrap();
        engine.session.status.station = Some(station.clone());
        engine.session.status.playback_state = PlaybackState::Playing;
        engine.playback.url = server.url.clone();
        engine.record_current(None, None).unwrap();
        let conn = rusqlite::Connection::open(dir.path().join("radio.db")).unwrap();
        conn.execute_batch("CREATE TRIGGER reject_recording_update BEFORE UPDATE ON recordings BEGIN SELECT RAISE(ABORT, 'test failure'); END;").unwrap();
        assert!(engine.play("https://example.com/audition").is_err());
        assert_eq!(
            engine.session.status.station.as_ref().unwrap().stationuuid,
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
                    engine.enable_background_media().unwrap();
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
                engine.session.store.upsert(&station).unwrap();
                engine.session.status.station = Some(station);
                engine.session.status.playback_state = PlaybackState::Playing;
                engine.session.status.title = title.clone();
                engine.playback.url = server.url.clone();
                engine
                    .record_current(Some(Duration::from_secs(1)), None)
                    .unwrap();
                engine.session.status.title = "Next track".into();
                let deadline = Instant::now() + Duration::from_secs(8);
                while engine.session.status.recording_path.is_empty() {
                    engine.tick().unwrap();
                    assert!(
                        Instant::now() < deadline,
                        "{}",
                        engine.session.status.message
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                let record = engine.session.store.recordings(1).unwrap().remove(0);
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
                    assert!(name.contains("-Station-Björk_中文-"));
                }
                if title.is_empty() {
                    assert!(name.contains("-Station-"));
                }
                while engine.session.status.recording()
                    || engine.session.status.recording_state == RecordingState::Finalizing
                {
                    engine.tick().unwrap();
                    assert!(
                        Instant::now() < deadline,
                        "{}",
                        engine.session.status.message
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                let saved = engine.session.store.recording(&record.id).unwrap();
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
        let config = crate::test_support::config(dir.path());
        let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
        for state in [
            RecordingState::Done,
            RecordingState::Failed {
                reason: "disk".into(),
            },
        ] {
            engine.session.status.recording_state = state;
            engine.session.status.recording_path = "old.mp3".into();
            engine.session.status.recording_ms = 1000;
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
            assert_eq!(engine.session.status.recording_state, RecordingState::Idle);
            assert!(engine.session.status.recording_path.is_empty());
            assert_eq!(engine.session.status.recording_ms, 0);
        }
    }

    #[test]
    fn flapping_reconnects_have_a_sliding_window_limit() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
        engine.session.status.station = Some(Station {
            custom: true,
            ..Station::default()
        });
        engine.playback.session_started = true;
        engine.playback.reconnect_window = std::iter::repeat_n(Instant::now(), 6).collect();
        engine
            .playback
            .stream_failed(&mut engine.session, &mut engine.recording, "EOF")
            .unwrap();
        assert!(engine.session.playback_failed);
        assert!(!engine.session.status.muted);
        assert_eq!(engine.playback.reconnect_attempts, 0);
        assert!(engine.playback.reconnect_window.is_empty());
        assert!(engine.playback.reconnect_due.is_none());
        let failed_state = engine.session.status.playback_state.clone();
        for _ in 0..3 {
            engine.tick().unwrap();
            assert!(engine.session.playback_failed);
            assert_eq!(engine.session.status.playback_state, failed_state);
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
        let config = crate::test_support::config(dir.path());
        let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
        engine.session.status.station = Some(Station {
            custom: true,
            name: "Test".into(),
            ..Station::default()
        });
        engine.session.status.muted = true;
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
            assert!(!engine.session.playback_failed);
        }
        engine.playback.reconnect_due = None;
        engine
            .playback
            .stream_failed(&mut engine.session, &mut engine.recording, "EOF")
            .unwrap();
        assert!(engine.session.playback_failed);
        assert!(!engine.session.status.muted);
        assert_eq!(engine.playback.reconnect_attempts, 0);
        assert!(engine.playback.reconnect_window.is_empty());
        assert!(engine.playback.reconnect_due.is_none());
        engine.playback.reconnect_attempts = 0;
        engine
            .playback
            .stream_failed(&mut engine.session, &mut engine.recording, "EOF")
            .unwrap();
        engine.session.status.muted = true;
        engine.stop_playback().unwrap();
        assert!(!engine.session.status.muted);
        assert_eq!(engine.playback.reconnect_attempts, 0);
        assert!(engine.playback.reconnect_window.is_empty());
        assert!(engine.playback.reconnect_due.is_none());
        let generation = engine.playback.generation;
        engine.tick().unwrap();
        assert_eq!(engine.playback.generation, generation);
        assert!(engine.playback.player.is_none() && engine.playback.player_start.is_none());
    }
}
