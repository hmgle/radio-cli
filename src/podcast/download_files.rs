use std::{
    collections::HashMap,
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
        let exists = Path::new(path).is_file();
        observed.insert(path.to_owned(), exists);
        exists
    }

    pub(super) fn take_observations(&self) -> HashMap<String, bool> {
        std::mem::take(&mut *self.observed.lock().unwrap())
    }
}
