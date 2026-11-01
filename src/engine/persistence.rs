//! Coalesced session writes never wait for SQLite on the media thread.
use crate::{process::MediaLock, store::Store};
use anyhow::Result;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Condvar, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

#[derive(Default)]
struct Pending {
    preferences: BTreeMap<String, String>,
    plays: Vec<(String, String)>,
    due: Option<Instant>,
    closing: bool,
    cancelled: bool,
    finished: bool,
}

struct Shared {
    pending: Mutex<Pending>,
    ready: Condvar,
}

pub(super) struct Writer {
    shared: Arc<Shared>,
    errors: mpsc::Receiver<anyhow::Error>,
}

impl Writer {
    pub fn start(path: PathBuf, lock: Arc<MediaLock>) -> Result<Self> {
        let shared = Arc::new(Shared {
            pending: Mutex::new(Pending::default()),
            ready: Condvar::new(),
        });
        let (errors, receiver) = mpsc::channel();
        let state = shared.clone();
        thread::Builder::new()
            .name("radio-session-writer".into())
            .spawn(move || {
                let _lock = lock;
                if let Err(error) = run(&path, &state, &errors) {
                    let _ = errors.send(error);
                }
                state.pending.lock().unwrap().finished = true;
            })?;
        Ok(Self {
            shared,
            errors: receiver,
        })
    }

    pub fn preference(&self, key: &str, value: &str) {
        let mut pending = self.shared.pending.lock().unwrap();
        pending.preferences.insert(key.into(), value.into());
        // Bound the delay from the first update, even under continuous input.
        pending
            .due
            .get_or_insert_with(|| Instant::now() + Duration::from_millis(250));
        self.shared.ready.notify_one();
    }

    pub fn played(&self, uuid: &str) {
        let mut pending = self.shared.pending.lock().unwrap();
        pending
            .plays
            .push((uuid.into(), chrono::Utc::now().to_rfc3339()));
        if pending.plays.len() > 1000 {
            pending.plays.remove(0);
        }
        pending.due = Some(Instant::now());
        self.shared.ready.notify_one();
    }

    pub fn close(&self) {
        self.shared.pending.lock().unwrap().closing = true;
        self.shared.ready.notify_one();
    }

    pub fn finished(&self) -> bool {
        self.shared.pending.lock().unwrap().finished
    }

    pub fn poll(&self) -> Result<()> {
        if let Ok(error) = self.errors.try_recv() {
            return Err(error);
        }
        Ok(())
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.shared.pending.lock().unwrap().cancelled = true;
        self.shared.ready.notify_one();
    }
}

fn run(
    path: &std::path::Path,
    shared: &Shared,
    errors: &mpsc::Sender<anyhow::Error>,
) -> Result<()> {
    let mut store = Store::open_writer(path)?;
    loop {
        let mut pending = shared.pending.lock().unwrap();
        loop {
            if pending.cancelled {
                return Ok(());
            }
            let empty = pending.preferences.is_empty() && pending.plays.is_empty();
            if empty && pending.closing {
                return Ok(());
            }
            if !empty && (pending.closing || pending.due.is_some_and(|at| Instant::now() >= at)) {
                break;
            }
            pending = if let Some(at) = pending.due.filter(|_| !empty) {
                shared
                    .ready
                    .wait_timeout(pending, at.saturating_duration_since(Instant::now()))
                    .unwrap()
                    .0
            } else {
                shared.ready.wait(pending).unwrap()
            };
        }
        let preferences = std::mem::take(&mut pending.preferences);
        let plays = std::mem::take(&mut pending.plays);
        pending.due = None;
        drop(pending);
        if let Err(error) = store.save_session(&preferences, &plays) {
            let busy = error.downcast_ref::<rusqlite::Error>().is_some_and(|e| {
                matches!(e, rusqlite::Error::SqliteFailure(code, _)
                    if matches!(code.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked))
            });
            if busy {
                let mut pending = shared.pending.lock().unwrap();
                for (key, value) in preferences {
                    pending.preferences.entry(key).or_insert(value);
                }
                let mut retained = plays;
                retained.append(&mut pending.plays);
                if retained.len() > 1000 {
                    retained.drain(..retained.len() - 1000);
                }
                pending.plays = retained;
                pending.due = Some(Instant::now() + Duration::from_millis(50));
                // Closing still retries through SQLite's bounded busy timeout.
            } else {
                let _ = errors.send(error);
            }
        }
    }
}
