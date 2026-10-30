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
    map: Option<(u64, super::super::map::Request)>,
    map_generation: u64,
    page_generation: u64,
    locate_generation: u64,
    facets_generation: u64,
}

impl Pending {
    fn urgent(&self) -> bool {
        self.page.is_some() || self.locate.is_some() || self.facets.is_some()
    }
}

fn map_cancelled(shared: &Shared, generation: u64) -> bool {
    if shared.stop.load(Ordering::Acquire) {
        return true;
    }
    let pending = shared.pending.lock().unwrap();
    pending.map_generation != generation || pending.urgent()
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
    pub fn cancel_map(&self) {
        let mut pending = self.0.pending.lock().unwrap();
        pending.map = None;
        pending.map_generation += 1;
    }

    pub fn map(&self, request: super::super::map::Request) {
        let mut pending = self.0.pending.lock().unwrap();
        pending.map_generation += 1;
        pending.map = Some((pending.map_generation, request));
        self.0.ready.notify_one();
    }

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
    Map(u64, super::super::map::Request),
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
    map_countries: Option<(Filter, u64, Vec<crate::model::Facet>)>,
}

impl Reader {
    fn map(
        &mut self,
        store: &Store,
        request: &super::super::map::Request,
        cached_land: Option<Arc<Vec<char>>>,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<super::super::map::Image> {
        if cancelled() {
            anyhow::bail!(crate::error::DomainError::new(
                crate::error::ErrorCode::Cancelled
            ));
        }
        store.read_snapshot(|store| {
            let counts = store.geo_counts(
                &request.filter,
                request.bounds,
                request.columns,
                request.rows,
                cancelled,
            )?;
            let revision = store.geo_revision()?;
            let countries = if !request.countries {
                vec![]
            } else if let Some((filter, known, countries)) = &self.map_countries
                && filter == &request.filter
                && *known == revision
            {
                countries.clone()
            } else {
                let countries = store.geo_missing_countries(&request.filter)?;
                self.map_countries = Some((request.filter.clone(), revision, countries.clone()));
                countries
            };
            Ok(super::super::map::Image {
                land: cached_land.unwrap_or_else(|| {
                    Arc::new(super::super::map::raster(
                        request.bounds,
                        request.columns,
                        request.rows,
                    ))
                }),
                counts,
                countries,
                revision,
                request: request.clone(),
            })
        })
    }

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
                if let Some((generation, request)) = pending.map.take() {
                    break Request::Map(generation, request);
                }
                pending = shared.ready.wait(pending).unwrap();
            }
        };
        #[cfg(test)]
        {
            let hook = shared.before_read.lock().unwrap().take();
            if let Some(hook) = hook {
                hook();
            }
        }
        if shared.stop.load(Ordering::Acquire) {
            return;
        }
        match request {
            Request::Map(generation, request) => {
                let cancellation = Arc::downgrade(&shared);
                store.cancel_queries_when(move || {
                    cancellation
                        .upgrade()
                        .is_none_or(|s| map_cancelled(&s, generation))
                });
                let cached_land = view.upgrade().and_then(|v| {
                    v.lock()
                        .unwrap()
                        .map_image
                        .as_ref()
                        .filter(|i| {
                            i.request.bounds == request.bounds
                                && i.request.columns == request.columns
                                && i.request.rows == request.rows
                        })
                        .map(|i| i.land.clone())
                });
                let result = reader.map(&store, &request, cached_land, || {
                    map_cancelled(&shared, generation)
                });
                let cancellation = Arc::downgrade(&shared);
                store.cancel_queries_when(move || {
                    cancellation
                        .upgrade()
                        .is_none_or(|s| s.stop.load(Ordering::Acquire))
                });
                let Some(view) = view.upgrade() else {
                    return;
                };
                let mut data = view.lock().unwrap();
                let mut pending = shared.pending.lock().unwrap();
                if shared.stop.load(Ordering::Acquire)
                    || pending.map_generation != generation
                    || data.map_request.as_ref() != Some(&request)
                {
                    continue;
                }
                if result.is_err() && pending.urgent() {
                    // Keep the same generation so the UI need not resubmit it.
                    // The next iteration services the pending interactive read.
                    pending.map = Some((generation, request));
                    continue;
                }
                drop(pending);
                match result {
                    Ok(image) => {
                        data.map_image = Some(Arc::new(image));
                        data.map_error = None;
                    }
                    Err(error) => data.map_error = Some(format!("{error:#}")),
                }
                data.revision += 1;
                data.immediate_revision += 1;
            }

            Request::Page(generation, page) => {
                let result = reader.page(&store, &page.filter);
                let Some(view) = view.upgrade() else {
                    return;
                };
                let mut data = view.lock().unwrap();
                if shared.stop.load(Ordering::Acquire)
                    || shared.pending.lock().unwrap().page_generation != generation
                    || data.store_revision != Some(page.revision)
                    || data.refresh_requested
                    || !data.page.as_ref().is_some_and(|p| {
                        p.generation == generation
                            && p.requested == page.filter
                            && matches!(p.phase, PagePhase::Loading | PagePhase::Refreshing)
                    })
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
                        data.page_succeeded();
                    }
                    Err(error) => {
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

    pub(super) fn wait(view: &Arc<Mutex<View>>, predicate: impl Fn(&View) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !predicate(&view.lock().unwrap()) {
            assert!(Instant::now() < deadline, "query did not complete");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn request_page(
        worker: &Worker,
        view: &Arc<Mutex<View>>,
        filter: Filter,
        revision: (u64, u64),
    ) -> u64 {
        let mut data = view.lock().unwrap();
        data.store_revision = Some(revision);
        data.page_retry_at = None;
        let generation = worker.sender.page(filter.clone(), revision, String::new());
        data.page = Some(PageState {
            generation,
            requested: filter,
            phase: PagePhase::Loading,
        });
        generation
    }

    #[test]
    fn slow_reader_coalesces_requests_without_blocking_media_controls() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
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
        request_page(&worker, &view, filter.clone(), store.revision().unwrap());
        wait(&view, |v| v.page_error(&filter).is_some());
        assert!(!view.lock().unwrap().page_ready(&filter));
        conn.execute("DELETE FROM recordings", []).unwrap();
        let revision = store.revision().unwrap();
        view.lock().unwrap().store_revision = Some(revision);
        request_page(&worker, &view, filter.clone(), revision);
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
        request_page(
            &worker,
            &view,
            Filter {
                recordings: true,
                limit: 100,
                ..Filter::default()
            },
            revision,
        );
        waiting.recv_timeout(Duration::from_secs(3)).unwrap();
        let filter = Filter {
            limit: 100,
            ..Filter::default()
        };
        let generation = request_page(&worker, &view, filter.clone(), revision);
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
    fn owner_failure_keeps_retry_when_an_older_reader_finishes() {
        use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
        for failing_reader in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let config = crate::test_support::config(dir.path());
            let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
            let path = dir.path().join("radio.db");
            rusqlite::Connection::open(&path)
                .unwrap()
                .execute(
                    "INSERT INTO recordings(id,status,json) VALUES ('broken','complete','bad')",
                    [],
                )
                .unwrap();
            let view = Arc::new(Mutex::new(View::new(engine.status.clone())));
            let worker = Worker::start(&path, &view).unwrap();
            view.lock().unwrap().queries = Some(worker.sender.clone());
            let (started, waiting) = mpsc::channel();
            let (release, blocked) = mpsc::channel();
            *worker.sender.0.before_read.lock().unwrap() = Some(Box::new(move || {
                started.send(()).unwrap();
                blocked.recv_timeout(Duration::from_secs(3)).unwrap();
            }));
            let mut filter = Filter {
                recordings: failing_reader,
                limit: 100,
                ..Filter::default()
            };
            worker::refresh_if_needed(&engine, &filter, &view, false).unwrap();
            waiting.recv_timeout(Duration::from_secs(3)).unwrap();
            worker::execute(
                Command::Search(Filter {
                    query: "latest request".into(),
                    limit: 100,
                    ..Filter::default()
                }),
                &mut engine,
                &mut filter,
                &mut None,
                &view,
            )
            .unwrap();
            engine
                .store
                .test_connection()
                .authorizer(Some(|context: AuthContext<'_>| {
                    if matches!(
                        context.action,
                        AuthAction::Pragma {
                            pragma_name: "data_version",
                            ..
                        }
                    ) {
                        Authorization::Deny
                    } else {
                        Authorization::Allow
                    }
                }));
            worker::refresh_if_needed(&engine, &filter, &view, false).unwrap();
            engine
                .store
                .test_connection()
                .authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
            let retry_at = view.lock().unwrap().page_retry_at;
            assert!(retry_at.is_some());
            // A facet query queued behind the blocked reader acts as a barrier:
            // once it finishes, the stale page result has been processed.
            view.lock().unwrap().facets_loading = true;
            worker.sender.facets(2, String::new());
            release.send(()).unwrap();
            wait(&view, |v| !v.facets_loading);
            {
                let data = view.lock().unwrap();
                assert!(data.page_error(&filter).is_some());
                assert_eq!(data.page_retry_at, retry_at);
                assert_eq!(data.rows_revision, 0);
                assert_eq!(data.recordings_revision, 0);
                assert_eq!(data.logs.len(), 1);
            }
            view.lock().unwrap().page_retry_at = Some(Instant::now());
            worker::refresh_if_needed(&engine, &filter, &view, false).unwrap();
            wait(&view, |v| v.page_ready(&filter));
            assert!(view.lock().unwrap().page_retry_at.is_none());
        }
    }

    #[test]
    fn external_commit_bypasses_page_retry_backoff() {
        for asynchronous in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let config = crate::test_support::config(dir.path());
            let engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
            let path = dir.path().join("radio.db");
            let row = Station {
                stationuuid: uuid::Uuid::new_v4().to_string(),
                name: "favorite".into(),
                active: true,
                ..Station::default()
            };
            engine.store.upsert(&row).unwrap();
            let view = Arc::new(Mutex::new(View::new(engine.status.clone())));
            let reader = asynchronous.then(|| Worker::start(&path, &view).unwrap());
            if let Some(reader) = &reader {
                view.lock().unwrap().queries = Some(reader.sender.clone());
            }
            let filter = Filter {
                limit: 100,
                ..Filter::default()
            };
            worker::refresh_if_needed(&engine, &filter, &view, false).unwrap();
            wait(&view, |v| v.page_ready(&filter));
            assert!(!view.lock().unwrap().rows[0].favorite);
            let before = engine.store.revision().unwrap();
            {
                let mut data = view.lock().unwrap();
                data.fail_page(&filter, "Transient query failure".into());
                data.page_retry_at = Some(Instant::now() + Duration::from_secs(30));
            }
            let external = Store::open(&path).unwrap();
            external.favorite(&row.stationuuid, true).unwrap();
            assert_ne!(engine.store.revision().unwrap(), before);
            worker::refresh_if_needed(&engine, &filter, &view, false).unwrap();
            assert!(
                view.lock().unwrap().page_error(&filter).is_none(),
                "external commit ignored while retry deadline is pending"
            );
            wait(&view, |v| v.page_ready(&filter));
            assert!(view.lock().unwrap().rows[0].favorite);
            assert!(view.lock().unwrap().page_retry_at.is_none());
        }
    }

    #[test]
    fn playback_confirmation_keeps_the_page_visible_and_interactive_during_refresh() {
        use crate::{engine::PlaybackState, tui::state::App};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let dir = tempfile::tempdir().unwrap();
        let mut config = crate::test_support::config(dir.path());
        config.settings.mpv = crate::test_support::fake_mpv(dir.path())
            .to_string_lossy()
            .into();
        let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
        for i in 0..40 {
            engine
                .store
                .upsert(&Station {
                    stationuuid: uuid::Uuid::new_v4().to_string(),
                    name: format!("Row {i:03}"),
                    url: "http://127.0.0.1:1/audio".into(),
                    custom: true,
                    active: true,
                    ..Station::default()
                })
                .unwrap();
        }
        let view = Arc::new(Mutex::new(View::new(engine.status.clone())));
        let worker = Worker::start(&dir.path().join("radio.db"), &view).unwrap();
        view.lock().unwrap().queries = Some(worker.sender.clone());
        let mut filter = Filter {
            limit: 100,
            ..Filter::default()
        };
        worker::refresh_if_needed(&engine, &filter, &view, false).unwrap();
        wait(&view, |v| v.page_ready(&filter));

        let mut app = App::default();
        app.list.select(Some(15));
        *app.list.offset_mut() = 10;
        let initial = view.lock().unwrap().snapshot();
        app.reconcile(&initial);
        let playing = app.selected(&initial).unwrap().stationuuid.clone();
        let (sender, receiver) = crate::tui::command::channel(4);
        app.key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &initial,
            &sender,
        );
        worker::execute(
            receiver.try_recv().unwrap(),
            &mut engine,
            &mut filter,
            &mut None,
            &view,
        )
        .unwrap();
        assert_eq!(engine.status.playback_state, PlaybackState::Connecting);
        app.key(
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
            &initial,
            &sender,
        );
        assert_eq!(app.list.selected(), Some(16));

        let deadline = Instant::now() + Duration::from_secs(3);
        while engine.status.playback_state != PlaybackState::Playing {
            assert!(Instant::now() < deadline, "playback was not confirmed");
            engine.tick().unwrap();
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            engine.store.meta("pref_last_station").unwrap().as_deref(),
            Some(playing.as_str())
        );

        // Hold the real reader so the otherwise brief refresh is deterministic.
        let (started, waiting) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        *worker.sender.0.before_read.lock().unwrap() = Some(Box::new(move || {
            started.send(()).unwrap();
            blocked.recv_timeout(Duration::from_secs(3)).unwrap();
        }));
        worker::refresh_if_needed(&engine, &filter, &view, false).unwrap();
        waiting.recv_timeout(Duration::from_secs(3)).unwrap();
        let refreshing = view.lock().unwrap().snapshot();
        assert_eq!(
            refreshing.page.as_ref().unwrap().phase,
            PagePhase::Refreshing
        );
        assert_eq!(refreshing.stations_for(&filter).len(), 40);
        app.reconcile(&refreshing);
        app.key(
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
            &refreshing,
            &sender,
        );
        assert_eq!((app.list.selected(), app.list.offset()), (Some(17), 10));
        let selected = app.selected(&refreshing).unwrap().stationuuid.clone();

        // Another mutation while the reader waits must retain the visible page.
        engine.store.set_meta("test_refresh", "again").unwrap();
        worker::refresh_if_needed(&engine, &filter, &view, false).unwrap();
        {
            let data = view.lock().unwrap();
            assert_eq!(data.page.as_ref().unwrap().phase, PagePhase::Refreshing);
            assert_eq!(data.stations_for(&filter).len(), 40);
        }
        release.send(()).unwrap();
        wait(&view, |v| {
            v.page.as_ref().unwrap().phase == PagePhase::Ready
        });
        let refreshed = view.lock().unwrap().snapshot();
        app.reconcile(&refreshed);
        assert_eq!((app.list.selected(), app.list.offset()), (Some(17), 10));
        assert_eq!(app.selected(&refreshed).unwrap().stationuuid, selected);
        assert!(receiver.try_recv().is_err());
        engine.shutdown().unwrap();
    }

    #[test]
    fn owner_revision_failure_retries_the_pending_request_without_a_store_change() {
        use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
        let view = Arc::new(Mutex::new(View::new(engine.status.clone())));
        let mut filter = Filter {
            limit: 100,
            ..Filter::default()
        };
        worker::refresh_if_needed(&engine, &filter, &view, false).unwrap();
        let mut job = None;
        for command in [
            Command::Search(Filter {
                query: "needle".into(),
                ..filter.clone()
            }),
            Command::Recordings(filter.clone()),
            Command::Refresh,
        ] {
            worker::execute(command, &mut engine, &mut filter, &mut job, &view).unwrap();
            let revision = engine.store.revision().unwrap();
            // Isolate retry intent from the preference write's revision change.
            view.lock().unwrap().store_revision = Some(revision);
            engine
                .store
                .test_connection()
                .authorizer(Some(|context: AuthContext<'_>| {
                    if matches!(
                        context.action,
                        AuthAction::Pragma {
                            pragma_name: "data_version",
                            ..
                        }
                    ) {
                        Authorization::Deny
                    } else {
                        Authorization::Allow
                    }
                }));
            worker::refresh_if_needed(&engine, &filter, &view, false).unwrap();
            assert!(view.lock().unwrap().page_error(&filter).is_some());
            let retry_state = {
                let data = view.lock().unwrap();
                (
                    data.page_retry_at,
                    data.page_retry_attempt,
                    data.logs_revision,
                    data.revision,
                )
            };
            for _ in 0..3 {
                worker::refresh_if_needed(&engine, &filter, &view, false).unwrap();
                let data = view.lock().unwrap();
                assert_eq!(
                    (
                        data.page_retry_at,
                        data.page_retry_attempt,
                        data.logs_revision,
                        data.revision
                    ),
                    retry_state
                );
            }
            engine
                .store
                .test_connection()
                .authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
            assert_eq!(engine.store.revision().unwrap(), revision);
            let logs = view.lock().unwrap().logs.len();
            worker::refresh_if_needed(&engine, &filter, &view, false).unwrap();
            assert_eq!(view.lock().unwrap().logs.len(), logs);
            assert!(view.lock().unwrap().page_error(&filter).is_some());
            view.lock().unwrap().page_retry_at = Some(Instant::now());
            worker::refresh_if_needed(&engine, &filter, &view, false).unwrap();
            let data = view.lock().unwrap();
            assert!(data.page_ready(&filter));
            assert!(data.page_retry_at.is_none());
        }
    }

    #[test]
    fn reader_failure_retries_with_backoff_and_deduplicates_logs() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
        let path = dir.path().join("radio.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "INSERT INTO recordings(id,status,json) VALUES ('broken','complete','bad')",
            [],
        )
        .unwrap();
        let view = Arc::new(Mutex::new(View::new(engine.status.clone())));
        let worker = Worker::start(&path, &view).unwrap();
        view.lock().unwrap().queries = Some(worker.sender.clone());
        let filter = Filter {
            recordings: true,
            limit: 100,
            ..Filter::default()
        };
        worker::refresh_if_needed(&engine, &filter, &view, false).unwrap();
        wait(&view, |v| v.page_error(&filter).is_some());
        for attempt in 1..=8 {
            let data = view.lock().unwrap();
            assert_eq!(data.page_retry_attempt, attempt);
            let remaining = data
                .page_retry_at
                .unwrap()
                .saturating_duration_since(Instant::now());
            assert!(remaining <= Duration::from_secs(30));
            let revision = data.revision;
            drop(data);
            worker::refresh_if_needed(&engine, &filter, &view, false).unwrap();
            assert_eq!(
                view.lock().unwrap().revision,
                revision,
                "retried before its deadline"
            );
            view.lock().unwrap().page_retry_at = Some(Instant::now());
            worker::refresh_if_needed(&engine, &filter, &view, false).unwrap();
            wait(&view, |v| {
                v.page_error(&filter).is_some() && v.page_retry_attempt > attempt
            });
            assert_eq!(view.lock().unwrap().logs.len(), 1);
        }
        // Acknowledge the repair's revision first, so only the retry deadline
        // can trigger recovery rather than detection of an external commit.
        let before = engine.store.revision().unwrap();
        conn.execute("DELETE FROM recordings", []).unwrap();
        let after = engine.store.revision().unwrap();
        view.lock().unwrap().store_revision = Some(after);
        assert_ne!(before, after);
        view.lock().unwrap().page_retry_at = Some(Instant::now());
        worker::refresh_if_needed(&engine, &filter, &view, false).unwrap();
        wait(&view, |v| v.page_ready(&filter));
        let data = view.lock().unwrap();
        assert!(data.page_retry_at.is_none());
        assert_eq!(data.page_retry_attempt, 0);
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

