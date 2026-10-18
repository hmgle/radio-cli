use super::*;

pub(super) struct RecordingController {
    pub(super) recording_probe: Option<RecordingProbe>,
    pub(super) finalizing: Option<Background<crate::model::Recording>>,
    pub(super) recorder: Option<Recorder>,
}

pub(super) struct RecordingProbe {
    generation: u64,
    station: Station,
    url: String,
    dir: std::path::PathBuf,
    duration: Option<Duration>,
    task: Background<serde_json::Value>,
}

impl RecordingController {
    pub(super) fn toggle_recording(
        &mut self,
        session: &mut Session,
        playback: &mut PlaybackController,
    ) -> Result<()> {
        if self.recorder.is_some() || self.recording_probe.is_some() {
            return self.stop_recording(session, playback, None);
        }
        self.record_current(session, playback, None, None)
    }

    pub(super) fn record_current(
        &mut self,
        session: &mut Session,
        playback: &mut PlaybackController,
        duration: Option<Duration>,
        dir: Option<&std::path::Path>,
    ) -> Result<()> {
        if playback.local_playback {
            bail!(DomainError::new(ErrorCode::LocalRecording));
        }
        if !session.status.playing {
            bail!(DomainError::new(ErrorCode::WaitForPlayback));
        }
        let station = session
            .status
            .station
            .clone()
            .context(DomainError::new(ErrorCode::NotPlaying))?;
        if session.background_media {
            if self.recorder.is_some()
                || self.recording_probe.is_some()
                || self.finalizing.is_some()
            {
                bail!(DomainError::new(ErrorCode::RecordingBusy));
            }
            let config = session.config.clone();
            let url = playback.url.clone();
            let input = url.clone();
            let task = Background::start("radio-record-probe", move |cancel| {
                recorder::probe(&config, &input, &cancel)
            })?;
            self.recording_probe = Some(RecordingProbe {
                generation: playback.generation,
                station,
                url,
                dir: dir.unwrap_or(&session.config.recording_dir).into(),
                duration,
                task,
            });
            session.status.recording = true;
            session.status.recording_ms = 0;
            session.status.recording_path.clear();
            session.status.message = "正在后台检测录音编码…".into();
            Ok(())
        } else {
            self.start_recording(
                session,
                playback,
                station,
                playback.url.clone(),
                duration,
                dir,
            )
        }
    }

