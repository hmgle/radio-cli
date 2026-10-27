use super::models;
use crate::{
    config::Config,
    error::{DomainError, ErrorCode},
    process::Background,
};
use anyhow::{Result, bail, ensure};
use serde::Deserialize;
use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Read},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    pub epoch: u64,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub enabled: bool,
    pub revision: u64,
    pub message: String,
    pub history: Arc<VecDeque<Segment>>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            enabled: false,
            revision: 0,
            message: crate::t!("subtitle_off").into(),
            history: Arc::default(),
        }
    }
}

enum Notice {
    Message(String),
    Segment(Segment),
}

struct Task {
    job: Background<bool>,
    events: Receiver<Notice>,
}

#[derive(Default)]
pub struct Controller {
    pub snapshot: Snapshot,
    task: Option<Task>,
    retiring: Vec<Background<bool>>,
    key: Option<(u64, String)>,
    epoch: u64,
    attempts: usize,
    due: Option<Instant>,
    failed: bool,
}

impl Controller {
    fn message(&mut self, message: String) {
        if self.snapshot.message != message {
            self.snapshot.message = message;
            self.snapshot.revision += 1;
        }
    }
    pub fn clear(&mut self) {
        self.snapshot.history = Arc::default();
        self.snapshot.revision += 1;
    }
    pub fn retry(&mut self) {
        self.cancel();
        self.failed = false;
        self.attempts = 0;
        self.due = None;
    }
    pub fn cancel(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        if let Some(task) = self.task.take() {
            self.retiring.push(task.job);
        }
        // Dropping Background requests cancellation, but retain retiring work
        // until its resources are released so a new worker cannot overlap it.
        for job in &self.retiring {
            job.cancel();
        }
        self.due = None;
    }
    pub fn quiescent(&self) -> bool {
        self.task.is_none() && self.retiring.is_empty()
    }
    pub fn poll_retiring(&mut self) {
        self.retiring.retain(|task| task.poll().is_none());
    }
    pub fn active(&self) -> bool {
        self.task.is_some()
    }
    pub fn enabled(&mut self, enabled: bool) {
        if self.snapshot.enabled != enabled {
            self.snapshot.enabled = enabled;
            self.snapshot.revision += 1;
            self.retry();
        }
    }
    pub fn update(
        &mut self,
        config: &Config,
        key: (u64, String),
        playing: bool,
        paused: bool,
        local: bool,
        stop: &Arc<AtomicBool>,
    ) {
        self.poll_retiring();
        if playing && self.key.as_ref() != Some(&key) {
            // Station generation changes clear history; reconnects have their
            // own epoch even when generation and URL remain the same.
            if self.key.as_ref().is_some_and(|old| old.0 != key.0) {
                self.clear();
            }
            self.retry();
            self.key = Some(key);
        }
        if !self.snapshot.enabled || !playing || paused || local || stop.load(Ordering::Relaxed) {
            self.cancel();
            self.message(
                if !self.snapshot.enabled {
                    crate::t!("subtitle_off")
                } else if local {
                    crate::t!("subtitle_local")
                } else if paused {
                    crate::t!("subtitle_paused")
                } else {
                    crate::t!("subtitle_waiting")
                }
                .into(),
            );
            return;
        }
        // Poll completion before draining events: once a finished job is removed,
        // no final segment may remain stranded in its event receiver.
        let finished = self.task.as_ref().and_then(|t| t.job.poll());
        if let Some(task) = &self.task {
            let notices: Vec<_> = task.events.try_iter().collect();
            for notice in notices {
                match notice {
                    Notice::Message(message) => self.message(message),
                    Notice::Segment(seg) if seg.epoch == self.epoch => {
                        let history = Arc::make_mut(&mut self.snapshot.history);
                        history.push_back(seg);
                        while history.len() > 500 {
                            history.pop_front();
                        }
                        self.snapshot.revision += 1;
                    }
                    _ => {}
                }
            }
        }
        if let Some(result) = finished {
            self.task = None;
            let retry = match result {
                Ok(retry) => retry,
                Err(error) => {
                    self.message(crate::t!("subtitle_failed", "detail" => crate::redact::urls(&format!("{error:#}"))));
                    false
                }
            };
            if retry && self.attempts < 3 {
                let seconds = [1, 3, 7][self.attempts];
                self.attempts += 1;
                self.due = Some(Instant::now() + Duration::from_secs(seconds));
                self.message(crate::t!("subtitle_retrying", "attempt" => self.attempts));
            } else {
                self.failed = true;
                if retry {
                    self.message(
                        crate::t!("subtitle_failed", "detail" => crate::t!("subtitle_connection")),
                    );
                }
            }
        }
        if self.task.is_none()
            && self.retiring.is_empty()
            && !self.failed
            && self.due.is_none_or(|d| Instant::now() >= d)
        {
            self.due = None;
            self.epoch = self.epoch.wrapping_add(1);
            self.message(crate::t!("subtitle_loading").into());
            let (sender, events) = mpsc::sync_channel(64);
            let config = config.clone();
            let url = self.key.as_ref().unwrap().1.clone();
            let epoch = self.epoch;
            match Background::start("radio-speech", move |cancel| {
                run(&config, &url, epoch, &cancel, sender)
            }) {
                Ok(job) => self.task = Some(Task { job, events }),
                Err(error) => {
                    self.failed = true;
                    self.message(format!("{error:#}"));
                }
            }
        }
    }
}

