//! A warm recognizer for bounded windows, with timestamps supplied by the caller.
use super::{Segment, live::worker_command_mode, models};
use crate::{
    config::Config,
    error::{DomainError, ErrorCode},
};
use anyhow::{Result, bail, ensure};
use serde::Deserialize;
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Deserialize)]
struct Event {
    v: u8,
    epoch: u64,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    start_ms: u64,
    #[serde(default)]
    end_ms: u64,
    #[serde(default)]
    text: String,
}

pub(crate) struct ChunkRecognizer {
    child: Child,
    events: Option<Receiver<std::result::Result<Event, String>>>,
    reader: Option<JoinHandle<()>>,
    stderr: Option<JoinHandle<Vec<u8>>>,
    _lock: models::ResourceLock,
}

impl Drop for ChunkRecognizer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.events.take();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        if let Some(stderr) = self.stderr.take() {
            let _ = stderr.join();
        }
    }
}

impl ChunkRecognizer {
    pub fn start(config: &Config, stop: &Arc<AtomicBool>) -> Result<Self> {
        let lock = models::lock(config, false)?;
        models::runtime_check(config, stop)?;
        models::VerificationCache::default().verify(
            config,
            &config.settings.subtitles.model,
            stop,
        )?;
        Self::spawn(worker_command_mode(config, 0, true, true), lock, stop)
    }

    fn spawn(command: Command, lock: models::ResourceLock, stop: &AtomicBool) -> Result<Self> {
        let mut child = crate::process::spawn_owned(command)?;
        let stderr = crate::process::drain(child.stderr.take().unwrap(), 16384);
        let stdout = child.stdout.take().unwrap();
        let (sender, events) = mpsc::sync_channel(64);
        let reader = thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = vec![];
                match reader.by_ref().take(65537).read_until(b'\n', &mut line) {
                    Ok(0) => break,
                    Ok(n) if n <= 65536 => {
                        if sender
                            .send(serde_json::from_slice(&line).map_err(|e| e.to_string()))
                            .is_err()
                        {
                            break;
                        }
                    }
                    _ => {
                        let _ = sender.send(Err("invalid speech protocol line".into()));
                        break;
                    }
                }
            }
        });
        let mut session = Self {
            child,
            events: Some(events),
            reader: Some(reader),
            stderr: Some(stderr),
            _lock: lock,
        };
        let event = session.event(stop, Instant::now() + Duration::from_secs(90))?;
        ensure!(event.kind == "ready", "speech worker is not ready");
        Ok(session)
    }

    fn event(&mut self, stop: &AtomicBool, deadline: Instant) -> Result<Event> {
        loop {
            ensure!(
                !stop.load(Ordering::Relaxed),
                DomainError::new(ErrorCode::Cancelled)
            );
            ensure!(Instant::now() < deadline, "speech inference timed out");
            match self
                .events
                .as_ref()
                .unwrap()
                .recv_timeout(Duration::from_millis(25))
            {
                Ok(event) => {
                    let event = event.map_err(anyhow::Error::msg)?;
                    ensure!(event.v == 1 && event.epoch == 0, "invalid speech protocol");
                    return Ok(event);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => (),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    let diagnostics = self.stderr.take().unwrap().join().unwrap_or_default();
                    bail!(
                        "speech worker closed its output: {}",
                        crate::clean_text(&crate::redact::urls(&String::from_utf8_lossy(
                            &diagnostics
                        )))
                    );
                }
            }
        }
    }

    pub fn recognize(
        &mut self,
        audio: &DecodedChunk,
        stop: &AtomicBool,
        mut emit: impl FnMut(Segment) -> Result<()>,
    ) -> Result<()> {
        let request = serde_json::json!({"path":audio.path});
        writeln!(self.child.stdin.as_mut().unwrap(), "{request}")?;
        let mut deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let event = self.event(stop, deadline)?;
            deadline = Instant::now() + Duration::from_secs(60);
            match event.kind.as_str() {
                "decoding" | "listening" => (),
                "segment" => {
                    ensure!(
                        event.end_ms > event.start_ms && event.end_ms <= audio.duration_ms + 32,
                        "invalid speech segment time range"
                    );
                    let text = crate::clean_text(&event.text);
                    if !text.trim().is_empty() && event.start_ms < audio.duration_ms {
                        emit(Segment {
                            epoch: 0,
                            start_ms: audio.start_ms + event.start_ms,
                            end_ms: audio.start_ms + event.end_ms.min(audio.duration_ms),
                            text,
                        })?;
                    }
                }
                "done" => return Ok(()),
                _ => bail!("unexpected speech protocol event"),
            }
        }
    }
}

