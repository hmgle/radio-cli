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
                        download(&config, &id, &stop)
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
        store.podcast_episode(id)?;
        let mut d = store.podcast_download(id)?.unwrap_or_else(|| Download {
            episode_id: id.into(),
            ..Default::default()
        });
        if d.state != "complete" || !std::path::Path::new(&d.path).is_file() {
            d.generation = d.generation.saturating_add(1);
            d.state = "queued".into();
            d.error = None;
            store.podcast_save_download(&d)?;
        }
        Ok(())
    }
}