struct OwnedChildren {
    worker: Child,
    ffmpeg: Option<Child>,
    _lock: models::ResourceLock,
}
impl Drop for OwnedChildren {
    fn drop(&mut self) {
        if let Some(child) = &mut self.ffmpeg {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = self.worker.kill();
        let _ = self.worker.wait();
    }
}

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
    lag_ms: u64,
    #[serde(default)]
    text: String,
}

fn send(sender: &SyncSender<Notice>, notice: Notice) {
    let _ = sender.try_send(notice);
}

fn run(
    config: &Config,
    url: &str,
    epoch: u64,
    cancel: &Arc<AtomicBool>,
    sender: SyncSender<Notice>,
) -> Result<bool> {
    let lock = models::lock(config, false)?;
    models::runtime_check(config, cancel)?;
    models::verify(config, &config.settings.subtitles.model, cancel)?;
    ensure!(
        !cancel.load(Ordering::Relaxed),
        DomainError::new(ErrorCode::Cancelled)
    );
    let settings = &config.settings.subtitles;
    let args = serde_json::json!({"epoch":epoch,"model":settings.model,"model_dir":models::model_dir(config, &settings.model),"vad":models::model_dir(config,"silero-vad").join("silero_vad.onnx"),"language":settings.language,"threads":settings.effective_threads(),"max_segment_secs":settings.max_segment_secs});
    let mut command = Command::new(models::python(config));
    command
        .args([
            "-I",
            "-u",
            "-c",
            include_str!("worker.py"),
            &args.to_string(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    run_processes(config, url, epoch, cancel, sender, command, lock)
}

fn run_processes(
    config: &Config,
    url: &str,
    epoch: u64,
    cancel: &Arc<AtomicBool>,
    sender: SyncSender<Notice>,
    command: Command,
    lock: models::ResourceLock,
) -> Result<bool> {
    let worker = crate::process::spawn_owned(command)?;
    let mut children = OwnedChildren {
        worker,
        ffmpeg: None,
        _lock: lock,
    };
    let stderr = crate::process::drain(children.worker.stderr.take().unwrap(), 16384);
    let stdout = children.worker.stdout.take().unwrap();
    let (events, receiver) = mpsc::sync_channel(64);
    let reader = thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut line = Vec::new();
            let n = reader.by_ref().take(65537).read_until(b'\n', &mut line);
            match n {
                Ok(0) => break,
                Ok(n) if n <= 65536 => {
                    let parsed = serde_json::from_slice::<Event>(&line).map_err(|e| e.to_string());
                    if events.send(parsed).is_err() {
                        break;
                    }
                }
                _ => {
                    let _ = events.send(Err("invalid speech protocol line".into()));
                    break;
                }
            }
        }
    });
    let started = Instant::now();
    let mut ready = false;
    let mut decoding = None::<Instant>;
    let mut pump = None;
    let mut ffmpeg_stderr = None;
    let bytes = Arc::new(AtomicU64::new(0));
    let mut last_bytes = 0;
    let mut last_audio = Instant::now();
    let result = (|| -> Result<bool> {
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Ok(false);
            }
            ensure!(
                ready || started.elapsed() < Duration::from_secs(90),
                "speech model loading timed out"
            );
            ensure!(
                decoding.is_none_or(|t| t.elapsed() < Duration::from_secs(60)),
                "speech inference timed out"
            );
            let incoming = match receiver.recv_timeout(Duration::from_millis(25)) {
                Ok(event) => Some(event),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("speech worker closed its output; run radio subtitles doctor")
                }
            };
            if let Some(event) = incoming {
                let event = event.map_err(anyhow::Error::msg)?;
                ensure!(
                    event.v == 1 && event.epoch == epoch,
                    "speech protocol version or epoch mismatch"
                );
                match event.kind.as_str() {
                    "ready" if !ready => {
                        ready = true;
                        send(
                            &sender,
                            Notice::Message(crate::t!("subtitle_listening").into()),
                        );
                        let mut command = Command::new(&config.settings.ffmpeg);
                        command
                            .args([
                                "-hide_banner",
                                "-nostdin",
                                "-loglevel",
                                "error",
                                "-rw_timeout",
                                &(config.settings.request_timeout_secs * 1_000_000).to_string(),
                                "-user_agent",
                                crate::USER_AGENT,
                                "-readrate",
                                "1",
                                "-i",
                                url,
                                "-map",
                                "0:a:0",
                                "-vn",
                                "-ac",
                                "1",
                                "-ar",
                                "16000",
                                "-f",
                                "f32le",
                                "pipe:1",
                            ])
                            .stdin(Stdio::null())
                            .stdout(Stdio::piped())
                            .stderr(Stdio::piped());
                        let child = crate::process::spawn_owned(command)?;
                        children.ffmpeg = Some(child);
                        let ffmpeg = children.ffmpeg.as_mut().unwrap();
                        ffmpeg_stderr =
                            Some(crate::process::drain(ffmpeg.stderr.take().unwrap(), 16384));
                        let mut input = ffmpeg.stdout.take().unwrap();
                        let mut output = children.worker.stdin.take().unwrap();
                        let bytes = bytes.clone();
                        pump = Some(thread::spawn(move || {
                            use std::io::Write;
                            let mut buf = [0; 8192];
                            while let Ok(n) = input.read(&mut buf) {
                                if n == 0 || output.write_all(&buf[..n]).is_err() {
                                    break;
                                }
                                bytes.fetch_add(n as u64, Ordering::Relaxed);
                            }
                        }));
                        last_audio = Instant::now();
                    }
                    "decoding" => {
                        decoding = Some(Instant::now());
                        send(
                            &sender,
                            Notice::Message(crate::t!("subtitle_decoding").into()),
                        );
                    }
                    "listening" => {
                        decoding = None;
                        send(
                            &sender,
                            Notice::Message(crate::t!("subtitle_listening").into()),
                        );
                    }
                    "segment" => {
                        ensure!(
                            event.end_ms >= event.start_ms
                                && event.end_ms - event.start_ms <= 20000,
                            "invalid speech segment time range"
                        );
                        if event.lag_ms <= 30000 {
                            let text = crate::clean_text(&event.text);
                            if !text.is_empty() {
                                send(
                                    &sender,
                                    Notice::Segment(Segment {
                                        epoch,
                                        start_ms: event.start_ms,
                                        end_ms: event.end_ms,
                                        text,
                                    }),
                                );
                            }
                        }
                    }
                    "gap" => send(&sender, Notice::Message(crate::t!("subtitle_gap").into())),
                    "done" => return Ok(true),
                    _ => bail!("unexpected speech protocol event"),
                }
            }
            let now_bytes = bytes.load(Ordering::Relaxed);
            if now_bytes != last_bytes {
                last_bytes = now_bytes;
                last_audio = Instant::now();
            }
            if ready
                && last_audio.elapsed() >= Duration::from_secs(config.settings.request_timeout_secs)
            {
                return Ok(true);
            }
        }
    })();
    drop(children); // Close pipes before joining their readers/writers.
    drop(receiver);
    let _ = reader.join();
    if let Some(pump) = pump {
        let _ = pump.join();
    }
    let diagnostics = stderr.join().unwrap_or_default();
    if let Some(stderr) = ffmpeg_stderr {
        let _ = stderr.join();
    }
    if !diagnostics.is_empty() {
        return result.map_err(|e| {
            e.context(crate::clean_text(&crate::redact::urls(
                &String::from_utf8_lossy(&diagnostics),
            )))
        });
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Server, response};

    #[test]
    #[ignore = "requires explicitly installed models and RADIO_TEST_SPEECH_DATA/WAV"]
    fn real_models_transcribe_a_local_http_fixture() {
        let data =
            std::env::var_os("RADIO_TEST_SPEECH_DATA").expect("isolated speech data directory");
        let wav = std::fs::read(
            std::env::var_os("RADIO_TEST_SPEECH_WAV").expect("local speech WAV fixture"),
        )
        .unwrap();
        let server = Server::new(move |_, stream| response(stream, 200, "audio/wav", &wav));
        let dir = tempfile::tempdir().unwrap();
        let mut config = crate::test_support::config(dir.path());
        config.data_dir = data.into();
        for model in ["sensevoice-small-yue-int8", "qwen3-asr-0.6b-int8"] {
            config.settings.subtitles.model = model.into();
            let (sender, events) = mpsc::sync_channel(64);
            let start = Instant::now();
            assert!(
                run(
                    &config,
                    &server.url,
                    1,
                    &Arc::new(AtomicBool::new(false)),
                    sender
                )
                .unwrap()
            );
            let mut count = 0;
            for notice in events.try_iter() {
                if let Notice::Segment(segment) = notice {
                    eprintln!(
                        "{model} {}-{} ms: {}",
                        segment.start_ms, segment.end_ms, segment.text
                    );
                    count += 1;
                }
            }
            assert!(count > 0, "{model} produced no transcription");
            eprintln!(
                "{model}: {count} segments, {:.2}s including realtime audio",
                start.elapsed().as_secs_f32()
            );
        }
    }

    fn fake_command(script: &str) -> Command {
        let mut command = Command::new("sh");
        command
            .args(["-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    #[test]
    fn pcm_reaches_worker_and_protocol_segments_are_sanitized() {
        let audio = crate::test_support::fixture("pcm_s16le", "wav");
        let server = Server::new(move |_, stream| response(stream, 200, "audio/wav", &audio));
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let (sender, events) = mpsc::sync_channel(64);
        let command = fake_command(
            "printf '%s\n' '{\"v\":1,\"epoch\":7,\"type\":\"ready\"}'; head -c 4096 >/dev/null; printf '%s\n' '{\"v\":1,\"epoch\":7,\"type\":\"segment\",\"start_ms\":0,\"end_ms\":64,\"text\":\"hello\\u001b world\"}' '{\"v\":1,\"epoch\":7,\"type\":\"done\"}'; cat >/dev/null",
        );
        assert!(
            run_processes(
                &config,
                &server.url,
                7,
                &Arc::new(AtomicBool::new(false)),
                sender,
                command,
                models::lock(&config, false).unwrap()
            )
            .unwrap()
        );
        let segments: Vec<_> = events
            .try_iter()
            .filter_map(|n| {
                if let Notice::Segment(s) = n {
                    Some(s)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].text, "hello world");
        assert!(models::lock(&config, true).is_ok());
    }

    #[test]
    fn hls_and_icy_sources_feed_the_same_pcm_worker_protocol() {
        use std::io::Write;
        let source = tempfile::tempdir().unwrap();
        assert!(
            Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:duration=4",
                    "-c:a",
                    "aac",
                    "-f",
                    "hls",
                    "-hls_time",
                    "1",
                    "-hls_list_size",
                    "0"
                ])
                .arg(source.path().join("index.m3u8"))
                .status()
                .unwrap()
                .success()
        );
        let root = source.path().to_path_buf();
        let hls = Server::new(move |request, stream| {
            let path = request
                .split_whitespace()
                .nth(1)
                .unwrap()
                .trim_start_matches('/');
            if path.contains("..") {
                response(stream, 404, "text/plain", b"");
                return;
            }
            let data = std::fs::read(root.join(path)).unwrap();
            response(
                stream,
                200,
                if path.ends_with("m3u8") {
                    "application/vnd.apple.mpegurl"
                } else {
                    "video/mp2t"
                },
                &data,
            );
        });
        let mp3 = crate::test_support::fixture("libmp3lame", "mp3");
        let icy = Server::new(move |request, stream| {
            assert!(request.to_ascii_lowercase().contains("icy-metadata: 1"));
            if stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nicy-metaint: 1024\r\nConnection: close\r\n\r\n").is_err() { return; }
            for chunk in mp3.chunks(1024) {
                if stream.write_all(chunk).is_err() {
                    break;
                }
                if chunk.len() == 1024 && stream.write_all(&[0]).is_err() {
                    break;
                }
            }
        });
        for url in [format!("{}/index.m3u8", hls.url), icy.url.clone()] {
            let dir = tempfile::tempdir().unwrap();
            let config = crate::test_support::config(dir.path());
            let (sender, events) = mpsc::sync_channel(64);
            let command = fake_command(
                "printf '%s\n' '{\"v\":1,\"epoch\":7,\"type\":\"ready\"}'; head -c 4096 >/dev/null; printf '%s\n' '{\"v\":1,\"epoch\":7,\"type\":\"segment\",\"start_ms\":0,\"end_ms\":64,\"text\":\"audio received\"}' '{\"v\":1,\"epoch\":7,\"type\":\"done\"}'; cat >/dev/null",
            );
            run_processes(
                &config,
                &url,
                7,
                &Arc::new(AtomicBool::new(false)),
                sender,
                command,
                models::lock(&config, false).unwrap(),
            )
            .unwrap();
            assert!(
                events.try_iter().any(|n| matches!(n, Notice::Segment(_))),
                "{url}"
            );
        }
    }

    #[test]
    fn wrong_epoch_and_oversized_protocol_are_rejected_and_reaped() {
        for script in [
            "printf '%s\n' '{\"v\":1,\"epoch\":8,\"type\":\"ready\"}'",
            "head -c 70000 /dev/zero | tr '\\000' x",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let config = crate::test_support::config(dir.path());
            let (sender, _) = mpsc::sync_channel(64);
            assert!(
                run_processes(
                    &config,
                    "http://127.0.0.1:1",
                    7,
                    &Arc::new(AtomicBool::new(false)),
                    sender,
                    fake_command(script),
                    models::lock(&config, false).unwrap()
                )
                .is_err()
            );
            assert!(models::lock(&config, true).is_ok());
        }
    }

    #[test]
    fn cancellation_reaps_worker_while_it_waits_for_audio() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let (sender, events) = mpsc::sync_channel(64);
        let (done_tx, done) = mpsc::sync_channel(1);
        let command = fake_command(
            "printf '%s\n' '{\"v\":1,\"epoch\":7,\"type\":\"ready\"}'; exec cat >/dev/null",
        );
        thread::spawn(move || {
            let lock = models::lock(&config, false).unwrap();
            done_tx
                .send(run_processes(
                    &config,
                    "http://127.0.0.1:1",
                    7,
                    &flag,
                    sender,
                    command,
                    lock,
                ))
                .unwrap();
        });
        events.recv_timeout(Duration::from_secs(5)).unwrap();
        cancel.store(true, Ordering::Relaxed);
        assert!(!done.recv_timeout(Duration::from_secs(2)).unwrap().unwrap());
    }

    #[test]
    fn cancellation_kills_and_reaps_a_busy_decoder_without_waiting_for_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let (sender, events) = mpsc::sync_channel(64);
        let (done_tx, done) = mpsc::sync_channel(1);
        let pid_path = dir.path().join("decoder.pid");
        // This worker stays CPU busy and does not read stdin. SIGTERM is ignored;
        // the parent must explicitly kill and reap it, on Linux and macOS alike.
        let mut command = fake_command(
            "printf '%s' $$ > \"$1\"; trap '' TERM; printf '%s\n' '{\"v\":1,\"epoch\":7,\"type\":\"decoding\"}'; while :; do :; done",
        );
        command.arg("decoder").arg(&pid_path);
        let handle = thread::spawn(move || {
            let lock = models::lock(&config, false).unwrap();
            done_tx
                .send(run_processes(
                    &config,
                    "http://127.0.0.1:1",
                    7,
                    &flag,
                    sender,
                    command,
                    lock,
                ))
                .unwrap();
        });
        // Wait for inference to begin, rather than cancelling during startup.
        assert!(
            matches!(events.recv_timeout(Duration::from_secs(5)).unwrap(), Notice::Message(message) if message == crate::t!("subtitle_decoding"))
        );
        let pid: libc::pid_t = std::fs::read_to_string(pid_path).unwrap().parse().unwrap();
        let started = Instant::now();
        cancel.store(true, Ordering::Relaxed);
        assert!(!done.recv_timeout(Duration::from_secs(2)).unwrap().unwrap());
        assert!(started.elapsed() < Duration::from_secs(2));
        handle.join().unwrap();
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[test]
    fn pause_and_same_url_restart_discard_old_epoch_results() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let stop = Arc::new(AtomicBool::new(false));
        let mut controller = Controller::default();
        controller.enabled(true);
        controller.key = Some((1, "http://example.com/live".into()));
        let (sender, events) = mpsc::sync_channel(64);
        let job = Background::start("fake-speech", |cancel| {
            while !cancel.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(5));
            }
            Ok(false)
        })
        .unwrap();
        controller.task = Some(Task { job, events });
        sender
            .send(Notice::Segment(Segment {
                epoch: controller.epoch,
                start_ms: 0,
                end_ms: 500,
                text: "current".into(),
            }))
            .ok();
        sender
            .send(Notice::Segment(Segment {
                epoch: controller.epoch.wrapping_sub(1),
                start_ms: 0,
                end_ms: 500,
                text: "stale".into(),
            }))
            .ok();
        let key = controller.key.clone().unwrap();
        controller.update(&config, key.clone(), true, false, false, &stop);
        assert_eq!(controller.snapshot.history.len(), 1);
        let epoch = controller.epoch;
        controller.update(&config, key.clone(), false, true, false, &stop);
        assert!(controller.epoch != epoch && controller.task.is_none());
        assert_eq!(controller.snapshot.history.len(), 1);
        controller.update(&config, (2, key.1), true, false, false, &stop);
        assert!(controller.snapshot.history.is_empty());
        controller.enabled(false);
    }
}
