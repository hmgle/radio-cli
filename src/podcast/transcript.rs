//! Finite episode captions use media time, never the live capture clock.
use super::{Episode, EpisodeMedia, error};
use crate::{
    config::Config,
    error::{DomainError, ErrorCode},
    speech::Segment,
    store::Store,
};
use anyhow::{Result, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub(crate) const DOWNLOADING: u64 = u64::MAX;

const LIMIT: u64 = 10 * 1024 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transcript {
    pub url: String,
    pub mime: String,
    pub language: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Cache {
    version: u8,
    media: EpisodeMedia,
    source: String,
    segments: Vec<Segment>,
}

fn invalid() -> anyhow::Error {
    error(crate::t!("podcast_subtitle_invalid"))
}
pub(super) fn check(stop: &AtomicBool) -> Result<()> {
    ensure!(
        !stop.load(Ordering::Relaxed),
        DomainError::new(ErrorCode::Cancelled)
    );
    Ok(())
}
pub(super) fn path(config: &Config, episode: &Episode) -> PathBuf {
    // IDs normally are UUIDs; hash anyway so imported or corrupt IDs cannot escape.
    use sha2::{Digest, Sha256};
    let key = format!("{:x}", Sha256::digest(episode.id.as_bytes()));
    config
        .data_dir
        .join("podcasts/transcripts")
        .join(format!("{key}.json"))
}
pub(super) fn read(path: &Path) -> Result<Vec<u8>> {
    let mut data = vec![];
    fs::File::open(path)?
        .take(LIMIT + 1)
        .read_to_end(&mut data)?;
    ensure!(data.len() as u64 <= LIMIT, invalid());
    Ok(data)
}
pub(super) fn validate(segments: &mut [Segment]) -> Result<()> {
    ensure!(segments.len() <= 100_000, invalid());
    for s in segments.iter_mut() {
        ensure!(s.end_ms > s.start_ms, invalid());
        s.epoch = 0;
        s.text = crate::clean_text(&s.text);
        ensure!(!s.text.trim().is_empty(), invalid());
    }
    segments.sort_by_key(|s| (s.start_ms, s.end_ms));
    Ok(())
}
fn cached(config: &Config, episode: &Episode) -> Result<Option<Vec<Segment>>> {
    let p = path(config, episode);
    if !p.exists() {
        return Ok(None);
    }
    let mut c: Cache = serde_json::from_slice(&read(&p)?)?;
    if c.version != 1 || c.media != episode.media() {
        return Ok(None);
    }
    // Explicit imports and generated captions stay usable offline. Feed captions
    // are invalidated when their source disappears or changes.
    if c.source.starts_with("http") && !episode.transcripts.iter().any(|t| t.url == c.source) {
        return Ok(None);
    }
    validate(&mut c.segments)?;
    Ok(Some(c.segments))
}
pub(super) fn save(
    config: &Config,
    episode: &Episode,
    source: String,
    mut segments: Vec<Segment>,
    stop: &AtomicBool,
) -> Result<Vec<Segment>> {
    check(stop)?;
    validate(&mut segments)?;
    let p = path(config, episode);
    fs::create_dir_all(p.parent().unwrap())?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(p.with_extension("lock"))?;
    lock.lock_exclusive()?;
    check(stop)?;
    // A slow automatic fetch must never overwrite an explicit import or ASR
    // result that finished while its request was in flight.
    if source.starts_with("http")
        && let Ok(Some(existing)) = cached(config, episode)
    {
        return Ok(existing);
    }
    let tmp = p.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let data = serde_json::to_vec(&Cache {
            version: 1,
            media: episode.media(),
            source,
            segments: segments.clone(),
        })?;
        ensure!(data.len() as u64 <= LIMIT, invalid());
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(&data)?;
        file.sync_all()?;
        check(stop)?;
        fs::rename(&tmp, &p)?;
        Ok(segments)
    })();
    let _ = fs::remove_file(tmp);
    result
}

