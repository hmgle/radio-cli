use super::*;
use crate::{config::Config, store::Store};
use anyhow::{Context, Result};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
pub fn download_path(config: &Config, id: &str) -> PathBuf {
    config
        .settings
        .podcasts
        .directory
        .clone()
        .unwrap_or_else(|| config.data_dir.join("podcasts"))
        .join(format!("{id}.audio"))
}
pub fn download(config: &Config, id: &str, stop: &Arc<AtomicBool>) -> Result<Download> {
    run(config, id, stop, None)
}
pub(super) fn foreground(
    config: &Config,
    store: &Store,
    id: &str,
    stop: &Arc<AtomicBool>,
) -> Result<Download> {
    Runtime::enqueue(store, id)?;
    loop {
        let result = download(config, id, stop);
        if result.is_ok() {
            return result;
        }
        let mut current = store
            .podcast_download(id)?
            .ok_or_else(|| error(crate::t!("podcast_not_found")))?;
        if stop.load(Ordering::Relaxed) {
            current.state = "paused".into();
            current.retry_at = None;
            current.generation = current.generation.saturating_add(1);
            store.podcast_save_download(&current)?;
            return Err(DomainError::new(ErrorCode::Cancelled).into());
        }
        let retry = current
            .retry_at
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());
        if current.state != "queued" || retry.is_none() {
            return result;
        }
        if expected_download_progress() {
            eprintln!(
                "{}",
                crate::t!("podcast_download_retry", "time" => current.retry_at.as_deref().unwrap())
            );
        }
        while !stop.load(Ordering::Relaxed) && chrono::Utc::now() < retry.unwrap() {
            std::thread::sleep(Duration::from_millis(100));
        }
        if stop.load(Ordering::Relaxed) {
            current.state = "paused".into();
            current.retry_at = None;
            current.generation = current.generation.saturating_add(1);
            store.podcast_save_download(&current)?;
            return Err(DomainError::new(ErrorCode::Cancelled).into());
        }
    }
}
pub(super) fn scheduled(
    config: &Config,
    expected: &Download,
    stop: &Arc<AtomicBool>,
) -> Result<Download> {
    run(config, &expected.episode_id, stop, Some(expected))
}
fn run(
    config: &Config,
    id: &str,
    stop: &Arc<AtomicBool>,
    expected: Option<&Download>,
) -> Result<Download> {
    let store = Store::open(&config.data_dir.join("radio.db"))?;
    let previous = if let Some(expected) = expected {
        Some(
            store
                .podcast_claim_download(expected)?
                .ok_or_else(|| DomainError::new(ErrorCode::Cancelled))?,
        )
    } else {
        store.podcast_download(id)?
    };
    let final_path = download_path(config, id);
    let part = final_path.with_extension("part");
    let exists = previous.is_some();
    let mut d = previous.unwrap_or_else(|| Download {
        episode_id: id.into(),
        ..Default::default()
    });
    let previous_path = d.path.clone();
    d.state = "downloading".into();
    d.error = None;
    d.retry_at = None;
    d.bytes_per_second = 0;
    d.path = final_path.to_string_lossy().into_owned();
    if exists {
        store.podcast_download_update(&d)?;
    } else {
        store.podcast_save_download(&d)?;
    }
    let mut transient = false;
    let mut retry_after = 0i64;
    let result = (|| -> Result<()> {
        let e = store.podcast_episode(id)?;
        fs::create_dir_all(final_path.parent().unwrap()).with_context(|| {
            format!(
                "create download directory {}",
                final_path.parent().unwrap().display()
            )
        })?;
        if final_path.is_file()
            && previous_path == d.path
            && d.bytes > 0
            && fs::metadata(&final_path)?.len() == d.bytes
            && !part.exists()
        {
            if d.matches_media(&e) {
                d.state = "complete".into();
                return Ok(());
            }
            // Remove our obsolete completed file before recording a new media
            // snapshot, so a crash cannot relabel the old file as the new one.
            fs::remove_file(&final_path)?;
        }
        let existing = fs::metadata(&part).map_or(0, |m| m.len());
        let offset = if d.matches_media(&e)
            && (d.etag.as_ref().is_some_and(|v| !v.starts_with("W/")) || d.modified.is_some())
        {
            existing
        } else {
            0
        };
        let available = fs2::available_space(final_path.parent().unwrap())?;
        if available
            < (200u64 * 1024 * 1024).saturating_add(e.bytes.unwrap_or(0).saturating_sub(offset))
        {
            return Err(DomainError::new(ErrorCode::InsufficientSpace).into());
        }
        if existing > 0 && !d.matches_media(&e) {
            // Clear old bytes before publishing the new fingerprint. Otherwise
            // a crash before truncation could resume them as the new media.
            fs::remove_file(&part)?;
        }
        let client = super::feed::client(config)?;
        let mut req = client
            .get(super::feed::url(&e.url, None)?)
            .header("Accept-Encoding", "identity");
        if offset > 0 {
            req = req.header("Range", format!("bytes={offset}-")).header(
                "If-Range",
                d.etag
                    .as_ref()
                    .filter(|v| !v.starts_with("W/"))
                    .or(d.modified.as_ref())
                    .unwrap(),
            );
        }
        let mut r = req.send().map_err(|e| {
            transient = e.is_timeout() || e.is_connect() || e.is_body();
            super::feed::network_error(e)
        })?;
        if r.status() == 416 {
            r = client
                .get(&e.url)
                .header("Accept-Encoding", "identity")
                .send()
                .map_err(|e| {
                    transient = e.is_timeout() || e.is_connect() || e.is_body();
                    super::feed::network_error(e)
                })?;
        }
        if !r.status().is_success() {
            transient = matches!(r.status().as_u16(), 408 | 429 | 500 | 502 | 503 | 504);
            retry_after = r
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| {
                    v.parse().ok().or_else(|| {
                        chrono::DateTime::parse_from_rfc2822(v)
                            .ok()
                            .map(|d| (d.to_utc() - chrono::Utc::now()).num_seconds())
                    })
                })
                .unwrap_or(0);
            return Err(error(&format!(
                "{} {}",
                crate::t!("podcast_http"),
                r.status().as_u16()
            )));
        }
        let append = r.status() == 206 && offset > 0;
        let mut total_length = None;
        let mut expected = r.content_length();
        if r.status() == 206 {
            let range = r
                .headers()
                .get("content-range")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            let (start, end, total) = range
                .strip_prefix("bytes ")
                .and_then(|v| v.split_once('/'))
                .and_then(|(r, t)| {
                    let (a, b) = r.split_once('-')?;
                    let start = a.parse::<u64>().ok()?;
                    let end = b.parse::<u64>().ok()?;
                    let total = t.parse::<u64>().ok()?;
                    Some((start, end, total))
                })
                .filter(|&(start, end, total)| {
                    start == offset
                        && end >= start
                        && total > end
                        && expected.is_none_or(|length| length == end - start + 1)
                })
                .ok_or_else(|| error(crate::t!("podcast_invalid_range")))?;
            if !append {
                return Err(error(crate::t!("podcast_invalid_range")));
            }
            expected = Some(end - start + 1);
            total_length = Some(total);
        }
        if append {
            let validator = r.headers().get("etag").and_then(|v| v.to_str().ok());
            if d.etag
                .as_ref()
                .filter(|v| !v.starts_with("W/"))
                .is_some_and(|old| validator.is_some_and(|v| v != old))
            {
                return Err(error(crate::t!("podcast_invalid_range")));
            }
        }
        let mut file = store.podcast_download_transaction(id, |current| {
            if current.is_none_or(|current| current.generation != d.generation) {
                return Err(DomainError::new(ErrorCode::Cancelled).into());
            }
            let file = OpenOptions::new()
                .create(true)
                .write(true)
                .append(append)
                .truncate(!append)
                .open(&part)
                .with_context(|| format!("initialize download file {}", part.display()))?;
            if !append {
                // Make truncation durable before associating this path with
                // the response validators, including in the error handler.
                file.sync_all()?;
            }
            d.etag = r
                .headers()
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            d.modified = r
                .headers()
                .get("last-modified")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            d.url = e.url.clone();
            d.media = Some(e.media());
            d.bytes = if append { offset } else { 0 };
            store.podcast_download_update(&d)?;
            Ok(file)
        })?;
        let mut buf = [0; 65536];
        let mut saved = Instant::now();
        d.total_bytes = total_length
            .or_else(|| expected.and_then(|n| n.checked_add(if append { offset } else { 0 })));
        let start = d.bytes;
        let started = Instant::now();
        let show_progress = expected_download_progress();
        loop {
            if stop.load(Ordering::Relaxed) {
                return Err(DomainError::new(ErrorCode::Cancelled).into());
            }
            if saved.elapsed() >= Duration::from_secs(1) {
                if let Some(current) = store.podcast_download(id)?
                    && matches!(current.state.as_str(), "paused" | "cancelled")
                {
                    d.state = current.state;
                    return Err(DomainError::new(ErrorCode::Cancelled).into());
                }
                if fs2::available_space(final_path.parent().unwrap())? < 200 * 1024 * 1024 {
                    return Err(DomainError::new(ErrorCode::InsufficientSpace).into());
                }
                d.bytes_per_second =
                    ((d.bytes - start) as f64 / started.elapsed().as_secs_f64().max(0.001)) as u64;
                store.podcast_download_update(&d)?;
                if show_progress {
                    eprint!(
                        "\r{} / {} B · {} B/s",
                        d.bytes,
                        d.total_bytes
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "?".into()),
                        d.bytes_per_second
                    );
                }
                saved = Instant::now();
            }
            let n = r.read(&mut buf).map_err(|e| {
                transient = true;
                error(&format!("{}: {}", crate::t!("podcast_network"), e.kind()))
            })?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n])?;
            d.bytes += n as u64;
        }
        file.sync_all()?;
        drop(file);
        if expected.is_some_and(|n| n != d.bytes - start)
            || total_length.is_some_and(|n| n != d.bytes)
            || d.bytes == 0
        {
            return Err(error(crate::t!("podcast_invalid_download")));
        }
        let output = crate::process::output(
            std::process::Command::new(&config.settings.ffprobe)
                .args([
                    "-v",
                    "error",
                    "-select_streams",
                    "a:0",
                    "-show_entries",
                    "stream=codec_type",
                    "-of",
                    "csv=p=0",
                ])
                .arg(&part),
            Duration::from_secs(config.settings.probe_timeout_secs),
            stop,
        )?;
        if !String::from_utf8_lossy(&output).contains("audio") {
            return Err(error(crate::t!("podcast_invalid_download")));
        }
        if final_path.exists() {
            return Err(error(crate::t!("podcast_file_exists")));
        }
        if let Some(current) = store.podcast_download(id)?
            && matches!(current.state.as_str(), "paused" | "cancelled")
        {
            d.state = current.state;
            return Err(DomainError::new(ErrorCode::Cancelled).into());
        }
        store.podcast_download_update(&d)?;
        fs::rename(&part, &final_path)?;
        d.state = "complete".into();
        Ok(())
    })();
    if expected_download_progress() {
        eprintln!(
            "\r{} / {} B",
            d.bytes,
            d.total_bytes
                .map(|n| n.to_string())
                .unwrap_or_else(|| "?".into())
        );
    }
    if let Err(e) = &result {
        if !matches!(d.state.as_str(), "paused" | "cancelled") {
            d.state = if stop.load(Ordering::Relaxed) {
                "queued"
            } else {
                "failed"
            }
            .into();
        }
        if transient && d.retries < 3 && d.state == "failed" {
            let delay = [5i64, 30, 120][usize::from(d.retries)].max(retry_after);
            d.retries += 1;
            let delay = chrono::TimeDelta::try_seconds(delay)
                .and_then(|delay| chrono::Utc::now().checked_add_signed(delay))
                .unwrap_or(chrono::DateTime::<chrono::Utc>::MAX_UTC);
            d.retry_at = Some(delay.to_rfc3339());
            d.state = "queued".into();
        }
        d.error = Some(crate::clean_text(&format!("{e:#}")));
    }
    if let Err(update) = store.podcast_download_update(&d) {
        result?;
        return Err(update);
    }
    result?;
    Ok(d)
}

thread_local! { static CLI_PROGRESS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
pub(crate) fn set_cli_progress(enabled: bool) {
    CLI_PROGRESS.set(enabled);
}
fn expected_download_progress() -> bool {
    CLI_PROGRESS.get()
}
