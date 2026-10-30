//! Opt-in measurements; fixtures are local and deterministic.
use radio_cli::{
    model::{FacetKind, Filter, Sort, Station},
    store::Store,
};
use std::time::Instant;

fn measure(mut work: impl FnMut(), repetitions: usize) -> (f64, f64) {
    let mut samples = Vec::new();
    for _ in 0..repetitions {
        let start = Instant::now();
        work();
        samples.push(start.elapsed().as_secs_f64() * 1000.);
    }
    samples.sort_by(f64::total_cmp);
    (
        samples[samples.len() / 2],
        samples[(samples.len() * 95 / 100).min(samples.len() - 1)],
    )
}

fn stage_catalog(store: &mut Store, diverse: bool) {
    use rand::{SeedableRng, distr::SampleString};

    let mut rng = rand::rngs::StdRng::seed_from_u64(0);
    store.begin_sync().unwrap();
    for page in 0..200 {
        let rows: Vec<_> = (0..1000)
            .map(|i| {
                let id = page * 1000 + i;
                Station {
                    stationuuid: uuid::Uuid::from_u128(id as u128 + 1).to_string(),
                    name: if diverse {
                        format!(
                            "上海 Jazz Station {id} {}",
                            rand::distr::Alphanumeric.sample_string(&mut rng, 50)
                        )
                    } else {
                        format!("上海 Jazz Station {id}")
                    },
                    url: format!("http://example.test/{id}"),
                    countrycode: if id % 3 == 0 { "CN" } else { "FR" }.into(),
                    language: "chinese,chinese,english".into(),
                    languagecodes: "zh,en".into(),
                    tags: "jazz,public radio,public radio".into(),
                    clickcount: (id % 1000) as u64,
                    bitrate: (id % 4) * 64,
                    active: true,
                    ..Station::default()
                }
            })
            .collect();
        store.stage(&rows).unwrap();
    }
}

#[test]
#[ignore]
fn catalog_statistics_scan_cost() {
    for diverse in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("radio.db");
        let mut store = Store::open(&path).unwrap();
        stage_catalog(&mut store, diverse);
        let start = Instant::now();
        assert_eq!(store.finish_sync().unwrap(), 200000);
        let publication = start.elapsed();
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA cache_size=-4096").unwrap();
        let scan = measure(
            || {
                let mut stmt = conn.prepare("SELECT term,doc FROM station_search_terms UNION ALL SELECT '',count(*) FROM stations").unwrap();
                let stats: Vec<_> = stmt
                    .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
                    .unwrap()
                    .collect::<rusqlite::Result<_>>()
                    .unwrap();
                assert!(stats.contains(&(String::new(), 200000)));
                std::hint::black_box(stats);
            },
            3,
        );
        let terms: usize = conn
            .query_row("SELECT count(*) FROM station_search_stats", [], |r| {
                r.get(0)
            })
            .unwrap();
        println!(
            "200,000 stations diverse={diverse}: publish including maintenance {:.2}s; {terms} statistics rows; vocabulary scan P50/P95 {:.2}/{:.2} ms",
            publication.as_secs_f64(),
            scan.0,
            scan.1
        );
    }
}

