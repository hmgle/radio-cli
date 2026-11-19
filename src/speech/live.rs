use super::models;
use crate::{
    config::Config,
    error::{DomainError, ErrorCode},
    process::Background,
};
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    collections::VecDeque,
    io::{BufRead, BufReader, Read},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

const STABLE_SUBTITLE_PERIOD: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    pub epoch: u64,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub enabled: bool,
    pub failed: bool,
    pub revision: u64,
    pub message: String,
    pub history: Arc<VecDeque<Segment>>,
    pub timed: bool,
    pub timeline: Option<(u64, String)>,
    pub current: Vec<usize>,
    pub anchor: Option<usize>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            enabled: false,
            failed: false,
            revision: 0,
            message: crate::t!("subtitle_off").into(),
            history: Arc::default(),
            timed: false,
            timeline: None,
            current: vec![],
            anchor: None,
        }
    }
}

impl Snapshot {
    fn timeline(&mut self, generation: u64, source: &str) {
        if self
            .timeline
            .as_ref()
            .is_none_or(|(old, id)| *old != generation || id != source)
        {
            self.timeline = Some((generation, source.into()));
            self.revision += 1;
        }
    }
    fn seek(&mut self, position: u64, index: &mut IntervalIndex) {
        let end = self.history.partition_point(|s| s.start_ms <= position);
        let current = index.query(&self.history, end, position);
        let anchor = end.checked_sub(1);
        if self.current != current || self.anchor != anchor {
            self.current = current;
            self.anchor = anchor;
            self.revision += 1;
        }
    }
}

/// Max-end interval tree over cues sorted by start time. Long overlapping cues
/// stay discoverable without scanning every earlier cue on each player tick.
#[derive(Default)]
struct IntervalIndex {
    history: std::sync::Weak<VecDeque<Segment>>,
    max_end: Vec<u64>,
    leaves: usize,
    len: usize,
    #[cfg(test)]
    visits: usize,
    #[cfg(test)]
    rebuilds: usize,
}
impl IntervalIndex {
    fn query(
        &mut self,
        history: &Arc<VecDeque<Segment>>,
        upper: usize,
        position: u64,
    ) -> Vec<usize> {
        let key = Arc::downgrade(history);
        if !std::sync::Weak::ptr_eq(&self.history, &key) || self.len != history.len() {
            self.history = key;
            self.len = history.len();
            self.leaves = self.len.next_power_of_two();
            self.max_end = vec![0; self.leaves * 2];
            for (i, cue) in history.iter().enumerate() {
                self.max_end[self.leaves + i] = cue.end_ms;
            }
            for i in (1..self.leaves).rev() {
                self.max_end[i] = self.max_end[i * 2].max(self.max_end[i * 2 + 1]);
            }
            #[cfg(test)]
            {
                self.rebuilds += 1;
            }
        }
        #[cfg(test)]
        {
            self.visits = 0;
        }
        let mut found = vec![];
        if !history.is_empty() {
            self.collect(1, 0, self.leaves, upper, position, &mut found);
        }
        found
    }
    fn collect(
        &mut self,
        node: usize,
        from: usize,
        to: usize,
        upper: usize,
        position: u64,
        found: &mut Vec<usize>,
    ) {
        #[cfg(test)]
        {
            self.visits += 1;
        }
        if from >= upper || self.max_end[node] <= position {
            return;
        }
        if to - from == 1 {
            found.push(from);
            return;
        }
        let mid = from + (to - from) / 2;
        self.collect(node * 2, from, mid, upper, position, found);
        self.collect(node * 2 + 1, mid, to, upper, position, found);
    }
}

enum Notice {
    Message(String),
    Segment(Segment),
}

struct Task {
    job: Background<bool>,
    events: Receiver<Notice>,
    first_segment_at: Option<Instant>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StatusMessage {
    Off,
    Local,
    Paused,
    Waiting,
    Loading,
}

impl StatusMessage {
    fn text(self, locale: crate::messages::Locale) -> &'static str {
        match self {
            Self::Off => crate::messages::text_in(locale, "subtitle_off"),
            Self::Local => crate::messages::text_in(locale, "subtitle_local"),
            Self::Paused => crate::messages::text_in(locale, "subtitle_paused"),
            Self::Waiting => crate::messages::text_in(locale, "subtitle_waiting"),
            Self::Loading => crate::messages::text_in(locale, "subtitle_loading"),
        }
    }
}