#[cfg(test)]
mod map_tests {
    use super::*;
    use std::{
        sync::mpsc,
        time::{Duration, Instant},
    };
    #[test]
    fn country_counts_are_read_only_when_requested_and_follow_catalog_revisions() {
        use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("radio.db")).unwrap();
        let mut reader = Reader::default();
        let mut request = super::super::super::map::Request {
            filter: Filter::default(),
            bounds: crate::geo::GeoBounds::default(),
            columns: 80,
            rows: 20,
            countries: false,
        };
        store
            .test_connection()
            .authorizer(Some(|context: AuthContext<'_>| {
                if matches!(
                    context.action,
                    AuthAction::Read {
                        column_name: "country",
                        ..
                    }
                ) {
                    Authorization::Deny
                } else {
                    Authorization::Allow
                }
            }));
        assert!(
            reader
                .map(&store, &request, None, || false)
                .unwrap()
                .countries
                .is_empty()
        );
        request.countries = true;
        assert!(reader.map(&store, &request, None, || false).is_err());
        store
            .test_connection()
            .authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
        assert!(
            reader
                .map(&store, &request, None, || false)
                .unwrap()
                .countries
                .is_empty()
        );
        store
            .upsert(&Station {
                stationuuid: uuid::Uuid::new_v4().to_string(),
                name: "Unmapped station".into(),
                countrycode: "CN".into(),
                active: true,
                ..Station::default()
            })
            .unwrap();
        let image = reader.map(&store, &request, None, || false).unwrap();
        assert_eq!(image.revision, store.geo_revision().unwrap());
        assert_eq!(
            image.countries,
            vec![crate::model::Facet {
                value: "CN".into(),
                count: 1
            }]
        );
    }

    #[test]
    fn pending_page_preempts_a_map_without_publishing_an_error_and_map_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("radio.db");
        let engine = crate::engine::Engine::new(
            crate::test_support::config(dir.path()),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let view = Arc::new(Mutex::new(View::new(engine.status.clone())));
        let worker = Worker::start(&path, &view).unwrap();
        view.lock().unwrap().queries = Some(worker.sender.clone());
        let (started, waiting) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        *worker.sender.0.before_read.lock().unwrap() = Some(Box::new(move || {
            started.send(()).unwrap();
            blocked.recv_timeout(Duration::from_secs(3)).unwrap();
        }));
        view.lock()
            .unwrap()
            .request_map(super::super::super::map::Request {
                filter: Filter::default(),
                bounds: crate::geo::GeoBounds::default(),
                columns: 80,
                rows: 20,
                countries: false,
            });
        waiting.recv_timeout(Duration::from_secs(3)).unwrap();
        let filter = Filter {
            limit: 100,
            ..Filter::default()
        };
        super::super::refresh_if_needed(&engine, &filter, &view, false).unwrap();
        let (next_started, next_waiting) = mpsc::channel();
        let (next_release, next_blocked) = mpsc::channel();
        *worker.sender.0.before_read.lock().unwrap() = Some(Box::new(move || {
            next_started.send(()).unwrap();
            next_blocked.recv_timeout(Duration::from_secs(3)).unwrap();
        }));
        release.send(()).unwrap();
        next_waiting.recv_timeout(Duration::from_secs(3)).unwrap();
        {
            let data = view.lock().unwrap();
            assert!(data.map_image.is_none());
            assert!(data.map_error.is_none());
            let pending = worker.sender.0.pending.lock().unwrap();
            assert!(pending.map.is_some());
            assert!(pending.page.is_none());
        }
        next_release.send(()).unwrap();
        super::tests::wait(&view, |data| {
            data.page_ready(&filter) && data.map_image.is_some()
        });
        assert!(view.lock().unwrap().map_error.is_none());
    }

    #[test]
    fn superseded_map_requests_publish_only_the_latest_grid_and_share_land() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("radio.db");
        let store = Store::open(&path).unwrap();
        store
            .upsert(&Station {
                stationuuid: uuid::Uuid::new_v4().to_string(),
                name: "geographic".into(),
                active: true,
                geo_lat: Some(45.0),
                geo_long: Some(10.0),
                ..Station::default()
            })
            .unwrap();
        let engine = crate::engine::Engine::new(
            crate::test_support::config(dir.path()),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let view = Arc::new(Mutex::new(View::new(engine.status.clone())));
        let worker = Worker::start(&path, &view).unwrap();
        view.lock().unwrap().queries = Some(worker.sender.clone());
        let request = super::super::super::map::Request {
            filter: Filter::default(),
            bounds: crate::geo::GeoBounds::default(),
            columns: 80,
            rows: 20,
            countries: false,
        };
        let (started, waiting) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        *worker.sender.0.before_read.lock().unwrap() = Some(Box::new(move || {
            started.send(()).unwrap();
            blocked.recv_timeout(Duration::from_secs(3)).unwrap();
        }));
        view.lock().unwrap().request_map(request.clone());
        waiting.recv_timeout(Duration::from_secs(3)).unwrap();
        let latest = super::super::super::map::Request {
            columns: 100,
            ..request
        };
        view.lock().unwrap().request_map(latest.clone());
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let data = view.lock().unwrap();
            if let Some(image) = &data.map_image {
                assert_eq!(image.request, latest);
                assert_eq!(image.counts.iter().sum::<u32>(), 1);
                break;
            }
            drop(data);
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        let land = view
            .lock()
            .unwrap()
            .map_image
            .as_ref()
            .unwrap()
            .land
            .clone();
        let filtered = super::super::super::map::Request {
            filter: Filter {
                query: "absent".into(),
                ..Filter::default()
            },
            ..latest
        };
        view.lock().unwrap().request_map(filtered.clone());
        loop {
            let data = view.lock().unwrap();
            if let Some(image) = &data.map_image
                && image.request == filtered
            {
                assert!(Arc::ptr_eq(&image.land, &land));
                assert_eq!(image.counts.iter().sum::<u32>(), 0);
                break;
            }
            drop(data);
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
    }
}
