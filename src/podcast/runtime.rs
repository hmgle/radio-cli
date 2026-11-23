//! Session-scoped jobs keep network and file work away from media controls.
use super::download_files::{BrowseFileProbe, DownloadFiles};
use super::*;
use crate::{
    config::Config,
    process::{Background, MediaLock},
    store::Store,
};
use anyhow::Result;
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub(crate) struct BrowseSnapshot {
    revision: u64,
    files: HashMap<String, bool>,
}

impl BrowseSnapshot {
    fn files_changed(&self, stop: &AtomicBool, probe: &BrowseFileProbe) -> bool {
        self.files
            .iter()
            .take_while(|_| !stop.load(Ordering::Relaxed))
            .any(|(path, exists)| probe.is_file(path, stop) != *exists)
    }
}

pub(crate) struct BrowsePage {
    pub value: Value,
    pub snapshot: Option<Arc<BrowseSnapshot>>,
}

struct BrowseTask {
    job: Background<Option<BrowsePage>>,
    probe: Arc<BrowseFileProbe>,
}

impl Drop for BrowseTask {
    fn drop(&mut self) {
        self.job.cancel();
        self.probe.wake_waiters();
    }
}

pub struct Runtime {
    action: Option<Background<Value>>,
    browse: Option<BrowseTask>,
    file_probe: Arc<BrowseFileProbe>,
    events: std::collections::VecDeque<Result<Value>>,
    subtitle_job: Option<Background<Value>>,
    retiring_subtitles: Vec<(String, Background<Value>)>,
    subtitle_episode: Option<String>,
    subtitle_progress: Option<Arc<std::sync::atomic::AtomicU64>>,
    captions: Option<Arc<super::progressive::Captions>>,
    player: Option<Episode>,
    downloads: HashMap<String, Background<Download>>,
    refresh: Option<Background<Value>>,
    refresh_retry_at: Option<Instant>,
    scan: Instant,
    scan_task: Option<Background<(Vec<Download>, Vec<Feed>)>>,
    cleanup: Option<Background<Value>>,
    cleanup_ids: Vec<String>,
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            action: None,
            browse: None,
            file_probe: Arc::default(),
            events: Default::default(),
            subtitle_job: None,
            retiring_subtitles: vec![],
            subtitle_episode: None,
            subtitle_progress: None,
            captions: None,
            player: None,
            downloads: HashMap::new(),
            refresh: None,
            refresh_retry_at: None,
            scan_task: None,
            cleanup: None,
            cleanup_ids: Vec::new(),
            scan: Instant::now() - Duration::from_secs(10),
        }
    }
}
impl Runtime {
    pub(crate) fn cleaning(&self, id: &str) -> bool {
        self.cleanup_ids.iter().any(|v| v == id)
    }
    pub(crate) fn subtitle_target(&self) -> Option<&str> {
        self.subtitle_episode.as_deref()
    }
    pub(crate) fn follow_captions(&mut self, episode: Option<&Episode>) {
        if let (Some(captions), Some(episode)) = (&self.captions, episode) {
            captions.follow(episode);
        }
        self.player = episode.cloned();
    }
    pub(crate) fn caption_update(
        &self,
        episode: &Episode,
        reset: bool,
    ) -> Option<super::progressive::CaptionUpdate> {
        let captions = self.captions.as_ref()?;
        if captions.episode.id != episode.id || captions.episode.media() != episode.media() {
            return None;
        }
        captions.take(reset)
    }
    pub(crate) fn subtitle_progress(&self) -> Option<u64> {
        self.subtitle_job.as_ref()?;
        self.subtitle_progress
            .as_ref()
            .map(|p| p.load(std::sync::atomic::Ordering::Relaxed))
    }
    pub(crate) fn tasks(&self) -> (usize, bool) {
        (
            self.downloads.len(),
            self.action.is_some() || self.refresh.is_some() || self.subtitle_job.is_some(),
        )
    }
    pub(crate) fn action(
        &mut self,
        config: &Config,
        action: Action,
        lock: Arc<MediaLock>,
    ) -> Result<()> {
        self.retiring_subtitles
            .retain(|(_, job)| job.poll().is_none());
        if matches!(
            &action,
            Action::Download { .. }
                | Action::Downloads {
                    action: DownloadAction::Pause { .. }
                        | DownloadAction::Resume { .. }
                        | DownloadAction::Cancel { .. }
                        | DownloadAction::Remove { .. }
                }
        ) {
            let mut target = action.clone();
            let id = target.episode_id_mut().unwrap();
            if self.subtitle_job.is_some() && self.subtitle_episode.as_ref() == Some(id)
                || self.retiring_subtitles.iter().any(|(key, _)| key == id)
            {
                return Err(error(crate::t!("podcast_subtitle_busy")));
            }
        }
        if let Action::Subtitles { ref id, .. } = action
            && self.cleaning(id)
        {
            return Err(error(crate::t!("podcast_download_busy")));
        }
        if let Action::Subtitles {
            id,
            file,
            transcribe,
        } = action
        {
            if self.subtitle_job.is_some() || !self.retiring_subtitles.is_empty() {
                return Err(error(crate::t!("podcast_subtitle_busy")));
            }
            let progress = transcribe.then(|| Arc::new(std::sync::atomic::AtomicU64::new(0)));
            let captions = if transcribe {
                let store = Store::open(&config.data_dir.join("radio.db"))?;
                let mut episode = store.podcast_episode(&id)?;
                if let Some(player) = &self.player
                    && player.id == id
                    && player.media() == episode.media()
                {
                    episode.position_ms = player.position_ms;
                }
                Some(Arc::new(super::progressive::Captions::new(episode)))
            } else {
                None
            };
            let updates = captions.clone();
            let progress_status = progress.clone();
            let target = id.clone();
            let config = config.clone();
            self.subtitle_job = Some(Background::start("radio-podcast-subtitles", move |stop| {
                let _lock = lock;
                let store = Store::open(&config.data_dir.join("radio.db"))?;
                let title = store.podcast_episode(&id)?.title;
                let result = if let Some(captions) = captions {
                    super::progressive::generate(
                        &config,
                        &store,
                        &captions,
                        &stop,
                        progress.as_deref().unwrap(),
                    )
                } else {
                    super::transcript::manage(
                        &config,
                        &store,
                        &id,
                        file.as_deref(),
                        transcribe,
                        &stop,
                        progress.as_deref(),
                    )
                };
                let mut value = result?;
                value["title"] = serde_json::json!(title);
                Ok(value)
            })?);
            self.captions = updates;
            self.subtitle_progress = progress_status;
            self.subtitle_episode = Some(target);
            return Ok(());
        }
        if action.is_query() {
            return self.browse(config, action, lock, None);
        }
        if self.action.is_some() {
            return Err(error(crate::t!("podcast_busy")));
        }
        let config = config.clone();
        self.action = Some(Background::start("radio-podcast", move |stop| {
            let _lock = lock;
            let store = Store::open(&config.data_dir.join("radio.db"))?;
            if let Action::Download { id }
            | Action::Downloads {
                action: DownloadAction::Resume { id },
            } = action
            {
                Self::enqueue(&store, &id)?;
                return Ok(serde_json::json!({"id":id,"state":"queued"}));
            }
            manage(&config, &store, action, &stop)
        })?);
        Ok(())
    }
    pub(crate) fn browse(
        &mut self,
        config: &Config,
        action: Action,
        lock: Arc<MediaLock>,
        previous: Option<Arc<BrowseSnapshot>>,
    ) -> Result<()> {
        self.cancel_browse();
        let config = config.clone();
        let probe = self.file_probe.clone();
        let job = Background::start("radio-podcast-browse", move |stop| {
            let periodic = action.polls_browse();
            let _lock = (!periodic).then_some(lock);
            let path = config.data_dir.join("radio.db");
            let store = if periodic {
                Store::open_reader(&path)?
            } else {
                Store::open(&path)?
            };
            let revision = periodic.then(|| store.podcast_revision()).transpose()?;
            if let Some(previous) = previous
                && revision == Some(previous.revision)
                && !previous.files_changed(&stop, &probe)
            {
                return Ok(None);
            }
            if stop.load(Ordering::Relaxed) {
                return Err(DomainError::new(ErrorCode::Cancelled).into());
            }
            let files = Arc::new(DownloadFiles::for_browse(stop.clone(), probe));
            let value = super::commands::manage_with_files(&config, &store, action, &stop, &files)?;
            let snapshot = revision.map(|revision| {
                Arc::new(BrowseSnapshot {
                    revision,
                    files: files.take_observations(),
                })
            });
            Ok(Some(BrowsePage { value, snapshot }))
        })?;
        self.browse = Some(BrowseTask {
            job,
            probe: self.file_probe.clone(),
        });
        Ok(())
    }

