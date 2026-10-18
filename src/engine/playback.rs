use super::*;

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
    pub(super) history_recorded: bool,
    pub(super) local_playback: bool,
    pub(super) local_recording_id: Option<String>,
    pub(super) reconnect_attempts: usize,
    pub(super) reconnect_due: Option<Instant>,
    pub(super) started: Instant,
    pub(super) random_filter: Option<Filter>,
    pub(super) random_tried: Vec<String>,
}

impl PlaybackController {
    pub(super) fn play(
        &mut self,
        session: &mut Session,
        recording: &mut RecordingController,
        uuid: &str,
    ) -> Result<()> {
        self.random_filter = None;
        self.random_tried.clear();
        self.play_station(session, recording, session.store.station(uuid)?)
    }

    /// TUI-only opt-in restoration; CLI media commands remain explicit.
    pub(super) fn resume_last_station(
        &mut self,
        session: &mut Session,
        recording: &mut RecordingController,
    ) -> Result<()> {
        if session.config.settings.resume
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
        self.play_source(session, recording, station, candidates, false)
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
            name: format!("{} · 本地录音", record.station_name),
            custom: true,
            ..Station::default()
        };
        self.random_filter = None;
        self.play_source(
            session,
            recording,
            station,
            vec![path.to_string_lossy().into_owned()],
            true,
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
        local: bool,
    ) -> Result<()> {
        self.generation = self.generation.wrapping_add(1);
        self.refresh_task = None;
        recording.stop_recording(session, self, None)?;
        self.retire_player(session, recording);
        self.tried.clear();
        self.candidates.clear();
        self.refreshed = false;
        self.confirmed = false;
        self.history_recorded = false;
        self.local_playback = local;
        self.local_recording_id = None;
        self.reconnect_attempts = 0;
        self.reconnect_due = None;
        session.playback_failed = false;
        session.status.title.clear();
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
        session.config.settings.volume = session.status.volume;
        if session.background_media {
            let local = self.local_playback;
            let config = session.config.clone();
            let url = self.url.clone();
            self.player_start = Some((
                self.generation,
                session.status.volume,
                Background::start("radio-player-start", move |cancel| {
                    if local {
                        Player::start_local(&config, std::path::Path::new(&url), &cancel)
                    } else {
                        Player::start(&config, &url, &cancel)
                    }
                })?,
            ));
        } else {
            self.player = Some(if self.local_playback {
                Player::start_local(
                    &session.config,
                    std::path::Path::new(&self.url),
                    &session.stop,
                )?
            } else {
                Player::start(&session.config, &self.url, &session.stop)?
            });
        }
        self.started = Instant::now();
        self.confirmed = false;
        session.status.message = "连接中…".into();
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
        if self.history_recorded && self.reconnect_attempts < BACKOFF.len() {
            let delay = BACKOFF[self.reconnect_attempts];
            self.reconnect_attempts += 1;
            self.retire_player(session, recording);
            session.status.playback_state = PlaybackState::Reconnecting {
                attempt: self.reconnect_attempts,
            };
            self.reconnect_due = Some(Instant::now() + Duration::from_secs(delay));
            session.status.message = format!(
                "重连中 · {}/{} · {delay}s: {reason}",
                self.reconnect_attempts,
                BACKOFF.len()
            );
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
            session.status.message = "正在后台刷新线路…".into();
            return Ok(());
        }
        self.exhausted(session, recording, reason)
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
        session.playback_failed = true;
        session.status.playback_state = PlaybackState::Failed {
            reason: reason.into(),
        };
        session.status.message = format!("播放失败: {reason}");
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
                self.stream_failed(session, recording, &format!("{error:#}"))?;
            }
        }
        if let Some(player) = self.player.as_mut() {
            let events = player.poll();
            match events {
                Ok(events) => {
                    for event in events {
                        match event {
                            Event::Playing => {
                                session.status.playback_state = PlaybackState::Playing;
                                session.status.message = "播放中".into();
                                if !self.confirmed {
                                    self.confirmed = true;
                                    if let Some(s) = &session.status.station
                                        && !self.history_recorded
                                        && !self.local_playback
                                    {
                                        session.store.played(&s.stationuuid)?;
                                        session
                                            .store
                                            .set_meta("pref_last_station", &s.stationuuid)?;
                                        self.history_recorded = true;
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
                            Event::Title(title) => session.status.title = title,
                            Event::Paused(paused) => {
                                if self.confirmed {
                                    session.status.playback_state = if paused {
                                        PlaybackState::Paused
                                    } else {
                                        PlaybackState::Playing
                                    };
                                }
                            }
                            Event::Volume(volume) => {
                                if volume != session.status.volume {
                                    session.status.volume = volume;
                                    session.store.set_meta("pref_volume", &volume.to_string())?;
                                }
                            }
                            Event::End(reason) if reason == "redirect" => {}
                            Event::End(reason) if self.local_playback && reason == "eof" => {
                                self.stop_playback(session, recording)?;
                                session.status.message = "本地录音播放结束".into();
                                break;
                            }
                            Event::End(reason) if matches!(reason.as_str(), "stop" | "quit") => {
                                self.stop_playback(session, recording)?;
                                session.status.message = "已停止播放".into();
                                break;
                            }
                            Event::End(reason) => {
                                if matches!(reason.as_str(), "eof" | "error") {
                                    self.stream_failed(
                                        session,
                                        recording,
                                        &format!("音频流结束 ({reason})"),
                                    )?;
                                } else {
                                    self.fail(
                                        session,
                                        recording,
                                        &format!("音频流结束 ({reason})"),
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
                self.stream_failed(session, recording, "起播超时")?;
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
        self.generation = self.generation.wrapping_add(1);
        self.refresh_task = None;
        self.retire_player(session, recording);
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
                        self.stream_failed(
                            session,
                            recording,
                            &format!("播放器启动失败: {error:#}"),
                        )?;
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
