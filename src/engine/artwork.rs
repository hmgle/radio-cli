//! Optional ICY metadata probes share the auxiliary audio connection with recording.
use crate::{
    artwork::{Task, client},
    config::Config,
    error::{DomainError, ErrorCode},
};
use anyhow::{Result, bail, ensure};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

const READ_LIMIT: usize = 1024 * 1024;
const INTERVAL: Duration = Duration::from_secs(25);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Request {
    pub stationuuid: String,
    pub source_url: String,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TrackArtwork {
    pub generation: u64,
    pub request: Request,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Key {
    generation: u64,
    request: Request,
}

#[derive(Debug, PartialEq, Eq)]
struct Metadata {
    title: String,
    url: Option<String>,
}

pub(crate) struct Controller {
    pub demand: Option<String>,
    pub cover: Option<TrackArtwork>,
    key: Option<Key>,
    generation: u64,
    task: Option<(Key, Task<Option<Metadata>>)>,
    due: Instant,
    unsupported: bool,
}

impl Default for Controller {
    fn default() -> Self {
        Self {
            demand: None,
            cover: None,
            key: None,
            generation: 0,
            task: None,
            due: Instant::now(),
            unsupported: false,
        }
    }
}

impl Controller {
    pub fn cancel(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.key = None;
        self.cover = None;
        self.unsupported = false;
        self.due = Instant::now();
        if let Some((_, task)) = &mut self.task {
            task.cancel();
        }
    }

    pub fn quiescent(&self) -> bool {
        self.task.as_ref().is_none_or(|(_, task)| task.finished())
    }

    pub fn completion(&self) -> Option<Arc<AtomicBool>> {
        self.task.as_ref().map(|(_, task)| task.completion())
    }

    pub fn update(&mut self, config: &Config, request: Option<Request>) {
        let demanded = request.is_some();
        if !demanded {
            if let Some((_, task)) = &mut self.task {
                task.cancel();
            }
        } else if self.key.as_ref().map(|key| &key.request) != request.as_ref() {
            self.cancel();
            self.key = request.map(|request| Key {
                generation: self.generation,
                request,
            });
        }
        if let Some((key, task)) = &self.task
            && task.finished()
            && let Some(result) = task.poll()
        {
            if self.key.as_ref() == Some(key) {
                match result {
                    Ok(Some(metadata)) => {
                        self.cover = metadata
                            .url
                            .filter(|_| {
                                !metadata.title.is_empty() && metadata.title == key.request.title
                            })
                            .map(|url| TrackArtwork {
                                generation: key.generation,
                                request: key.request.clone(),
                                url,
                            });
                    }
                    Ok(None) => {
                        self.cover = None;
                        self.unsupported = true;
                    }
                    Err(_) => {}
                }
                // A probe cancelled by demand withdrawal also starts the next
                // interval, damping repeated pause or popup toggles.
                self.due = Instant::now() + INTERVAL;
            }
            self.task = None;
        }
        if demanded
            && self.task.is_none()
            && !self.unsupported
            && Instant::now() >= self.due
            && let Some(key) = self.key.clone()
        {
            let config = config.clone();
            let url = key.request.source_url.clone();
            match Task::start(move || async move { probe(&config, &url).await }) {
                Ok(task) => self.task = Some((key, task)),
                Err(_) => self.due = Instant::now() + INTERVAL,
            }
        }
    }
}

/// Connection handoffs remain bounded even if an optional artwork worker stalls.
pub(super) fn handoff_deadline(config: &Config) -> Instant {
    Instant::now() + Duration::from_secs(config.settings.connect_timeout_secs.saturating_mul(2))
}

pub(super) fn handoff_expired(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        bail!(DomainError::new(ErrorCode::ArtworkHandoffTimeout));
    }
    Ok(())
}

pub(super) fn wait_for_handoff(
    done: Option<&AtomicBool>,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<()> {
    while done.is_some_and(|done| !done.load(std::sync::atomic::Ordering::Acquire)) {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            bail!(DomainError::new(ErrorCode::Cancelled));
        }
        handoff_expired(deadline)?;
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn metadata(bytes: &[u8]) -> Metadata {
    let bytes = bytes.split(|byte| *byte == 0).next().unwrap_or_default();
    // Field syntax is ASCII. Keep values as bytes until their encoding is
    // known, so an invalid title cannot change how another field is decoded.
    let mut fields = bytes;
    let mut title = String::new();
    let mut url = None;
    while let Some(equal) = fields.iter().position(|&byte| byte == b'=') {
        let name = &fields[..equal];
        let Some(remaining) = fields[equal + 1..].trim_ascii_start().strip_prefix(b"'") else {
            break;
        };
        let Some(end) = remaining.windows(2).position(|pair| pair == b"';") else {
            break;
        };
        let value = &remaining[..end];
        if name.trim_ascii().eq_ignore_ascii_case(b"StreamTitle") {
            title = crate::player::icy_title_bytes(value);
        } else if name.trim_ascii().eq_ignore_ascii_case(b"StreamUrl") {
            url = std::str::from_utf8(value)
                .ok()
                .filter(|value| {
                    crate::clean_text(value) == *value && crate::config::validate_url(value).is_ok()
                })
                .map(str::to_owned);
        }
        fields = remaining[end + 2..].trim_ascii_start();
    }
    Metadata { title, url }
}

struct Reader {
    response: reqwest::Response,
    chunk: Vec<u8>,
    offset: usize,
    read: usize,
}

impl Reader {
    async fn take(&mut self, length: usize) -> Result<Vec<u8>> {
        ensure!(length <= READ_LIMIT - self.read, "ICY read limit");
        let mut out = Vec::with_capacity(length);
        while out.len() < length {
            if self.offset == self.chunk.len() {
                self.chunk = self
                    .response
                    .chunk()
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("truncated ICY metadata"))?
                    .to_vec();
                self.offset = 0;
                ensure!(self.chunk.len() <= READ_LIMIT, "large ICY chunk");
            }
            let count = (length - out.len()).min(self.chunk.len() - self.offset);
            out.extend_from_slice(&self.chunk[self.offset..self.offset + count]);
            self.offset += count;
            self.read += count;
        }
        Ok(out)
    }
}

async fn probe(config: &Config, url: &str) -> Result<Option<Metadata>> {
    crate::config::validate_url(url)?;
    let response = client(config)?
        .get(url)
        .header("Icy-MetaData", "1")
        .send()
        .await?
        .error_for_status()?;
    let interval = response
        .headers()
        .get("icy-metaint")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok());
    let Some(interval) = interval.filter(|n| *n > 0 && *n < READ_LIMIT) else {
        return Ok(None);
    };
    let mut reader = Reader {
        response,
        chunk: vec![],
        offset: 0,
        read: 0,
    };
    loop {
        reader.take(interval).await?;
        let length = usize::from(reader.take(1).await?[0]) * 16;
        if length > 0 {
            return Ok(Some(metadata(&reader.take(length).await?)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{self, Server, response};
    use std::{
        io::{Read, Write},
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
            mpsc,
        },
    };

    fn run<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    fn body(title: &str, url: &str) -> Vec<u8> {
        let mut block = format!("StreamTitle='{title}';StreamUrl='{url}';").into_bytes();
        block.resize(block.len().div_ceil(16) * 16, 0);
        let mut body = vec![0; 4];
        body.push((block.len() / 16) as u8);
        body.extend(block);
        body
    }

    fn icy(stream: &mut std::net::TcpStream, bytes: &[u8]) {
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nicy-metaint: 4\r\nConnection: close\r\n\r\n").unwrap();
        // Deliberately split both length and text across writes.
        for byte in bytes {
            if stream.write_all(&[*byte]).is_err() {
                break;
            }
        }
    }

    fn request(url: &str, title: &str) -> Request {
        Request {
            stationuuid: "station".into(),
            source_url: url.into(),
            title: title.into(),
        }
    }

    fn finish(controller: &mut Controller, config: &Config, request: Option<Request>) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            controller.update(config, request.clone());
            if controller.task.is_none() {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn icy_parser_preserves_track_text_and_rejects_garbage_urls() {
        for url in [
            "",
            "12345",
            "file:///tmp/picture",
            "https://example.test/\x1b",
        ] {
            let block = format!("StreamTitle='Artist's song; 中文';StreamUrl='{url}';\0\0");
            let metadata = metadata(block.as_bytes());
            assert_eq!(metadata.title, "Artist's song; 中文");
            assert!(metadata.url.is_none());
        }
        let parsed =
            metadata(b"StreamTitle='Beyonc\xe9';StreamUrl='https://example.test/cover.png';");
        assert_eq!(parsed.title, "Beyoncé");
        assert_eq!(
            parsed.url.as_deref(),
            Some("https://example.test/cover.png")
        );
        assert!(metadata(b"broken metadata").url.is_none());
    }

    #[test]
    fn icy_parser_matches_player_titles_with_mixed_encodings() {
        // Raw GBK song text and UTF-8 station text arrive in the same block.
        // The final UTF-8 character is truncated in the source itself.
        let mut raw = b"\xcd\xf5\xb7\xc6 -  (AsiaFM".to_vec();
        raw.extend_from_slice("亚洲粤语".as_bytes());
        raw.extend_from_slice(b"\xe5\x8f?)");
        let wrapped = "\u{cd}\u{f5}\u{b7}\u{c6} -  (AsiaFM亚洲粤语\u{e5}\u{8f}?)";
        let mut block = b"StreamTitle='".to_vec();
        block.extend_from_slice(&raw);
        block.extend_from_slice("';StreamUrl='https://example.test/封面.png';".as_bytes());
        let parsed = metadata(&block);
        assert_eq!(parsed.title, "王菲 -  (AsiaFM亚洲粤语å?)");
        assert_eq!(parsed.title, crate::player::icy_title(wrapped));
        assert_eq!(parsed.url.as_deref(), Some("https://example.test/封面.png"));

        // Fully wrapped GBK titles also use the same repair as mpv metadata.
        let wrapped = "ºÓÍ¼ - ×Ï´¨Â¼";
        assert_eq!(
            metadata(format!("StreamTitle='{wrapped}';").as_bytes()).title,
            crate::player::icy_title(wrapped)
        );
    }

    #[test]
    fn icy_parser_keeps_unicode_fields_independent_of_invalid_bytes() {
        let mut block = "StreamTitle='Björk - Jóga';StreamUrl='https://example.test/封面.png';"
            .as_bytes()
            .to_vec();
        block.extend_from_slice(b"Other='\xff';");
        let parsed = metadata(&block);
        assert_eq!(parsed.title, "Björk - Jóga");
        assert_eq!(parsed.url.as_deref(), Some("https://example.test/封面.png"));

        let parsed = metadata(b"StreamTitle='Track';StreamUrl='https://example.test/\xff';");
        assert_eq!(parsed.title, "Track");
        assert!(parsed.url.is_none());
    }

    #[test]
    fn icy_probe_reads_fragmented_metadata_and_skips_empty_blocks() {
        let image =
            Server::new(|_, stream| response(stream, 200, "image/png", b"local image source"));
        let url = image.url.clone();
        let expected = url.clone();
        let server = Server::new(move |request, stream| {
            assert!(request.to_lowercase().contains("icy-metadata: 1"));
            let mut bytes = vec![0; 5]; // Four audio bytes, then an empty block.
            bytes.extend(body("Artist - Track", &url));
            icy(stream, &bytes);
        });
        let dir = tempfile::tempdir().unwrap();
        let parsed = run(probe(&test_support::config(dir.path()), &server.url))
            .unwrap()
            .unwrap();
        assert_eq!(parsed.title, "Artist - Track");
        assert_eq!(parsed.url.as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn icy_probe_bounds_headers_body_truncation_and_timeout() {
        for header in [
            "",
            "icy-metaint: 0\r\n",
            "icy-metaint: bad\r\n",
            "icy-metaint: 999999999\r\n",
        ] {
            let server = Server::new(move |_, stream| {
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/vnd.apple.mpegurl\r\n{header}Content-Length: 0\r\n\r\n"
                );
            });
            let dir = tempfile::tempdir().unwrap();
            assert!(
                run(probe(&test_support::config(dir.path()), &server.url))
                    .unwrap()
                    .is_none()
            );
        }
        for bytes in [vec![0; 3], vec![0, 0, 0, 0, 1, b'x']] {
            let server = Server::new(move |_, stream| icy(stream, &bytes));
            let dir = tempfile::tempdir().unwrap();
            assert!(run(probe(&test_support::config(dir.path()), &server.url)).is_err());
        }
        let server = Server::new(|_, stream| {
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nicy-metaint: {}\r\n\r\n",
                READ_LIMIT - 1
            );
            let _ = stream.write_all(&vec![0; READ_LIMIT]);
        });
        let dir = tempfile::tempdir().unwrap();
        assert!(run(probe(&test_support::config(dir.path()), &server.url)).is_err());
        let stalled = Server::new(|_, stream| {
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nicy-metaint: 4\r\n\r\n")
                .unwrap();
            let _ = stream.read(&mut [0]);
        });
        let mut config = test_support::config(dir.path());
        config.settings.request_timeout_secs = 1;
        let started = Instant::now();
        assert!(run(probe(&config, &stalled.url)).is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn track_association_poll_budget_and_unsupported_sources_are_stable() {
        let hits = Arc::new(AtomicUsize::new(0));
        let requests = hits.clone();
        let server = Server::new(move |request, stream| {
            requests.fetch_add(1, Ordering::Relaxed);
            if request.starts_with("GET /plain ") {
                response(stream, 200, "audio/mpeg", b"plain");
            } else {
                icy(stream, &body("Track one", "http://localhost/cover.png"));
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let config = test_support::config(dir.path());
        let mut controller = Controller::default();
        let first = request(&server.url, "Track one");
        finish(&mut controller, &config, Some(first.clone()));
        assert!(controller.cover.is_some());
        for _ in 0..10 {
            controller.update(&config, Some(first.clone()));
        }
        assert_eq!(hits.load(Ordering::Relaxed), 1);
        controller.due = Instant::now();
        finish(&mut controller, &config, Some(first.clone()));
        assert_eq!(hits.load(Ordering::Relaxed), 2);
        controller.update(&config, Some(request(&server.url, "Track two")));
        assert!(
            controller.cover.is_none(),
            "a title change must clear the old cover immediately"
        );
        finish(
            &mut controller,
            &config,
            Some(request(&server.url, "Track two")),
        );
        assert!(
            controller.cover.is_none(),
            "mismatching probe title must never publish art"
        );
        let plain = request(&format!("{}/plain", server.url), "Track one");
        finish(&mut controller, &config, Some(plain.clone()));
        let count = hits.load(Ordering::Relaxed);
        for _ in 0..10 {
            controller.due = Instant::now();
            controller.update(&config, Some(plain.clone()));
        }
        assert_eq!(hits.load(Ordering::Relaxed), count);
        assert!(controller.unsupported);
        controller.update(&config, None);
        finish(&mut controller, &config, Some(first));
        assert!(controller.cover.is_some());
    }

    #[test]
    fn temporary_demand_changes_retain_cover_and_probe_schedule() {
        let hits = Arc::new(AtomicUsize::new(0));
        let requests = hits.clone();
        let server = Server::new(move |_, stream| {
            requests.fetch_add(1, Ordering::Relaxed);
            icy(stream, &body("Track", "http://localhost/cover.png"));
        });
        let dir = tempfile::tempdir().unwrap();
        let config = test_support::config(dir.path());
        let mut controller = Controller::default();
        let selected = request(&server.url, "Track");
        finish(&mut controller, &config, Some(selected.clone()));
        let cover = controller.cover.clone();
        let due = controller.due;
        for _ in 0..5 {
            controller.update(&config, None);
            assert_eq!(controller.cover, cover);
            controller.update(&config, Some(selected.clone()));
            assert_eq!(controller.due, due);
            assert!(controller.task.is_none());
        }
        assert_eq!(hits.load(Ordering::Relaxed), 1);
        controller.cancel();
        assert!(controller.cover.is_none());
    }

    #[test]
    fn withdrawing_demand_cancels_an_inflight_probe_and_delays_retry() {
        let hits = Arc::new(AtomicUsize::new(0));
        let requests = hits.clone();
        let (opened, ready) = mpsc::channel();
        let (closed, released) = mpsc::channel();
        let server = Server::new(move |_, stream| {
            if requests.fetch_add(1, Ordering::Relaxed) == 0 {
                icy(stream, &body("Track", "http://localhost/cover.png"));
            } else {
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nicy-metaint: 4\r\n\r\n")
                    .unwrap();
                opened.send(()).unwrap();
                let mut byte = [0];
                let result = stream.read(&mut byte);
                if matches!(result, Ok(0))
                    || result.is_err_and(|error| {
                        matches!(
                            error.kind(),
                            std::io::ErrorKind::ConnectionReset
                                | std::io::ErrorKind::ConnectionAborted
                        )
                    })
                {
                    closed.send(()).unwrap();
                }
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let config = test_support::config(dir.path());
        let selected = request(&server.url, "Track");
        let mut controller = Controller::default();
        finish(&mut controller, &config, Some(selected.clone()));
        assert!(controller.cover.is_some());
        let cover = controller.cover.clone();
        let key = controller.key.clone();
        controller.due = Instant::now();
        controller.update(&config, Some(selected.clone()));
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        let cancelled_at = Instant::now();
        controller.update(&config, None);
        released.recv_timeout(Duration::from_secs(1)).unwrap();
        finish(&mut controller, &config, None);
        assert_eq!(controller.cover, cover);
        assert_eq!(controller.key, key);
        assert!(controller.due >= cancelled_at + INTERVAL);
        assert!(controller.due <= Instant::now() + INTERVAL);
        let due = controller.due;
        controller.update(&config, Some(selected));
        assert_eq!(controller.due, due);
        assert!(controller.task.is_none());
        assert_eq!(hits.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn handoff_timeout_retains_its_domain_code() {
        let done = AtomicBool::new(false);
        let error =
            wait_for_handoff(Some(&done), Instant::now(), &AtomicBool::new(false)).unwrap_err();
        assert_eq!(
            error.downcast_ref::<DomainError>().unwrap().code,
            ErrorCode::ArtworkHandoffTimeout
        );
        assert_eq!(
            crate::error::json_error(&error)["error"]["code"],
            "artwork_handoff_timeout"
        );
        assert_eq!(crate::error::exit_code(&error), 1);
        let cancelled =
            wait_for_handoff(Some(&done), Instant::now(), &AtomicBool::new(true)).unwrap_err();
        assert_eq!(
            cancelled.downcast_ref::<DomainError>().unwrap().code,
            ErrorCode::Cancelled
        );
    }

    #[test]
    fn stalled_handoffs_fail_before_opening_another_audio_connection() {
        use crate::{
            engine::{Engine, PlaybackState, RecordingState},
            model::Station,
        };
        let dir = tempfile::tempdir().unwrap();
        let config = test_support::config(dir.path());
        let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
        engine.enable_background_media();
        engine.status.station = Some(Station {
            stationuuid: uuid::Uuid::new_v4().to_string(),
            url: "http://localhost/live".into(),
            ..Station::default()
        });
        engine.status.playback_state = PlaybackState::Playing;
        engine.playback.url = "http://localhost/live".into();
        let (entered, ready) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let task = Task::start(move || async move {
            entered.send(()).unwrap();
            gate.recv().unwrap();
            Ok(None)
        })
        .unwrap();
        ready.recv_timeout(Duration::from_secs(1)).unwrap();
        engine.artwork.task = Some((
            Key {
                generation: 1,
                request: request("http://localhost/live", "Track"),
            },
            task,
        ));
        engine.record_current(None, None).unwrap();
        engine
            .recording
            .recording_probe
            .as_mut()
            .unwrap()
            .artwork_deadline = Instant::now();
        engine.tick().unwrap();
        assert!(matches!(
            engine.status.recording_state,
            RecordingState::Failed { .. }
        ));
        assert!(engine.recording.recording_probe.is_none());
        assert!(engine.store.recordings(1).unwrap().is_empty());
        assert_eq!(engine.status.playback_state, PlaybackState::Playing);
        assert!(
            wait_for_handoff(
                Some(&AtomicBool::new(false)),
                Instant::now(),
                &AtomicBool::new(false)
            )
            .is_err()
        );
        release.send(()).unwrap();
        engine.shutdown().unwrap();
    }

    #[test]
    fn cancelled_probe_cannot_publish_after_reconnecting_to_the_same_track() {
        let hits = Arc::new(AtomicUsize::new(0));
        let requests = hits.clone();
        let (opened, ready) = mpsc::channel();
        let server = Server::new(move |_, stream| {
            if requests.fetch_add(1, Ordering::Relaxed) == 0 {
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nicy-metaint: 4\r\n\r\n")
                    .unwrap();
                opened.send(()).unwrap();
                let _ = stream.read(&mut [0]);
            } else {
                icy(stream, &body("Track", "http://localhost/new.png"));
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let config = test_support::config(dir.path());
        let mut controller = Controller::default();
        let selected = request(&server.url, "Track");
        controller.update(&config, Some(selected.clone()));
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        let generation = controller.generation;
        controller.cancel();
        finish(&mut controller, &config, Some(selected));
        assert!(controller.cover.as_ref().unwrap().generation > generation);
        assert_eq!(hits.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn recording_waits_for_icy_release_and_preserves_the_requested_track() {
        use crate::{
            engine::{Engine, PlaybackState, RecordingState},
            model::Station,
        };
        use std::{net::TcpStream, os::fd::AsRawFd};
        let audio = test_support::fixture("libmp3lame", "mp3");
        let sockets = Arc::new(Mutex::new(Vec::<TcpStream>::new()));
        let connections = sockets.clone();
        let peak = Arc::new(AtomicUsize::new(0));
        let maximum = peak.clone();
        let completion = Arc::new(Mutex::new(None::<Arc<AtomicBool>>));
        let probe_done = completion.clone();
        let (opened, ready) = mpsc::channel();
        let server = Server::new(move |request, stream| {
            {
                let mut sockets = connections.lock().unwrap();
                // Count live peers, including a simulated playback connection.
                // EOF is checked without consuming data or waiting for another
                // handler thread to observe a close.
                sockets.retain(|socket| unsafe {
                    libc::recv(
                        socket.as_raw_fd(),
                        [0_u8; 1].as_mut_ptr().cast(),
                        1,
                        libc::MSG_PEEK | libc::MSG_DONTWAIT,
                    ) != 0
                });
                sockets.push(stream.try_clone().unwrap());
                maximum.fetch_max(sockets.len(), Ordering::Relaxed);
            }
            if request.starts_with("GET /audio ") {
                assert!(
                    probe_done
                        .lock()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .load(Ordering::Acquire)
                );
                response(stream, 200, "audio/mpeg", &audio);
            } else {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nicy-metaint: 4\r\n\r\n",
                    )
                    .unwrap();
                if request.starts_with("GET /icy ") {
                    opened.send(()).unwrap();
                }
                let _ = stream.read(&mut [0]);
            }
        });
        let mut listening = TcpStream::connect(server.url.trim_start_matches("http://")).unwrap();
        listening
            .write_all(b"GET /playing HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut header = [0; 1];
        listening.read_exact(&mut header).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut config = test_support::config(dir.path());
        config.settings.artwork = crate::config::ArtworkMode::Track;
        let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
        engine.enable_background_media();
        let station = Station {
            stationuuid: uuid::Uuid::new_v4().to_string(),
            name: "Local ICY".into(),
            url: format!("{}/audio", server.url),
            custom: true,
            active: true,
            ..Station::default()
        };
        engine.store.upsert(&station).unwrap();
        engine.status.station = Some(station.clone());
        engine.status.title = "Requested track".into();
        engine.status.source_url = format!("{}/icy", server.url);
        engine.status.playback_state = PlaybackState::Playing;
        engine.playback.url = station.url;
        engine.artwork.demand = Some(station.stationuuid);
        engine.tick().unwrap();
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        *completion.lock().unwrap() = engine.artwork.completion();
        engine
            .record_current(Some(Duration::from_secs(1)), None)
            .unwrap();
        assert_eq!(engine.status.recording_state, RecordingState::Probing);
        engine.status.title = "Later track".into();
        let deadline = Instant::now() + Duration::from_secs(5);
        while engine.store.recordings(1).unwrap().is_empty() {
            engine.tick().unwrap();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            engine.store.recordings(1).unwrap()[0].track.as_deref(),
            Some("Requested track")
        );
        assert!(engine.artwork.cover.is_none());
        assert!(
            peak.load(Ordering::Relaxed) <= 2,
            "audio connection budget exceeded"
        );
        drop(listening);
        engine.shutdown().unwrap();
    }

    #[test]
    fn inactive_playback_recording_and_other_station_selections_never_probe() {
        use crate::{
            config::ArtworkMode,
            engine::{Engine, PlaybackState, RecordingState},
            model::Station,
        };
        let dir = tempfile::tempdir().unwrap();
        let mut config = test_support::config(dir.path());
        config.settings.artwork = ArtworkMode::Track;
        let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
        engine.enable_background_media();
        engine.status.station = Some(Station {
            stationuuid: "playing".into(),
            ..Station::default()
        });
        engine.status.title = "Track".into();
        engine.status.source_url = "http://localhost/live".into();
        engine.artwork.demand = Some("playing".into());
        for state in [
            PlaybackState::Idle,
            PlaybackState::Paused,
            PlaybackState::Connecting,
            PlaybackState::Reconnecting { attempt: 1 },
            PlaybackState::Failed {
                reason: "failed".into(),
            },
        ] {
            engine.status.playback_state = state;
            engine.artwork_tick();
            assert!(engine.artwork.task.is_none());
        }
        engine.status.playback_state = PlaybackState::Playing;
        for state in [
            RecordingState::Probing,
            RecordingState::Recording,
            RecordingState::Finalizing,
        ] {
            engine.status.recording_state = state;
            engine.artwork_tick();
            assert!(engine.artwork.task.is_none());
        }
        engine.status.recording_state = RecordingState::Idle;
        engine.status.station.as_mut().unwrap().hls = 1;
        engine.artwork_tick();
        assert!(engine.artwork.task.is_none());
        engine.status.station.as_mut().unwrap().hls = 0;
        engine.artwork.demand = Some("selected only".into());
        engine.artwork_tick();
        assert!(engine.artwork.task.is_none());
        engine.artwork.demand = Some("playing".into());
        engine.config.settings.artwork = ArtworkMode::Station;
        engine.artwork_tick();
        assert!(engine.artwork.task.is_none());
        engine.config.settings.artwork = ArtworkMode::Track;
        engine.playback.source_kind = super::super::playback::SourceKind::Local;
        engine.artwork_tick();
        assert!(engine.artwork.task.is_none());
    }

    #[test]
    fn stop_cancels_a_recording_waiting_for_the_artwork_connection() {
        use crate::{
            engine::{Engine, PlaybackState, RecordingState},
            model::Station,
        };
        let dir = tempfile::tempdir().unwrap();
        let config = test_support::config(dir.path());
        let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
        engine.enable_background_media();
        let station = Station {
            stationuuid: uuid::Uuid::new_v4().to_string(),
            name: "Local".into(),
            url: "http://localhost/live".into(),
            ..Station::default()
        };
        engine.status.station = Some(station.clone());
        engine.status.playback_state = PlaybackState::Playing;
        engine.status.title = "Frozen title".into();
        engine.playback.url = station.url;
        let (entered, ready) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        // Hold completion independently of cancellation so the pending
        // recording path is exercised deterministically.
        let task = Task::start(move || async move {
            entered.send(()).unwrap();
            gate.recv().unwrap();
            Ok(None)
        })
        .unwrap();
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        engine.artwork.task = Some((
            Key {
                generation: 1,
                request: request("http://localhost/live", "Frozen title"),
            },
            task,
        ));
        engine.record_current(None, None).unwrap();
        assert_eq!(engine.status.recording_state, RecordingState::Probing);
        assert!(engine.recording.recording_probe.is_some());
        engine.stop_playback().unwrap();
        assert!(engine.recording.recording_probe.is_none());
        assert_eq!(engine.status.recording_state, RecordingState::Idle);
        release.send(()).unwrap();
        engine.shutdown().unwrap();
        assert!(engine.store.recordings(1).unwrap().is_empty());
    }
}