/// SRT and WebVTT cues. Layout/style metadata is ignored; text is always plain.
pub fn parse(data: &[u8]) -> Result<Vec<Segment>> {
    let text = std::str::from_utf8(data)
        .map_err(|_| invalid())?
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n");
    ensure!(data.len() as u64 <= LIMIT, invalid());
    let mut segments = vec![];
    for block in text.split("\n\n") {
        let lines: Vec<_> = block.lines().collect();
        if lines.first().is_none_or(|l| {
            l.starts_with("NOTE") || l.starts_with("STYLE") || l.starts_with("REGION")
        }) {
            continue;
        }
        let Some(index) = lines.iter().position(|l| l.contains("-->")) else {
            continue;
        };
        let (start, end) = lines[index].split_once("-->").ok_or_else(invalid)?;
        let start_ms = timestamp(start.trim())?;
        let end_ms = timestamp(end.split_whitespace().next().ok_or_else(invalid)?)?;
        let body = lines[index + 1..].join("\n");
        let body = scraper::Html::parse_fragment(&body)
            .root_element()
            .text()
            .collect::<Vec<_>>()
            .join("");
        let body = body.split_whitespace().collect::<Vec<_>>().join(" ");
        segments.push(Segment {
            epoch: 0,
            start_ms,
            end_ms,
            text: body,
        });
    }
    ensure!(!segments.is_empty(), invalid());
    validate(&mut segments)?;
    Ok(segments)
}
fn timestamp(s: &str) -> Result<u64> {
    let normalized = s.replace(',', ".");
    let fields: Vec<_> = normalized.split(':').collect();
    ensure!((2..=3).contains(&fields.len()), invalid());
    let mut total = 0u64;
    for (i, part) in fields.iter().enumerate() {
        let value = if i + 1 == fields.len() {
            let (whole, fraction) = part.split_once('.').ok_or_else(invalid)?;
            ensure!(
                fraction.len() == 3 && fraction.bytes().all(|c| c.is_ascii_digit()),
                invalid()
            );
            let seconds = whole.parse::<u64>().map_err(|_| invalid())?;
            ensure!(seconds < 60, invalid());
            seconds * 1000 + fraction.parse::<u64>().map_err(|_| invalid())?
        } else {
            let n = part.parse::<u64>().map_err(|_| invalid())?;
            ensure!(i == 0 && fields.len() == 3 || n < 60, invalid());
            n.checked_mul(1000).ok_or_else(invalid)?
        };
        total = total
            .checked_mul(60)
            .and_then(|t| t.checked_add(value))
            .ok_or_else(invalid)?;
    }
    Ok(total)
}

pub(crate) fn load(
    config: &Config,
    episode: &Episode,
    stop: &Arc<AtomicBool>,
) -> Result<Option<Vec<Segment>>> {
    check(stop)?;
    if let Ok(Some(segments)) = cached(config, episode) {
        return Ok(Some(segments));
    }
    if let Ok(Some(segments)) = super::progressive::load_partial(config, episode) {
        return Ok(Some(segments));
    }
    let mut last_error = None;
    for t in &episode.transcripts {
        if !matches!(
            t.mime.as_str(),
            "text/vtt" | "application/x-subrip" | "text/srt"
        ) {
            continue;
        }
        let result = (|| {
            check(stop)?;
            let url = super::feed::url(&t.url, None)?;
            let response = super::feed::client(config)?
                .get(url)
                .send()
                .map_err(super::feed::network_error)?;
            let (_, data) = super::feed::body(response, stop)?;
            save(config, episode, t.url.clone(), parse(&data)?, stop)
        })();
        match result {
            Ok(s) => return Ok(Some(s)),
            Err(e) => {
                check(stop)?;
                last_error = Some(e);
            }
        }
    }
    if let Some(e) = last_error {
        return Err(e);
    }
    Ok(None)
}

fn prepare_audio(
    config: &Config,
    store: &Store,
    episode: &Episode,
    stop: &Arc<AtomicBool>,
    progress: Option<&std::sync::atomic::AtomicU64>,
) -> Result<super::Download> {
    let download = store
        .podcast_download(&episode.id)?
        .filter(|d| d.offline_path(episode).is_some());
    let download = if let Some(download) = download {
        download
    } else {
        if let Some(progress) = progress {
            progress.store(DOWNLOADING, Ordering::Relaxed);
        } else {
            eprintln!("{}", crate::t!("podcast_subtitle_downloading"));
        }
        super::download(config, &episode.id, stop)?
    };
    ensure!(
        download.matches_media(episode),
        error(crate::t!("podcast_subtitle_changed"))
    );
    if let Some(progress) = progress {
        progress.store(0, Ordering::Relaxed);
    }
    Ok(download)
}

