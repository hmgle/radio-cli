use crate::error::{DomainError, ErrorCode};
use crate::{
    USER_AGENT,
    config::{Config, validate_url},
    model::{Recording, Station},
    process,
    store::Store,
};
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub struct Recorder {
    pub space_estimated: bool,
    child: Child,
    pub record: Recording,
    partial: PathBuf,
    deadline: Option<Instant>,
    target_duration: Option<Duration>,
    ended_without_stop: bool,
    timed_out: bool,
    progress: Arc<AtomicU64>,
    duration_ms: u64,
    readers: Vec<thread::JoinHandle<()>>,
    errors: Arc<std::sync::Mutex<String>>,
    finished: bool,
    stop_requested: bool,
}

pub fn probe(config: &Config, url: &str, stop: &Arc<AtomicBool>) -> Result<Value> {
    probe_with_timeout(
        config,
        url,
        stop,
        Duration::from_secs(config.settings.probe_timeout_secs),
    )
}

fn probe_with_timeout(
    config: &Config,
    url: &str,
    stop: &Arc<AtomicBool>,
    timeout: Duration,
) -> Result<Value> {
    let data = process::output(
        Command::new(&config.settings.ffprobe).args([
            "-v",
            "error",
            "-rw_timeout",
            "5000000",
            "-analyzeduration",
            "1000000",
            "-probesize",
            "262144",
            "-user_agent",
            USER_AGENT,
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=codec_name,bit_rate:format=duration,bit_rate",
            "-of",
            "json",
            "-i",
            url,
        ]),
        timeout,
        stop,
    )?;
    Ok(serde_json::from_slice(&data)?)
}

pub fn format_for_codec(codec: &str) -> Result<(&'static str, &'static str)> {
    match codec {
        "mp3" => Ok(("mp3", "mp3")),
        "aac" => Ok(("aac", "adts")),
        "vorbis" => Ok(("ogg", "ogg")),
        "opus" => Ok(("opus", "opus")),
        _ => bail!(DomainError::new(ErrorCode::UnsupportedCodec).with("codec", codec)),
    }
}

pub fn sidecar(record: &Recording) -> Result<()> {
    let path = PathBuf::from(format!("{}.json", record.path));
    let temp = PathBuf::from(format!("{}.tmp", path.display()));
    fs::write(&temp, serde_json::to_vec_pretty(record)?)?;
    fs::rename(temp, path)?;
    Ok(())
}

// Rename without replacement works on filesystems without hard links too.
fn publish(partial: &Path, final_path: &Path) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let from = CString::new(partial.as_os_str().as_bytes())?;
        let to = CString::new(final_path.as_os_str().as_bytes())?;
        let result = unsafe {
            // musl does not expose a renameat2 wrapper on every toolchain.
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        fs::hard_link(partial, final_path)?;
        fs::remove_file(partial)?;
    }
    Ok(())
}

fn complete_finalization(store: &Store, record: &mut Recording) -> Result<()> {
    let partial = PathBuf::from(&record.path);
    let final_path = PathBuf::from(
        record
            .final_path
            .as_ref()
            .context(DomainError::new(ErrorCode::FinalPathMissing))?,
    );
    if partial.exists() {
        publish(&partial, &final_path).with_context(|| {
            DomainError::new(ErrorCode::RecordingPublish)
                .with("id", &record.id)
                .with("path", partial.display())
                .with("final_path", final_path.display())
        })?;
    }
    let size = fs::metadata(&final_path)?.len();
    if size != record.size_bytes || size == 0 {
        bail!(DomainError::new(ErrorCode::RecordingSizeMismatch));
    }
    let mut complete = record.clone();
    complete.path = final_path.to_string_lossy().into();
    complete.status = "complete".into();
    complete.error = None;
    complete.recovery_pending = false;
    // The DB remains finalizing until the final sidecar is safely written.
    sidecar(&complete)?;
    store.save_recording(&complete)?;
    let _ = fs::remove_file(format!("{}.json", partial.display()));
    *record = complete;
    Ok(())
}

