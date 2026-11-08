//! Session-scoped jobs keep network and file work away from media controls.
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
    sync::Arc,
    time::{Duration, Instant},
};
pub struct Runtime {
    action: Option<Background<Value>>,
    downloads: HashMap<String, Background<Download>>,
    refresh: Option<Background<Value>>,
    scan: Instant,
    scan_task: Option<Background<(Vec<Download>, Vec<Feed>)>>,
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            action: None,
            downloads: HashMap::new(),
            refresh: None,
            scan_task: None,
            scan: Instant::now() - Duration::from_secs(10),
        }
    }
}
impl Runtime {
    pub(crate) fn tasks(&self) -> (usize, bool) {
        (
            self.downloads.len(),
            self.action.is_some() || self.refresh.is_some(),
        )
    }
    pub(crate) fn action(
        &mut self,
        config: &Config,
        action: Action,
        lock: Arc<MediaLock>,
    ) -> Result<()> {
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
    pub(crate) fn poll(&mut self) -> Option<Result<Value>> {
        let result = self.action.as_ref()?.poll()?;
        self.action = None;
        Some(result)
    }
    pub(crate) fn download_active(&self, id: &str) -> bool {
        self.downloads.contains_key(id)
    }
    pub(crate) fn cancel(&mut self) -> bool {
        self.action.take().is_some()
    }
    pub(crate) fn tick(&mut self, config: &Config, lock: Arc<MediaLock>) -> Result<()> {
        self.downloads.retain(|_, task| task.poll().is_none());
        if self.refresh.as_ref().is_some_and(|t| t.poll().is_some()) {
            self.refresh = None;
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
                Ok((store.podcast_downloads()?, due))
            })?);
        }
        let Some(result) = self.scan_task.as_ref().and_then(|t| t.poll()) else {
            return Ok(());
        };
        self.scan_task = None;
        let (downloads, due) = result?;
        for d in downloads {
            if self.downloads.len() >= 2 {
                break;
            }
            if matches!(d.state.as_str(), "queued" | "downloading")
                && !self.downloads.contains_key(&d.episode_id)
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
        if self.refresh.is_none() && config.settings.podcasts.refresh_hours > 0 && !due.is_empty() {
            let config = config.clone();
            self.refresh = Some(Background::start("radio-podcast-refresh", move |stop| {
                let _lock = lock;
                let store = Store::open(&config.data_dir.join("radio.db"))?;
                let mut results = vec![];
                for f in due {
                    results.push(super::refresh(&config, &store, Some(&f.id), &stop)?);
                }
                Ok(serde_json::json!(results))
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
            if d.state != "complete"
                || !d.matches_media(&episode)
                || !std::path::Path::new(&d.path).is_file()
            {
                d.generation = d.generation.saturating_add(1);
                d.state = "queued".into();
                d.error = None;
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