struct EpisodeTask {
    key: (u64, crate::podcast::EpisodeMedia),
    job: Option<Background<Option<Vec<Segment>>>>,
    incremental: bool,
}

#[derive(Default)]
pub struct Controller {
    pub snapshot: Snapshot,
    task: Option<Task>,
    episode: Option<EpisodeTask>,
    retiring: Vec<Background<bool>>,
    key: Option<(u64, String)>,
    epoch: u64,
    attempts: usize,
    due: Option<Instant>,
    status_key: Option<(StatusMessage, crate::messages::Locale)>,
    verification: Option<Arc<Mutex<models::VerificationCache>>>,
    intervals: IntervalIndex,
}

impl Controller {
    fn message(&mut self, message: impl Into<Cow<'static, str>>) {
        self.status_key = None;
        let message = message.into();
        if self.snapshot.message != message {
            self.snapshot.message = message.into_owned();
            self.snapshot.revision += 1;
        }
    }
    fn status(&mut self, key: StatusMessage) {
        let locale = crate::messages::locale();
        if self.status_key != Some((key, locale)) {
            self.message(key.text(locale));
            self.status_key = Some((key, locale));
        }
    }
    pub fn clear(&mut self) {
        if let Some(episode) = &mut self.episode {
            episode.incremental = false;
        }
        if !self.snapshot.history.is_empty() {
            self.snapshot.history = Arc::default();
            self.snapshot.current.clear();
            self.snapshot.anchor = None;
            self.snapshot.revision += 1;
        }
    }
    pub fn retry(&mut self) {
        self.cancel();
        if self.snapshot.failed {
            self.snapshot.failed = false;
            self.snapshot.revision += 1;
        }
        self.attempts = 0;
        self.due = None;
    }
    pub fn cancel(&mut self) {
        self.episode = None;
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
    pub fn update_episode(
        &mut self,
        config: &Config,
        generation: u64,
        episode: &crate::podcast::Episode,
        playing: bool,
    ) {
        self.poll_retiring();
        if !self.snapshot.enabled {
            self.status(StatusMessage::Off);
            return;
        }
        self.snapshot.timeline(generation, &episode.id);
        if !self.snapshot.timed {
            self.cancel();
            self.clear();
            self.snapshot.timed = true;
            self.snapshot.revision += 1;
        }
        let key = (generation, episode.media());
        if self.episode.as_ref().is_none_or(|e| e.key != key) && playing {
            self.clear();
            self.snapshot.failed = false;
            self.message(crate::t!("podcast_subtitle_loading"));
            let config = config.clone();
            let episode = episode.clone();
            let job = Background::start("radio-episode-subtitles", move |stop| {
                crate::podcast::transcript::load(&config, &episode, &stop)
            });
            match job {
                Ok(job) => {
                    self.episode = Some(EpisodeTask {
                        key,
                        job: Some(job),
                        incremental: false,
                    })
                }
                Err(e) => {
                    self.snapshot.failed = true;
                    self.message(format!("{e:#}"));
                }
            }
        }
        if let Some(result) = self
            .episode
            .as_ref()
            .and_then(|e| e.job.as_ref())
            .and_then(|j| j.poll())
        {
            self.episode.as_mut().unwrap().job = None;
            match result {
                Ok(Some(segments)) => {
                    self.snapshot.history = Arc::new(segments.into());
                    self.message(crate::t!("podcast_subtitle_synced"));
                }
                Ok(None) => self.message(crate::t!("podcast_subtitle_missing")),
                Err(e) => {
                    self.snapshot.failed = true;
                    self.message(crate::t!("podcast_subtitle_failed", "detail" => crate::redact::urls(&format!("{e:#}"))));
                }
            }
            self.snapshot.revision += 1;
        }
        self.snapshot.seek(episode.position_ms, &mut self.intervals);
    }

    pub(crate) fn publish_episode(
        &mut self,
        generation: u64,
        episode: &crate::podcast::Episode,
        update: crate::podcast::progressive::CaptionUpdate,
    ) {
        if !self.snapshot.enabled {
            return;
        }
        self.snapshot.timeline(generation, &episode.id);
        // Supersede a slow feed fetch so it cannot replace an incremental result.
        self.episode = Some(EpisodeTask {
            key: (generation, episode.media()),
            job: None,
            incremental: true,
        });
        self.snapshot.timed = true;
        self.snapshot.failed = false;
        if update.reset {
            let mut segments = update.segments;
            segments.sort_by_key(|s| (s.start_ms, s.end_ms));
            self.snapshot.history = Arc::new(segments.into());
        } else if !update.segments.is_empty() {
            let history = Arc::make_mut(&mut self.snapshot.history);
            for segment in update.segments {
                let key = (segment.start_ms, segment.end_ms);
                let at = history.partition_point(|s| (s.start_ms, s.end_ms) <= key);
                history.insert(at, segment);
            }
        }
        self.snapshot.revision += 1;
        self.message(crate::t!("podcast_subtitle_generating"));
        self.snapshot.seek(episode.position_ms, &mut self.intervals);
    }

    pub(crate) fn incremental_episode(
        &self,
        generation: u64,
        episode: &crate::podcast::Episode,
    ) -> bool {
        self.episode
            .as_ref()
            .is_some_and(|task| task.key == (generation, episode.media()) && task.incremental)
    }

    pub fn update(
        &mut self,
        config: &Config,
        key: (u64, &str),
        playing: bool,
        paused: bool,
        local: bool,
        stop: &Arc<AtomicBool>,
    ) {
        self.poll_retiring();
        if playing {
            self.snapshot.timeline(key.0, key.1);
        }
        if self.snapshot.timed {
            self.cancel();
            self.clear();
            self.snapshot.timed = false;
            self.snapshot.revision += 1;
        }
        if !self.snapshot.enabled {
            if playing && self.key.as_ref().is_some_and(|old| old.0 != key.0) {
                self.clear();
                self.key = None;
            }
            self.status(StatusMessage::Off);
            return;
        }
        if playing
            && self
                .key
                .as_ref()
                .map(|(generation, url)| (*generation, url.as_str()))
                != Some(key)
        {
            // Station generation changes clear history; reconnects have their
            // own epoch even when generation and URL remain the same.
            if self.key.as_ref().is_some_and(|old| old.0 != key.0) {
                self.clear();
            }
            self.retry();
            self.key = Some((key.0, key.1.to_owned()));
        }
        if !playing || paused || local || stop.load(Ordering::Relaxed) {
            self.cancel();
            self.status(if local {
                StatusMessage::Local
            } else if paused {
                StatusMessage::Paused
            } else {
                StatusMessage::Waiting
            });
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
                        let first = self
                            .task
                            .as_mut()
                            .unwrap()
                            .first_segment_at
                            .get_or_insert_with(Instant::now);
                        if first.elapsed() >= STABLE_SUBTITLE_PERIOD {
                            self.attempts = 0;
                        }
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
                self.snapshot.failed = true;
                self.snapshot.revision += 1;
                if retry {
                    self.message(
                        crate::t!("subtitle_failed", "detail" => crate::t!("subtitle_connection")),
                    );
                }
            }
        }
        if self.task.is_none()
            && self.retiring.is_empty()
            && !self.snapshot.failed
            && self.due.is_none_or(|d| Instant::now() >= d)
        {
            self.due = None;
            self.epoch = self.epoch.wrapping_add(1);
            self.status(StatusMessage::Loading);
            let (sender, events) = mpsc::sync_channel(64);
            let config = config.clone();
            let url = self.key.as_ref().unwrap().1.clone();
            let epoch = self.epoch;
            let verification = self
                .verification
                .get_or_insert_with(Default::default)
                .clone();
            match Background::start("radio-speech", move |cancel| {
                run(&config, &url, epoch, &cancel, sender, &verification)
            }) {
                Ok(job) => {
                    self.task = Some(Task {
                        job,
                        events,
                        first_segment_at: None,
                    })
                }
                Err(error) => {
                    self.snapshot.failed = true;
                    self.snapshot.revision += 1;
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
    verification: &Mutex<models::VerificationCache>,
) -> Result<bool> {
    let lock = models::lock(config, false)?;
    models::runtime_check(config, cancel)?;
    verification
        .lock()
        .unwrap()
        .verify(config, &config.settings.subtitles.model, cancel)?;
    ensure!(
        !cancel.load(Ordering::Relaxed),
        DomainError::new(ErrorCode::Cancelled)
    );
    let command = worker_command(config, epoch, false);
    run_processes(config, url, epoch, cancel, sender, command, lock)
}

fn worker_command(config: &Config, epoch: u64, offline: bool) -> Command {
    worker_command_mode(config, epoch, offline, false)
}

pub(super) fn worker_command_mode(
    config: &Config,
    epoch: u64,
    offline: bool,
    chunks: bool,
) -> Command {
    let settings = &config.settings.subtitles;
    let args = serde_json::json!({"epoch":epoch,"offline":offline,"chunks":chunks,"model":settings.model,"model_dir":models::model_dir(config, &settings.model),"vad":models::model_dir(config,"silero-vad").join("silero_vad.onnx"),"language":settings.language,"threads":settings.effective_threads(),"max_segment_secs":settings.max_segment_secs.min(if chunks { 8 } else { 20 })});
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
    command
}

pub(crate) fn transcribe(
    config: &Config,
    file: &std::path::Path,
    stop: &Arc<AtomicBool>,
    progress: Option<&AtomicU64>,
) -> Result<Vec<Segment>> {
    let lock = models::lock(config, false)?;
    models::runtime_check(config, stop)?;
    models::VerificationCache::default().verify(config, &config.settings.subtitles.model, stop)?;
    let command = worker_command(config, 0, true);
    let (sender, _events) = mpsc::sync_channel(1);
    let mut segments = Vec::new();
    run_processes_inner(
        config,
        (
            file.to_str()
                .ok_or_else(|| anyhow::anyhow!("invalid audio path"))?,
            0,
        ),
        stop,
        sender,
        command,
        lock,
        Some(FileOutput {
            segments: &mut segments,
            progress,
        }),
    )?;
    ensure!(
        !stop.load(Ordering::Relaxed),
        DomainError::new(ErrorCode::Cancelled)
    );
    Ok(segments)
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
    run_processes_inner(config, (url, epoch), cancel, sender, command, lock, None)
}

struct FileOutput<'a> {
    segments: &'a mut Vec<Segment>,
    progress: Option<&'a AtomicU64>,
}

fn run_processes_inner(
    config: &Config,
    input: (&str, u64),
    cancel: &Arc<AtomicBool>,
    sender: SyncSender<Notice>,
    command: Command,
    lock: models::ResourceLock,
    mut output: Option<FileOutput<'_>>,
) -> Result<bool> {
    let (url, epoch) = input;
    let offline = output.is_some();
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
    let mut last_event = Instant::now();
    let mut reported_ms = 0;
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
                last_event = Instant::now();
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
                        command.args(["-hide_banner", "-nostdin", "-loglevel", "error"]);
                        if !offline {
                            command.args([
                                "-rw_timeout",
                                &(config.settings.request_timeout_secs * 1_000_000).to_string(),
                                "-user_agent",
                                crate::USER_AGENT,
                                "-readrate",
                                "1",
                            ]);
                        }
                        command
                            .args([
                                "-i", url, "-map", "0:a:0", "-vn", "-ac", "1", "-ar", "16000",
                                "-f", "f32le", "pipe:1",
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
                                let segment = Segment {
                                    epoch,
                                    start_ms: event.start_ms,
                                    end_ms: event.end_ms,
                                    text,
                                };
                                if let Some(file) = &mut output {
                                    ensure!(
                                        file.segments.len() < 100_000,
                                        "too many speech segments"
                                    );
                                    if let Some(progress) = file.progress {
                                        progress.store(segment.end_ms, Ordering::Relaxed);
                                    } else if segment.end_ms.saturating_sub(reported_ms) >= 30000 {
                                        reported_ms = segment.end_ms;
                                        eprintln!(
                                            "{}",
                                            crate::t!("podcast_subtitle_progress", "seconds" => reported_ms / 1000)
                                        );
                                    }
                                    file.segments.push(segment);
                                } else {
                                    send(&sender, Notice::Segment(segment));
                                }
                            }
                        }
                    }
                    "gap" => send(&sender, Notice::Message(crate::t!("subtitle_gap").into())),
                    "done" => {
                        ensure!(ready, "speech worker finished before ready");
                        if offline {
                            let timeout = Duration::from_secs(5);
                            ensure!(
                                crate::process::wait_until(&mut children.worker, timeout, cancel)?
                                    .success(),
                                "speech worker failed"
                            );
                            ensure!(
                                crate::process::wait_until(
                                    children.ffmpeg.as_mut().unwrap(),
                                    timeout,
                                    cancel
                                )?
                                .success(),
                                "audio decoding failed"
                            );
                        }
                        return Ok(true);
                    }
                    _ => bail!("unexpected speech protocol event"),
                }
            }
            let now_bytes = bytes.load(Ordering::Relaxed);
            if now_bytes != last_bytes {
                last_bytes = now_bytes;
                last_audio = Instant::now();
            }
            if ready
                && !(offline && decoding.is_some())
                && if offline {
                    last_audio.max(last_event)
                } else {
                    last_audio
                }
                .elapsed()
                    >= Duration::from_secs(config.settings.request_timeout_secs)
            {
                ensure!(!offline, "audio transcription stalled before completion");
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
    let mut diagnostics = diagnostics;
    if let Some(stderr) = ffmpeg_stderr {
        diagnostics.extend(stderr.join().unwrap_or_default());
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
    fn incremental_captions_supersede_a_pending_fetch_and_stay_on_media_time() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let episode = crate::podcast::Episode {
            id: "episode A".into(),
            position_ms: 61_500,
            ..Default::default()
        };
        let mut controller = Controller::default();
        controller.enabled(true);
        controller.episode = Some(EpisodeTask {
            key: (7, episode.media()),
            incremental: false,
            job: Some(
                Background::start("old-caption-fetch", |_| {
                    Ok(Some(vec![Segment {
                        epoch: 0,
                        start_ms: 0,
                        end_ms: 1000,
                        text: "old fetch".into(),
                    }]))
                })
                .unwrap(),
            ),
        });
        controller.publish_episode(
            7,
            &episode,
            crate::podcast::progressive::CaptionUpdate {
                reset: true,
                segments: vec![Segment {
                    epoch: 0,
                    start_ms: 61_000,
                    end_ms: 62_000,
                    text: "incremental".into(),
                }],
            },
        );
        controller.update_episode(&config, 7, &episode, true);
        assert_eq!(controller.snapshot.history[0].text, "incremental");
        assert_eq!(controller.snapshot.current, [0]);
        assert!(controller.incremental_episode(7, &episode));
        let previous = controller.snapshot.history.clone();
        controller.publish_episode(
            7,
            &episode,
            crate::podcast::progressive::CaptionUpdate {
                reset: false,
                segments: vec![Segment {
                    epoch: 0,
                    start_ms: 60_000,
                    end_ms: 62_000,
                    text: "overlapping earlier cue".into(),
                }],
            },
        );
        assert_eq!(previous.len(), 1, "published snapshots remain immutable");
        assert_eq!(
            controller.snapshot.history[0].text,
            "overlapping earlier cue"
        );
        assert_eq!(controller.snapshot.current, [0, 1]);
        let mut rewind = episode.clone();
        rewind.position_ms = 60_000;
        controller.update_episode(&config, 7, &rewind, true);
        assert_eq!(controller.snapshot.current, [0]);
        assert_eq!(controller.snapshot.timeline, Some((7, episode.id.clone())));
        controller.enabled(false);
        assert!(!controller.incremental_episode(7, &episode));
        controller.publish_episode(
            7,
            &episode,
            crate::podcast::progressive::CaptionUpdate {
                reset: false,
                segments: vec![],
            },
        );
        assert!(!controller.snapshot.enabled);
        assert_eq!(
            controller.snapshot.history[0].text,
            "overlapping earlier cue"
        );
        controller.enabled(true);
        controller.update_episode(&config, 8, &episode, false);
        assert_eq!(controller.snapshot.timeline, Some((8, episode.id.clone())));
        let mut next = episode;
        next.id = "episode B".into();
        controller.update_episode(&config, 8, &next, false);
        assert_eq!(controller.snapshot.timeline, Some((8, next.id)));
    }

    #[test]
    fn interval_queries_skip_expired_history_and_keep_long_overlapping_cues() {
        let mut snapshot = Snapshot {
            history: Arc::new(
                (0..100_000)
                    .map(|i| Segment {
                        epoch: 0,
                        start_ms: i * 1000,
                        end_ms: if i == 0 { 100_000_000 } else { i * 1000 + 500 },
                        text: String::new(),
                    })
                    .collect(),
            ),
            ..Default::default()
        };
        let mut index = IntervalIndex::default();
        snapshot.seek(99_999_250, &mut index);
        assert_eq!(snapshot.current, [0, 99_999]);
        assert!(index.visits < 100, "visited {} nodes", index.visits);
        for position in [0, 999, 1500, 99_999_250, 100_000_000, 42_250] {
            snapshot.seek(position, &mut index);
            let expected: Vec<_> = snapshot
                .history
                .iter()
                .enumerate()
                .filter_map(|(i, s)| (s.start_ms <= position && position < s.end_ms).then_some(i))
                .collect();
            assert_eq!(snapshot.current, expected);
        }
        assert_eq!(
            index.rebuilds, 1,
            "reuse the index across pause, seek and speed ticks"
        );
        let last = Arc::make_mut(&mut snapshot.history).back_mut().unwrap();
        last.end_ms = 100_001_000;
        snapshot.seek(100_000_500, &mut index);
        assert_eq!(snapshot.current, [99_999]);
        assert_eq!(index.rebuilds, 2);
    }

    #[test]
    fn timed_cues_follow_media_position_through_gaps_overlaps_and_rewind() {
        let mut index = IntervalIndex::default();
        let mut snapshot = Snapshot {
            timed: true,
            history: Arc::new(
                [
                    Segment {
                        epoch: 0,
                        start_ms: 1000,
                        end_ms: 3000,
                        text: "one".into(),
                    },
                    Segment {
                        epoch: 0,
                        start_ms: 2000,
                        end_ms: 2500,
                        text: "two".into(),
                    },
                    Segment {
                        epoch: 0,
                        start_ms: 4000,
                        end_ms: 5000,
                        text: "three".into(),
                    },
                ]
                .into(),
            ),
            ..Default::default()
        };
        for (position, expected) in [
            (0, vec![]),
            (1000, vec![0]),
            (2000, vec![0, 1]),
            (2500, vec![0]),
            (3000, vec![]),
            (4900, vec![2]),
            (6000, vec![]),
            (1100, vec![0]),
        ] {
            snapshot.seek(position, &mut index);
            assert_eq!(snapshot.current, expected, "{position}");
        }
        let revision = snapshot.revision;
        snapshot.seek(1200, &mut index);
        assert_eq!(
            snapshot.revision, revision,
            "no redraw until a cue boundary"
        );
    }

    #[test]
    fn file_transcription_waits_for_done_after_slow_inference() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = crate::test_support::config(dir.path());
        config.settings.request_timeout_secs = 1;
        let path = dir.path().join("audio.wav");
        std::fs::write(&path, crate::test_support::fixture("pcm_s16le", "wav")).unwrap();
        let command = fake_command(
            "printf '%s\\n' '{\"v\":1,\"epoch\":0,\"type\":\"ready\"}'; cat >/dev/null; printf '%s\\n' '{\"v\":1,\"epoch\":0,\"type\":\"decoding\"}'; sleep 2; printf '%s\\n' '{\"v\":1,\"epoch\":0,\"type\":\"listening\"}' '{\"v\":1,\"epoch\":0,\"type\":\"segment\",\"start_ms\":0,\"end_ms\":1000,\"text\":\"final caption\"}' '{\"v\":1,\"epoch\":0,\"type\":\"done\"}'",
        );
        let (sender, _events) = mpsc::sync_channel(1);
        let mut segments = vec![];
        let result = run_processes_inner(
            &config,
            (path.to_str().unwrap(), 0),
            &Arc::new(AtomicBool::new(false)),
            sender,
            command,
            models::lock(&config, false).unwrap(),
            Some(FileOutput {
                segments: &mut segments,
                progress: None,
            }),
        );
        assert!(result.unwrap());
        assert_eq!(
            segments.len(),
            1,
            "do not succeed before the final segment and done"
        );
        assert_eq!(segments[0].text, "final caption");
    }

