//! Coalesced session writes never wait for SQLite on the media thread.
use crate::{process::MediaLock, store::Store};
use anyhow::{Result, bail};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Condvar, Mutex, MutexGuard},
    thread,
    time::{Duration, Instant},
};

// Initial catalog publication can take several seconds. Leave room for slow
// storage while still reporting locks that never clear during normal use.
const LOCK_RETRY_BUDGET: Duration = Duration::from_secs(30);

#[derive(Default)]
struct Pending {
    preferences: BTreeMap<String, String>,
    plays: Vec<(String, String)>,
    due: Option<Instant>,
    closing: Option<Instant>,
    cancelled: bool,
    finished: bool,
    failure: Option<String>,
    failure_reported: bool,
}

struct Shared {
    pending: Mutex<Pending>,
    ready: Condvar,
}

impl Shared {
    fn pending(&self) -> MutexGuard<'_, Pending> {
        // A worker panic may poison this mutex. Recover access so its terminal
        // failure can still be published and observed by the session owner.
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }
}

pub(super) struct Writer {
    shared: Arc<Shared>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Writer {
    pub fn start(path: PathBuf, lock: Arc<MediaLock>) -> Result<Self> {
        Self::start_with_retry_budget(path, lock, LOCK_RETRY_BUDGET)
    }

    fn start_with_retry_budget(
        path: PathBuf,
        lock: Arc<MediaLock>,
        retry_budget: Duration,
    ) -> Result<Self> {
        Self::start_worker(lock, move |state| run(&path, state, retry_budget))
    }

    fn start_worker(
        lock: Arc<MediaLock>,
        work: impl FnOnce(&Shared) -> Result<()> + std::panic::UnwindSafe + Send + 'static,
    ) -> Result<Self> {
        let shared = Arc::new(Shared {
            pending: Mutex::new(Pending::default()),
            ready: Condvar::new(),
        });
        let state = shared.clone();
        let handle = thread::Builder::new()
            .name("radio-session-writer".into())
            .spawn(move || {
                let _lock = lock;
                let result = std::panic::catch_unwind(|| work(&state))
                    .unwrap_or_else(|_| Err(anyhow::anyhow!(crate::t!("session_save_failed"))));
                let mut pending = state.pending();
                // Publish the outcome and terminal state together. Neither an
                // error consumer nor a new update can see a half-closed writer.
                pending.failure = result.err().map(|error| format!("{error:#}"));
                pending.finished = true;
                state.pending.clear_poison();
                state.ready.notify_all();
            })?;
        Ok(Self {
            shared,
            handle: Some(handle),
        })
    }

    pub fn preference(&self, key: &str, value: &str) -> Result<()> {
        let mut pending = self.shared.pending();
        if pending.closing.is_some() || pending.cancelled || pending.finished {
            bail!(crate::t!("session_save_failed"));
        }
        pending.preferences.insert(key.into(), value.into());
        // Bound the delay from the first update, even under continuous input.
        pending
            .due
            .get_or_insert_with(|| Instant::now() + Duration::from_millis(250));
        self.shared.ready.notify_one();
        Ok(())
    }

    pub fn played(&self, uuid: &str) -> Result<()> {
        let mut pending = self.shared.pending();
        if pending.closing.is_some() || pending.cancelled || pending.finished {
            bail!(crate::t!("session_save_failed"));
        }
        pending
            .plays
            .push((uuid.into(), chrono::Utc::now().to_rfc3339()));
        if pending.plays.len() > 1000 {
            pending.plays.remove(0);
        }
        pending.due = Some(Instant::now());
        self.shared.ready.notify_one();
        Ok(())
    }

    pub fn close(&self, deadline: Instant) {
        let mut pending = self.shared.pending();
        pending.closing = Some(deadline);
        pending.due = Some(Instant::now());
        self.shared.ready.notify_one();
    }

    pub fn finish(mut self) -> Result<()> {
        // Settle any in-flight SQLite attempt before reporting its outcome.
        // The connection's 100 ms busy timeout bounds this final lock wait.
        self.shared.pending().cancelled = true;
        self.shared.ready.notify_one();
        if let Some(handle) = self.handle.take()
            && handle.join().is_err()
        {
            bail!(crate::t!("session_save_failed"));
        }
        let pending = self.shared.pending();
        if let Some(error) = &pending.failure {
            bail!(error.clone());
        }
        Ok(())
    }

    pub fn finished(&self) -> bool {
        self.shared.pending().finished
    }

    pub fn poll(&self) -> Result<()> {
        let mut pending = self.shared.pending();
        if !pending.failure_reported
            && let Some(error) = pending.failure.clone()
        {
            pending.failure_reported = true;
            bail!(error);
        }
        Ok(())
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.shared.pending().cancelled = true;
        self.shared.ready.notify_one();
    }
}

fn run(path: &std::path::Path, shared: &Shared, retry_budget: Duration) -> Result<()> {
    let mut store = Store::open_writer(path)?;
    let mut retry_deadline = None;
    loop {
        let mut pending = shared.pending();
        loop {
            let empty = pending.preferences.is_empty() && pending.plays.is_empty();
            if empty && (pending.cancelled || pending.closing.is_some()) {
                return Ok(());
            }
            if pending.cancelled || pending.closing.is_some_and(|at| Instant::now() >= at) {
                bail!(crate::t!("session_save_failed"));
            }
            if !empty && pending.due.is_some_and(|at| Instant::now() >= at) {
                break;
            }
            pending = if let Some(at) = pending.due.filter(|_| !empty) {
                let at = pending.closing.map_or(at, |deadline| at.min(deadline));
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
        let attempted_at = Instant::now();
        if let Err(error) = store.save_session(&preferences, &plays) {
            let busy = error.downcast_ref::<rusqlite::Error>().is_some_and(|e| {
                matches!(e, rusqlite::Error::SqliteFailure(code, _)
                    if matches!(code.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked))
            });
            if busy {
                let mut pending = shared.pending();
                // New updates must not extend continuous lock contention.
                // Shutdown gets its own cleanup budget, even if the runtime
                // budget was nearly exhausted when close() was called.
                let deadline = pending
                    .closing
                    .unwrap_or_else(|| *retry_deadline.get_or_insert(attempted_at + retry_budget));
                if Instant::now() >= deadline {
                    return Err(error);
                }
                for (key, value) in preferences {
                    pending.preferences.entry(key).or_insert(value);
                }
                let mut retained = plays;
                retained.append(&mut pending.plays);
                if retained.len() > 1000 {
                    retained.drain(..retained.len() - 1000);
                }
                pending.plays = retained;
                pending.due = Some((Instant::now() + Duration::from_millis(50)).min(deadline));
            } else {
                return Err(error);
            }
        } else {
            retry_deadline = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_panics_publish_terminal_failure_even_with_a_poisoned_mutex() {
        let dir = tempfile::tempdir().unwrap();
        let lock = Arc::new(MediaLock::acquire(dir.path()).unwrap());
        for poison_pending in [false, true] {
            let (release, wait) = std::sync::mpsc::channel();
            let writer = Writer::start_worker(lock.clone(), move |shared| {
                wait.recv().unwrap();
                let _pending = poison_pending.then(|| shared.pending());
                panic!("session writer panic fixture");
            })
            .unwrap();
            writer.preference("pref_volume", "50").unwrap();
            release.send(()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                if writer.poll().is_err() {
                    assert!(writer.finished());
                    break;
                }
                assert!(Instant::now() < deadline, "worker panic was not reported");
                thread::sleep(Duration::from_millis(10));
            }
            writer.poll().unwrap();
            assert!(writer.preference("pref_volume", "40").is_err());
            assert!(writer.played("station").is_err());
            assert!(writer.finish().is_err());
        }
    }

    #[test]
    fn default_retry_budget_tolerates_slow_catalog_publication() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("radio.db");
        let store = Store::open(&path).unwrap();
        let uuid = uuid::Uuid::new_v4().to_string();
        store
            .upsert(&crate::model::Station {
                stationuuid: uuid.clone(),
                name: "Station".into(),
                ..Default::default()
            })
            .unwrap();
        let lock = Arc::new(MediaLock::acquire(dir.path()).unwrap());
        let writer = Writer::start(path.clone(), lock).unwrap();
        let connection = rusqlite::Connection::open(path).unwrap();
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        writer.preference("pref_volume", "50").unwrap();
        writer.played(&uuid).unwrap();
        // Reproduce the lock duration of a large first publication. The old
        // five-second budget permanently disabled persistence during this wait.
        thread::sleep(Duration::from_secs(8));
        writer.poll().unwrap();
        assert!(!writer.finished());
        writer.preference("pref_volume", "40").unwrap();
        connection.execute_batch("ROLLBACK").unwrap();
        writer.close(Instant::now() + Duration::from_secs(2));
        let deadline = Instant::now() + Duration::from_secs(3);
        while !writer.finished() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        writer.finish().unwrap();
        assert_eq!(store.meta("pref_volume").unwrap().as_deref(), Some("40"));
        assert_eq!(
            store.meta("pref_last_station").unwrap().as_deref(),
            Some(uuid.as_str())
        );
    }

    #[test]
    fn shutdown_uses_its_own_budget_after_runtime_contention() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("radio.db");
        let store = Store::open(&path).unwrap();
        let lock = Arc::new(MediaLock::acquire(dir.path()).unwrap());
        let writer =
            Writer::start_with_retry_budget(path.clone(), lock, Duration::from_secs(1)).unwrap();
        let connection = rusqlite::Connection::open(path).unwrap();
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        writer.preference("pref_volume", "50").unwrap();
        // Let the first attempt hit the lock before requesting shutdown.
        thread::sleep(Duration::from_millis(400));
        writer.close(Instant::now() + Duration::from_secs(3));
        thread::sleep(Duration::from_millis(1200));
        writer.poll().unwrap();
        assert!(!writer.finished());
        connection.execute_batch("ROLLBACK").unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !writer.finished() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        writer.finish().unwrap();
        assert_eq!(store.meta("pref_volume").unwrap().as_deref(), Some("50"));
    }

    #[test]
    fn sustained_lock_contention_fails_even_with_new_preferences() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("radio.db");
        let store = Store::open(&path).unwrap();
        let uuid = uuid::Uuid::new_v4().to_string();
        store.set_meta("pref_volume", "80").unwrap();
        store
            .upsert(&crate::model::Station {
                stationuuid: uuid.clone(),
                name: "Station".into(),
                ..Default::default()
            })
            .unwrap();
        let lock = Arc::new(MediaLock::acquire(dir.path()).unwrap());
        let writer =
            Writer::start_with_retry_budget(path.clone(), lock, Duration::from_millis(300))
                .unwrap();
        let connection = rusqlite::Connection::open(path).unwrap();
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        writer.preference("pref_volume", "50").unwrap();
        writer.played(&uuid).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Err(error) = writer.poll() {
                assert!(error.to_string().contains("locked"));
                assert!(writer.finished());
                break;
            }
            assert!(
                Instant::now() < deadline,
                "lock contention was not reported"
            );
            // Continuous input must not postpone the retry deadline. Failure
            // may be published between poll() and preference().
            if writer.preference("pref_volume", "40").is_err() {
                assert!(writer.finished());
            }
            thread::sleep(Duration::from_millis(10));
        }
        writer.poll().unwrap(); // Report the terminal failure only once.
        assert!(writer.preference("pref_volume", "30").is_err());
        assert!(writer.played(&uuid).is_err());
        assert!(writer.finish().is_err());
        connection.execute_batch("ROLLBACK").unwrap();
        // Releasing the lock after failure must not cause a late commit.
        assert_eq!(store.meta("pref_volume").unwrap().as_deref(), Some("80"));
        assert!(store.meta("pref_last_station").unwrap().is_none());
        assert_eq!(
            store
                .search_count(&crate::model::Filter {
                    history: true,
                    ..Default::default()
                })
                .unwrap(),
            0
        );
    }

    #[test]
    fn successful_commits_reset_the_lock_retry_budget() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("radio.db");
        let store = Store::open(&path).unwrap();
        let uuid = uuid::Uuid::new_v4().to_string();
        store
            .upsert(&crate::model::Station {
                stationuuid: uuid.clone(),
                name: "Station".into(),
                ..Default::default()
            })
            .unwrap();
        let lock = Arc::new(MediaLock::acquire(dir.path()).unwrap());
        let retry_budget = Duration::from_secs(1);
        let writer = Writer::start_with_retry_budget(path.clone(), lock, retry_budget).unwrap();
        let connection = rusqlite::Connection::open(path).unwrap();
        for (index, value) in ["40", "60"].into_iter().enumerate() {
            connection.execute_batch("BEGIN IMMEDIATE").unwrap();
            writer.preference("pref_volume", "20").unwrap();
            writer.played(&uuid).unwrap();
            thread::sleep(Duration::from_millis(150));
            writer.preference("pref_volume", value).unwrap();
            writer.poll().unwrap();
            connection.execute_batch("ROLLBACK").unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            while store.meta("pref_volume").unwrap().as_deref() != Some(value) {
                writer.poll().unwrap();
                assert!(Instant::now() < deadline, "queued changes were not saved");
                thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(
                store
                    .test_connection()
                    .query_row("SELECT count(*) FROM history", [], |row| row
                        .get::<_, usize>(0))
                    .unwrap(),
                index + 1
            );
            if index == 0 {
                // The next contention outlives the first retry deadline.
                thread::sleep(retry_budget);
            }
        }
        writer.close(Instant::now() + Duration::from_secs(1));
        writer.finish().unwrap();
    }

    #[test]
    fn failed_or_closed_writer_rejects_new_updates() {
        let dir = tempfile::tempdir().unwrap();
        let lock = Arc::new(MediaLock::acquire(dir.path()).unwrap());
        let writer = Writer::start(dir.path().join("missing/radio.db"), lock.clone()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if writer.poll().is_err() {
                // Observing a terminal error also observes the closed state.
                assert!(writer.finished());
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        // Consuming the original error must not make future updates succeed.
        assert!(writer.preference("pref_volume", "50").is_err());
        assert!(writer.played("station").is_err());
        assert!(writer.finish().is_err());

        Store::open(&dir.path().join("radio.db")).unwrap();
        let writer = Writer::start(dir.path().join("radio.db"), lock).unwrap();
        writer.close(Instant::now() + Duration::from_secs(1));
        assert!(writer.preference("pref_volume", "50").is_err());
        assert!(writer.played("station").is_err());
        writer.finish().unwrap();
    }
}