pub fn recover(store: &Store) -> Result<Vec<String>> {
    let mut warnings = Vec::new();
    for mut r in store.unfinished_recordings()? {
        let result = (|| -> Result<()> {
            if r.status == "finalizing" {
                return complete_finalization(store, &mut r);
            }
            if r.status == "recording" {
                r.status = "interrupted".into();
                r.stopped_at = Some(chrono::Utc::now().to_rfc3339());
                r.error = Some("上一个会话未正常结束；保留临时文件供恢复".into());
                r.size_bytes = fs::metadata(&r.path).map(|m| m.len()).unwrap_or(0);
                r.recovery_pending = true;
                store.save_recording(&r)?;
            }
            let retry = r.recovery_pending;
            if retry {
                r.error = Some(
                    if Path::new(&r.path).exists() {
                        "上一个会话未正常结束；临时文件保留，元数据已恢复"
                    } else {
                        "上一个会话未正常结束；音频文件缺失，元数据已恢复"
                    }
                    .into(),
                );
            }
            r.recovery_pending = false;
            sidecar(&r)?;
            if retry {
                store.save_recording(&r)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            let message = format!(
                "恢复失败: 录音 {} ({})：{error:#}；使用 recordings recover 重试",
                r.id, r.path
            );
            r.error = Some(message.clone());
            r.recovery_pending = true;
            if let Err(db_error) = store.save_recording(&r) {
                warnings.push(format!("{message}；数据库: {db_error:#}"));
            } else {
                warnings.push(message);
            }
        }
    }
    Ok(warnings)
}

// An unbounded recording or unknown bitrate has no finite total estimate.
// Preserve the reserve check and make that limitation visible to the caller.
fn required_space(
    reserve_mb: u64,
    station_kbps: u32,
    info: &Value,
    duration: Option<Duration>,
) -> (u64, bool) {
    let positive = |v: &Value| {
        v.as_str()
            .and_then(|v| v.parse::<u64>().ok())
            .or_else(|| v.as_u64())
            .filter(|v| *v > 0)
    };
    let bitrate = positive(&info["streams"][0]["bit_rate"])
        .or_else(|| positive(&info["format"]["bit_rate"]))
        .or_else(|| (station_kbps > 0).then_some(u64::from(station_kbps) * 1000));
    let reserve = reserve_mb.saturating_mul(1024 * 1024);
    match (duration, bitrate) {
        (Some(duration), Some(bitrate)) => {
            let bytes = u128::from(bitrate) * duration.as_nanos();
            let bytes = bytes.div_ceil(8_000_000_000).min(u128::from(u64::MAX)) as u64;
            (reserve.saturating_add(bytes), true)
        }
        (None, _) | (_, None) => (reserve, false),
    }
}

impl Recorder {
    pub fn start(
        config: &Config,
        store: &Store,
        station: &Station,
        url: &str,
        dir: &Path,
        duration: Option<Duration>,
        stop: &Arc<AtomicBool>,
    ) -> Result<Self> {
        validate_url(url)?;
        let info = probe(config, url, stop)?;
        Self::start_with_info(config, store, station, url, dir, duration, info)
    }

    pub fn start_with_info(
        config: &Config,
        store: &Store,
        station: &Station,
        url: &str,
        dir: &Path,
        duration: Option<Duration>,
        info: Value,
    ) -> Result<Self> {
        validate_url(url)?;
        let codec = info["streams"][0]["codec_name"]
            .as_str()
            .context(DomainError::new(ErrorCode::NoAudio))?
            .to_owned();
        let (extension, muxer) = format_for_codec(&codec)?;
        fs::create_dir_all(dir)?;
        let dir = fs::canonicalize(dir)?;
        let (required, space_estimated) = required_space(
            config.settings.recording_min_free_mb,
            station.bitrate,
            &info,
            duration,
        );
        let available = fs2::available_space(&dir)?;
        if available < required {
            bail!(
                DomainError::new(ErrorCode::InsufficientSpace)
                    .with("available", available)
                    .with("required", required)
            );
        }
        let id = uuid::Uuid::new_v4().to_string();
        let name: String = station
            .name
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
            .take(60)
            .collect();
        let base = format!(
            "{}-{}-{}",
            chrono::Local::now().format("%Y%m%d-%H%M%S"),
            name,
            &id[..8]
        );
        let final_path = dir.join(format!("{base}.{extension}"));
        let partial = dir.join(format!("{base}.{extension}.part"));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial)?;
        let mut command = Command::new(&config.settings.ffmpeg);
        command.args([
            "-hide_banner",
            "-loglevel",
            "warning",
            "-nostats",
            "-rw_timeout",
            "12000000",
            "-analyzeduration",
            "1000000",
            "-probesize",
            "262144",
            "-user_agent",
            USER_AGENT,
            "-i",
            url,
            "-map",
            "0:a:0",
            "-c:a",
            "copy",
        ]);
        if let Some(duration) = duration {
            command.arg("-t").arg(duration.as_secs_f64().to_string());
        }
        command.args(["-progress", "pipe:2", "-f", muxer, "pipe:1"]);
        // An exclusively created file is passed as stdout; no shell or unsafe FD setup.
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::from(file))
            .stderr(Stdio::piped());
        let child_result = process::spawn_owned(command);
        let mut child = match child_result {
            Ok(child) => child,
            Err(e) => {
                let _ = fs::remove_file(&partial);
                return Err(e).context(
                    DomainError::new(ErrorCode::MissingDependency).with("program", "ffmpeg"),
                );
            }
        };
        let progress = Arc::new(AtomicU64::new(0));
        let latest = progress.clone();
        let errors = Arc::new(std::sync::Mutex::new(String::new()));
        let target = errors.clone();
        let stderr = child.stderr.take().unwrap();
        let reader = thread::spawn(move || {
            let mut input = BufReader::new(stderr);
            let mut line = String::new();
            loop {
                line.clear();
                match input.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    _ => {}
                }
                if let Some(value) = line.trim().strip_prefix("out_time_us=") {
                    if let Ok(value) = value.parse::<u64>() {
                        latest.fetch_max(value / 1000, Ordering::Relaxed);
                    }
                } else if !line.contains('=') {
                    let mut text = target.lock().unwrap();
                    if text.len() < 8192 {
                        text.push_str(&line);
                    }
                }
            }
        });
        let record = Recording {
            id,
            stationuuid: station.stationuuid.clone(),
            station_name: station.name.clone(),
            source_url_resolved: url.into(),
            started_at: chrono::Utc::now().to_rfc3339(),
            stopped_at: None,
            media_duration_ms: None,
            codec,
            container: muxer.into(),
            path: partial.to_string_lossy().into(),
            final_path: Some(final_path.to_string_lossy().into()),
            size_bytes: 0,
            status: "recording".into(),
            error: None,
            recovery_pending: false,
        };
        let recorder = Self {
            space_estimated,
            child,
            record,
            partial,
            deadline: duration.map(|d| {
                Instant::now() + d + Duration::from_secs(config.settings.playback_timeout_secs)
            }),
            target_duration: duration,
            ended_without_stop: false,
            timed_out: false,
            progress,
            duration_ms: 0,
            readers: vec![reader],
            errors,
            finished: false,
            stop_requested: false,
        };
        store.save_recording(&recorder.record)?;
        sidecar(&recorder.record)?;
        Ok(recorder)
    }

    pub fn poll(&mut self) -> Result<bool> {
        self.duration_ms = self.progress.load(Ordering::Relaxed);
        self.ended_without_stop = self.child.try_wait()?.is_some();
        self.timed_out =
            !self.ended_without_stop && self.deadline.is_some_and(|d| Instant::now() >= d);
        Ok(self.ended_without_stop || self.timed_out)
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.duration_ms
    }

    fn stop_child(&mut self) -> Result<std::process::ExitStatus> {
        if let Some(status) = self.child.try_wait()? {
            return Ok(status);
        }
        // FFmpeg only reads "q" from a terminal, never from a piped stdin;
        // SIGINT is its supported graceful stop and still finalizes the file.
        self.stop_requested = true;
        unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGINT) };
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                self.child.kill()?;
                return Ok(self.child.wait()?);
            }
            thread::sleep(Duration::from_millis(30));
        }
    }

    pub fn finish(self, config: &Config, store: &Store, reason: Option<&str>) -> Result<Recording> {
        self.finish_cancellable(config, store, reason, &Arc::new(AtomicBool::new(false)))
    }

    pub fn finish_cancellable(
        mut self,
        config: &Config,
        store: &Store,
        reason: Option<&str>,
        stop: &Arc<AtomicBool>,
    ) -> Result<Recording> {
        let status = self.stop_child()?;
        for handle in self.readers.drain(..) {
            let _ = handle.join();
        }
        self.duration_ms = self.progress.load(Ordering::Relaxed);
        self.record.stopped_at = Some(chrono::Utc::now().to_rfc3339());
        self.record.size_bytes = fs::metadata(&self.partial)?.len();
        let verification = probe_with_timeout(
            config,
            &self.partial.to_string_lossy(),
            stop,
            Duration::from_secs(2),
        );
        if let Ok(info) = &verification {
            self.record.media_duration_ms = info["format"]["duration"]
                .as_str()
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|d| d.is_finite() && *d >= 0.)
                .map(|d| (d * 1000.) as u64);
        }
        let premature = self.ended_without_stop
            && self.target_duration.is_none_or(|d| {
                self.record
                    .media_duration_ms
                    .unwrap_or(self.duration_ms)
                    .saturating_add(100)
                    < d.as_millis() as u64
            });
        let reason = reason.or(if self.timed_out {
            Some("录音未在时限内完成")
        } else if premature && status.success() {
            Some("直播源提前结束")
        } else {
            None
        });
        // FFmpeg exits 255 after finalizing on our SIGINT; a stop we asked for
        // is not a failure as long as the output verifies below.
        let graceful = status.success() || self.stop_requested;
        self.record.status = if graceful && reason.is_none() && self.record.size_bytes > 0 {
            "complete"
        } else if reason.is_some() {
            "interrupted"
        } else {
            "failed"
        }
        .into();
        if self.record.status == "complete"
            && verification
                .as_ref()
                .ok()
                .and_then(|info| info["streams"][0]["codec_name"].as_str())
                != Some(self.record.codec.as_str())
        {
            self.record.status = "failed".into();
            self.record.error = Some("录音文件未通过音频轨道验证；保留临时文件".into());
        }
        // Progress remains useful when a native live container has no duration field.
        if self.record.media_duration_ms.is_none() && self.duration_ms > 0 {
            self.record.media_duration_ms = Some(self.duration_ms);
        }
        if self.record.status == "complete" {
            // Persist intent before moving the only audio file. A failed DB
            // write leaves the original recording and its .part file intact.
            self.record.status = "finalizing".into();
            store.save_recording(&self.record)?;
            sidecar(&self.record)?;
            if let Err(error) = complete_finalization(store, &mut self.record) {
                self.record.recovery_pending = true;
                self.record.error = Some(format!(
                    "录音 {} ({}) 收尾待重试: {error:#}",
                    self.record.id, self.record.path
                ));
                let _ = store.save_recording(&self.record);
                let _ = sidecar(&self.record);
                return Err(error).context(self.record.error.clone().unwrap());
            }
        } else {
            let errors = self.errors.lock().unwrap();
            if self.record.error.is_none() {
                self.record.error = Some(crate::clean_text(reason.unwrap_or(&errors)));
            }
        }
        if self.record.status != "complete" {
            store.save_recording(&self.record)?;
            sidecar(&self.record)?;
        }
        self.finished = true;
        Ok(self.record.clone())
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.stop_child();
        }
        for handle in self.readers.drain(..) {
            let _ = handle.join();
        }
    }
}