    pub(super) fn standalone_record(
        &mut self,
        session: &mut Session,
        playback: &mut PlaybackController,
        uuid: &str,
        duration: Option<Duration>,
        dir: Option<&std::path::Path>,
    ) -> Result<()> {
        if self.recorder.is_some() {
            bail!(DomainError::new(ErrorCode::RecordingBusy));
        }
        let mut station = session.store.station(uuid)?;
        let mut tried = Vec::new();
        let mut last = None;
        for refresh in [false, true] {
            if refresh {
                if station.custom {
                    break;
                }
                if let Ok(Some(updated)) = session.directory.refresh(uuid) {
                    session.store.upsert(&updated)?;
                    station = updated;
                }
            }
            for url in station.stream_candidates(&session.store.backups(uuid)?) {
                if tried.contains(&url) {
                    continue;
                }
                tried.push(url.clone());
                match recorder::probe(&session.config, &url, &session.stop).and_then(|info| {
                    let codec = info["streams"][0]["codec_name"]
                        .as_str()
                        .context(DomainError::new(ErrorCode::NoAudio))?;
                    recorder::format_for_codec(codec)?;
                    Ok(info)
                }) {
                    Ok(info) => {
                        let recorder = Recorder::start_with_info(
                            &session.config,
                            &session.store,
                            &station,
                            &url,
                            dir.unwrap_or(&session.config.recording_dir),
                            duration,
                            info,
                        )?;
                        self.attach_recorder(session, playback, recorder);
                        return Ok(());
                    }
                    Err(error) => {
                        if session.stop.load(std::sync::atomic::Ordering::Relaxed)
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

    pub(super) fn start_recording(
        &mut self,
        session: &mut Session,
        playback: &mut PlaybackController,
        station: Station,
        url: String,
        duration: Option<Duration>,
        dir: Option<&std::path::Path>,
    ) -> Result<()> {
        if self.recorder.is_some() {
            bail!(DomainError::new(ErrorCode::RecordingBusy));
        }
        let dir = dir.unwrap_or(&session.config.recording_dir);
        let recorder = Recorder::start(
            &session.config,
            &session.store,
            &station,
            &url,
            dir,
            duration,
            &session.stop,
        )?;
        self.attach_recorder(session, playback, recorder);
        Ok(())
    }

    pub(super) fn attach_recorder(
        &mut self,
        session: &mut Session,
        _playback: &mut PlaybackController,
        recorder: Recorder,
    ) {
        session.status.recording_path = recorder.record.path.clone();
        self.recorder = Some(recorder);
        session.status.recording = true;
        session.status.recording_ms = 0;
        session.status.message = "正在录音".into();
    }

    pub(super) fn recording_finished(
        &mut self,
        session: &mut Session,
        _playback: &mut PlaybackController,
        record: crate::model::Recording,
    ) {
        session.status.recording_path = record.path;
        session.status.message =
            format!("录音 {}: {}", record.status, session.status.recording_path);
        if record.status != "complete" {
            session
                .status
                .message
                .push_str(&format!(" ({})", record.error.unwrap_or_default()));
        }
    }

    pub(super) fn stop_recording(
        &mut self,
        session: &mut Session,
        playback: &mut PlaybackController,
        reason: Option<&str>,
    ) -> Result<()> {
        if self.recording_probe.take().is_some() {
            session.status.message = "录音检测已取消".into();
        }
        session.status.recording = false;
        if let Some(recorder) = self.recorder.take() {
            if session.background_media {
                let config = session.config.clone();
                let reason = reason.map(str::to_owned);
                let lock = session._lock.clone();
                self.finalizing = Some(Background::start("radio-finalize", move |cancel| {
                    let _lock = lock;
                    let store = Store::open(&config.data_dir.join("radio.db"))?;
                    recorder.finish_cancellable(&config, &store, reason.as_deref(), &cancel)
                })?);
                session.status.message = "录音正在后台收尾…".into();
            } else {
                let record = recorder.finish(&session.config, &session.store, reason)?;
                self.recording_finished(session, playback, record);
            }
        }
        Ok(())
    }

    pub(super) fn record_poll_background(
        &mut self,
        session: &mut Session,
        playback: &mut PlaybackController,
    ) -> Result<()> {
        if let Some(result) = self
            .recording_probe
            .as_ref()
            .and_then(|probe| probe.task.poll())
        {
            let probe = self.recording_probe.take().unwrap();
            session.status.recording = false;
            if probe.generation == playback.generation {
                match result.and_then(|info| {
                    Recorder::start_with_info(
                        &session.config,
                        &session.store,
                        &probe.station,
                        &probe.url,
                        &probe.dir,
                        probe.duration,
                        info,
                    )
                }) {
                    Ok(recorder) => self.attach_recorder(session, playback, recorder),
                    Err(error) => session.status.message = format!("录音启动失败: {error:#}"),
                }
            }
        }
        if let Some(result) = self.finalizing.as_ref().and_then(Background::poll) {
            self.finalizing = None;
            match result {
                Ok(record) => self.recording_finished(session, playback, record),
                Err(error) => {
                    session.status.message =
                        format!("录音收尾失败，可用 recordings recover 重试: {error:#}")
                }
            }
        }
        Ok(())
    }

    pub(super) fn recording_tick(
        &mut self,
        session: &mut Session,
        playback: &mut PlaybackController,
    ) -> Result<()> {
        if let Some(recorder) = self.recorder.as_mut() {
            let ended = recorder.poll()?;
            session.status.recording_ms = recorder.elapsed_ms();
            if ended {
                self.stop_recording(session, playback, None)?;
            }
        }
        Ok(())
    }
}
