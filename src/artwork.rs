//! Bounded, optional artwork I/O. Nothing in this module writes to the terminal.
use crate::config::Config;
use anyhow::{Context, Result, bail, ensure};
use image::{DynamicImage, ImageFormat, ImageReader, Limits};
use sha2::{Digest, Sha256};
use std::{
    fs,
    future::Future,
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    time::{Duration, SystemTime},
};
use tokio::sync::oneshot;

const DOWNLOAD_LIMIT: usize = 256 * 1024;
const CACHE_ENTRY_LIMIT: u64 = 1024 * 1024;
const CACHE_BYTES: u64 = 32 * 1024 * 1024;
const CACHE_ENTRIES: usize = 128;

/// Cancellation drops the entire HTTP future. Completion is published only
/// after that future and its HTTP response have been destroyed. Blocking
/// DNS or cache work may finish independently after runtime shutdown.
pub(crate) struct Task<T> {
    cancel: Option<oneshot::Sender<()>>,
    finished: Arc<AtomicBool>,
    result: Receiver<Result<T>>,
}

struct Completion(Arc<AtomicBool>);
impl Drop for Completion {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

impl<T: Send + 'static> Task<T> {
    pub(crate) fn start<F, Fut>(work: F) -> Result<Self>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>>,
    {
        let (cancel, cancelled) = oneshot::channel();
        let (sender, result) = mpsc::sync_channel(1);
        let finished = Arc::new(AtomicBool::new(false));
        let done = finished.clone();
        std::thread::Builder::new()
            .name("radio-artwork".into())
            .spawn(move || {
                let completion = Completion(done);
                let result = (|| {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    let result = runtime.block_on(async {
                        tokio::select! {
                            biased;
                            _ = cancelled => bail!("artwork cancelled"),
                            result = work() => result,
                        }
                    });
                    // Dropped futures cannot open another connection. Do not
                    // wait for uncancellable blocking DNS or disk operations.
                    runtime.shutdown_background();
                    result
                })();
                drop(completion);
                let _ = sender.send(result);
            })?;
        Ok(Self {
            cancel: Some(cancel),
            finished,
            result,
        })
    }

    pub(crate) fn cancel(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
    }

    pub(crate) fn finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    pub(crate) fn completion(&self) -> Arc<AtomicBool> {
        self.finished.clone()
    }

    pub(crate) fn poll(&self) -> Option<Result<T>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err(anyhow::anyhow!("artwork worker ended")))
            }
        }
    }
}

impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
    }
}

pub(crate) fn client(config: &Config) -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(crate::USER_AGENT)
        .connect_timeout(Duration::from_secs(config.settings.connect_timeout_secs))
        .timeout(Duration::from_secs(config.settings.request_timeout_secs))
        .pool_max_idle_per_host(0)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 3
                || crate::config::validate_url(attempt.url().as_str()).is_err()
            {
                attempt.error("invalid artwork redirect")
            } else {
                attempt.follow()
            }
        }))
        .build()?)
}

fn decode(bytes: &[u8]) -> Result<DynamicImage> {
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    ensure!(
        matches!(reader.format(), Some(ImageFormat::Jpeg | ImageFormat::Png)),
        "unsupported artwork format"
    );
    let mut limits = Limits::default();
    limits.max_image_width = Some(2048);
    limits.max_image_height = Some(2048);
    limits.max_alloc = Some(32 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode()?;
    Ok(if image.width() > 512 || image.height() > 512 {
        image.thumbnail(512, 512)
    } else {
        image
    })
}

fn cache_path(config: &Config, identity: &str, url: &str) -> PathBuf {
    let mut hash = Sha256::new();
    hash.update(identity.as_bytes());
    hash.update([0]);
    hash.update(url.as_bytes());
    config
        .data_dir
        .join("artwork")
        .join(format!("{:x}.png", hash.finalize()))
}

fn cached(path: &Path) -> Result<DynamicImage> {
    let file = fs::File::open(path)?;
    ensure!(
        file.metadata()?.len() <= CACHE_ENTRY_LIMIT,
        "large cache entry"
    );
    let mut bytes = Vec::new();
    (&file)
        .take(CACHE_ENTRY_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= CACHE_ENTRY_LIMIT, "large cache entry");
    let image = decode(&bytes)?;
    // A best-effort LRU timestamp; a read-only cache remains usable.
    let _ = file.set_modified(SystemTime::now());
    Ok(image)
}

fn prune(dir: &Path) -> Result<()> {
    let mut entries = vec![];
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.len() != 68
            || !name.ends_with(".png")
            || !name.as_bytes()[..64].iter().all(|b| b.is_ascii_hexdigit())
        {
            continue;
        }
        let metadata = entry.metadata()?;
        entries.push((metadata.modified()?, metadata.len(), entry.path()));
    }
    entries.sort_by_key(|(time, _, _)| *time);
    let mut bytes: u64 = entries.iter().map(|(_, size, _)| size).sum();
    let mut count = entries.len();
    for (_, size, path) in entries {
        if bytes <= CACHE_BYTES && count <= CACHE_ENTRIES {
            break;
        }
        fs::remove_file(path)?;
        bytes -= size;
        count -= 1;
    }
    Ok(())
}