#[test]
#[ignore]
fn catalog_performance_matrix() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("radio.db");
    let mut store = Store::open(&path).unwrap();
    let start = Instant::now();
    stage_catalog(&mut store, false);
    let staged = start.elapsed();
    let publish = Instant::now();
    store.finish_sync().unwrap();
    println!(
        "200,000 stations: stage {:.2}s; publish {:.2}s",
        staged.as_secs_f64(),
        publish.elapsed().as_secs_f64()
    );
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA cache_size=-4096; CREATE TEMP TABLE baseline AS SELECT * FROM stations;
        CREATE INDEX baseline_country ON baseline(country,bitrate); CREATE INDEX baseline_popularity ON baseline(popularity DESC);").unwrap();
    for query in ["jazz", "Station 199999", "不存在", "上海", "%_"] {
        for (sort, order) in [
            (Sort::Clickcount, "s.popularity DESC,s.name,s.uuid"),
            (Sort::Name, "s.name COLLATE NOCASE,s.uuid"),
            (Sort::Bitrate, "s.bitrate DESC,s.name,s.uuid"),
        ] {
            for offset in [0, 199900] {
                let filter = Filter {
                    query: query.into(),
                    sort,
                    limit: 100,
                    offset,
                    ..Filter::default()
                };
                let baseline_sql = format!("SELECT s.json,s.active,f.uuid IS NOT NULL FROM baseline s
                    LEFT JOIN favorites f ON f.uuid=s.uuid WHERE (s.active=1 OR s.custom=1 OR f.uuid IS NOT NULL)
                    AND instr(s.search,?1)>0 AND s.bitrate>=0 ORDER BY {order} LIMIT 100 OFFSET ?2");
                let old = measure(
                    || {
                        let mut stmt = conn.prepare(&baseline_sql).unwrap();
                        let rows = stmt
                            .query_map(
                                rusqlite::params![query.to_lowercase(), offset as i64],
                                |r| {
                                    Ok((
                                        r.get::<_, String>(0)?,
                                        r.get::<_, bool>(1)?,
                                        r.get::<_, bool>(2)?,
                                    ))
                                },
                            )
                            .unwrap();
                        for row in rows {
                            let (json, active, favorite) = row.unwrap();
                            let mut s: Station = serde_json::from_str(&json).unwrap();
                            s.active = active;
                            s.favorite = favorite;
                            std::hint::black_box(s);
                        }
                    },
                    5,
                );
                let new = measure(
                    || {
                        std::hint::black_box(store.search(&filter).unwrap());
                    },
                    5,
                );
                println!(
                    "search {query:?} {sort:?} offset={offset}: baseline P50/P95 {:.2}/{:.2} ms; current {:.2}/{:.2} ms",
                    old.0, old.1, new.0, new.1
                );
            }
        }
        let filter = Filter {
            query: query.into(),
            limit: 100,
            ..Filter::default()
        };
        let old = measure(
            || {
                std::hint::black_box(conn.query_row("SELECT count(*) FROM baseline s LEFT JOIN favorites f ON f.uuid=s.uuid WHERE (s.active=1 OR s.custom=1 OR f.uuid IS NOT NULL) AND instr(s.search,?1)>0 AND s.bitrate>=0",[query.to_lowercase()],|r|r.get::<_,usize>(0)).unwrap());
            },
            5,
        );
        let new = measure(
            || {
                std::hint::black_box(store.search_count(&filter).unwrap());
            },
            5,
        );
        println!(
            "count {query:?}: baseline {:.2}/{:.2} ms; current {:.2}/{:.2} ms",
            old.0, old.1, new.0, new.1
        );
    }
    let id = uuid::Uuid::from_u128(190001).to_string();
    let old = measure(
        || {
            std::hint::black_box(conn.query_row("WITH ranked AS (SELECT s.uuid,row_number() OVER (ORDER BY s.popularity DESC,s.name,s.uuid)-1 AS p FROM baseline s LEFT JOIN favorites f ON f.uuid=s.uuid WHERE (s.active=1 OR s.custom=1 OR f.uuid IS NOT NULL) AND s.bitrate>=0) SELECT p FROM ranked WHERE uuid=?1",[&id],|r|r.get::<_,usize>(0)).unwrap());
        },
        5,
    );
    let new = measure(
        || {
            std::hint::black_box(store.station_position(&id, &Filter::default()).unwrap());
        },
        5,
    );
    println!(
        "position: baseline {:.2}/{:.2} ms; current {:.2}/{:.2} ms",
        old.0, old.1, new.0, new.1
    );
    for (kind, column) in [(FacetKind::Language, "languages"), (FacetKind::Tag, "tags")] {
        let sql = format!(
            "WITH RECURSIVE split(uuid,value,rest) AS (SELECT uuid,'',trim({column},',')||',' FROM baseline WHERE active=1 UNION ALL SELECT uuid,substr(rest,1,instr(rest,',')-1),substr(rest,instr(rest,',')+1) FROM split WHERE rest<>''), entries AS (SELECT DISTINCT uuid,value FROM split WHERE value<>'') SELECT value,count(*) FROM entries GROUP BY value ORDER BY value LIMIT 100"
        );
        let old = measure(
            || {
                let mut stmt = conn.prepare(&sql).unwrap();
                std::hint::black_box(
                    stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, usize>(1)?)))
                        .unwrap()
                        .collect::<rusqlite::Result<Vec<_>>>()
                        .unwrap(),
                );
            },
            3,
        );
        let new = measure(
            || {
                std::hint::black_box(store.facets_limited(kind, "", 100).unwrap());
            },
            5,
        );
        println!(
            "facets {kind:?}: baseline {:.2}/{:.2} ms; current {:.2}/{:.2} ms",
            old.0, old.1, new.0, new.1
        );
    }
    for (label, condition, directory) in [
        ("active", "active=1", false),
        ("directory", "active=1 AND custom=0", true),
    ] {
        let old = measure(
            || {
                std::hint::black_box(
                    conn.query_row(
                        &format!("SELECT count(*) FROM baseline WHERE {condition}"),
                        [],
                        |r| r.get::<_, usize>(0),
                    )
                    .unwrap(),
                );
            },
            5,
        );
        let new = measure(
            || {
                std::hint::black_box(if directory {
                    store.directory_count().unwrap()
                } else {
                    store.count().unwrap()
                });
            },
            5,
        );
        println!(
            "{label} count: baseline {:.2}/{:.2} ms; current {:.2}/{:.2} ms",
            old.0, old.1, new.0, new.1
        );
    }
    let custom = Station {
        stationuuid: uuid::Uuid::new_v4().to_string(),
        name: "custom".into(),
        url: "https://example.test/custom".into(),
        active: true,
        custom: true,
        ..Station::default()
    };
    let old = measure(
        || {
            let key = radio_cli::service::normalized_url(custom.primary_url()).unwrap();
            let mut stmt = conn
                .prepare("SELECT uuid,json FROM baseline WHERE uuid<>?1 AND active=1")
                .unwrap();
            let rows = stmt
                .query_map([&custom.stationuuid], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .unwrap();
            for row in rows {
                let (_, json) = row.unwrap();
                let other: Station = serde_json::from_str(&json).unwrap();
                assert_ne!(
                    radio_cli::service::normalized_url(other.primary_url())
                        .ok()
                        .as_ref(),
                    Some(&key)
                );
            }
        },
        5,
    );
    let new = measure(
        || {
            store.save_custom(&custom, None).unwrap();
        },
        5,
    );
    println!(
        "save custom: baseline {:.2}/{:.2} ms; current {:.2}/{:.2} ms",
        old.0, old.1, new.0, new.1
    );
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    println!(
        "database {:.2} MiB",
        std::fs::metadata(&path).unwrap().len() as f64 / 1048576.
    );
    let mut stmt = conn
        .prepare("SELECT name,sum(pgsize) FROM dbstat GROUP BY name ORDER BY name")
        .unwrap();
    for row in stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?)))
        .unwrap()
    {
        let (name, bytes) = row.unwrap();
        if name.starts_with("station_") || name == "facets_cache" {
            println!("{name}: {:.2} MiB", bytes as f64 / 1048576.);
        }
    }
    drop(stmt);
    // Recreate a v2 database to measure the complete first-open migration.
    let legacy_path = dir.path().join("legacy.db");
    let legacy = rusqlite::Connection::open(&legacy_path).unwrap();
    legacy
        .execute("ATTACH DATABASE ?1 AS source", [path.to_str().unwrap()])
        .unwrap();
    legacy.execute_batch("CREATE TABLE stations AS SELECT uuid,name,search,country,languages,tags,bitrate,popularity,custom,active,json FROM source.stations;
        CREATE UNIQUE INDEX legacy_uuid ON stations(uuid);
        CREATE INDEX station_country ON stations(country,bitrate);
        CREATE INDEX station_popularity ON stations(popularity DESC);
        CREATE TABLE favorites(uuid TEXT PRIMARY KEY,created_at TEXT NOT NULL);
        CREATE TABLE history(id INTEGER PRIMARY KEY,uuid TEXT NOT NULL,played_at TEXT NOT NULL);
        CREATE INDEX history_station ON history(uuid);
        CREATE TABLE recordings(id TEXT PRIMARY KEY,status TEXT NOT NULL,json TEXT NOT NULL,recovery_pending INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
        CREATE TABLE streams(uuid TEXT NOT NULL,url TEXT NOT NULL,PRIMARY KEY(uuid,url));
        PRAGMA user_version=2;").unwrap();
    drop(legacy);
    println!(
        "legacy database {:.2} MiB",
        std::fs::metadata(&legacy_path).unwrap().len() as f64 / 1048576.
    );
    let start = Instant::now();
    let migrated = Store::open(&legacy_path).unwrap();
    println!("v2-to-v5 migration {:.2}s", start.elapsed().as_secs_f64());
    assert_eq!(migrated.count().unwrap(), 200001);
}

#[test]
#[ignore = "release geography performance and resource fixture"]
fn geographic_catalog_benchmark() {
    use radio_cli::geo::{GeoBounds, GeoFilter};
    let dir = tempfile::tempdir().unwrap();
    let path = std::env::var_os("RADIO_MAP_BENCH_DB")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| dir.path().join("radio.db"));
    let mut store = Store::open(&path).unwrap();
    store.begin_sync().unwrap();
    for page in 0..200 {
        let rows: Vec<_> = (0..1000)
            .map(|i| {
                let id = page * 1000 + i;
                let (lat, lon) = match id % 5 {
                    0 => (48.85, 2.35), // Dense, overlapping city points.
                    1 => (0.0, 179.9),
                    _ => (
                        (id % 18000) as f64 / 100.0 - 90.0,
                        ((id * 7919) % 36000) as f64 / 100.0 - 180.0,
                    ),
                };
                Station {
                    stationuuid: uuid::Uuid::from_u128(id as u128 + 1).to_string(),
                    name: format!("Geo Station {id}"),
                    url: format!("http://127.0.0.1:1/{id}"),
                    countrycode: if id % 2 == 0 { "FR" } else { "CN" }.into(),
                    geo_lat: Some(lat),
                    geo_long: Some(lon),
                    active: true,
                    ..Station::default()
                }
            })
            .collect();
        store.stage(&rows).unwrap();
    }
    store.finish_sync().unwrap();
    for (name, bounds) in [
        ("world", GeoBounds::default()),
        (
            "8x",
            GeoBounds {
                x: 170_000_000,
                y: 30_000_000,
                width: 45_000_000,
                height: 22_500_000,
            },
        ),
    ] {
        let (p50, p95) = measure(
            || {
                std::hint::black_box(
                    store
                        .geo_counts(&Filter::default(), bounds, 100, 25, || false)
                        .unwrap(),
                );
            },
            20,
        );
        eprintln!("map {name} P50={p50:.2}ms P95={p95:.2}ms");
    }
    let f = Filter {
        geo: Some(GeoFilter::Cell(GeoBounds::default().cell(50, 5, 100, 25))),
        limit: 100,
        ..Filter::default()
    };
    let (p50, p95) = measure(
        || {
            std::hint::black_box(store.search(&f).unwrap());
            std::hint::black_box(store.search_count(&f).unwrap());
        },
        20,
    );
    eprintln!("map page P50={p50:.2}ms P95={p95:.2}ms");
    let conn = rusqlite::Connection::open(&path).unwrap();
    let size: i64 = conn
        .query_row(
            "SELECT sum(pgsize) FROM dbstat WHERE name='station_geo'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    eprintln!("geography index {:.2} MiB", size as f64 / 1048576.0);
}