pub fn parse_duration(value: &str) -> Result<Duration> {
    let split = value
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(value.len());
    let n = value[..split]
        .parse::<u64>()
        .context(DomainError::new(ErrorCode::InvalidDuration))?;
    let factor = match &value[split..] {
        "" | "s" => 1,
        "m" => 60,
        "h" => 3600,
        _ => bail!(DomainError::new(ErrorCode::DurationUnit)),
    };
    let secs = n
        .checked_mul(factor)
        .context(DomainError::new(ErrorCode::DurationOverflow))?;
    if secs == 0 || secs > 365 * 24 * 3600 {
        bail!(DomainError::new(ErrorCode::DurationRange));
    }
    Ok(Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(dir: &Path) -> Recording {
        let partial = dir.join("test.mp3.part");
        fs::write(&partial, b"verified audio").unwrap();
        Recording {
            id: uuid::Uuid::new_v4().to_string(),
            stationuuid: uuid::Uuid::new_v4().to_string(),
            station_name: "test".into(),
            source_url_resolved: "https://example.com/audio".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
            stopped_at: Some("2026-01-01T00:00:01Z".into()),
            media_duration_ms: Some(1000),
            codec: "mp3".into(),
            container: "mp3".into(),
            path: partial.to_string_lossy().into(),
            final_path: Some(dir.join("test.mp3").to_string_lossy().into()),
            size_bytes: 14,
            status: "finalizing".into(),
            error: None,
            recovery_pending: false,
        }
    }

    #[test]
    fn space_estimate_prefers_audio_bitrate_and_reports_unknown_totals() {
        let reserve = 16 * 1024 * 1024;
        let mut info =
            serde_json::json!({"streams":[{"bit_rate":"128000"}], "format":{"bit_rate":"256000"}});
        assert_eq!(
            required_space(16, 320, &info, Some(Duration::from_millis(1500))),
            (reserve + 24000, true)
        );
        assert_eq!(required_space(16, 320, &info, None), (reserve, false));
        info["streams"][0]["bit_rate"] = "0".into();
        assert_eq!(
            required_space(16, 320, &info, Some(Duration::from_secs(1))),
            (reserve + 32000, true)
        );
        assert_eq!(
            required_space(16, 0, &Value::Null, Some(Duration::from_secs(1))),
            (reserve, false)
        );
        assert_eq!(
            required_space(16, 128, &Value::Null, Some(Duration::from_secs(1))),
            (reserve + 16000, true)
        );
    }

    #[test]
    fn recovery_repairs_each_publication_boundary_idempotently() {
        for phase in 0..4 {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open(&dir.path().join("radio.db")).unwrap();
            let mut r = pending(dir.path());
            store.save_recording(&r).unwrap();
            sidecar(&r).unwrap();
            if phase >= 1 {
                publish(
                    Path::new(&r.path),
                    Path::new(r.final_path.as_ref().unwrap()),
                )
                .unwrap();
            }
            if phase >= 2 {
                let mut complete = r.clone();
                complete.path = complete.final_path.clone().unwrap();
                complete.status = "complete".into();
                sidecar(&complete).unwrap();
            }
            if phase == 3 {
                // A blocked DB write after publication must leave recoverable intent.
                let lock = rusqlite::Connection::open(dir.path().join("radio.db")).unwrap();
                lock.execute_batch("BEGIN IMMEDIATE").unwrap();
                assert!(complete_finalization(&store, &mut r).is_err());
                lock.execute_batch("ROLLBACK").unwrap();
            }
            recover(&store).unwrap();
            recover(&store).unwrap();
            let saved = store.recordings(1).unwrap().remove(0);
            assert_eq!(saved.status, "complete");
            assert_eq!(fs::read(&saved.path).unwrap(), b"verified audio");
            assert_eq!(
                serde_json::from_slice::<Recording>(
                    &fs::read(format!("{}.json", saved.path)).unwrap()
                )
                .unwrap()
                .id,
                saved.id
            );
            assert!(!Path::new(&r.path).exists());
        }
    }

    #[test]
    fn successful_sidecar_retry_clears_recovery_pending_state() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("radio.db")).unwrap();
        let mut r = pending(dir.path());
        r.status = "recording".into();
        r.path = dir.path().join("missing/old.part").to_string_lossy().into();
        store.save_recording(&r).unwrap();
        assert_eq!(recover(&store).unwrap().len(), 1);
        assert_eq!(store.unfinished_recordings().unwrap().len(), 1);
        let mut retry = store.recordings(1).unwrap().remove(0);
        assert!(retry.recovery_pending);
        retry.error = Some("diagnostic wording has changed".into());
        store.save_recording(&retry).unwrap();
        assert_eq!(store.unfinished_recordings().unwrap().len(), 1);
        fs::create_dir(dir.path().join("missing")).unwrap();
        assert!(recover(&store).unwrap().is_empty());
        assert!(store.unfinished_recordings().unwrap().is_empty());
        let saved = store.recordings(1).unwrap().remove(0);
        assert_eq!(saved.status, "interrupted");
        assert!(!saved.recovery_pending);
        assert!(saved.error.as_deref().unwrap().contains("音频文件缺失"));
        assert!(Path::new(&format!("{}.json", saved.path)).exists());
        assert!(recover(&store).unwrap().is_empty());
    }

    #[test]
    fn recovery_selection_uses_the_flag_instead_of_error_text() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("radio.db")).unwrap();
        let mut record = pending(dir.path());
        record.status = "interrupted".into();
        record.error = Some("恢复失败: diagnostic text alone".into());
        store.save_recording(&record).unwrap();
        assert!(store.unfinished_recordings().unwrap().is_empty());
        record.recovery_pending = true;
        record.error = None;
        store.save_recording(&record).unwrap();
        assert_eq!(store.unfinished_recordings().unwrap().len(), 1);
        assert!(recover(&store).unwrap().is_empty());
        assert!(store.unfinished_recordings().unwrap().is_empty());
    }

    #[test]
    fn version_one_database_migrates_legacy_recovery_state_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.db");
        let mut record = pending(dir.path());
        record.status = "interrupted".into();
        record.error = Some("恢复失败: old-format sidecar failure".into());
        let mut json = serde_json::to_value(&record).unwrap();
        json.as_object_mut().unwrap().remove("recovery_pending");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE recordings (id TEXT PRIMARY KEY,status TEXT NOT NULL,json TEXT NOT NULL); PRAGMA user_version=1;").unwrap();
        conn.execute(
            "INSERT INTO recordings VALUES (?1,?2,?3)",
            rusqlite::params![record.id, record.status, json.to_string()],
        )
        .unwrap();
        drop(conn);
        let store = Store::open(&path).unwrap();
        assert!(store.unfinished_recordings().unwrap()[0].recovery_pending);
        assert!(recover(&store).unwrap().is_empty());
        assert!(!store.recordings(1).unwrap()[0].recovery_pending);
        drop(store);
        let store = Store::open(&path).unwrap();
        assert!(store.unfinished_recordings().unwrap().is_empty());
        let conn = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(
            conn.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
                .unwrap(),
            2
        );
    }

    #[test]
    fn publication_never_overwrites_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("radio.db")).unwrap();
        let mut r = pending(dir.path());
        store.save_recording(&r).unwrap();
        fs::write(r.final_path.as_ref().unwrap(), b"other recording").unwrap();
        assert!(complete_finalization(&store, &mut r).is_err());
        assert_eq!(
            fs::read(r.final_path.as_ref().unwrap()).unwrap(),
            b"other recording"
        );
        assert_eq!(fs::read(&r.path).unwrap(), b"verified audio");
        fs::remove_file(r.final_path.as_ref().unwrap()).unwrap();
        recover(&store).unwrap();
        assert_eq!(store.recordings(1).unwrap()[0].status, "complete");
    }
}