    pub(crate) fn poll_browse(&mut self) -> Option<Result<Option<BrowsePage>>> {
        let result = self.browse.as_ref()?.job.poll()?;
        self.browse = None;
        Some(result)
    }
    pub(crate) fn poll(&mut self) -> Option<Result<Value>> {
        if let Some(event) = self.events.pop_front() {
            return Some(event);
        }
        let result = self.action.as_ref()?.poll()?;
        self.action = None;
        Some(result)
    }
    pub(crate) fn poll_subtitles(&mut self) -> Option<Result<Value>> {
        let result = self.subtitle_job.as_ref()?.poll()?;
        self.subtitle_job = None;
        self.subtitle_episode = None;
        self.captions = None;
        Some(result)
    }
    pub(crate) fn cancel_subtitles(&mut self) {
        self.captions = None;
        if let Some(job) = self.subtitle_job.take() {
            job.cancel();
            self.retiring_subtitles
                .push((self.subtitle_episode.take().unwrap(), job));
        }
    }
    pub(crate) fn cancel_subtitles_for(&mut self, id: &str) -> Result<()> {
        if self
            .subtitle_episode
            .as_deref()
            .is_some_and(|active| active != id)
        {
            return Err(error(crate::t!("podcast_subtitle_busy")));
        }
        self.cancel_subtitles();
        Ok(())
    }
    pub(crate) fn cancel_browse(&mut self) {
        self.browse = None;
    }
    pub(crate) fn download_active(&self, id: &str) -> bool {
        self.cleanup_ids.iter().any(|v| v == id)
            || self.downloads.contains_key(id)
            || self.subtitle_job.is_some() && self.subtitle_episode.as_deref() == Some(id)
            || self.retiring_subtitles.iter().any(|(key, _)| key == id)
    }
    pub(crate) fn cancel(&mut self) -> bool {
        let browse = self.browse.is_some();
        self.cancel_browse();
        let action = self.action.take().is_some();
        let subtitles = self.subtitle_job.is_some();
        self.cancel_subtitles();
        action || browse || subtitles
    }
    pub(crate) fn tick(&mut self, config: &Config, lock: Arc<MediaLock>) -> Result<()> {
        self.retiring_subtitles
            .retain(|(_, job)| job.poll().is_none());
        self.downloads.retain(|_, task| task.poll().is_none());
        if let Some(result) = self.cleanup.as_ref().and_then(|t| t.poll()) {
            self.cleanup = None;
            self.cleanup_ids.clear();
            if result
                .as_ref()
                .map_or(true, |v| v["cleaned"].as_u64().unwrap_or(0) > 0)
            {
                self.events.push_back(result);
            }
        }
        if let Some(result) = self.refresh.as_ref().and_then(|t| t.poll()) {
            self.refresh = None;
            self.refresh_retry_at = result
                .as_ref()
                .err()
                .map(|_| Instant::now() + Duration::from_secs(300));
            self.events.push_back(result);
        }
        if self.scan_task.is_none() && self.scan.elapsed() >= Duration::from_secs(5) {
            self.scan = Instant::now();
            let config = config.clone();
            let lock = lock.clone();
            self.scan_task = Some(Background::start("radio-podcast-schedule", move |_| {
                let _lock = lock;
                let store = Store::open(&config.data_dir.join("radio.db"))?;
                let due = if config.settings.podcasts.refresh_hours == 0 {
                    vec![]
                } else {
                    store
                        .podcast_feeds()?
                        .into_iter()
                        .filter(|f| {
                            f.refreshed_at
                                .as_deref()
                                .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
                                .is_none_or(|at| {
                                    (chrono::Utc::now() - at.to_utc()).num_seconds()
                                        >= if f.error.is_some() {
                                            300i64
                                                .saturating_mul(1i64 << f.failures.min(8))
                                                .min(86400)
                                        } else {
                                            i64::from(config.settings.podcasts.refresh_hours) * 3600
                                        }
                                })
                        })
                        .collect()
                };
                let mut downloads = store.podcast_downloads()?;
                for d in &mut downloads {
                    if d.state == "complete" {
                        let e = store.podcast_episode(&d.episode_id)?;
                        d.cleanup_eligible =
                            e.completed && store.podcast_settings(&e.feed_id)?.auto_cleanup;
                    }
                }
                Ok((downloads, due))
            })?);
        }
        let Some(result) = self.scan_task.as_ref().and_then(|t| t.poll()) else {
            return Ok(());
        };
        self.scan_task = None;
        let (downloads, due) = result?;
        if self.cleanup.is_none() {
            let candidates: Vec<_> = downloads
                .iter()
                .filter(|d| {
                    d.cleanup_eligible
                        && !self.download_active(&d.episode_id)
                        && self.player.as_ref().is_none_or(|e| e.id != d.episode_id)
                })
                .map(|d| d.episode_id.clone())
                .collect();
            if !candidates.is_empty() {
                self.cleanup_ids = candidates.clone();
                let config = config.clone();
                let lock = lock.clone();
                self.cleanup = Some(Background::start("radio-podcast-cleanup", move |stop| {
                    let _lock = lock;
                    let store = Store::open(&config.data_dir.join("radio.db"))?;
                    let queued = store.podcast_queue()?;
                    let mut removed = 0;
                    for id in candidates {
                        if stop.load(std::sync::atomic::Ordering::Relaxed) {
                            break;
                        }
                        let e = store.podcast_episode(&id)?;
                        if e.completed
                            && store.podcast_settings(&e.feed_id)?.auto_cleanup
                            && !queued.iter().any(|q| q.id == id)
                        {
                            let expected = super::download_path(&config, &id);
                            if store
                                .podcast_download(&id)?
                                .is_some_and(|d| std::path::Path::new(&d.path) == expected)
                            {
                                manage(
                                    &config,
                                    &store,
                                    Action::Downloads {
                                        action: DownloadAction::Remove { id },
                                    },
                                    &stop,
                                )?;
                                removed += 1;
                            }
                        }
                    }
                    Ok(serde_json::json!({"cleaned":removed}))
                })?);
            }
        }
        for d in downloads {
            if self.downloads.len() >= 2 {
                break;
            }
            if matches!(d.state.as_str(), "queued" | "downloading")
                && d.retry_at
                    .as_deref()
                    .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
                    .is_none_or(|at| at <= chrono::Utc::now())
                && !self.downloads.contains_key(&d.episode_id)
                && !self
                    .retiring_subtitles
                    .iter()
                    .any(|(id, _)| id == &d.episode_id)
                && !(self.subtitle_job.is_some()
                    && self.subtitle_episode.as_deref() == Some(d.episode_id.as_str()))
            {
                let id = d.episode_id.clone();
                let config = config.clone();
                let lock = lock.clone();
                self.downloads.insert(
                    id.clone(),
                    Background::start("radio-podcast-download", move |stop| {
                        let _lock = lock;
                        super::download::scheduled(&config, &d, &stop)
                    })?,
                );
            }
        }
        if self.refresh.is_none()
            && self.refresh_retry_at.is_none_or(|at| Instant::now() >= at)
            && config.settings.podcasts.refresh_hours > 0
            && !due.is_empty()
        {
            let config = config.clone();
            self.refresh = Some(Background::start("radio-podcast-refresh", move |stop| {
                let _lock = lock;
                let store = Store::open(&config.data_dir.join("radio.db"))?;
                super::commands::refresh_feeds(&config, &store, &due, &stop)
            })?);
        }
        Ok(())
    }
    pub(crate) fn enqueue(store: &Store, id: &str) -> Result<()> {
        store.podcast_download_transaction(id, |current| {
            let episode = store.podcast_episode(id)?;
            let mut d = current.unwrap_or_else(|| Download {
                episode_id: id.into(),
                ..Default::default()
            });
            if d.offline_path(&episode).is_none() {
                d.generation = d.generation.saturating_add(1);
                d.state = "queued".into();
                d.error = None;
                d.retries = 0;
                d.retry_at = None;
                d.enqueued_at = Some(chrono::Utc::now().to_rfc3339());
                store.podcast_save_download(&d)?;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn online_episodes_load_while_another_episodes_probe_is_blocked() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let store = Store::open(&config.data_dir.join("radio.db")).unwrap();
        let subscribe = |name: &str| {
            let url = format!("https://example.test/{name}");
            store
                .podcast_subscribe(
                    &Preview {
                        feed: Feed {
                            url: url.clone(),
                            ..Default::default()
                        },
                        episodes: vec![Episode {
                            title: name.into(),
                            url: format!("{url}.mp3"),
                            ..Default::default()
                        }],
                    },
                    &url,
                )
                .unwrap()
        };
        let a = subscribe("Downloaded");
        let b = subscribe("Online");
        let episode = store.podcast_episodes(&a.id).unwrap().remove(0);
        store
            .podcast_save_download(&Download {
                episode_id: episode.id.clone(),
                state: "complete".into(),
                url: episode.url.clone(),
                media: Some(episode.media()),
                path: dir.path().join("audio").to_string_lossy().into_owned(),
                ..Default::default()
            })
            .unwrap();
        let action = |id: &str| Action::Episodes {
            id: id.into(),
            query: String::new(),
            page: LibraryQuery::default().page,
        };
        let lock = Arc::new(MediaLock::acquire(&config.runtime_dir).unwrap());
        let (entered, ready) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let wait = std::sync::Mutex::new(wait);
        let mut runtime = Runtime::default();
        Arc::get_mut(&mut runtime.file_probe).unwrap().before_probe = Some(Box::new(move || {
            entered.send(()).unwrap();
            wait.lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        }));
        runtime
            .browse(&config, action(&a.id), lock.clone(), None)
            .unwrap();
        ready.recv_timeout(Duration::from_secs(3)).unwrap();
        for _ in 0..8 {
            runtime
                .browse(&config, action(&a.id), lock.clone(), None)
                .unwrap();
        }
        runtime.browse(&config, action(&b.id), lock, None).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(result) = runtime.poll_browse() {
                let page = result.unwrap().unwrap();
                assert_eq!(page.value["items"][0]["title"], "Online");
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(ready.try_recv().is_err());
        release.send(()).unwrap();
    }

    #[test]
    fn browse_snapshot_uses_the_same_file_observation_as_library_rows() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("radio.db")).unwrap();
        let feed = store
            .podcast_subscribe(
                &Preview {
                    feed: Feed {
                        url: "https://example.test/feed".into(),
                        ..Default::default()
                    },
                    episodes: vec![Episode {
                        url: "https://example.test/audio".into(),
                        ..Default::default()
                    }],
                },
                "https://example.test/feed",
            )
            .unwrap();
        let episode = store.podcast_episodes(&feed.id).unwrap().remove(0);
        let audio = dir.path().join("local.audio");
        let path = audio.to_str().unwrap();
        store
            .podcast_save_download(&Download {
                episode_id: episode.id.clone(),
                state: "complete".into(),
                url: episode.url.clone(),
                media: Some(episode.media()),
                path: path.into(),
                ..Default::default()
            })
            .unwrap();
        let files = Arc::new(super::super::download_files::DownloadFiles::default());
        assert!(!files.is_file(path));
        std::fs::write(&audio, b"restored").unwrap();
        let page = store
            .podcast_library_with_files(&LibraryQuery::default(), &files)
            .unwrap();
        assert_eq!(page["items"][0]["download_state"], "stale");
        let filter = LibraryQuery {
            downloaded: true,
            ..Default::default()
        };
        let page = store.podcast_library_with_files(&filter, &files).unwrap();
        assert_eq!(page["total"], 0);
        assert_eq!(page["items"], serde_json::json!([]));
        let snapshot = BrowseSnapshot {
            revision: store.podcast_revision().unwrap(),
            files: files.take_observations(),
        };
        assert_eq!(snapshot.files.get(path), Some(&false));
        assert!(snapshot.files_changed(&AtomicBool::new(false), &BrowseFileProbe::default()));
        let page = store.podcast_library(&filter).unwrap();
        assert_eq!(page["total"], 1);
        assert_eq!(page["items"][0]["download_state"], "complete");
    }

    #[test]
    fn unrelated_players_do_not_consume_caption_deltas() {
        let episode = Episode {
            id: "target".into(),
            url: "https://example.test/audio".into(),
            ..Default::default()
        };
        let runtime = Runtime {
            captions: Some(Arc::new(super::super::progressive::Captions::new(
                episode.clone(),
            ))),
            ..Default::default()
        };
        let mut other = episode.clone();
        other.id = "next".into();
        assert!(runtime.caption_update(&other, false).is_none());
        let first = runtime.caption_update(&episode, false).unwrap();
        assert!(first.reset);
        assert!(runtime.caption_update(&episode, false).is_none());
        assert!(
            runtime.caption_update(&episode, true).unwrap().reset,
            "returning players can restore a baseline"
        );
        other.id = episode.id.clone();
        other.url.push_str("?changed");
        assert!(runtime.caption_update(&other, true).is_none());
    }

    #[test]
    fn browsing_never_cancels_subtitles_or_consumes_their_result() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let mut runtime = Runtime::default();
        let (release, wait) = std::sync::mpsc::channel();
        runtime.subtitle_job = Some(
            Background::start("held-captions", move |_| {
                wait.recv().unwrap();
                Ok(serde_json::json!({"id":"playing","segments":3}))
            })
            .unwrap(),
        );
        runtime.subtitle_episode = Some("playing".into());
        let lock = Arc::new(MediaLock::acquire(&config.runtime_dir).unwrap());
        runtime
            .action(
                &config,
                Action::List {
                    page: crate::cli::Page {
                        offset: 0,
                        limit: 100,
                        all: false,
                    },
                    query: String::new(),
                },
                lock.clone(),
            )
            .unwrap();
        runtime.cancel_browse();
        assert!(runtime.subtitle_job.is_some());
        assert!(runtime.poll().is_none());
        assert!(
            runtime
                .action(
                    &config,
                    Action::Download {
                        id: "playing".into()
                    },
                    lock
                )
                .is_err(),
            "do not write an ASR input concurrently"
        );
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(result) = runtime.poll_subtitles() {
                assert_eq!(result.unwrap()["segments"], 3);
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!runtime.download_active("playing"));
    }

    #[test]
    fn cancelled_subtitles_keep_their_input_reserved_until_worker_exits() {
        let mut runtime = Runtime::default();
        let (release, wait) = std::sync::mpsc::channel();
        let (cancelled, seen) = std::sync::mpsc::channel();
        runtime.subtitle_job = Some(
            Background::start("held-captions", move |stop| {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(2));
                }
                cancelled.send(()).unwrap();
                wait.recv().unwrap();
                Ok(serde_json::Value::Null)
            })
            .unwrap(),
        );
        runtime.subtitle_episode = Some("playing".into());
        assert!(runtime.cancel_subtitles_for("other").is_err());
        assert!(runtime.subtitle_job.is_some());
        runtime.cancel_subtitles_for("playing").unwrap();
        seen.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(runtime.download_active("playing"));
        assert!(runtime.poll_subtitles().is_none());
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !runtime.retiring_subtitles.is_empty() {
            runtime
                .retiring_subtitles
                .retain(|(_, j)| j.poll().is_none());
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!runtime.download_active("playing"));
    }

    #[test]
    fn scheduled_download_completes_with_the_claimed_generation() {
        let audio = crate::test_support::fixture("libmp3lame", "mp3");
        let bytes = audio.len() as u64;
        let server = crate::test_support::Server::new(move |_, stream| {
            crate::test_support::response(stream, 200, "audio/mpeg", &audio);
        });
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let store = Store::open(&config.data_dir.join("radio.db")).unwrap();
        let preview = Preview {
            feed: Feed {
                url: format!("{}/feed", server.url),
                ..Default::default()
            },
            episodes: vec![Episode {
                url: format!("{}/audio", server.url),
                ..Default::default()
            }],
        };
        let feed = store
            .podcast_subscribe(&preview, &preview.feed.url)
            .unwrap();
        let episode = store.podcast_episodes(&feed.id).unwrap().remove(0);
        Runtime::enqueue(&store, &episode.id).unwrap();
        let snapshot = store.podcast_download(&episode.id).unwrap().unwrap();
        let finished = crate::podcast::download::scheduled(
            &config,
            &snapshot,
            &Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .unwrap();
        assert_eq!(finished.generation, snapshot.generation + 1);
        assert_eq!(finished.state, "complete");
        assert_eq!(finished.bytes, bytes);
        assert!(std::path::Path::new(&finished.path).is_file());
        assert_eq!(
            store.podcast_download(&episode.id).unwrap(),
            Some(finished.clone())
        );
        Runtime::enqueue(&store, &episode.id).unwrap();
        assert_eq!(
            store.podcast_download(&episode.id).unwrap(),
            Some(finished.clone())
        );

        let mut refreshed = preview;
        refreshed.episodes[0].duration_ms = Some(8000);
        store.podcast_refresh_merge(&feed, Some(refreshed)).unwrap();
        Runtime::enqueue(&store, &episode.id).unwrap();
        let queued = store.podcast_download(&episode.id).unwrap().unwrap();
        assert_eq!(queued.state, "queued");
        assert_eq!(queued.generation, finished.generation + 1);
        let replaced = crate::podcast::download::scheduled(
            &config,
            &queued,
            &Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .unwrap();
        assert!(replaced.matches_media(&store.podcast_episode(&episode.id).unwrap()));
    }

    #[test]
    fn download_initialization_failure_is_saved_and_not_scheduled_again() {
        for scheduled in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut config = crate::test_support::config(dir.path());
            config.settings.podcasts.refresh_hours = 0;
            let blocked = dir.path().join("blocked");
            std::fs::write(&blocked, b"not a directory").unwrap();
            config.settings.podcasts.directory = Some(blocked.clone());
            let store = Store::open(&config.data_dir.join("radio.db")).unwrap();
            let preview = Preview {
                feed: Feed {
                    url: "https://example.test/feed".into(),
                    ..Default::default()
                },
                episodes: vec![Episode {
                    url: "https://example.test/audio".into(),
                    ..Default::default()
                }],
            };
            let feed = store
                .podcast_subscribe(&preview, &preview.feed.url)
                .unwrap();
            let episode = store.podcast_episodes(&feed.id).unwrap().remove(0);
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let result = if scheduled {
                Runtime::enqueue(&store, &episode.id).unwrap();
                let snapshot = store.podcast_download(&episode.id).unwrap().unwrap();
                crate::podcast::download::scheduled(&config, &snapshot, &stop)
            } else {
                download(&config, &episode.id, &stop)
            };
            assert!(result.is_err());
            let failed = store.podcast_download(&episode.id).unwrap().unwrap();
            assert_eq!(failed.state, "failed");
            let diagnostic = failed.error.as_ref().unwrap();
            assert!(
                diagnostic.contains(blocked.to_str().unwrap()),
                "{diagnostic}"
            );
            assert_eq!(std::fs::read(&blocked).unwrap(), b"not a directory");
            let mut runtime = Runtime {
                scan: Instant::now(),
                scan_task: Some(
                    Background::start("test-podcast-scan", move |_| Ok((vec![failed], vec![])))
                        .unwrap(),
                ),
                ..Default::default()
            };
            let lock = Arc::new(MediaLock::acquire(&config.data_dir).unwrap());
            let deadline = Instant::now() + Duration::from_secs(3);
            while runtime.scan_task.is_some() {
                runtime.tick(&config, lock.clone()).unwrap();
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(runtime.downloads.is_empty());
            assert_eq!(
                store.podcast_download(&episode.id).unwrap().unwrap().state,
                "failed"
            );
        }
    }

    #[test]
    fn stale_scan_cannot_restart_changed_downloads() {
        for state in [
            "paused",
            "cancelled",
            "removed",
            "queued",
            "failed",
            "complete",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut config = crate::test_support::config(dir.path());
            config.settings.podcasts.refresh_hours = 0;
            let store = Store::open(&config.data_dir.join("radio.db")).unwrap();
            let preview = Preview {
                feed: Feed {
                    url: "http://127.0.0.1:1/feed".into(),
                    ..Default::default()
                },
                episodes: vec![Episode {
                    url: "http://127.0.0.1:1/audio".into(),
                    ..Default::default()
                }],
            };
            let feed = store
                .podcast_subscribe(&preview, &preview.feed.url)
                .unwrap();
            let episode = store.podcast_episodes(&feed.id).unwrap().remove(0);
            Runtime::enqueue(&store, &episode.id).unwrap();
            let snapshot = store.podcast_download(&episode.id).unwrap().unwrap();
            let mut current = snapshot.clone();
            current.state = state.into();
            // Also reject state changes without a generation bump.
            if matches!(state, "paused" | "cancelled" | "removed" | "queued") {
                current.generation += 1;
            }
            store.podcast_save_download(&current).unwrap();
            let mut runtime = Runtime {
                scan: Instant::now(),
                scan_task: Some(
                    Background::start("test-podcast-scan", move |_| Ok((vec![snapshot], vec![])))
                        .unwrap(),
                ),
                ..Default::default()
            };
            let lock = Arc::new(MediaLock::acquire(&config.data_dir).unwrap());
            let deadline = Instant::now() + Duration::from_secs(3);
            while runtime.scan_task.is_some() || !runtime.downloads.is_empty() {
                runtime.tick(&config, lock.clone()).unwrap();
                assert!(Instant::now() < deadline, "{state}");
                std::thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(store.podcast_download(&episode.id).unwrap(), Some(current));
            assert!(!download_path(&config, &episode.id).exists());
            assert!(
                !download_path(&config, &episode.id)
                    .with_extension("part")
                    .exists()
            );
        }
    }

    #[test]
    fn claiming_downloads_fences_old_snapshots_and_workers() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("radio.db")).unwrap();
        let preview = Preview {
            feed: Feed {
                url: "https://example.test/feed".into(),
                ..Default::default()
            },
            episodes: vec![Episode {
                url: "https://example.test/audio".into(),
                ..Default::default()
            }],
        };
        let feed = store
            .podcast_subscribe(&preview, &preview.feed.url)
            .unwrap();
        let episode = store.podcast_episodes(&feed.id).unwrap().remove(0);
        for state in ["queued", "downloading"] {
            let snapshot = Download {
                episode_id: episode.id.clone(),
                state: state.into(),
                generation: 7,
                ..Default::default()
            };
            let mut latest = snapshot.clone();
            latest.bytes = 100;
            store.podcast_save_download(&latest).unwrap();
            let claimed = store.podcast_claim_download(&snapshot).unwrap().unwrap();
            assert_eq!(claimed.state, "downloading");
            assert_eq!(claimed.generation, 8);
            assert_eq!(claimed.bytes, 100);
            assert!(store.podcast_claim_download(&snapshot).unwrap().is_none());
            assert!(store.podcast_download_update(&snapshot).is_err());
            latest = claimed.clone();
            latest.generation += 1;
            latest.state = "paused".into();
            store.podcast_save_download(&latest).unwrap();
            assert!(store.podcast_download_update(&claimed).is_err());
            assert_eq!(store.podcast_download(&episode.id).unwrap(), Some(latest));
        }
    }
}

