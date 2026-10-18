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
            "stream=codec_name:format=duration",
            "-of",
            "json",
            "-i",
            url,
        ]),
        Duration::from_secs(config.settings.playback_timeout_secs),
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
        _ => bail!("暂不支持录制编码 {codec}；不会自动转码"),
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
            libc::renameat2(
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
    let final_path = PathBuf::from(record.final_path.as_ref().context("缺少最终文件路径")?);
    if partial.exists() {
        publish(&partial, &final_path).with_context(|| {
            format!(
                "发布录音 {}: {} → {}",
                record.id,
                partial.display(),
                final_path.display()
            )
        })?;
    }
    let size = fs::metadata(&final_path)?.len();
    if size != record.size_bytes || size == 0 {
        bail!("最终文件大小不匹配；保留恢复状态");
    }
    let mut complete = record.clone();
    complete.path = final_path.to_string_lossy().into();
    complete.status = "complete".into();
    complete.error = None;
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
                store.save_recording(&r)?;
            }
            let retry = r
                .error
                .as_deref()
                .is_some_and(|e| e.starts_with("恢复失败:"));
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
            if let Err(db_error) = store.save_recording(&r) {
                warnings.push(format!("{message}；数据库: {db_error:#}"));
            } else {
                warnings.push(message);
            }
        }
    }
    Ok(warnings)
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
        let codec = info["streams"][0]["codec_name"]
            .as_str()
            .context("源站没有音频轨道")?
            .to_owned();
        let (extension, muxer) = format_for_codec(&codec)?;
        fs::create_dir_all(dir)?;
        let dir = fs::canonicalize(dir)?;
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
        let child_result = process::owned_child(&mut command)
            .stdin(Stdio::piped())
            .stdout(Stdio::from(file))
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match child_result {
            Ok(child) => child,
            Err(e) => {
                let _ = fs::remove_file(&partial);
                return Err(e).context("无法启动 FFmpeg");
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
        };
        let recorder = Self {
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

    pub fn finish(
        mut self,
        config: &Config,
        store: &Store,
        reason: Option<&str>,
    ) -> Result<Recording> {
        let status = self.stop_child()?;
        for handle in self.readers.drain(..) {
            let _ = handle.join();
        }
        self.duration_ms = self.progress.load(Ordering::Relaxed);
        self.record.stopped_at = Some(chrono::Utc::now().to_rfc3339());
        self.record.size_bytes = fs::metadata(&self.partial)?.len();
        let never_stop = Arc::new(AtomicBool::new(false));
        let verification = probe(config, &self.partial.to_string_lossy(), &never_stop);
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
        .context("时长格式如 30s、10m、2h")?;
    let factor = match &value[split..] {
        "" | "s" => 1,
        "m" => 60,
        "h" => 3600,
        _ => bail!("时长单位只支持 s/m/h"),
    };
    let secs = n.checked_mul(factor).context("时长溢出")?;
    if secs == 0 || secs > 365 * 24 * 3600 {
        bail!("时长必须大于零且不超过 365 天");
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
        }
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
        fs::create_dir(dir.path().join("missing")).unwrap();
        assert!(recover(&store).unwrap().is_empty());
        assert!(store.unfinished_recordings().unwrap().is_empty());
        let saved = store.recordings(1).unwrap().remove(0);
        assert_eq!(saved.status, "interrupted");
        assert!(saved.error.as_deref().unwrap().contains("音频文件缺失"));
        assert!(Path::new(&format!("{}.json", saved.path)).exists());
        assert!(recover(&store).unwrap().is_empty());
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
