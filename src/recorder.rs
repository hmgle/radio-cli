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
    final_path: PathBuf,
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

pub fn recover(store: &Store) -> Result<()> {
    for mut r in store.unfinished_recordings()? {
        r.status = "interrupted".into();
        r.stopped_at = Some(chrono::Utc::now().to_rfc3339());
        r.error = Some("上一个会话未正常结束；保留临时文件供恢复".into());
        r.size_bytes = fs::metadata(&r.path).map(|m| m.len()).unwrap_or(0);
        store.save_recording(&r)?;
        sidecar(&r)?;
    }
    Ok(())
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
            size_bytes: 0,
            status: "recording".into(),
            error: None,
        };
        let recorder = Self {
            child,
            record,
            partial,
            final_path,
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
            // hard_link is atomic and fails if the target already exists.
            fs::hard_link(&self.partial, &self.final_path)?;
            fs::remove_file(&self.partial)?;
            let _ = fs::remove_file(format!("{}.json", self.partial.display()));
            self.record.path = self.final_path.to_string_lossy().into();
        } else {
            let errors = self.errors.lock().unwrap();
            if self.record.error.is_none() {
                self.record.error = Some(crate::clean_text(reason.unwrap_or(&errors)));
            }
        }
        store.save_recording(&self.record)?;
        sidecar(&self.record)?;
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