    #[test]
    fn file_transcription_rejects_stalls_missing_done_and_failed_workers() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = crate::test_support::config(dir.path());
        config.settings.request_timeout_secs = 1;
        let path = dir.path().join("audio.wav");
        std::fs::write(&path, crate::test_support::fixture("pcm_s16le", "wav")).unwrap();
        for (tail, expected) in [
            ("sleep 2", "stalled before completion"),
            ("exit 0", "closed its output"),
            (
                "printf '%s\\n' '{\"v\":1,\"epoch\":0,\"type\":\"done\"}'; exit 1",
                "speech worker failed",
            ),
        ] {
            let script = format!(
                "printf '%s\\n' '{{\"v\":1,\"epoch\":0,\"type\":\"ready\"}}'; cat >/dev/null; printf '%s\\n' '{{\"v\":1,\"epoch\":0,\"type\":\"segment\",\"start_ms\":0,\"end_ms\":1000,\"text\":\"partial\"}}'; {tail}"
            );
            let (sender, _events) = mpsc::sync_channel(1);
            let mut segments = vec![];
            let result = run_processes_inner(
                &config,
                (path.to_str().unwrap(), 0),
                &Arc::new(AtomicBool::new(false)),
                sender,
                fake_command(&script),
                models::lock(&config, false).unwrap(),
                Some(FileOutput {
                    segments: &mut segments,
                    progress: None,
                }),
            );
            assert_eq!(segments.len(), 1);
            assert!(
                format!("{:#}", result.unwrap_err()).contains(expected),
                "{tail}"
            );
            assert!(models::lock(&config, true).is_ok());
        }
    }

    #[test]
    fn file_transcription_collects_every_segment_and_checks_decoder_exit() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let path = dir.path().join("audio.wav");
        std::fs::write(&path, crate::test_support::fixture("pcm_s16le", "wav")).unwrap();
        for valid in [true, false] {
            let (sender, _events) = mpsc::sync_channel(1);
            let command = fake_command(
                "printf '%s\\n' '{\"v\":1,\"epoch\":0,\"type\":\"ready\"}'; cat >/dev/null; i=0; while [ $i -lt 80 ]; do printf '%s\\n' '{\"v\":1,\"epoch\":0,\"type\":\"segment\",\"start_ms\":0,\"end_ms\":1000,\"text\":\"caption\"}'; i=$((i+1)); done; printf '%s\\n' '{\"v\":1,\"epoch\":0,\"type\":\"done\"}'",
            );
            let mut segments = vec![];
            let input = if valid {
                path.to_str().unwrap()
            } else {
                "/does/not/exist.wav"
            };
            let result = run_processes_inner(
                &config,
                (input, 0),
                &Arc::new(AtomicBool::new(false)),
                sender,
                command,
                models::lock(&config, false).unwrap(),
                Some(FileOutput {
                    segments: &mut segments,
                    progress: None,
                }),
            );
            assert_eq!(result.is_ok(), valid, "{result:?}");
            if valid {
                assert_eq!(segments.len(), 80);
            }
            assert!(models::lock(&config, true).is_ok());
        }
    }

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
        for model in [
            "sensevoice-small-int8",
            "sensevoice-small-yue-int8",
            "qwen3-asr-0.6b-int8",
        ] {
            config.settings.subtitles.model = model.into();
            let (sender, events) = mpsc::sync_channel(64);
            let start = Instant::now();
            assert!(
                run(
                    &config,
                    &server.url,
                    1,
                    &Arc::new(AtomicBool::new(false)),
                    sender,
                    &Mutex::default()
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
        controller.task = Some(Task {
            job,
            events,
            first_segment_at: None,
        });
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
        controller.update(&config, (key.0, &key.1), true, false, false, &stop);
        assert_eq!(controller.snapshot.history.len(), 1);
        let epoch = controller.epoch;
        controller.update(&config, (key.0, &key.1), false, true, false, &stop);
        assert!(controller.epoch != epoch && controller.task.is_none());
        assert_eq!(controller.snapshot.history.len(), 1);
        controller.update(&config, (2, &key.1), true, false, false, &stop);
        assert!(controller.snapshot.history.is_empty());
        controller.enabled(false);
    }

    #[test]
    fn short_connections_exhaust_retries_even_when_they_produce_segments() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let stop = Arc::new(AtomicBool::new(false));
        for productive in [false, true] {
            let mut controller = Controller::default();
            controller.enabled(true);
            controller.key = Some((1, "http://localhost/live".into()));
            for attempt in 0..6 {
                let (sender, events) = mpsc::sync_channel(64);
                if productive {
                    sender
                        .send(Notice::Segment(Segment {
                            epoch: controller.epoch,
                            start_ms: 0,
                            end_ms: 500,
                            text: "speech".into(),
                        }))
                        .unwrap();
                }
                let job = Background::start("fake-speech", |_| Ok(true)).unwrap();
                controller.task = Some(Task {
                    job,
                    events,
                    first_segment_at: None,
                });
                controller.due = None;
                let deadline = Instant::now() + Duration::from_secs(2);
                while controller.task.is_some() {
                    controller.update(
                        &config,
                        (1, "http://localhost/live"),
                        true,
                        false,
                        false,
                        &stop,
                    );
                    assert!(Instant::now() < deadline);
                    thread::yield_now();
                }
                if attempt == 3 {
                    assert!(controller.snapshot.failed);
                    assert!(controller.due.is_none());
                    break;
                } else {
                    assert_eq!(controller.attempts, attempt + 1);
                    assert!(!controller.snapshot.failed);
                }
            }
        }
    }

    #[test]
    fn only_sustained_speech_on_the_same_connection_resets_retries() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let stop = Arc::new(AtomicBool::new(false));
        // Startup, model verification and silence before the first segment do
        // not count as productive time. Only later segments on this task do.
        for elapsed in [
            None,
            Some(Duration::from_secs(30)),
            Some(STABLE_SUBTITLE_PERIOD),
        ] {
            let mut controller = Controller::default();
            controller.enabled(true);
            controller.key = Some((1, "http://localhost/live".into()));
            controller.attempts = 3;
            let (sender, events) = mpsc::sync_channel(64);
            sender
                .send(Notice::Segment(Segment {
                    epoch: controller.epoch,
                    start_ms: 0,
                    end_ms: 500,
                    text: "speech".into(),
                }))
                .unwrap();
            let job = Background::start("fake-speech", |_| Ok(true)).unwrap();
            controller.task = Some(Task {
                job,
                events,
                first_segment_at: elapsed.map(|elapsed| Instant::now() - elapsed),
            });
            let deadline = Instant::now() + Duration::from_secs(2);
            while controller.task.is_some() {
                controller.update(
                    &config,
                    (1, "http://localhost/live"),
                    true,
                    false,
                    false,
                    &stop,
                );
                assert!(Instant::now() < deadline);
                thread::yield_now();
            }
            if elapsed == Some(STABLE_SUBTITLE_PERIOD) {
                assert_eq!(controller.attempts, 1);
                assert!(!controller.snapshot.failed);
                assert!(controller.due.is_some());
            } else {
                assert_eq!(controller.attempts, 3);
                assert!(controller.snapshot.failed);
                assert!(controller.due.is_none());
            }
        }
    }

    #[test]
    fn history_is_bounded_and_empty_clear_preserves_revision() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let stop = Arc::new(AtomicBool::new(false));
        let mut controller = Controller::default();
        controller.clear();
        assert_eq!(controller.snapshot.revision, 0);
        controller.enabled(true);
        controller.key = Some((1, "http://localhost/live".into()));
        let (sender, events) = mpsc::sync_channel(501);
        for i in 0..501 {
            sender
                .send(Notice::Segment(Segment {
                    epoch: controller.epoch,
                    start_ms: i,
                    end_ms: i + 1,
                    text: i.to_string(),
                }))
                .unwrap();
        }
        let job = Background::start("fake-speech", |_| Ok(true)).unwrap();
        controller.task = Some(Task {
            job,
            events,
            first_segment_at: None,
        });
        controller.update(
            &config,
            (1, "http://localhost/live"),
            true,
            false,
            false,
            &stop,
        );
        assert_eq!(controller.snapshot.history.len(), 500);
        assert_eq!(controller.snapshot.history.front().unwrap().start_ms, 1);
        controller.enabled(false);
        controller.update(
            &config,
            (2, "http://localhost/other"),
            true,
            false,
            false,
            &stop,
        );
        assert!(controller.snapshot.history.is_empty());
        let revision = controller.snapshot.revision;
        controller.clear();
        assert_eq!(controller.snapshot.revision, revision);
    }
}