#[cfg(test)]
mod operation_isolation_tests {
    use super::*;
    #[test]
    fn cancelling_a_browse_retains_the_mutating_operation() {
        let (release, wait) = std::sync::mpsc::channel();
        let mut runtime = Runtime {
            action: Some(
                Background::start("test-import", move |_| {
                    wait.recv().unwrap();
                    Ok(serde_json::json!({"imported":1}))
                })
                .unwrap(),
            ),
            ..Default::default()
        };
        runtime.cancel_browse();
        assert!(runtime.action.is_some());
        release.send(()).unwrap();
    }
}

#[cfg(test)]
mod cleanup_tests {
    use super::*;
    #[test]
    fn automatic_cleanup_preserves_active_queued_and_unmanaged_audio() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = crate::test_support::config(dir.path());
        config.settings.podcasts.refresh_hours = 0;
        let store = Store::open(&config.data_dir.join("radio.db")).unwrap();
        let preview = Preview {
            feed: Feed {
                url: "https://cleanup.test/feed".into(),
                ..Default::default()
            },
            episodes: (0..5)
                .map(|i| Episode {
                    guid: Some(i.to_string()),
                    url: format!("https://cleanup.test/{i}.mp3"),
                    ..Default::default()
                })
                .collect(),
        };
        let feed = store
            .podcast_subscribe(&preview, &preview.feed.url)
            .unwrap();
        assert!(!store.podcast_settings(&feed.id).unwrap().auto_cleanup);
        store
            .podcast_set_setting(&feed.id, "auto_cleanup", "true")
            .unwrap();
        let episodes = store.podcast_episodes(&feed.id).unwrap();
        let mut paths = vec![];
        for (i, episode) in episodes.iter().enumerate() {
            let path = if i == 3 {
                dir.path().join("external.mp3")
            } else {
                download_path(&config, &episode.id)
            };
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"audio").unwrap();
            store
                .podcast_save_download(&Download {
                    episode_id: episode.id.clone(),
                    state: "complete".into(),
                    path: path.to_string_lossy().into_owned(),
                    bytes: 5,
                    ..Default::default()
                })
                .unwrap();
            store
                .podcast_progress(episode, 10, Some(10), i != 2)
                .unwrap();
            paths.push(path);
        }
        store.podcast_queue_set(&[episodes[1].id.clone()]).unwrap();
        let lock = Arc::new(MediaLock::acquire(&config.runtime_dir).unwrap());
        let mut runtime = Runtime::default();
        runtime.follow_captions(Some(&episodes[4]));
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            runtime.tick(&config, lock.clone()).unwrap();
            if store
                .podcast_download(&episodes[0].id)
                .unwrap()
                .unwrap()
                .state
                == "removed"
            {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!paths[0].exists());
        for path in &paths[1..] {
            assert!(path.is_file(), "{}", path.display());
        }
        for e in &episodes {
            assert!(store.podcast_episode(&e.id).is_ok());
        }
    }
}
