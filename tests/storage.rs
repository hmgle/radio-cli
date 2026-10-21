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
fn station_logos_are_optional_in_older_json_and_round_trip() {
    let mut old = serde_json::to_value(station("old")).unwrap();
    old.as_object_mut().unwrap().remove("favicon");
    let mut station: Station = serde_json::from_value(old).unwrap();
    assert!(station.favicon.is_empty());
    station.favicon = "https://example.test/logo.png".into();
    let (_dir, store) = db();
    store.upsert(&station).unwrap();
    assert_eq!(
        store.station(&station.stationuuid).unwrap().favicon,
        station.favicon
    );
}

#[test]
fn unicode_filters_literals_and_pagination() {
    let (_dir, store) = db();
    let s = station("上海 Jazz 100%_FM");
    store.upsert(&s).unwrap();
    for query in [
        "上海", "jAZZ", "%_", "100%_FM", " jazz ", "　jazz", "\tjazz\t",
    ] {
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
    for query in ["", " \t　 "] {
        let filter = Filter {
            query: query.into(),
            limit: 100,
            ..Filter::default()
        };
        assert_eq!(store.search(&filter).unwrap().len(), 1);
        assert_eq!(store.search_count(&filter).unwrap(), 1);
        assert_eq!(
            store.random(&filter, &[]).unwrap().unwrap().stationuuid,
            s.stationuuid
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
fn chinese_territory_queries_preserve_upstream_metadata_and_code_filters() {
    let (_dir, store) = db();
    let mut taiwan = station("Catalog station");
    taiwan.country = "Taiwan, Republic Of China".into();
    taiwan.countrycode = "TW".into();
    store.upsert(&taiwan).unwrap();
    for code in ["CN", "HK", "MO", "FR", "CD", "CG"] {
        let mut row = station("Other station");
        row.countrycode = code.into();
        store.upsert(&row).unwrap();
    }

    for query in ["台湾", " 台湾 ", "Republic Of China"] {
        let filter = Filter {
            query: query.into(),
            limit: 100,
            ..Filter::default()
        };
        let rows = store.search(&filter).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].stationuuid, taiwan.stationuuid);
        assert_eq!(rows[0].country, taiwan.country);
        assert_eq!(rows[0].countrycode, "TW");
        assert_eq!(store.search_count(&filter).unwrap(), 1);
        assert_eq!(
            store
                .station_position(&taiwan.stationuuid, &filter)
                .unwrap(),
            Some(0)
        );
        assert_eq!(
            store.random(&filter, &[]).unwrap().unwrap().stationuuid,
            taiwan.stationuuid
        );
        assert!(
            store
                .random(&filter, &[taiwan.stationuuid.clone()])
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .search(&Filter {
                    country: Some("CN".into()),
                    ..filter
                })
                .unwrap()
                .is_empty()
        );
    }
    for (query, expected) in [("中国", 3), ("刚果", 2), ("法国", 1)] {
        let filter = Filter {
            query: query.into(),
            limit: 100,
            ..Filter::default()
        };
        assert_eq!(store.search(&filter).unwrap().len(), expected);
        assert_eq!(store.search_count(&filter).unwrap(), expected);
    }
    let mut literal = station("台湾音乐");
    literal.countrycode = "FR".into();
    store.upsert(&literal).unwrap();
    assert_eq!(
        store
            .search_count(&Filter {
                query: "台湾".into(),
                ..Filter::default()
            })
            .unwrap(),
        2
    );
    let stored = store.station(&taiwan.stationuuid).unwrap();
    assert_eq!(
        serde_json::to_value(stored).unwrap()["country"],
        taiwan.country
    );
    let facets = store
        .facets(radio_cli::model::FacetKind::Country, "T")
        .unwrap();
    assert_eq!(facets.len(), 1);
    assert_eq!(facets[0].value, "TW");
}

#[test]
fn single_character_queries_do_not_widen_to_territory_names() {
    let (_dir, store) = db();
    let mut singapore = station("Catalog station");
    singapore.country = "Singapore".into();
    singapore.countrycode = "SG".into();
    store.upsert(&singapore).unwrap();
    let mut named = station("新歌电台");
    named.countrycode = "FR".into();
    store.upsert(&named).unwrap();
    let matches = |query: &str| {
        store
            .search(&Filter {
                query: query.into(),
                limit: 100,
                ..Filter::default()
            })
            .unwrap()
            .len()
    };
    // "新" is a prefix of 新加坡 but also of 新喀里多尼亚, 新西兰 and 巴布亚新几内亚,
    // so a one-character query must not pull in whole territories.
    assert_eq!(matches("新"), 1);
    assert_eq!(matches("新加坡"), 1);
    assert_eq!(matches(" 新加坡 "), 1);
    assert_eq!(matches("新歌"), 1);
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
    assert!(
        store
            .random(
                &Filter {
                    query: "missing".into(),
                    ..filter.clone()
                },
                &[]
            )
            .unwrap()
            .is_none()
    );
    let selected = store
        .random(&filter, &["' OR 1=1 --".into()])
        .unwrap()
        .unwrap();
    assert!(ids.contains(&selected.stationuuid));
}

#[test]
fn random_pages_preserve_filters_limits_and_history_order() {
    use radio_cli::model::Sort;

    let (_dir, store) = db();
    let mut matching = Vec::new();
    for i in 0..30 {
        let row = station(if i % 2 == 0 { "needle" } else { "other" });
        store.upsert(&row).unwrap();
        store.played(&row.stationuuid).unwrap();
        if i % 2 == 0 {
            matching.push(row.stationuuid);
        }
    }
    let filter = Filter {
        query: "NEEDLE".into(),
        sort: Sort::Random,
        offset: 3,
        limit: 5,
        ..Filter::default()
    };
    let page = store.search(&filter).unwrap();
    assert_eq!(page.len(), 5);
    let ids: std::collections::BTreeSet<_> = page.iter().map(|row| &row.stationuuid).collect();
    assert_eq!(ids.len(), 5);
    assert!(ids.iter().all(|id| matching.contains(id)));
    assert!(
        store
            .search(&Filter {
                offset: matching.len(),
                ..filter.clone()
            })
            .unwrap()
            .is_empty()
    );
    let history: Vec<_> = store
        .search(&Filter {
            history: true,
            ..filter
        })
        .unwrap()
        .into_iter()
        .map(|row| row.stationuuid)
        .collect();
    assert_eq!(
        history,
        matching
            .into_iter()
            .rev()
            .skip(3)
            .take(5)
            .collect::<Vec<_>>()
    );
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
fn bootstrap_rolls_back_mid_batch_failure_and_cancellation() {
    let (_dir, mut store) = db();
    let old = station("Original");
    store.upsert(&old).unwrap();
    let mut replacement = old.clone();
    replacement.name = "Replacement".into();
    let new = station("New");
    let mut invalid = station("Invalid");
    invalid.stationuuid = "bad-uuid".into();
    assert!(
        store
            .bootstrap(&[replacement.clone(), new.clone(), invalid], || false)
            .is_err()
    );
    assert_eq!(store.count().unwrap(), 1);
    assert_eq!(store.station(&old.stationuuid).unwrap().name, old.name);
    for cancel_at in 1..=3 {
        let mut checks = 0;
        assert!(
            store
                .bootstrap(&[replacement.clone(), new.clone()], || {
                    checks += 1;
                    checks == cancel_at
                })
                .is_err()
        );
        assert_eq!(store.count().unwrap(), 1);
        assert_eq!(store.station(&old.stationuuid).unwrap().name, old.name);
    }
    assert_eq!(store.bootstrap(&[replacement, new], || false).unwrap(), 2);
    assert_eq!(store.count().unwrap(), 2);
    assert!(store.bootstrap(&[], || false).is_err());
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

#[test]
fn domain_errors_have_stable_codes_and_parameters() {
    use radio_cli::{
        config::validate_url,
        error::{DomainError, ErrorCode},
        recorder,
    };
    for value in ["file:///tmp/audio", "invalid"] {
        let error = validate_url(value).unwrap_err();
        assert_eq!(
            error.downcast_ref::<DomainError>().unwrap().code,
            ErrorCode::InvalidUrl
        );
    }
    let (_dir, store) = db();
    let error = store.station("missing").unwrap_err();
    assert_eq!(
        error.downcast_ref::<DomainError>().unwrap().code,
        ErrorCode::StationNotFound
    );
    let error = recorder::format_for_codec("alac").unwrap_err();
    let domain = error.downcast_ref::<DomainError>().unwrap();
    assert_eq!(domain.code, ErrorCode::UnsupportedCodec);
    assert_eq!(domain.params["codec"], "alac");
}

#[test]
fn custom_station_mutations_are_atomic_and_preserve_references() {
    use radio_cli::{
        error::{DomainError, ErrorCode},
        service,
    };
    let (_dir, mut store) = db();
    let station = service::add_station(
        &mut store,
        "Original".into(),
        "https://EXAMPLE.com:443/live#fragment".into(),
        &["https://example.com/backup".into()],
    )
    .unwrap();
    let duplicate = service::add_station(
        &mut store,
        "Duplicate".into(),
        "https://example.com/live".into(),
        &[],
    )
    .unwrap_err();
    assert_eq!(
        duplicate.downcast_ref::<DomainError>().unwrap().code,
        ErrorCode::DuplicateStation
    );
    assert!(
        service::add_station(
            &mut store,
            "Invalid backup".into(),
            "https://example.com/new".into(),
            &["file:///tmp/x".into()]
        )
        .is_err()
    );
    assert_eq!(store.count().unwrap(), 1);
    store.favorite(&station.stationuuid, true).unwrap();
    store.played(&station.stationuuid).unwrap();
    let edited = service::edit_station(
        &mut store,
        &station.stationuuid,
        Some("Renamed".into()),
        Some("https://example.com/changed".into()),
        &[],
    )
    .unwrap();
    assert_eq!(edited.name, "Renamed");
    assert_eq!(edited.primary_url(), "https://example.com/changed");
    assert!(edited.favorite);
    assert_eq!(store.backups(&station.stationuuid).unwrap().len(), 1);
    assert!(
        store
            .update_backups(
                &station.stationuuid,
                &["bad".into()],
                &["https://example.com/backup".into()]
            )
            .is_err()
    );
    assert_eq!(store.backups(&station.stationuuid).unwrap().len(), 1);
    assert_eq!(
        store
            .update_backups(
                &station.stationuuid,
                &["https://example.com/backup2".into()],
                &["https://example.com/backup".into()]
            )
            .unwrap(),
        vec!["https://example.com/backup2"]
    );
    store.remove_custom(&station.stationuuid, false).unwrap();
    let placeholder = store.station(&station.stationuuid).unwrap();
    assert_eq!(placeholder.name, "Renamed");
    assert!(!placeholder.active && !placeholder.custom);
    assert!(
        placeholder
            .stream_candidates(&store.backups(&station.stationuuid).unwrap())
            .is_empty()
    );
    let history = store
        .search(&Filter {
            history: true,
            limit: 100,
            ..Filter::default()
        })
        .unwrap();
    assert_eq!(history[0].name, "Renamed");
    assert!(store.remove_custom(&station.stationuuid, false).is_err());
    let unused = service::add_station(
        &mut store,
        "Unused".into(),
        "https://example.com/unused".into(),
        &[],
    )
    .unwrap();
    store.remove_custom(&unused.stationuuid, false).unwrap();
    assert!(store.station(&unused.stationuuid).is_err());
    assert_eq!(
        store
            .search_count(&Filter {
                custom: true,
                ..Filter::default()
            })
            .unwrap(),
        0
    );
}

#[test]
fn facets_count_distinct_stations_and_sort_combines_with_filters() {
    use radio_cli::model::{FacetKind, Sort};
    let (_dir, store) = db();
    let mut first = station("Alpha");
    first.languagecodes = "zh, zh,cu".into();
    first.bitrate = 128;
    first.votes = 50;
    first.clickcount = 1;
    let mut second = station("Beta");
    second.bitrate = 320;
    second.votes = 1;
    second.clickcount = 100;
    store.upsert(&first).unwrap();
    store.upsert(&second).unwrap();
    let languages = store.facets(FacetKind::Language, " ZH ").unwrap();
    assert_eq!(languages.len(), 1);
    assert_eq!(languages[0].value, "zh");
    assert_eq!(languages[0].count, 2);
    assert_eq!(
        store.facets(FacetKind::Country, "c").unwrap()[0].value,
        "CN"
    );
    assert_eq!(store.facets(FacetKind::Tag, "public").unwrap()[0].count, 2);
    assert!(store.facets(FacetKind::Tag, "%").unwrap().is_empty());
    for (sort, expected) in [
        (Sort::Name, &first),
        (Sort::Bitrate, &second),
        (Sort::Votes, &first),
        (Sort::Clickcount, &second),
    ] {
        let rows = store
            .search(&Filter {
                sort,
                min_bitrate: 128,
                limit: 100,
                ..Filter::default()
            })
            .unwrap();
        assert_eq!(rows[0].stationuuid, expected.stationuuid);
    }
    let rows = store
        .search(&Filter {
            sort: Sort::Bitrate,
            min_bitrate: 200,
            limit: 100,
            ..Filter::default()
        })
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].stationuuid, second.stationuuid);
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

#[test]
fn concurrent_favorite_toggles_preserve_every_transition() {
    use std::{
        sync::{Arc, Barrier},
        thread,
    };
    let (dir, store) = db();
    let row = station("concurrent favorite");
    store.upsert(&row).unwrap();
    let ready = Arc::new(Barrier::new(3));
    let mut threads = Vec::new();
    for _ in 0..2 {
        let ready = ready.clone();
        let path = dir.path().join("radio.db");
        let id = row.stationuuid.clone();
        threads.push(thread::spawn(move || {
            let mut store = Store::open(&path).unwrap();
            ready.wait();
            (0..50)
                .map(|_| store.toggle_favorite(&id).unwrap())
                .collect::<Vec<_>>()
        }));
    }
    ready.wait();
    let transitions: Vec<_> = threads
        .into_iter()
        .flat_map(|t| t.join().unwrap())
        .collect();
    assert_eq!(transitions.iter().filter(|&&added| added).count(), 50);
    assert_eq!(transitions.iter().filter(|&&added| !added).count(), 50);
    assert!(!store.station(&row.stationuuid).unwrap().favorite);
    let mut store = Store::open(&dir.path().join("radio.db")).unwrap();
    assert!(store.toggle_favorite("missing").is_err());
    assert!(store.toggle_favorite(&row.stationuuid).unwrap());
}

#[test]
fn history_retention_uses_an_indexed_id_threshold() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("radio.db");
    let store = Store::open(&path).unwrap();
    let station = Station {
        stationuuid: uuid::Uuid::new_v4().to_string(),
        name: "History".into(),
        ..Default::default()
    };
    store.upsert(&station).unwrap();
    for _ in 0..1200 {
        store.played(&station.stationuuid).unwrap();
    }
    let connection = rusqlite::Connection::open(&path).unwrap();
    let (count, min, max): (i64, i64, i64) = connection
        .query_row("SELECT count(*),min(id),max(id) FROM history", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    assert_eq!((count, min, max), (1000, 201, 1200));
}

#[test]
fn bounded_facets_can_reach_values_beyond_the_initial_limit() {
    use radio_cli::model::FacetKind;
    let (_dir, store) = db();
    let mut row = station("Many tags");
    row.tags = (0..150)
        .map(|i| format!("tag{i:03}"))
        .collect::<Vec<_>>()
        .join(",");
    store.upsert(&row).unwrap();
    assert_eq!(
        store.facets_limited(FacetKind::Tag, "", 100).unwrap().len(),
        100
    );
    assert_eq!(store.facets(FacetKind::Tag, "").unwrap().len(), 150);
    assert_eq!(
        store
            .facets_limited(FacetKind::Tag, " TAG149 ", 100)
            .unwrap()[0]
            .value,
        "tag149"
    );
    assert!(
        store
            .facets_limited(FacetKind::Tag, "", 0)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn url_index_backfills_legacy_rows_without_rejecting_shared_directory_urls() {
    use radio_cli::{
        error::{DomainError, ErrorCode},
        service,
    };
    let (dir, mut store) = db();
    let mut first = station("first");
    first.url = "https://EXAMPLE.com:443/live#one".into();
    let mut second = first.clone();
    second.stationuuid = uuid::Uuid::new_v4().to_string();
    store.upsert(&first).unwrap();
    store.upsert(&second).unwrap();
    drop(store);
    let path = dir.path().join("radio.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("DROP TRIGGER station_search_insert; DROP TRIGGER station_search_update; DROP TRIGGER station_search_delete;
        DROP TABLE station_search_stats; DROP TABLE station_search_terms; DROP TABLE station_search; DROP INDEX station_sort; DROP INDEX station_name; DROP INDEX station_bitrate;
        DROP INDEX station_active; DROP INDEX station_directory;
        DROP TABLE facets_cache; DROP INDEX station_url; ALTER TABLE stations DROP COLUMN normalized_url; PRAGMA user_version=2;").unwrap();
    drop(conn);
    store = Store::open(&path).unwrap();
    assert_eq!(store.directory_count().unwrap(), 2);
    let error = service::add_station(
        &mut store,
        "duplicate".into(),
        "https://example.com/live".into(),
        &[],
    )
    .unwrap_err();
    assert_eq!(
        error.downcast_ref::<DomainError>().unwrap().code,
        ErrorCode::DuplicateStation
    );
    let mut changed = second.clone();
    changed.url = "https://example.com/changed".into();
    store.begin_sync().unwrap();
    store.stage(&[changed]).unwrap();
    store.finish_sync().unwrap();
    service::add_station(
        &mut store,
        "old inactive URL".into(),
        "https://example.com/live".into(),
        &[],
    )
    .unwrap();
}

#[test]
fn facet_cache_tracks_atomic_mutations_sync_and_external_writers() {
    use radio_cli::model::FacetKind;
    let (dir, mut store) = db();
    let mut row = station("facets");
    row.language = "中文,中文".into();
    row.languagecodes = "中文".into();
    row.tags = "Jazz,jazz,100%_".into();
    store.bootstrap(&[row.clone()], || false).unwrap();
    assert_eq!(store.facets(FacetKind::Language, "中").unwrap()[0].count, 1);
    assert_eq!(store.facets(FacetKind::Tag, "100%_").unwrap()[0].count, 1);
    let other = Store::open(&dir.path().join("radio.db")).unwrap();
    row.active = false;
    other.upsert(&row).unwrap();
    assert!(store.facets(FacetKind::Tag, "").unwrap().is_empty());
    row.active = true;
    row.custom = true;
    store.save_custom(&row, None).unwrap();
    let mut invalid = row.clone();
    invalid.tags = "changed".into();
    assert!(store.save_custom(&invalid, Some(&["bad".into()])).is_err());
    assert_eq!(other.facets(FacetKind::Tag, "jazz").unwrap()[0].count, 1);
    assert!(other.facets(FacetKind::Tag, "changed").unwrap().is_empty());
    store.begin_sync().unwrap();
    store.stage(&[station("directory")]).unwrap();
    store.finish_sync().unwrap();
    assert_eq!(other.facets(FacetKind::Country, "cn").unwrap()[0].count, 2);
    store.favorite(&row.stationuuid, true).unwrap();
    store.remove_custom(&row.stationuuid, false).unwrap();
    assert!(other.facets(FacetKind::Tag, "100%_").unwrap().is_empty());
    let mut custom = station("delete without references");
    custom.custom = true;
    custom.url = "https://example.com/unique".into();
    custom.tags = "unique".into();
    store.save_custom(&custom, None).unwrap();
    store.remove_custom(&custom.stationuuid, false).unwrap();
    assert!(other.facets(FacetKind::Tag, "unique").unwrap().is_empty());
}

#[test]
fn trigram_candidates_preserve_literal_substrings_and_sorting() {
    use radio_cli::model::Sort;
    let (dir, store) = db();
    for (i, name) in [
        "上海 Jazz 100%_FM",
        "literal AND NOT OR NEAR",
        "a\"b quote",
        "nul\0suffix",
        "ÉCOLE İSTANBUL Σίσυφος Straße",
        "ab cd ef",
        "other jazz",
        "jazz",
    ]
    .iter()
    .enumerate()
    {
        let mut s = station(name);
        s.clickcount = (i % 3) as u64;
        s.bitrate = (i as u32 % 2) * 128;
        store.upsert(&s).unwrap();
        if i % 2 == 0 {
            store.favorite(&s.stationuuid, true).unwrap();
        }
    }
    let conn = rusqlite::Connection::open(dir.path().join("radio.db")).unwrap();
    for query in [
        "上",
        "上海",
        "jAZZ",
        "jaz",
        "100%_",
        "%_",
        "AND",
        "NOT OR",
        "a\"b",
        "nul\0suffix",
        "suffix",
        "école",
        "İST",
        "Σίσ",
        "straße",
        "ab cd",
        "不存在",
        " ",
    ] {
        for (sort, order) in [
            (Sort::Clickcount, "popularity DESC,name,uuid"),
            (Sort::Name, "name COLLATE NOCASE,uuid"),
            (Sort::Bitrate, "bitrate DESC,name,uuid"),
            (Sort::Votes, "json_extract(json,'$.votes') DESC,name,uuid"),
        ] {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT uuid FROM stations WHERE instr(search,?1)>0 ORDER BY {order}"
                ))
                .unwrap();
            let expected: Vec<String> = stmt
                .query_map([query.trim().to_lowercase()], |r| r.get(0))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            let mut filter = Filter {
                query: query.into(),
                sort,
                limit: 100,
                ..Filter::default()
            };
            let actual: Vec<_> = store
                .search(&filter)
                .unwrap()
                .into_iter()
                .map(|s| s.stationuuid)
                .collect();
            assert_eq!(actual, expected, "query {query:?}, sort {sort:?}");
            assert_eq!(store.search_count(&filter).unwrap(), expected.len());
            if let Some(s) = store.random(&filter, &[]).unwrap() {
                assert!(expected.contains(&s.stationuuid));
            }
            filter.offset = 1;
            filter.limit = 2;
            let actual: Vec<_> = store
                .search(&filter)
                .unwrap()
                .into_iter()
                .map(|s| s.stationuuid)
                .collect();
            assert_eq!(
                actual,
                expected.into_iter().skip(1).take(2).collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn trigram_index_tracks_updates_deletes_and_rolled_back_publications() {
    let (dir, mut store) = db();
    let mut s = station("originalneedle");
    s.custom = true;
    store.save_custom(&s, None).unwrap();
    let find = |text: &str| Filter {
        query: text.into(),
        limit: 100,
        ..Filter::default()
    };
    let mut edited = s.clone();
    edited.name = "replacementneedle".into();
    assert!(store.save_custom(&edited, Some(&["bad".into()])).is_err());
    assert_eq!(store.search_count(&find("originalneedle")).unwrap(), 1);
    store.save_custom(&edited, None).unwrap();
    assert_eq!(store.search_count(&find("originalneedle")).unwrap(), 0);
    let other = Store::open(&dir.path().join("radio.db")).unwrap();
    assert_eq!(other.search_count(&find("replacementneedle")).unwrap(), 1);
    store.remove_custom(&s.stationuuid, false).unwrap();
    assert_eq!(other.search_count(&find("replacementneedle")).unwrap(), 0);
    store.begin_sync().unwrap();
    store.stage(&[station("syncneedle")]).unwrap();
    store.finish_sync().unwrap();
    assert_eq!(other.search_count(&find("syncneedle")).unwrap(), 1);
    let mut broken = station("rollbackneedle");
    broken.stationuuid = "invalid".into();
    assert!(
        store
            .bootstrap(&[station("firstneedle"), broken], || false)
            .is_err()
    );
    assert_eq!(other.search_count(&find("firstneedle")).unwrap(), 0);
}

#[test]
fn indexed_positions_match_pages_with_mixed_directions_and_invisible_targets() {
    use radio_cli::model::Sort;
    let (_dir, store) = db();
    let mut all = Vec::new();
    for i in 0..31 {
        let mut s = station(["A", "a", "B", "same jazz"][i % 4]);
        s.clickcount = (i % 7) as u64;
        s.bitrate = (i as u32 % 3) * 128;
        s.active = i % 5 != 0;
        store.upsert(&s).unwrap();
        if i % 2 == 0 {
            store.favorite(&s.stationuuid, true).unwrap();
        }
        if i % 3 == 0 {
            store.played(&s.stationuuid).unwrap();
        }
        all.push(s);
    }
    for sort in [Sort::Clickcount, Sort::Bitrate, Sort::Name, Sort::Votes] {
        for variant in 0..5 {
            let filter = Filter {
                sort,
                limit: 100,
                favorites: variant == 1,
                history: variant == 2,
                min_bitrate: if variant == 3 { 128 } else { 0 },
                query: if variant == 4 {
                    "jazz".into()
                } else {
                    String::new()
                },
                ..Filter::default()
            };
            let rows = store.search(&filter).unwrap();
            for s in &all {
                let expected = rows.iter().position(|r| r.stationuuid == s.stationuuid);
                assert_eq!(
                    store.station_position(&s.stationuuid, &filter).unwrap(),
                    expected,
                    "{sort:?} variant {variant}"
                );
            }
            assert!(
                store
                    .station_position("missing", &filter)
                    .unwrap()
                    .is_none()
            );
        }
    }
}

#[test]
fn migrations_from_intermediate_versions_preserve_derived_data() {
    for version in [3, 4] {
        let (dir, store) = db();
        let row = station("migrationneedle");
        store.upsert(&row).unwrap();
        drop(store);
        let path = dir.path().join("radio.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("DROP TRIGGER station_search_insert; DROP TRIGGER station_search_update; DROP TRIGGER station_search_delete;
            DROP TABLE station_search_stats; DROP TABLE station_search_terms; DROP TABLE station_search; DROP INDEX station_sort; DROP INDEX station_name;
            DROP INDEX station_bitrate; DROP INDEX station_active; DROP INDEX station_directory;").unwrap();
        if version == 3 {
            conn.execute_batch("DROP TABLE facets_cache;").unwrap();
        }
        conn.pragma_update(None, "user_version", version).unwrap();
        drop(conn);
        let store = Store::open(&path).unwrap();
        assert_eq!(
            store
                .search_count(&Filter {
                    query: "migrationneedle".into(),
                    ..Filter::default()
                })
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .facets(radio_cli::model::FacetKind::Country, "")
                .unwrap()[0]
                .count,
            1
        );
        assert_eq!(store.station(&row.stationuuid).unwrap().name, row.name);
    }
}

#[test]
fn migration_failure_rolls_back_and_future_versions_are_rejected() {
    let (dir, store) = db();
    store.upsert(&station("broken legacy")).unwrap();
    drop(store);
    let path = dir.path().join("radio.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("DROP TABLE facets_cache;
        DROP TRIGGER station_search_insert; DROP TRIGGER station_search_update; DROP TRIGGER station_search_delete;
        DROP TABLE station_search_stats; DROP TABLE station_search_terms; DROP TABLE station_search; DROP INDEX station_sort; DROP INDEX station_name;
        DROP INDEX station_bitrate; DROP INDEX station_active; DROP INDEX station_directory;
        DROP INDEX station_url; ALTER TABLE stations DROP COLUMN normalized_url;
        UPDATE stations SET json='invalid'; PRAGMA user_version=2;").unwrap();
    assert!(Store::open(&path).is_err());
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        2
    );
    assert!(conn.prepare("SELECT normalized_url FROM stations").is_err());
    conn.execute_batch("PRAGMA user_version=99").unwrap();
    let error = Store::open(&path).err().unwrap();
    assert_eq!(
        error
            .downcast_ref::<radio_cli::error::DomainError>()
            .unwrap()
            .code,
        radio_cli::error::ErrorCode::DatabaseTooNew
    );
}

#[test]
fn stale_trigram_statistics_do_not_exclude_new_or_edited_matches() {
    let (_dir, mut store) = db();
    let mut row = station("popular jazz");
    store.bootstrap(&[row.clone()], || false).unwrap();
    for name in ["brandnewneedle", "anothernewneedle", "popular jazz"] {
        row.name = name.into();
        store.upsert(&row).unwrap();
        let filter = Filter {
            query: name.into(),
            limit: 100,
            ..Filter::default()
        };
        assert_eq!(store.search_count(&filter).unwrap(), 1);
        assert_eq!(
            store.search(&filter).unwrap()[0].stationuuid,
            row.stationuuid
        );
    }
}
