use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SourceKind {
    Catalog,
    Ephemeral,
    Local,
}

pub(super) struct PlaybackController {
    pub(super) generation: u64,
    pub(super) refresh_task: Option<(u64, String, Background<Option<Station>>)>,
    pub(super) click_task: Option<Background<()>>,
    pub(super) player: Option<Player>,
    pub(super) player_start: Option<(u64, u8, Background<Player>)>,
    pub(super) retiring: Vec<Background<()>>,
    pub(super) url: String,
    pub(super) candidates: VecDeque<String>,
    pub(super) tried: Vec<String>,
    pub(super) refreshed: bool,
    pub(super) confirmed: bool,
    pub(super) session_started: bool,
    pub(super) source_kind: SourceKind,
    pub(super) local_recording_id: Option<String>,
    pub(super) reconnect_attempts: usize,
    pub(super) reconnect_due: Option<Instant>,
    pub(super) reconnect_window: VecDeque<Instant>,
    pub(super) started: Instant,
    pub(super) random_filter: Option<Filter>,
    pub(super) random_tried: Vec<String>,
    pub(super) failure: Option<DomainError>,
}

impl PlaybackController {
    pub(super) fn play(
        &mut self,
        session: &mut Session,
        recording: &mut RecordingController,
        target: &str,
    ) -> Result<()> {
        self.random_filter = None;
        self.random_tried.clear();
        if uuid::Uuid::parse_str(target).is_ok() {
            return self.play_station(session, recording, session.store.station(target)?);
        }
        crate::config::validate_url(target)?;
        let url = reqwest::Url::parse(target)?;
        let station = Station {
            stationuuid: uuid::Uuid::new_v4().to_string(),
            name: crate::clean_text(&format!("{}{}", url.host_str().unwrap_or(""), url.path()))
                .chars()
                .take(60)
                .collect(),
            url: target.into(),
            custom: true,
            active: true,
            ..Station::default()
        };
        self.play_source(
            session,
            recording,
            station,
            vec![target.into()],
            SourceKind::Ephemeral,
        )
    }

    /// TUI-only opt-in restoration; CLI media commands remain explicit.
    pub(super) fn resume_last_station(
        &mut self,
        session: &mut Session,
        recording: &mut RecordingController,
    ) -> Result<()> {
        if session.config.settings.resume == crate::config::ResumeMode::Playback
            && let Some(uuid) = session.store.meta("pref_last_station")?
            && let Ok(station) = session.store.station(&uuid)
            && !station
                .stream_candidates(&session.store.backups(&uuid)?)
                .is_empty()
        {
            self.play(session, recording, &uuid)?;
        }
        Ok(())
    }

    pub(super) fn play_station(
        &mut self,
        session: &mut Session,
        recording: &mut RecordingController,
        station: Station,
    ) -> Result<()> {
        let candidates = station.stream_candidates(&session.store.backups(&station.stationuuid)?);
        self.play_source(session, recording, station, candidates, SourceKind::Catalog)
    }

    pub(super) fn play_recording(
        &mut self,
        session: &mut Session,
        recording: &mut RecordingController,
        id: &str,
    ) -> Result<()> {
        let record = session.store.recording(id)?;
        if matches!(record.status.as_str(), "recording" | "finalizing") || record.recovery_pending {
            bail!(DomainError::new(ErrorCode::RecordingInUse));
        }
        let path = std::fs::canonicalize(&record.path)?;
        let station = Station {
            stationuuid: record.stationuuid,
            name: crate::t!("playback_local_name", "name" => record.station_name),
            custom: true,
            ..Station::default()
        };
        self.random_filter = None;
        self.play_source(
            session,
            recording,
            station,
            vec![path.to_string_lossy().into_owned()],
            SourceKind::Local,
        )?;
        self.local_recording_id = Some(id.into());
        Ok(())
    }

    pub(super) fn play_source(
        &mut self,
        session: &mut Session,
        recording: &mut RecordingController,
        station: Station,
        candidates: Vec<String>,
        source_kind: SourceKind,
    ) -> Result<()> {
        self.generation = self.generation.wrapping_add(1);
        self.refresh_task = None;
        recording.stop_recording(session, self, None)?;
        self.retire_player(session, recording);
        self.tried.clear();
        self.candidates.clear();
        self.refreshed = false;
        self.confirmed = false;
        self.session_started = false;
        self.source_kind = source_kind;
        self.local_recording_id = None;
        self.reconnect_attempts = 0;
        self.reconnect_window.clear();
        self.reconnect_due = None;
        recording.clear_finished(session);
        session.playback_failed = false;
        self.failure = None;
        session.status.title.clear();
        session.status.muted = false;
        session.status.playback_state = PlaybackState::Idle;
        session.status.station = Some(station.clone());
        self.candidates.extend(candidates);
        self.start_candidate(session, recording)
    }