pub(crate) fn manage(
    config: &Config,
    store: &Store,
    id: &str,
    file: Option<&Path>,
    transcribe: bool,
    stop: &Arc<AtomicBool>,
    progress: Option<&std::sync::atomic::AtomicU64>,
) -> Result<serde_json::Value> {
    let episode = store.podcast_episode(id)?;
    let segments = if let Some(file) = file {
        save(
            config,
            &episode,
            "import".into(),
            parse(&read(file)?)?,
            stop,
        )?
    } else if transcribe {
        crate::speech::check_transcription(config, stop)?;
        let download = prepare_audio(config, store, &episode, stop, progress)?;
        let segments =
            crate::speech::transcribe(config, Path::new(&download.path), stop, progress)?;
        // A refresh may have replaced the enclosure while inference was running.
        ensure!(
            store.podcast_episode(id)?.media() == episode.media(),
            error(crate::t!("podcast_subtitle_changed"))
        );
        save(config, &episode, "local-asr".into(), segments, stop)?
    } else {
        load(config, &episode, stop)?.ok_or_else(|| error(crate::t!("podcast_subtitle_missing")))?
    };
    Ok(serde_json::json!({"id":id,"segments":segments.len()}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transcription_prepares_audio_once_without_requiring_a_separate_download() {
        use super::super::{Feed, Preview};
        use crate::test_support::{Server, response};
        let audio = crate::test_support::fixture("libmp3lame", "mp3");
        let server = Server::new(move |_, s| response(s, 200, "audio/mpeg", &audio));
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let store = Store::open(&config.data_dir.join("radio.db")).unwrap();
        let p = Preview {
            feed: Feed {
                url: format!("{}/rss", server.url),
                ..Default::default()
            },
            episodes: vec![Episode {
                title: "Local episode".into(),
                url: format!("{}/audio", server.url),
                ..Default::default()
            }],
        };
        let f = store.podcast_subscribe(&p, &p.feed.url).unwrap();
        let episode = store.podcast_episodes(&f.id).unwrap().remove(0);
        let stop = Arc::new(AtomicBool::new(false));
        let progress = std::sync::atomic::AtomicU64::new(99);
        let d = prepare_audio(&config, &store, &episode, &stop, Some(&progress)).unwrap();
        assert_eq!(d.state, "complete");
        assert!(d.matches_media(&episode));
        assert_eq!(progress.load(Ordering::Relaxed), 0);
        drop(server);
        let cached = prepare_audio(&config, &store, &episode, &stop, None).unwrap();
        assert_eq!(d.path, cached.path, "reuse current downloads offline");
    }

    #[test]
    fn imports_are_atomic_offline_and_bound_to_media() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let mut episode = Episode {
            id: "../unsafe-id".into(),
            url: "https://example.test/audio".into(),
            ..Default::default()
        };
        let stop = AtomicBool::new(false);
        let captions = parse(b"00:00.000 --> 00:01.000\nHello").unwrap();
        save(&config, &episode, "import".into(), captions.clone(), &stop).unwrap();
        assert_eq!(cached(&config, &episode).unwrap(), Some(captions.clone()));
        assert!(path(&config, &episode).starts_with(config.data_dir.join("podcasts/transcripts")));
        stop.store(true, Ordering::Relaxed);
        assert!(save(&config, &episode, "import".into(), captions, &stop).is_err());
        assert_eq!(cached(&config, &episode).unwrap().unwrap()[0].text, "Hello");
        episode.bytes = Some(123);
        assert!(cached(&config, &episode).unwrap().is_none());
    }

    #[test]
    fn feed_subtitles_fall_back_then_cache_without_another_request() {
        use crate::test_support::{Server, response};
        let server = Server::new(|request, stream| {
            if request.contains("/bad") {
                response(stream, 200, "text/vtt", b"broken");
            } else {
                response(
                    stream,
                    200,
                    "text/vtt",
                    b"WEBVTT\n\n00:00.000 --> 00:02.000\nHello",
                );
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let mut episode = Episode {
            id: "one".into(),
            url: "https://example.test/audio".into(),
            transcripts: ["bad", "good"]
                .iter()
                .map(|p| Transcript {
                    url: format!("{}/{p}", server.url),
                    mime: "text/vtt".into(),
                    language: None,
                })
                .collect(),
            ..Default::default()
        };
        let stop = Arc::new(AtomicBool::new(false));
        assert_eq!(
            load(&config, &episode, &stop).unwrap().unwrap()[0].text,
            "Hello"
        );
        drop(server);
        assert_eq!(load(&config, &episode, &stop).unwrap().unwrap().len(), 1);
        episode.transcripts.clear();
        assert!(cached(&config, &episode).unwrap().is_none());
    }

    #[test]
    fn parses_vtt_and_srt_with_identifiers_settings_markup_and_unicode() {
        let s = parse(b"\xef\xbb\xbfWEBVTT\r\n\r\ncue\r\n00:01.000 --> 00:03.500 align:start\r\n<v Alice>Hello &amp; welcome</v>\r\nagain\r\n\r\nNOTE ignored\r\n00:00.000 --> 00:01.000\r\nignore").unwrap();
        assert_eq!((s[0].start_ms, s[0].end_ms), (1000, 3500));
        assert_eq!(s[0].text, "Hello & welcome again");
        let s = parse("1\n01:02:03,004 --> 01:02:04,005\n中文字幕".as_bytes()).unwrap();
        assert_eq!(s[0].start_ms, 3723004);
        assert_eq!(s[0].text, "中文字幕");
        for bad in [
            "text only",
            "00:61.000 --> 00:62.000\nx",
            "00:02.000 --> 00:01.000\nx",
            "00:01.000 --> 00:02.000\n",
        ] {
            assert!(parse(bad.as_bytes()).is_err(), "{bad}");
        }
    }
}
