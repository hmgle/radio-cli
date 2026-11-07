use super::*;
use crate::{config::Config, store::Store};
use anyhow::Result;
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
    let e = store.podcast_episode(id)?;
    let final_path = download_path(config, id);
    fs::create_dir_all(final_path.parent().unwrap())?;
    let part = final_path.with_extension("part");
    let exists = previous.is_some();
    let mut d = previous.unwrap_or_else(|| Download {
        episode_id: id.into(),
        ..Default::default()
    });
    if d.state == "complete" && std::path::Path::new(&d.path).is_file() {
        return Ok(d);
    }
    if final_path.is_file()
        && d.bytes > 0
        && fs::metadata(&final_path)?.len() == d.bytes
        && !part.exists()
    {
        d.state = "complete".into();
        d.error = None;
        store.podcast_download_update(&d)?;
        return Ok(d);
    }
    d.state = "downloading".into();
    d.error = None;
    d.path = final_path.to_string_lossy().into_owned();
    if exists {
        store.podcast_download_update(&d)?;
    } else {
        store.podcast_save_download(&d)?;
    }
    let result = (|| -> Result<()> {
        let available = fs2::available_space(final_path.parent().unwrap())?;
        let existing = fs::metadata(&part).map_or(0, |m| m.len());
        if available
            < (200u64 * 1024 * 1024).saturating_add(e.bytes.unwrap_or(0).saturating_sub(existing))
        {
            return Err(DomainError::new(ErrorCode::InsufficientSpace).into());
        }
        let offset = if d.url == e.url
            && (d.etag.as_ref().is_some_and(|v| !v.starts_with("W/")) || d.modified.is_some())
        {
            existing
        } else {
            0
        };
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
        let mut r = req
            .send()
            .map_err(|_| error(crate::t!("podcast_network")))?;
        if r.status() == 416 {
            r = client
                .get(&e.url)
                .header("Accept-Encoding", "identity")
                .send()
                .map_err(|_| error(crate::t!("podcast_network")))?;
        }
        if !r.status().is_success() {
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
        d.bytes = if append { offset } else { 0 };
        store.podcast_download_update(&d)?;
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .append(append)
            .truncate(!append)
            .open(&part)?;
        let mut buf = [0; 65536];
        let mut saved = Instant::now();
        let start = d.bytes;
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
                store.podcast_download_update(&d)?;
                saved = Instant::now();
            }
            let n = r
                .read(&mut buf)
                .map_err(|_| error(crate::t!("podcast_network")))?;
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
    if let Err(e) = &result {
        if !matches!(d.state.as_str(), "paused" | "cancelled") {
            d.state = if stop.load(Ordering::Relaxed) {
                "queued"
            } else {
                "failed"
            }
            .into();
        }
        d.error = Some(crate::clean_text(&format!("{e}")));
    }
    if let Err(update) = store.podcast_download_update(&d) {
        result?;
        return Err(update);
    }
    result?;
    Ok(d)
}