    pub(super) fn start_candidate(
        &mut self,
        session: &mut Session,
        recording: &mut RecordingController,
    ) -> Result<()> {
        self.reconnect_due = None;
        self.reconnect_attempts = 0;
        self.url = self
            .candidates
            .pop_front()
            .context(DomainError::new(ErrorCode::NoStream))?;
        session.status.source_url = self.url.clone();
        self.tried.push(self.url.clone());
        self.start_url(session, recording)
    }

    pub(super) fn start_url(
        &mut self,
        session: &mut Session,
        _recording: &mut RecordingController,
    ) -> Result<()> {
        session.artwork.cancel();
        let artwork_done = session.artwork.completion();
        session.config.settings.volume = session.status.volume;
        if session.background_media {
            let local = self.source_kind == SourceKind::Local;
            let config = session.config.clone();
            let url = self.url.clone();
            self.player_start = Some((
                self.generation,
                session.status.volume,
                Background::start("radio-player-start", move |cancel| {
                    super::artwork::wait_for_handoff(
                        artwork_done.as_deref(),
                        super::artwork::handoff_deadline(&config),
                        &cancel,
                    )?;
                    if local {
                        Player::start_local(&config, std::path::Path::new(&url), &cancel)
                    } else {
                        Player::start(&config, &url, &cancel)
                    }
                })?,
            ));
        } else {
            self.player = Some(if self.source_kind == SourceKind::Local {
                Player::start_local(
                    &session.config,
                    std::path::Path::new(&self.url),
                    &session.stop,
                )?
            } else {
                Player::start(&session.config, &self.url, &session.stop)?
            });
        }
        session.status.muted = false;
        self.started = Instant::now();
        self.confirmed = false;
        session.status.message = crate::t!("playback_connecting").into();
        session.status.playback_state = if self.reconnect_attempts > 0 {
            PlaybackState::Reconnecting {
                attempt: self.reconnect_attempts,
            }
        } else {
            PlaybackState::Connecting
        };
        Ok(())
    }

    pub(super) fn stream_failed(
        &mut self,
        session: &mut Session,
        recording: &mut RecordingController,
        reason: &str,
    ) -> Result<()> {
        // Three bounded attempts span a ten-second outage even when the
        // unavailable source rejects each connection immediately.
        const BACKOFF: [u64; 3] = [1, 3, 7];
        let now = Instant::now();
        while self
            .reconnect_window
            .front()
            .is_some_and(|time| now.saturating_duration_since(*time) >= Duration::from_secs(60))
        {
            self.reconnect_window.pop_front();
        }
        // Successful restarts renew the outage budget, but a flapping stream
        // cannot restart indefinitely (at most six retries per minute).
        if self.session_started
            && self.reconnect_attempts < BACKOFF.len()
            && self.reconnect_window.len() < 6
        {
            self.reconnect_window.push_back(now);
            let delay = BACKOFF[self.reconnect_attempts];
            self.reconnect_attempts += 1;
            self.retire_player(session, recording);
            session.status.playback_state = PlaybackState::Reconnecting {
                attempt: self.reconnect_attempts,
            };
            self.reconnect_due = Some(Instant::now() + Duration::from_secs(delay));
            session.status.message = crate::t!("playback_reconnecting", "attempt" => self.reconnect_attempts,
                "total" => BACKOFF.len(), "delay" => delay, "reason" => reason);
            return Ok(());
        }
        self.fail(session, recording, reason)
    }

    pub(super) fn random(
        &mut self,
        session: &mut Session,
        recording: &mut RecordingController,
        filter: Filter,
    ) -> Result<()> {
        self.random_tried = session
            .status
            .station
            .as_ref()
            .map(|s| vec![s.stationuuid.clone()])
            .unwrap_or_default();
        self.random_filter = Some(filter);
        self.pick_random(session, recording)
    }

    pub(super) fn pick_random(
        &mut self,
        session: &mut Session,
        recording: &mut RecordingController,
    ) -> Result<()> {
        let filter = self
            .random_filter
            .as_ref()
            .context(DomainError::new(ErrorCode::RandomDisabled))?;
        let station = session
            .store
            .random(filter, &self.random_tried)?
            .context(DomainError::new(ErrorCode::NoMatchingStation))?;
        self.random_tried.push(station.stationuuid.clone());
        self.play_station(session, recording, station)
    }

