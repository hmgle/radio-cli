use crate::store::Store;
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

/// At most one TUI browse thread may be inside a metadata syscall. Waiters
/// for that path share its result, even if the originating page is cancelled.
#[derive(Default)]
pub(super) struct BrowseFileProbe {
    active: Mutex<Option<Arc<ActiveProbe>>>,
    #[cfg(test)]
    pub(super) before_probe: Option<Box<dyn Fn() + Send + Sync>>,
    finished: Condvar,
}

struct ActiveProbe {
    path: String,
    result: OnceLock<bool>,
}

impl BrowseFileProbe {
    pub(super) fn wake_waiters(&self) {
        // Pair with the cancellation check under this lock so a waiter cannot
        // miss cancellation between checking its flag and going to sleep.
        let _active = self.active.lock().unwrap();
        self.finished.notify_all();
    }

    pub(super) fn is_file(&self, path: &str, stop: &AtomicBool) -> bool {
        let mut active = self.active.lock().unwrap();
        let mut shared: Option<Arc<ActiveProbe>> = None;
        loop {
            if stop.load(Ordering::Relaxed) {
                return false;
            }
            if let Some(probe) = &shared
                && let Some(exists) = probe.result.get()
            {
                return *exists;
            }
            match &*active {
                None => break,
                Some(probe) if probe.path == path => shared = Some(probe.clone()),
                _ => {}
            }
            active = self.finished.wait(active).unwrap();
        }
        let probe = Arc::new(ActiveProbe {
            path: path.to_owned(),
            result: OnceLock::new(),
        });
        *active = Some(probe.clone());
        drop(active);
        #[cfg(test)]
        if let Some(before_probe) = &self.before_probe {
            before_probe();
        }
        let exists = Path::new(path).is_file();
        let mut active = self.active.lock().unwrap();
        probe.result.set(exists).unwrap();
        *active = None;
        self.finished.notify_all();
        exists
    }
}

/// Cache each file check for the lifetime of a list query, including its count.
#[derive(Default)]
pub(super) struct DownloadFiles {
    observed: Mutex<HashMap<String, bool>>,
    pending: Mutex<HashSet<String>>,
    #[cfg(test)]
    pub(super) before_probe: Option<fn(&str)>,
    browse_probe: Option<Arc<BrowseFileProbe>>,
    stop: Arc<AtomicBool>,
}

impl DownloadFiles {
    pub(super) fn new(stop: Arc<AtomicBool>) -> Self {
        Self {
            stop,
            ..Default::default()
        }
    }

    pub(super) fn for_browse(stop: Arc<AtomicBool>, probe: Arc<BrowseFileProbe>) -> Self {
        Self {
            browse_probe: Some(probe),
            ..Self::new(stop)
        }
    }

    pub(super) fn is_file(&self, path: &str) -> bool {
        if self.stop.load(Ordering::Relaxed) {
            return false;
        }
        let mut observed = self.observed.lock().unwrap();
        if let Some(&exists) = observed.get(path) {
            return exists;
        }
        #[cfg(test)]
        if let Some(before_probe) = self.before_probe {
            before_probe(path);
        }
        let exists = match &self.browse_probe {
            Some(probe) => probe.is_file(path, &self.stop),
            None => Path::new(path).is_file(),
        };
        observed.insert(path.to_owned(), exists);
        exists
    }

    /// Called inside a read snapshot. Record missing observations without doing I/O.
    pub(super) fn observed_file(&self, path: &str) -> bool {
        if let Some(&exists) = self.observed.lock().unwrap().get(path) {
            return exists;
        }
        self.pending.lock().unwrap().insert(path.to_owned());
        false
    }

    /// Discard provisional rows until every path used by the query is known.
    /// A concurrent database change can introduce new paths on the next pass.
    pub(super) fn read_page<T>(
        &self,
        store: &Store,
        mut read: impl FnMut(&Store) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return Err(
                    crate::error::DomainError::new(crate::error::ErrorCode::Cancelled).into(),
                );
            }
            let page = store.read_snapshot(&mut read)?;
            let paths = std::mem::take(&mut *self.pending.lock().unwrap());
            if paths.is_empty() {
                return Ok(page);
            }
            drop(page);
            for path in paths {
                self.is_file(&path);
            }
        }
    }

    pub(super) fn take_observations(&self) -> HashMap<String, bool> {
        std::mem::take(&mut *self.observed.lock().unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{atomic::AtomicUsize, mpsc},
        thread,
        time::{Duration, Instant},
    };

    #[test]
    fn overlapping_checks_share_a_result_and_other_paths_can_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("download");
        std::fs::write(&path, b"audio").unwrap();
        let path = path.to_str().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed_calls = calls.clone();
        let (entered, ready) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let wait = Mutex::new(wait);
        let probe = BrowseFileProbe {
            before_probe: Some(Box::new(move || {
                if observed_calls.fetch_add(1, Ordering::Relaxed) == 0 {
                    entered.send(()).unwrap();
                    let _ = wait.lock().unwrap().recv_timeout(Duration::from_secs(5));
                }
            })),
            ..Default::default()
        };
        let live = AtomicBool::new(false);
        let cancelled = AtomicBool::new(false);
        thread::scope(|threads| {
            let first = threads.spawn(|| probe.is_file(path, &live));
            ready.recv_timeout(Duration::from_secs(3)).unwrap();
            let same = threads.spawn(|| probe.is_file(path, &live));
            // Both the owner and the gate hold the active observation. Wait for
            // the second caller to subscribe to it before releasing the probe.
            let deadline = Instant::now() + Duration::from_secs(3);
            while Arc::strong_count(probe.active.lock().unwrap().as_ref().unwrap()) < 3 {
                assert!(Instant::now() < deadline);
                thread::yield_now();
            }
            let other = threads.spawn(|| probe.is_file("other.audio", &cancelled));
            cancelled.store(true, Ordering::Relaxed);
            probe.wake_waiters();
            assert!(!other.join().unwrap());
            release.send(()).unwrap();
            assert!(first.join().unwrap());
            assert!(same.join().unwrap());
        });
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }
}
