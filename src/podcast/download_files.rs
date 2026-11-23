use crate::store::Store;
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

/// Cache each file check for the lifetime of a list query, including its count.
#[derive(Default)]
pub(super) struct DownloadFiles {
    observed: Mutex<HashMap<String, bool>>,
    pending: Mutex<HashSet<String>>,
    #[cfg(test)]
    pub(super) before_probe: Option<fn(&str)>,
    stop: Arc<AtomicBool>,
}

impl DownloadFiles {
    pub(super) fn new(stop: Arc<AtomicBool>) -> Self {
        Self {
            stop,
            ..Default::default()
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
        let exists = Path::new(path).is_file();
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
