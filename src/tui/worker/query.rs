//! A single catalog reader keeps SQLite work out of the media owner loop.
use super::{LocatedStation, PagePhase, PageState, View};
use crate::{
    model::{FacetKind, Filter, Recording, Sort, Station},
    store::Store,
};
use anyhow::Result;
use std::{
    path::Path,
    sync::{
        Arc, Condvar, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

struct Page {
    filter: Filter,
    revision: (u64, u64),
    note: String,
}

#[derive(Default)]
struct Pending {
    page: Option<(u64, Page)>,
    locate: Option<(u64, String, u64, usize)>,
    facets: Option<(u64, usize, String)>,
    page_generation: u64,
    locate_generation: u64,
    facets_generation: u64,
}

struct Shared {
    pending: Mutex<Pending>,
    ready: Condvar,
    stop: AtomicBool,
    interrupt: rusqlite::InterruptHandle,
    #[cfg(test)]
    before_read: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

#[derive(Clone)]
pub(super) struct Sender(Arc<Shared>);

impl Sender {
    pub fn page(&self, filter: Filter, revision: (u64, u64), note: String) -> u64 {
        let mut pending = self.0.pending.lock().unwrap();
        pending.page_generation += 1;
        pending.page = Some((
            pending.page_generation,
            Page {
                filter,
                revision,
                note,
            },
        ));
        self.0.ready.notify_one();
        pending.page_generation
    }

    pub fn locate(&self, uuid: String, request: u64, limit: usize) {
        let mut pending = self.0.pending.lock().unwrap();
        pending.locate_generation += 1;
        pending.locate = Some((pending.locate_generation, uuid, request, limit));
        self.0.ready.notify_one();
    }

    pub fn facets(&self, selected: usize, prefix: String) {
        let mut pending = self.0.pending.lock().unwrap();
        pending.facets_generation += 1;
        pending.facets = Some((pending.facets_generation, selected, prefix));
        self.0.ready.notify_one();
    }
}

pub(super) struct Worker {
    pub sender: Sender,
    handle: Option<thread::JoinHandle<()>>,
}

impl Worker {
    pub fn start(path: &Path, view: &Arc<Mutex<View>>) -> Result<Self> {
        // The owner has already initialized/migrated the database.
        let store = Store::open_reader(path)?;
        let shared = Arc::new(Shared {
            pending: Mutex::new(Pending::default()),
            ready: Condvar::new(),
            stop: AtomicBool::new(false),
            interrupt: store.interrupt_handle(),
            #[cfg(test)]
            before_read: Mutex::new(None),
        });
        let cancellation = Arc::downgrade(&shared);
        store.cancel_queries_when(move || {
            cancellation
                .upgrade()
                .is_none_or(|s| s.stop.load(Ordering::Acquire))
        });
        let state = shared.clone();
        let view = Arc::downgrade(view);
        let handle = thread::Builder::new()
            .name("radio-queries".into())
            .spawn(move || {
                run(store, state, view);
            })?;
        Ok(Self {
            sender: Sender(shared),
            handle: Some(handle),
        })
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Serialize with the wait predicate so shutdown cannot lose a wakeup.
        let pending = self.sender.0.pending.lock().unwrap();
        self.sender.0.stop.store(true, Ordering::Release);
        self.sender.0.interrupt.interrupt();
        self.sender.0.ready.notify_one();
        drop(pending);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

enum Request {
    Page(u64, Page),
    Locate(u64, String, u64, usize),
    Facets(u64, usize, String),
}

struct Rows {
    filter: Filter,
    stations: Vec<Station>,
    recordings: Vec<Recording>,
    total: usize,
}

#[derive(Default)]
struct Reader {
    count: Option<(Filter, (u64, u64), usize)>,
}

impl Reader {
    fn page(&mut self, store: &Store, filter: &Filter) -> Result<Rows> {
        let mut key = filter.clone();
        key.limit = 0;
        key.offset = 0;
        key.sort = Sort::default();
        let (rows, revision) = store.read_snapshot(|store| {
            // Sample after pinning the snapshot, and compare tokens only on
            // this reader connection.
            let revision = store.revision()?;
            let known = self
                .count
                .as_ref()
                .filter(|(k, r, _)| *k == key && *r == revision)
                .map(|(_, _, n)| *n);
            let (mut stations, mut recordings) = if filter.recordings {
                (vec![], store.recordings_page(filter.limit, filter.offset)?)
            } else {
                (store.search(filter)?, vec![])
            };
            let len = if filter.recordings {
                recordings.len()
            } else {
                stations.len()
            };
            let limit = filter.limit.clamp(1, 1000);
            let total = if let Some(total) = known {
                total
            } else if len < limit && (len > 0 || filter.offset == 0) {
                filter.offset + len
            } else if filter.recordings {
                store.recordings_count()?
            } else {
                store.search_count(filter)?
            };
            let mut effective = filter.clone();
            if filter.offset > 0 && filter.offset >= total {
                effective.offset = total.saturating_sub(1) / limit * limit;
                if filter.recordings {
                    recordings = store.recordings_page(limit, effective.offset)?;
                } else {
                    stations = store.search(&effective)?;
                }
            }
            Ok((
                Rows {
                    filter: effective,
                    stations,
                    recordings,
                    total,
                },
                revision,
            ))
        })?;
        // A commit during the read invalidates caching even though this page
        // and its total belong to one internally consistent snapshot.
        if store.revision()? == revision {
            self.count = Some((key, revision, rows.total));
        } else {
            self.count = None;
        }
        Ok(rows)
    }
}

fn run(store: Store, shared: Arc<Shared>, view: Weak<Mutex<View>>) {
    let mut reader = Reader::default();
    loop {
        let request = {
            let mut pending = shared.pending.lock().unwrap();
            loop {
                if shared.stop.load(Ordering::Acquire) {
                    return;
                }
                if let Some((g, uuid, r, limit)) = pending.locate.take() {
                    break Request::Locate(g, uuid, r, limit);
                }
                if let Some((g, page)) = pending.page.take() {
                    break Request::Page(g, page);
                }
                if let Some((g, selected, prefix)) = pending.facets.take() {
                    break Request::Facets(g, selected, prefix);
                }
                pending = shared.ready.wait(pending).unwrap();
            }
        };
        #[cfg(test)]
        if let Some(hook) = shared.before_read.lock().unwrap().take() {
            hook();
        }
        if shared.stop.load(Ordering::Acquire) {
            return;
        }
        match request {
            Request::Page(generation, page) => {
                let result = reader.page(&store, &page.filter);
                let Some(view) = view.upgrade() else {
                    return;
                };
                let mut data = view.lock().unwrap();
                if shared.stop.load(Ordering::Acquire)
                    || shared.pending.lock().unwrap().page_generation != generation
                    || data.store_revision != Some(page.revision)
                {
                    continue;
                }
                match result {
                    Ok(rows) => {
                        if page.filter.recordings {
                            data.recordings = Arc::new(rows.recordings);
                            data.recordings_total = rows.total;
                            data.recordings_revision += 1;
                        } else {
                            data.rows = Arc::new(rows.stations);
                            data.total = rows.total;
                            data.backups = Default::default();
                            data.backups_errors = Default::default();
                            data.rows_revision += 1;
                        }
                        data.filter = rows.filter;
                        data.page = Some(PageState {
                            generation,
                            requested: page.filter,
                            phase: PagePhase::Ready,
                        });
                        data.catalog_note = page.note;
                    }
                    Err(error) => {
                        data.page = Some(PageState {
                            generation,
                            requested: page.filter.clone(),
                            phase: PagePhase::Loading,
                        });
                        data.fail_page(&page.filter, format!("{error:#}"));
                    }
                }
                data.revision += 1;
                data.immediate_revision += 1;
            }
            Request::Locate(generation, uuid, request, limit) => {
                let mut filter = Filter {
                    limit: limit.clamp(1, 1000),
                    ..Filter::default()
                };
                let result = store.station_position(&uuid, &filter).and_then(|position| {
                    let position =
                        position.ok_or_else(|| anyhow::anyhow!(crate::t!("jump_unavailable")))?;
                    filter.offset = position / filter.limit * filter.limit;
                    Ok(filter)
                });
                let Some(view) = view.upgrade() else {
                    return;
                };
                let mut data = view.lock().unwrap();
                if shared.stop.load(Ordering::Acquire)
                    || shared.pending.lock().unwrap().locate_generation != generation
                {
                    continue;
                }
                data.located = Some(LocatedStation {
                    request,
                    filter: result.map_err(|e| format!("{e:#}")),
                });
                data.revision += 1;
                data.immediate_revision += 1;
            }
            Request::Facets(generation, selected, prefix) => {
                let kind = [FacetKind::Country, FacetKind::Language, FacetKind::Tag][selected];
                let result = store.facets_limited(kind, &prefix, 100);
                let Some(view) = view.upgrade() else {
                    return;
                };
                let mut data = view.lock().unwrap();
                if shared.stop.load(Ordering::Acquire)
                    || shared.pending.lock().unwrap().facets_generation != generation
                {
                    continue;
                }
                data.facets_loading = false;
                match result {
                    Ok(facets) => Arc::make_mut(&mut data.facets)[selected] = facets,
                    Err(error) => {
                        let message =
                            crate::t!("facet_query_failed", "detail" => format!("{error:#}"));
                        data.log(&message);
                        data.facets_error = Some(message);
                    }
                }
                data.revision += 1;
                data.immediate_revision += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::Config,
        engine::Engine,
        tui::{command::Command, worker},
    };
    use std::{
        sync::mpsc,
        time::{Duration, Instant},
    };

    fn status() -> crate::engine::Status {
        crate::engine::Status {
            station: None,
            title: String::new(),
            message: String::new(),
            playback_state: crate::engine::PlaybackState::Idle,
            volume: 80,
            muted: false,
            recording_state: crate::engine::RecordingState::Idle,
            recording_ms: 0,
            recording_path: String::new(),
            source_url: String::new(),
        }
    }

    fn wait(view: &Arc<Mutex<View>>, predicate: impl Fn(&View) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !predicate(&view.lock().unwrap()) {
            assert!(Instant::now() < deadline, "query did not complete");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn slow_reader_coalesces_requests_without_blocking_media_controls() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(None, Some(dir.path()), Some("http://127.0.0.1:1")).unwrap();
        config.prepare().unwrap();
        let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
        engine
            .store
            .upsert(&Station {
                stationuuid: uuid::Uuid::new_v4().to_string(),
                name: "needle".into(),
                active: true,
                ..Station::default()
            })
            .unwrap();
        let view = Arc::new(Mutex::new(View::new(engine.status.clone())));
        let worker = Worker::start(&engine.config.data_dir.join("radio.db"), &view).unwrap();
        view.lock().unwrap().queries = Some(worker.sender.clone());
        let (started, waiting) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        *worker.sender.0.before_read.lock().unwrap() = Some(Box::new(move || {
            started.send(()).unwrap();
            blocked.recv_timeout(Duration::from_secs(3)).unwrap();
        }));
        let mut filter = Filter {
            limit: 100,
            ..Filter::default()
        };
        worker::refresh_if_needed(&engine, &filter, &view, false).unwrap();
        waiting.recv_timeout(Duration::from_secs(3)).unwrap();
        let mut job = None;
        worker::execute(
            Command::Volume(37),
            &mut engine,
            &mut filter,
            &mut job,
            &view,
        )
        .unwrap();
        assert_eq!(engine.status.volume, 37);
        worker::execute(
            Command::Search(Filter {
                query: "missing".into(),
                limit: 100,
                ..Filter::default()
            }),
            &mut engine,
            &mut filter,
            &mut job,
            &view,
        )
        .unwrap();
        worker::refresh_if_needed(&engine, &filter, &view, false).unwrap();
        worker::execute(
            Command::Facets {
                selected: 2,
                prefix: "ja".into(),
            },
            &mut engine,
            &mut filter,
            &mut job,
            &view,
        )
        .unwrap();
        release.send(()).unwrap();
        wait(&view, |v| v.rows_revision > 0 && !v.facets_loading);
        let data = view.lock().unwrap();
        assert_eq!(data.filter.query, "missing");
        assert_eq!(data.rows_revision, 1, "stale first page was published");
        assert_eq!(data.total, 0);
        drop(data);
        drop(worker);
    }

    #[test]
    fn stale_locations_and_facets_are_not_published() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("radio.db");
        let store = Store::open(&path).unwrap();
        let row = Station {
            stationuuid: uuid::Uuid::new_v4().to_string(),
            name: "station".into(),
            active: true,
            tags: "jazz".into(),
            language: "english".into(),
            ..Station::default()
        };
        store.upsert(&row).unwrap();
        let view = Arc::new(Mutex::new(View::new(status())));
        let worker = Worker::start(&path, &view).unwrap();
        for locating in [true, false] {
            let (started, waiting) = mpsc::channel();
            let (release, blocked) = mpsc::channel();
            *worker.sender.0.before_read.lock().unwrap() = Some(Box::new(move || {
                started.send(()).unwrap();
                blocked.recv_timeout(Duration::from_secs(3)).unwrap();
            }));
            if locating {
                worker.sender.locate(row.stationuuid.clone(), 1, 100);
            } else {
                view.lock().unwrap().facets_loading = true;
                worker.sender.facets(2, "ja".into());
            }
            waiting.recv_timeout(Duration::from_secs(3)).unwrap();
            if locating {
                worker.sender.locate("missing".into(), 2, 100);
            } else {
                worker.sender.facets(1, "en".into());
            }
            release.send(()).unwrap();
            if locating {
                wait(&view, |v| {
                    v.located.as_ref().is_some_and(|l| l.request == 2)
                });
                assert!(
                    view.lock()
                        .unwrap()
                        .located
                        .as_ref()
                        .unwrap()
                        .filter
                        .is_err()
                );
            } else {
                wait(&view, |v| !v.facets_loading);
                let data = view.lock().unwrap();
                assert!(data.facets[2].is_empty());
                assert_eq!(data.facets[1][0].value, "english");
            }
        }
    }

    #[test]
    fn count_cache_ignores_pagination_but_observes_external_commits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("radio.db");
        let store = Store::open(&path).unwrap();
        let mut rows = vec![];
        for i in 0..3 {
            let s = Station {
                stationuuid: uuid::Uuid::new_v4().to_string(),
                name: format!("row {i}"),
                active: true,
                ..Station::default()
            };
            store.upsert(&s).unwrap();
            rows.push(s);
        }
        let reader_store = Store::open_reader(&path).unwrap();
        let mut reader = Reader::default();
        let mut filter = Filter {
            limit: 1,
            ..Filter::default()
        };
        assert_eq!(reader.page(&reader_store, &filter).unwrap().total, 3);
        filter.offset = 1;
        filter.sort = Sort::Name;
        assert_eq!(reader.page(&reader_store, &filter).unwrap().total, 3);
        rows[0].active = false;
        store.upsert(&rows[0]).unwrap();
        filter.offset = 8;
        let page = reader.page(&reader_store, &filter).unwrap();
        assert_eq!(page.stations.len(), 1);
        assert_eq!(page.filter.offset, 1);
        assert_eq!(page.total, 2);
        filter.offset = 0;
        filter.limit = 100;
        assert_eq!(reader.page(&reader_store, &filter).unwrap().total, 2);
    }

    #[test]
    fn reader_reports_errors_and_recovers_on_a_new_request() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("radio.db");
        let store = Store::open(&path).unwrap();
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "INSERT INTO recordings(id,status,json) VALUES ('broken','complete','bad')",
            [],
        )
        .unwrap();
        let view = Arc::new(Mutex::new(View::new(status())));
        let worker = Worker::start(&path, &view).unwrap();
        view.lock().unwrap().store_revision = Some(store.revision().unwrap());
        let filter = Filter {
            recordings: true,
            limit: 100,
            ..Filter::default()
        };
        worker
            .sender
            .page(filter.clone(), store.revision().unwrap(), String::new());
        wait(&view, |v| v.page_error(&filter).is_some());
        assert!(!view.lock().unwrap().page_ready(&filter));
        conn.execute("DELETE FROM recordings", []).unwrap();
        let revision = store.revision().unwrap();
        view.lock().unwrap().store_revision = Some(revision);
        worker.sender.page(filter.clone(), revision, String::new());
        wait(&view, |v| v.recordings_revision == 1);
        assert!(view.lock().unwrap().page_ready(&filter));
        assert!(view.lock().unwrap().page_error(&filter).is_none());
    }

    #[test]
    fn stale_failure_does_not_replace_a_new_query() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("radio.db");
        let store = Store::open(&path).unwrap();
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "INSERT INTO recordings(id,status,json) VALUES ('broken','complete','bad')",
            [],
        )
        .unwrap();
        let view = Arc::new(Mutex::new(View::new(status())));
        let worker = Worker::start(&path, &view).unwrap();
        let revision = store.revision().unwrap();
        view.lock().unwrap().store_revision = Some(revision);
        let (started, waiting) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        *worker.sender.0.before_read.lock().unwrap() = Some(Box::new(move || {
            started.send(()).unwrap();
            blocked.recv_timeout(Duration::from_secs(3)).unwrap();
        }));
        worker.sender.page(
            Filter {
                recordings: true,
                limit: 100,
                ..Filter::default()
            },
            revision,
            String::new(),
        );
        waiting.recv_timeout(Duration::from_secs(3)).unwrap();
        let filter = Filter {
            limit: 100,
            ..Filter::default()
        };
        let generation = worker.sender.page(filter.clone(), revision, String::new());
        release.send(()).unwrap();
        wait(&view, |v| {
            v.page
                .as_ref()
                .is_some_and(|p| p.generation == generation && p.phase == PagePhase::Ready)
        });
        let data = view.lock().unwrap();
        assert!(data.page_ready(&filter));
        assert!(data.logs.is_empty());
    }

    #[test]
    fn shutdown_interrupts_an_active_sqlite_query() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("radio.db");
        Store::open(&path).unwrap();
        let view = Arc::new(Mutex::new(View::new(status())));
        let worker = Worker::start(&path, &view).unwrap();
        // A view makes the real facets query deliberately expensive.
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "DROP TABLE facets_cache; CREATE VIEW facets_cache AS
            WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM n WHERE x<1000000000)
            SELECT 2 AS kind,'slow' AS value,sum(x) AS n FROM n;",
        )
        .unwrap();
        let (started, waiting) = mpsc::channel();
        let shared = worker.sender.0.clone();
        *shared.before_read.lock().unwrap() = Some(Box::new(move || {
            started.send(()).unwrap();
        }));
        worker.sender.facets(2, String::new());
        waiting.recv_timeout(Duration::from_secs(3)).unwrap();
        let start = Instant::now();
        drop(worker);
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(shared.stop.load(Ordering::Acquire));
    }
}