    pub(super) fn fail(
        &mut self,
        session: &mut Session,
        recording: &mut RecordingController,
        reason: &str,
    ) -> Result<()> {
        self.retire_player(session, recording);
        session.status.muted = false;
        session.status.playback_state = PlaybackState::Idle;
        recording.stop_recording(session, self, Some(reason))?;
        let station = session
            .status
            .station
            .clone()
            .context(DomainError::new(ErrorCode::NotPlaying))?;
        if !self.candidates.is_empty() {
            return self.start_candidate(session, recording);
        }
        if !self.refreshed && !station.custom {
            self.refreshed = true;
            let mut config = session.config.clone();
            config.settings.request_timeout_secs = config.settings.request_timeout_secs.min(3);
            let uuid = station.stationuuid.clone();
            let task = Background::start("radio-refresh", move |cancel| {
                Directory::new(&config, cancel)?.refresh(&uuid)
            })?;
            self.refresh_task = Some((self.generation, reason.into(), task));
            session.status.message = crate::t!("playback_refreshing").into();
            return Ok(());
        }
        self.exhausted(session, recording, reason)
    }

    fn player_failed(
        &mut self,
        session: &mut Session,
        recording: &mut RecordingController,
        error: anyhow::Error,
    ) -> Result<()> {
        // Local player and device failures cannot be repaired by changing
        // stations, retrying streams, or refreshing the network directory.
        let reason = format!("{error:#}");
        self.failure = error.downcast_ref::<DomainError>().cloned();
        self.refresh_task = None;
        self.candidates.clear();
        self.random_filter = None;
        self.retire_player(session, recording);
        recording.stop_recording(session, self, Some(&reason))?;
        self.exhausted(session, recording, &reason)
    }

    pub(super) fn exhausted(
        &mut self,
        session: &mut Session,
        recording: &mut RecordingController,
        reason: &str,
    ) -> Result<()> {
        if self.random_filter.is_some()
            && self.random_tried.len() < 6
            && self.pick_random(session, recording).is_ok()
        {
            return Ok(());
        }
        self.reconnect_due = None;
        self.reconnect_attempts = 0;
        self.reconnect_window.clear();
        session.status.muted = false;
        session.playback_failed = true;
        session.status.playback_state = PlaybackState::Failed {
            reason: reason.into(),
        };
        session.status.message = crate::t!("playback_failed", "reason" => reason);
        Ok(())
    }

