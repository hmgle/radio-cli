//! Seekable, incremental episode recognition. Partial coverage is never a full cache.
use super::{Episode, EpisodeMedia, error, transcript};
use crate::{
    config::Config,
    speech::{ChunkRecognizer, Segment, decode_chunk},
    store::Store,
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

const WINDOW: u64 = 15_000;
const CONTEXT: u64 = 1_000;

pub(crate) struct Captions {
    pub episode: Episode,
    position: AtomicU64,
    pending: Mutex<PendingCaptions>,
}
struct PendingCaptions {
    // Retain a sorted baseline for a player returning after subtitles were off
    // or another episode was selected. Normal updates carry only new cues.
    history: Vec<Segment>,
    update: Option<CaptionUpdate>,
}
pub(crate) struct CaptionUpdate {
    pub reset: bool,
    pub segments: Vec<Segment>,
}
impl Captions {
    pub fn new(episode: Episode) -> Self {
        Self {
            position: AtomicU64::new(episode.position_ms),
            episode,
            pending: Mutex::new(PendingCaptions {
                history: vec![],
                update: Some(CaptionUpdate {
                    reset: true,
                    segments: vec![],
                }),
            }),
        }
    }
    pub fn follow(&self, episode: &Episode) {
        if self.episode.id == episode.id && self.episode.media() == episode.media() {
            self.position.store(episode.position_ms, Ordering::Relaxed);
        }
    }
    pub fn take(&self, reset: bool) -> Option<CaptionUpdate> {
        let mut pending = self.pending.lock().unwrap();
        if reset {
            pending.update = None;
            Some(CaptionUpdate {
                reset: true,
                segments: pending.history.clone(),
            })
        } else {
            pending.update.take()
        }
    }
    fn reset(&self, segments: &[Segment]) {
        let mut pending = self.pending.lock().unwrap();
        pending.history = segments.to_vec();
        pending.update = Some(CaptionUpdate {
            reset: true,
            segments: segments.to_vec(),
        });
    }
    fn append(&self, segment: Segment) {
        let mut pending = self.pending.lock().unwrap();
        let key = (segment.start_ms, segment.end_ms);
        let at = pending
            .history
            .partition_point(|s| (s.start_ms, s.end_ms) <= key);
        pending.history.insert(at, segment.clone());
        pending
            .update
            .get_or_insert_with(|| CaptionUpdate {
                reset: false,
                segments: vec![],
            })
            .segments
            .push(segment);
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Partial {
    version: u8,
    media: EpisodeMedia,
    source: String,
    windows: BTreeSet<u64>,
    end_ms: Option<u64>,
    segments: Vec<Segment>,
}
impl Partial {
    fn next(&self, position: u64) -> Option<u64> {
        let from = position / WINDOW;
        let limit = self.end_ms.map(|end| end.div_ceil(WINDOW));
        if limit.is_none_or(|end| from < end) {
            let mut index = from;
            while self.windows.contains(&index) {
                index += 1;
            }
            if limit.is_none_or(|end| index < end) {
                return Some(index);
            }
        }
        // Once the end is reached, fill earlier gaps, including before resume.
        (0..limit.unwrap_or(from)).find(|i| !self.windows.contains(i))
    }
}
fn path(config: &Config, episode: &Episode) -> PathBuf {
    transcript::path(config, episode).with_extension("partial.json")
}
fn source(config: &Config) -> String {
    let s = &config.settings.subtitles;
    format!("{}:{}:{}", s.model, s.language, s.max_segment_secs)
}
fn read(config: &Config, episode: &Episode) -> Result<Option<Partial>> {
    let path = path(config, episode);
    if !path.exists() {
        return Ok(None);
    }
    let mut partial: Partial = serde_json::from_slice(&transcript::read(&path)?)?;
    if partial.version != 1 || partial.media != episode.media() {
        return Ok(None);
    }
    ensure!(
        partial.windows.len() <= 100_000 && partial.windows.iter().all(|i| *i <= u64::MAX / WINDOW),
        error(crate::t!("podcast_subtitle_invalid"))
    );
    transcript::validate(&mut partial.segments)?;
    Ok(Some(partial))
}
pub(super) fn load_partial(config: &Config, episode: &Episode) -> Result<Option<Vec<Segment>>> {
    Ok(read(config, episode)?.map(|p| p.segments))
}
fn save(config: &Config, episode: &Episode, partial: &Partial) -> Result<()> {
    let path = path(config, episode);
    fs::create_dir_all(path.parent().unwrap())?;
    let tmp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let data = serde_json::to_vec(partial)?;
        ensure!(
            data.len() <= 10 * 1024 * 1024,
            error(crate::t!("podcast_subtitle_invalid"))
        );
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(&data)?;
        file.sync_all()?;
        fs::rename(&tmp, &path)?;
        Ok(())
    })();
    let _ = fs::remove_file(tmp);
    result
}

pub(crate) fn generate(
    config: &Config,
    store: &Store,
    captions: &Captions,
    stop: &Arc<AtomicBool>,
    progress: &AtomicU64,
) -> Result<serde_json::Value> {
    let episode = &captions.episode;
    let mut partial = read(config, episode)?
        .filter(|p| p.source == source(config))
        .unwrap_or_else(|| Partial {
            version: 1,
            media: episode.media(),
            source: source(config),
            ..Default::default()
        });
    if !partial.segments.is_empty() {
        captions.reset(&partial.segments);
    }
    let local = store
        .podcast_download(&episode.id)?
        .filter(|d| d.offline_path(episode).is_some());
    let input = if let Some(d) = local {
        d.path
    } else {
        super::feed::url(&episode.url, None)?.to_string()
    };
    let mut recognizer = ChunkRecognizer::start(config, stop)?;
    while let Some(index) = partial.next(captions.position.load(Ordering::Relaxed)) {
        transcript::check(stop)?;
        ensure!(
            partial.windows.len() < 100_000,
            error(crate::t!("podcast_subtitle_invalid"))
        );
        ensure!(
            store.podcast_episode(&episode.id)?.media() == episode.media(),
            error(crate::t!("podcast_subtitle_changed"))
        );
        let origin = index * WINDOW;
        let start = origin.saturating_sub(CONTEXT);
        let length = origin - start + WINDOW + CONTEXT;
        progress.store(origin, Ordering::Relaxed);
        let audio = decode_chunk(config, &input, start, length, stop).map_err(|e| {
            if stop.load(Ordering::Relaxed) {
                return e;
            }
            e.context(crate::t!("podcast_subtitle_window_failed"))
        })?;
        if audio.duration_ms.saturating_add(2) < length {
            let end = start + audio.duration_ms;
            partial.end_ms = Some(partial.end_ms.map_or(end, |previous| previous.min(end)));
        }
        // Retain only the centre window's cues; overlap supplies speech context.
        let mut received = vec![];
        recognizer.recognize(&audio, stop, |mut segment| {
            let midpoint = segment.start_ms + (segment.end_ms - segment.start_ms) / 2;
            if (origin..origin + WINDOW).contains(&midpoint) {
                segment.start_ms = segment.start_ms.max(origin);
                segment.end_ms = segment.end_ms.min(origin + WINDOW);
                transcript::validate(std::slice::from_mut(&mut segment))?;
                ensure!(
                    partial.segments.len() + received.len() < 100_000,
                    error(crate::t!("podcast_subtitle_invalid"))
                );
                captions.append(segment.clone());
                received.push(segment);
            }
            Ok(())
        })?;
        transcript::check(stop)?;
        ensure!(
            store.podcast_episode(&episode.id)?.media() == episode.media(),
            error(crate::t!("podcast_subtitle_changed"))
        );
        merge(&mut partial.segments, received);
        partial.windows.insert(index);
        save(config, episode, &partial)?;
        progress.store(origin + WINDOW, Ordering::Relaxed);
    }
    transcript::save(
        config,
        episode,
        "local-asr".into(),
        partial.segments.clone(),
        stop,
    )?;
    let _ = fs::remove_file(path(config, episode));
    Ok(serde_json::json!({"id":episode.id,"segments":partial.segments.len(),"title":episode.title}))
}

fn merge(history: &mut Vec<Segment>, mut received: Vec<Segment>) {
    received.sort_by_key(|s| (s.start_ms, s.end_ms));
    if received.first().is_none_or(|first| {
        history
            .last()
            .is_none_or(|last| (last.start_ms, last.end_ms) <= (first.start_ms, first.end_ms))
    }) {
        history.extend(received);
    } else {
        for cue in received {
            let at =
                history.partition_point(|s| (s.start_ms, s.end_ms) <= (cue.start_ms, cue.end_ms));
            history.insert(at, cue);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cue(start: u64, text: &str) -> Segment {
        Segment {
            epoch: 0,
            start_ms: start,
            end_ms: start + 500,
            text: text.into(),
        }
    }

    #[test]
    fn caption_updates_coalesce_only_new_cues_and_merge_backwards_seek_results() {
        let captions = Captions::new(Episode::default());
        captions.reset(&[cue(60_000, "cached")]);
        captions.append(cue(61_000, "new"));
        let first = captions.take(false).unwrap();
        assert!(first.reset);
        assert_eq!(first.segments.len(), 2);
        captions.append(cue(15_000, "seek"));
        captions.append(cue(16_000, "next"));
        let next = captions.take(false).unwrap();
        assert!(!next.reset);
        assert_eq!(next.segments.len(), 2, "do not resend the previous history");
        let mut history = first.segments;
        merge(&mut history, next.segments);
        assert_eq!(
            history.iter().map(|s| s.start_ms).collect::<Vec<_>>(),
            [15_000, 16_000, 60_000, 61_000]
        );
        merge(
            &mut history,
            vec![cue(63_000, "later"), cue(62_000, "earlier")],
        );
        assert_eq!(
            history.iter().map(|s| s.start_ms).collect::<Vec<_>>(),
            [15_000, 16_000, 60_000, 61_000, 62_000, 63_000]
        );
        assert!(captions.take(false).is_none());
        let restored = captions.take(true).unwrap();
        assert!(restored.reset);
        assert_eq!(
            restored
                .segments
                .iter()
                .map(|s| s.start_ms)
                .collect::<Vec<_>>(),
            [15_000, 16_000, 60_000, 61_000],
            "restore a complete baseline when the player rejoins"
        );
    }

    #[test]
    fn seeks_for_other_episodes_or_changed_media_do_not_redirect_the_job() {
        let episode = Episode {
            id: "target".into(),
            url: "https://example.test/audio".into(),
            position_ms: 60_000,
            ..Default::default()
        };
        let captions = Captions::new(episode.clone());
        let mut other = episode.clone();
        other.id = "next episode".into();
        other.position_ms = 120_000;
        captions.follow(&other);
        assert_eq!(captions.position.load(Ordering::Relaxed), 60_000);
        other.id = episode.id;
        other.url.push_str("?changed");
        captions.follow(&other);
        assert_eq!(captions.position.load(Ordering::Relaxed), 60_000);
        other.url = episode.url;
        captions.follow(&other);
        assert_eq!(captions.position.load(Ordering::Relaxed), 120_000);
    }
    #[test]
    fn planning_prioritizes_seeks_and_fills_resume_gaps_before_completing() {
        let mut partial = Partial::default();
        assert_eq!(partial.next(3_601_000), Some(240));
        partial.windows.insert(240);
        assert_eq!(partial.next(3_601_000), Some(241));
        assert_eq!(partial.next(450_000), Some(30));
        partial.end_ms = Some(3_615_000);
        assert_eq!(partial.next(3_601_000), Some(0));
        partial.windows.extend(0..240);
        assert_eq!(partial.next(3_601_000), None);
    }
    #[test]
    fn partial_cache_preserves_silent_coverage_and_rejects_changed_media() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let mut episode = Episode {
            id: "test".into(),
            url: "https://example.test/audio".into(),
            ..Default::default()
        };
        let partial = Partial {
            version: 1,
            media: episode.media(),
            windows: [0, 1].into(),
            ..Default::default()
        };
        save(&config, &episode, &partial).unwrap();
        assert_eq!(read(&config, &episode).unwrap().unwrap().next(0), Some(2));
        assert_eq!(load_partial(&config, &episode).unwrap(), Some(vec![]));
        assert!(!transcript::path(&config, &episode).exists());
        episode.url.push_str("?changed");
        assert!(load_partial(&config, &episode).unwrap().is_none());
    }
}
