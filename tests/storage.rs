use radio_cli::{
    model::{Filter, Station},
    store::Store,
};
use tempfile::TempDir;

fn station(name: &str) -> Station {
    Station {
        stationuuid: uuid::Uuid::new_v4().to_string(),
        name: name.into(),
        url: "http://localhost/live".into(),
        countrycode: "CN".into(),
        language: "chinese, old church slavonic".into(),
        languagecodes: "zh,cu".into(),
        tags: "jazz, public radio".into(),
        bitrate: 128,
        active: true,
        lastcheckok: 1,
        ..Station::default()
    }
}
fn db() -> (TempDir, Store) {
    let dir = TempDir::new().unwrap();
    let store = Store::open(&dir.path().join("radio.db")).unwrap();
    (dir, store)
}

#[test]
fn unicode_filters_literals_and_pagination() {
    let (_dir, store) = db();
    let s = station("上海 Jazz 100%_FM");
    store.upsert(&s).unwrap();
    for query in ["上海", "jAZZ", "%_", "100%_FM"] {
        assert_eq!(
            store
                .search(&Filter {
                    query: query.into(),
                    limit: 100,
                    ..Filter::default()
                })
                .unwrap()
                .len(),
            1
        );
    }
    let mut filter = Filter {
        country: Some("cn".into()),
        language: Some("old church slavonic".into()),
        tag: Some("public radio".into()),
        min_bitrate: 128,
        limit: 100,
        ..Filter::default()
    };
    assert_eq!(store.search(&filter).unwrap().len(), 1);
    filter.language = Some("zh".into());
    assert_eq!(store.search(&filter).unwrap().len(), 1);
    filter.tag = Some("radio".into());
    assert!(store.search(&filter).unwrap().is_empty());
    filter.tag = None;
    filter.min_bitrate = 129;
    assert!(store.search(&filter).unwrap().is_empty());
    assert!(
        store
            .search(&Filter {
                query: "' OR 1=1 --".into(),
                limit: 100,
                ..Filter::default()
            })
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .search(&Filter {
                offset: 1,
                limit: 100,
                ..Filter::default()
            })
            .unwrap()
            .is_empty()
    );
}

#[test]
fn sync_is_atomic_and_preserves_local_state() {
    let (_dir, mut store) = db();
    let first = station("旧台");
    let mut custom = station("自定义");
    custom.custom = true;
    store.upsert(&first).unwrap();
    store.upsert(&custom).unwrap();
    store.favorite(&first.stationuuid, true).unwrap();
    store.played(&first.stationuuid).unwrap();
    store
        .add_backup(&first.stationuuid, "https://localhost/backup")
        .unwrap();
    store.begin_sync().unwrap();
    let second = station("新台");
    store.stage(std::slice::from_ref(&second)).unwrap();
    assert!(store.station(&second.stationuuid).is_err());
    assert_eq!(store.count().unwrap(), 2);
    // An aborted sync is replaced by the next attempt without publishing partial rows.
    store.begin_sync().unwrap();
    assert!(store.finish_sync().is_err());
    assert_eq!(store.count().unwrap(), 2);
    store.stage(std::slice::from_ref(&second)).unwrap();
    store.stage(std::slice::from_ref(&second)).unwrap();
    assert_eq!(store.finish_sync().unwrap(), 1);
    let saved = store.station(&first.stationuuid).unwrap();
    assert!(!saved.active);
    assert!(saved.favorite);
    assert!(store.station(&custom.stationuuid).unwrap().custom);
    assert_eq!(store.backups(&first.stationuuid).unwrap().len(), 1);
    assert_eq!(
        store
            .search(&Filter {
                history: true,
                limit: 100,
                ..Filter::default()
            })
            .unwrap()
            .len(),
        1
    );
    assert!(store.meta("last_sync").unwrap().is_some());
    store.favorite(&first.stationuuid, false).unwrap();
    assert_eq!(
        store
            .search(&Filter {
                limit: 100,
                ..Filter::default()
            })
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn random_uses_entire_matching_catalog_and_excludes_candidates() {
    let (_dir, store) = db();
    let mut ids = vec![];
    for i in 0..150 {
        let s = station(&format!("Station {i}"));
        ids.push(s.stationuuid.clone());
        store.upsert(&s).unwrap();
    }
    let filter = Filter {
        limit: 1,
        ..Filter::default()
    };
    let last = ids.pop().unwrap();
    assert_eq!(
        store.random(&filter, &ids).unwrap().unwrap().stationuuid,
        last
    );
    ids.push(last);
    assert!(store.random(&filter, &ids).unwrap().is_none());
}

#[test]
fn malformed_record_does_not_replace_directory() {
    let (_dir, mut store) = db();
    let s = station("原目录");
    store.upsert(&s).unwrap();
    store.begin_sync().unwrap();
    let mut invalid = station("坏记录");
    invalid.stationuuid = "not-a-uuid".into();
    assert!(store.stage(&[station("有效记录"), invalid]).is_err());
    assert!(store.finish_sync().is_err());
    assert_eq!(store.count().unwrap(), 1);
}

#[test]
fn duration_and_terminal_sanitization() {
    use radio_cli::recorder::parse_duration;
    assert_eq!(parse_duration("30m").unwrap().as_secs(), 1800);
    for value in ["0", "-1", "1d", "1.5m", "18446744073709551615h", ""] {
        assert!(parse_duration(value).is_err());
    }
    assert_eq!(radio_cli::clean_text("电台\x1b\n\t名称"), "电台名称");
}

/// Run explicitly with `cargo test --release --test storage catalog_benchmark -- --ignored --nocapture`.
#[test]
#[ignore]
fn catalog_benchmark() {
    use std::time::Instant;
    let (_dir, mut store) = db();
    store.begin_sync().unwrap();
    for page in 0..50 {
        let rows: Vec<_> = (0..1000)
            .map(|i| {
                let mut s = station(&format!("上海 Jazz Station {}", page * 1000 + i));
                s.clickcount = i;
                s
            })
            .collect();
        store.stage(&rows).unwrap();
    }
    store.finish_sync().unwrap();
    let mut samples = vec![];
    for query in ["上海", "jazz", "Station 499", "不存在", "%_"] {
        for _ in 0..20 {
            let start = Instant::now();
            let _ = store
                .search(&Filter {
                    query: query.into(),
                    limit: 100,
                    ..Filter::default()
                })
                .unwrap();
            samples.push(start.elapsed().as_secs_f64() * 1000.);
        }
    }
    samples.sort_by(f64::total_cmp);
    println!("50,000 stations: search P95 {:.2} ms", samples[94]);
    assert!(samples[94] < 100., "search P95 exceeds 100 ms");
}