fn save(path: &Path, image: &DynamicImage) -> Result<()> {
    let mut bytes = Cursor::new(Vec::new());
    image.write_to(&mut bytes, ImageFormat::Png)?;
    ensure!(
        bytes.get_ref().len() as u64 <= CACHE_ENTRY_LIMIT,
        "large encoded artwork"
    );
    let dir = path.parent().context("artwork cache directory")?;
    fs::create_dir_all(dir)?;
    let temp = dir.join(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes.get_ref())?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        prune(dir)
    })();
    let _ = fs::remove_file(temp);
    result
}

pub(crate) async fn fetch(config: &Config, identity: &str, url: &str) -> Result<DynamicImage> {
    crate::config::validate_url(url)?;
    let path = cache_path(config, identity, url);
    let cache = path.clone();
    if let Some(image) = tokio::task::spawn_blocking(move || match cached(&cache) {
        Ok(image) => Some(image),
        Err(_) => {
            let _ = fs::remove_file(cache);
            None
        }
    })
    .await?
    {
        return Ok(image);
    }
    let mut response = client(config)?.get(url).send().await?.error_for_status()?;
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    ensure!(content_type.starts_with("image/"), "non-image artwork");
    ensure!(
        response
            .content_length()
            .is_none_or(|n| n <= DOWNLOAD_LIMIT as u64),
        "large artwork"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(bytes.len() + chunk.len() <= DOWNLOAD_LIMIT, "large artwork");
        bytes.extend_from_slice(&chunk);
    }
    drop(response);
    tokio::task::spawn_blocking(move || {
        let image = decode(&bytes)?;
        // Cache failures must not hide an otherwise valid image.
        let _ = save(&path, &image);
        Ok(image)
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Server, config, response};
    use std::sync::atomic::AtomicUsize;

    pub(crate) fn picture(format: ImageFormat) -> Vec<u8> {
        let image = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            32,
            32,
            image::Rgb([220, 30, 80]),
        ));
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, format).unwrap();
        bytes.into_inner()
    }

    fn run<T>(future: impl Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    #[test]
    fn jpeg_and_png_cache_survive_restart_and_url_changes() {
        for format in [ImageFormat::Jpeg, ImageFormat::Png] {
            let hits = Arc::new(AtomicUsize::new(0));
            let requests = hits.clone();
            let bytes = picture(format);
            let server = Server::new(move |_, stream| {
                requests.fetch_add(1, Ordering::Relaxed);
                response(stream, 200, "image/example", &bytes);
            });
            let dir = tempfile::tempdir().unwrap();
            let cfg = config(dir.path());
            assert_eq!(
                run(fetch(&cfg, "station", &server.url)).unwrap().width(),
                32
            );
            let restarted = config(dir.path());
            assert_eq!(
                run(fetch(&restarted, "station", &server.url))
                    .unwrap()
                    .height(),
                32
            );
            assert_eq!(hits.load(Ordering::Relaxed), 1);
            run(fetch(&cfg, "station", &format!("{}/changed", server.url))).unwrap();
            assert_eq!(hits.load(Ordering::Relaxed), 2);
            fs::write(cache_path(&cfg, "station", &server.url), b"corrupt").unwrap();
            run(fetch(&cfg, "station", &server.url)).unwrap();
            assert_eq!(hits.load(Ordering::Relaxed), 3);
        }
    }

    #[test]
    fn rejects_non_images_corruption_large_bodies_and_large_dimensions() {
        let png = picture(ImageFormat::Png);
        let large = DynamicImage::new_rgb8(2049, 1);
        let mut dimensions = Cursor::new(Vec::new());
        large.write_to(&mut dimensions, ImageFormat::Png).unwrap();
        for (content_type, bytes) in [
            ("text/html", png),
            ("image/png", b"invalid".to_vec()),
            ("image/png", vec![0; DOWNLOAD_LIMIT + 1]),
            ("image/png", dimensions.into_inner()),
        ] {
            let server = Server::new(move |_, stream| response(stream, 200, content_type, &bytes));
            let dir = tempfile::tempdir().unwrap();
            assert!(run(fetch(&config(dir.path()), "station", &server.url)).is_err());
            assert!(!dir.path().join("artwork").exists());
        }
        // The limit also applies without Content-Length.
        let server = Server::new(|_, stream| {
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nConnection: close\r\n\r\n",
            );
            let _ = stream.write_all(&vec![0; DOWNLOAD_LIMIT + 1]);
        });
        let dir = tempfile::tempdir().unwrap();
        assert!(run(fetch(&config(dir.path()), "station", &server.url)).is_err());
    }

    #[test]
    fn follows_bounded_http_redirects_and_rejects_other_schemes() {
        let bytes = picture(ImageFormat::Png);
        let server = Server::new(move |request, stream| {
            if request.starts_with("GET /image ") {
                response(stream, 200, "image/png", &bytes);
            } else {
                let destination = if request.starts_with("GET /invalid ") {
                    "file:///tmp/art"
                } else if request.starts_with("GET /loop ") {
                    "/loop"
                } else {
                    "/image"
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 302 Found\r\nLocation: {destination}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        assert!(run(fetch(&cfg, "station", &server.url)).is_ok());
        for path in ["/loop", "/invalid"] {
            assert!(run(fetch(&cfg, "station", &format!("{}{path}", server.url))).is_err());
        }
        assert!(run(fetch(&cfg, "station", "file:///tmp/art")).is_err());
    }

    #[test]
    fn cancellation_closes_a_stalled_connection_before_completion() {
        let (opened, ready) = mpsc::channel();
        let (closed, released) = mpsc::channel();
        let server = Server::new(move |_, stream| {
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 100\r\n\r\n",
                )
                .unwrap();
            opened.send(()).unwrap();
            let mut byte = [0];
            if matches!(stream.read(&mut byte), Ok(0) | Err(_)) {
                let _ = closed.send(());
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        let url = server.url.clone();
        let mut task =
            Task::start(move || async move { fetch(&cfg, "station", &url).await.map(|_| ()) })
                .unwrap();
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        task.cancel();
        released.recv_timeout(Duration::from_secs(1)).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while !task.finished() && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(task.finished());
    }

    #[test]
    fn cancellation_does_not_wait_for_blocking_work() {
        let (started, ready) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let mut task = Task::start(move || async move {
            tokio::task::spawn_blocking(move || {
                started.send(()).unwrap();
                gate.recv().unwrap();
            })
            .await?;
            Ok(())
        })
        .unwrap();
        ready.recv_timeout(Duration::from_secs(1)).unwrap();
        task.cancel();
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while !task.finished() && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        let finished = task.finished();
        release.send(()).unwrap();
        assert!(finished, "cancellation must not wait for a blocking task");
    }

    #[test]
    fn cache_prunes_by_recency_and_handles_read_only_publication_failure() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        let image = decode(&picture(ImageFormat::Png)).unwrap();
        let oldest = cache_path(&cfg, "old", "http://old");
        save(&oldest, &image).unwrap();
        fs::File::open(&oldest)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH)
            .unwrap();
        for index in 0..CACHE_ENTRIES {
            save(
                &cache_path(&cfg, &index.to_string(), "http://image"),
                &image,
            )
            .unwrap();
        }
        assert!(!oldest.exists());
        assert_eq!(
            fs::read_dir(cfg.data_dir.join("artwork")).unwrap().count(),
            CACHE_ENTRIES
        );
        // A file where the cache directory belongs must not prevent display.
        let blocked = tempfile::tempdir().unwrap();
        let cfg = config(blocked.path());
        fs::write(cfg.data_dir.join("artwork"), b"blocked").unwrap();
        let bytes = picture(ImageFormat::Png);
        let server = Server::new(move |_, stream| response(stream, 200, "image/png", &bytes));
        assert!(run(fetch(&cfg, "station", &server.url)).is_ok());
    }
}