pub(crate) struct DecodedChunk {
    path: PathBuf,
    pub start_ms: u64,
    pub duration_ms: u64,
}
impl Drop for DecodedChunk {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub(crate) fn decode_chunk(
    config: &Config,
    input: &str,
    start_ms: u64,
    duration_ms: u64,
    stop: &Arc<AtomicBool>,
) -> Result<DecodedChunk> {
    ensure!(
        duration_ms > 0 && duration_ms <= 32_000,
        "invalid audio window"
    );
    let path = config
        .runtime_dir
        .join(format!("captions-{}.pcm", uuid::Uuid::new_v4()));
    // Reserve the name; FFmpeg may only overwrite this task's own temporary file.
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    let mut audio = DecodedChunk {
        path,
        start_ms,
        duration_ms: 0,
    };
    let mut command = Command::new(&config.settings.ffmpeg);
    command.args([
        "-hide_banner",
        "-nostdin",
        "-loglevel",
        "error",
        "-xerror",
        "-y",
    ]);
    if input.starts_with("http://") || input.starts_with("https://") {
        command.args([
            "-rw_timeout",
            &(config.settings.request_timeout_secs * 1_000_000).to_string(),
            "-user_agent",
            crate::USER_AGENT,
        ]);
    }
    if start_ms > 0 {
        command.args(["-ss", &format!("{:.3}", start_ms as f64 / 1000.)]);
    }
    command
        .args([
            "-i",
            input,
            "-t",
            &format!("{:.3}", duration_ms as f64 / 1000.),
            "-map",
            "0:a:0",
            "-vn",
            "-ac",
            "1",
            "-ar",
            "16000",
            "-f",
            "f32le",
        ])
        .arg(&audio.path)
        .stdin(Stdio::null());
    let decoded = crate::process::output(
        &mut command,
        Duration::from_secs(config.settings.request_timeout_secs.max(30)),
        stop,
    );
    ensure!(
        !stop.load(Ordering::Relaxed),
        DomainError::new(ErrorCode::Cancelled)
    );
    decoded?;
    let size = fs::metadata(&audio.path)?.len();
    ensure!(
        size > 0 && size % 4 == 0 && size <= duration_ms * 64 + 2048,
        "invalid decoded audio window"
    );
    audio.duration_ms = size / 64;
    Ok(audio)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_window_finishes_while_the_rest_of_the_episode_is_unavailable() {
        use crate::test_support::Server;
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let fixture = dir.path().join("long.wav");
        crate::process::output(
            Command::new(&config.settings.ffmpeg)
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:sample_rate=16000",
                    "-t",
                    "30",
                    "-ac",
                    "1",
                    "-c:a",
                    "pcm_s16le",
                    "-f",
                    "wav",
                ])
                .arg(&fixture),
            Duration::from_secs(5),
            &Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let wav = fs::read(fixture).unwrap();
        // The fixture exposes a long body but withholds its tail until decoding
        // has returned. Reading the complete episode would deadlock this test.
        let released = Arc::new(AtomicBool::new(false));
        let release = released.clone();
        let server = Server::new(move |_, stream| {
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                wav.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&wav[..128_000.min(wav.len())]);
            let deadline = Instant::now() + Duration::from_secs(5);
            while !release.load(Ordering::Relaxed) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(5));
            }
            if Instant::now() >= deadline {
                release.store(true, Ordering::Relaxed);
            }
        });
        let result = decode_chunk(
            &config,
            &server.url,
            0,
            500,
            &Arc::new(AtomicBool::new(false)),
        );
        let returned_before_tail = !released.swap(true, Ordering::Relaxed);
        let audio = result.unwrap();
        assert_eq!(audio.duration_ms, 500);
        assert!(returned_before_tail);
    }
    fn fake_worker(config: &Config, script: &str, stop: &AtomicBool) -> ChunkRecognizer {
        let mut command = Command::new("sh");
        command
            .args(["-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        ChunkRecognizer::spawn(command, models::lock(config, false).unwrap(), stop).unwrap()
    }
    #[test]
    fn warm_worker_publishes_before_window_completion_and_keeps_media_offsets() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let stop = AtomicBool::new(false);
        let mut worker = fake_worker(
            &config,
            "printf '%s\\n' '{\"v\":1,\"epoch\":0,\"type\":\"ready\"}'; while read -r request; do printf '%s\\n' '{\"v\":1,\"epoch\":0,\"type\":\"segment\",\"start_ms\":10,\"end_ms\":100,\"text\":\"caption\"}'; read -r ack; printf '%s\\n' '{\"v\":1,\"epoch\":0,\"type\":\"done\"}'; done",
            &stop,
        );
        // The worker cannot finish a window until the caption callback runs.
        let mut seen = vec![];
        // Use a second input descriptor to acknowledge publication in the callback.
        use std::os::fd::AsFd;
        let mut ack = fs::File::from(
            worker
                .child
                .stdin
                .as_ref()
                .unwrap()
                .as_fd()
                .try_clone_to_owned()
                .unwrap(),
        );
        for start in [60_000, 15_000] {
            let audio = DecodedChunk {
                path: dir.path().join("unused"),
                start_ms: start,
                duration_ms: 100,
            };
            worker
                .recognize(&audio, &stop, |segment| {
                    seen.push(segment.start_ms);
                    ack.write_all(b"published\n")?;
                    Ok(())
                })
                .unwrap();
        }
        assert_eq!(seen, [60_010, 15_010]);
        drop(ack);
        drop(worker);
        assert!(models::lock(&config, true).is_ok());
    }

    #[test]
    fn cancelled_chunk_stops_a_waiting_worker_and_releases_resources() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let stop = AtomicBool::new(false);
        let mut worker = fake_worker(
            &config,
            "printf '%s\\n' '{\"v\":1,\"epoch\":0,\"type\":\"ready\"}'; read -r request; printf '%s\\n' '{\"v\":1,\"epoch\":0,\"type\":\"segment\",\"start_ms\":0,\"end_ms\":100,\"text\":\"caption\"}'; read -r hold",
            &stop,
        );
        let audio = DecodedChunk {
            path: dir.path().join("unused"),
            start_ms: 0,
            duration_ms: 100,
        };
        assert!(
            worker
                .recognize(&audio, &stop, |_| {
                    stop.store(true, Ordering::Relaxed);
                    Ok(())
                })
                .is_err()
        );
        drop(worker);
        assert!(models::lock(&config, true).is_ok());
    }
    #[test]
    fn windows_decode_only_the_requested_media_time_and_remove_temporary_pcm() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let path = dir.path().join("source.wav");
        fs::write(&path, crate::test_support::fixture("pcm_s16le", "wav")).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let audio = decode_chunk(&config, path.to_str().unwrap(), 50, 100, &stop).unwrap();
        assert_eq!(audio.start_ms, 50);
        assert_eq!(audio.duration_ms, 100);
        let temp = audio.path.clone();
        drop(audio);
        assert!(!temp.exists());
        assert!(decode_chunk(&config, "/does/not/exist", 0, 100, &stop).is_err());
        assert!(
            !fs::read_dir(&config.runtime_dir).unwrap().any(|p| p
                .unwrap()
                .path()
                .extension()
                .is_some_and(|s| s == "pcm"))
        );
    }
}