    pub(super) fn tick(
        &mut self,
        session: &mut Session,
        recording: &mut RecordingController,
    ) -> Result<()> {
        if self.reconnect_due.is_some_and(|due| Instant::now() >= due) {
            self.reconnect_due = None;
            if let Err(error) = self.start_url(session, recording) {
                self.player_failed(session, recording, error)?;
            }
        }
        if let Some(player) = self.player.as_mut() {
            let events = player.poll();
            match events {
                Ok(events) => {
                    for event in events {
                        match event {
                            Event::Failed(error) => {
                                self.player_failed(session, recording, error.into())?;
                                break;
                            }
                            Event::Playing => {
                                self.reconnect_attempts = 0;
                                self.reconnect_due = None;
                                session.status.playback_state = PlaybackState::Playing;
                                session.status.message = crate::t!("playback_playing").into();
                                if !self.confirmed {
                                    self.confirmed = true;
                                    if let Some(s) = &session.status.station
                                        && !self.session_started
                                        && self.source_kind != SourceKind::Local
                                    {
                                        if self.source_kind != SourceKind::Ephemeral {
                                            session.store.played(&s.stationuuid)?;
                                            session
                                                .store
                                                .set_meta("pref_last_station", &s.stationuuid)?;
                                        }
                                        self.session_started = true;
                                        if !s.custom
                                            && self
                                                .click_task
                                                .as_ref()
                                                .is_none_or(|t| t.poll().is_some())
                                        {
                                            let config = session.config.clone();
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
                            Event::Title(title) => {
                                if session.status.title != title {
                                    session.artwork.cancel();
                                    session.status.title = title;
                                }
                            }
                            Event::Paused(paused) => {
                                if self.confirmed {
                                    session.status.playback_state = if paused {
                                        PlaybackState::Paused
                                    } else {
                                        PlaybackState::Playing
                                    };
                                }
                            }
                            Event::Muted(muted) => session.status.muted = muted,
                            Event::Volume(volume) => {
                                if volume != session.status.volume {
                                    session.status.volume = volume;
                                    session.store.set_meta("pref_volume", &volume.to_string())?;
                                }
                            }
                            Event::End(reason) if reason == "redirect" => {}
                            Event::End(reason)
                                if self.source_kind == SourceKind::Local && reason == "eof" =>
                            {
                                self.stop_playback(session, recording)?;
                                session.status.message =
                                    crate::t!("playback_local_finished").into();
                                break;
                            }
                            Event::End(reason) if matches!(reason.as_str(), "stop" | "quit") => {
                                self.stop_playback(session, recording)?;
                                session.status.message = crate::t!("playback_stopped").into();
                                break;
                            }
                            Event::End(reason) => {
                                if matches!(reason.as_str(), "eof" | "error") {
                                    self.stream_failed(
                                        session,
                                        recording,
                                        &crate::t!("playback_stream_ended", "reason" => reason),
                                    )?;
                                } else {
                                    self.fail(
                                        session,
                                        recording,
                                        &crate::t!("playback_stream_ended", "reason" => reason),
                                    )?;
                                }
                                break;
                            }
                        }
                    }
                }
                Err(e) => self.stream_failed(session, recording, &e.to_string())?,
            }
            if self.player.is_some()
                && !self.confirmed
                && self.started.elapsed()
                    > Duration::from_secs(session.config.settings.playback_timeout_secs)
            {
                self.stream_failed(session, recording, crate::t!("playback_timeout"))?;
            }
        }
        Ok(())
    }

    pub(super) fn pause(
        &mut self,
        _session: &mut Session,
        _recording: &mut RecordingController,
    ) -> Result<()> {
        self.player
            .as_mut()
            .context(DomainError::new(ErrorCode::NotPlaying))?
            .toggle_pause()
    }

    pub(super) fn volume(
        &mut self,
        session: &mut Session,
        _recording: &mut RecordingController,
        delta: i32,
    ) -> Result<()> {
        let value = (i32::from(session.status.volume) + delta).clamp(0, 100) as u8;
        if let Some(player) = &mut self.player {
            player.volume(i32::from(value) - i32::from(session.status.volume))?;
        }
        session.status.volume = value;
        session.config.settings.volume = value;
        session.store.set_meta("pref_volume", &value.to_string())?;
        Ok(())
    }

    pub(super) fn retire_player(
        &mut self,
        session: &mut Session,
        _recording: &mut RecordingController,
    ) {
        session.artwork.cancel();
        if let Some((_, _, task)) = self.player_start.take() {
            // A completed task may already contain a Player. Drop it away
            // from controls so its child reaping cannot block cancellation.
            let lock = session._lock.clone();
            if let Ok(cleanup) = Background::start("radio-player-cancel", move |_| {
                let _lock = lock;
                drop(task);
                Ok(())
            }) {
                self.retiring.push(cleanup);
            }
        }
        if let Some(player) = self.player.take() {
            if session.background_media {
                let lock = session._lock.clone();
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

    pub(super) fn stop_playback(
        &mut self,
        session: &mut Session,
        recording: &mut RecordingController,
    ) -> Result<()> {
        self.reconnect_due = None;
        self.reconnect_attempts = 0;
        self.reconnect_window.clear();
        self.generation = self.generation.wrapping_add(1);
        self.refresh_task = None;
        self.retire_player(session, recording);
        session.status.muted = false;
        session.status.playback_state = PlaybackState::Idle;
        recording.stop_recording(session, self, None)
    }

    pub(super) fn poll_background(
        &mut self,
        session: &mut Session,
        recording: &mut RecordingController,
    ) -> Result<()> {
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
                        let delta = i32::from(session.status.volume) - i32::from(initial_volume);
                        if delta != 0 {
                            player.volume(delta)?;
                        }
                        self.player = Some(player);
                        self.started = Instant::now();
                    }
                    Err(error) => {
                        self.player_failed(session, recording, error)?;
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
                    session.store.upsert(&station)?;
                    self.candidates.extend(
                        station.stream_candidates(&session.store.backups(&station.stationuuid)?),
                    );
                    self.candidates.retain(|url| !self.tried.contains(url));
                }
                if self.candidates.is_empty() {
                    self.exhausted(session, recording, &reason)?;
                } else {
                    self.start_candidate(session, recording)?;
                }
            }
        }
        Ok(())
    }
}
