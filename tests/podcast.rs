mod support;
use radio_cli::{
    engine::Engine,
    podcast::{self, Action, Episode, Feed, Preview},
    store::Store,
};
use std::{
    io::Write,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};
fn preview(url: &str) -> Preview {
    Preview {
        feed: Feed {
            url: url.into(),
            title: "节目".into(),
            ..Default::default()
        },
        episodes: vec![Episode {
            guid: Some("opaque-guid".into()),
            title: "单集".into(),
            url: format!("{url}/audio"),
            ..Default::default()
        }],
    }
}

#[test]
fn cli_feed_connection_error_exposes_the_cause_without_the_request_url() {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/opaque-path?token=secret",
        port.local_addr().unwrap()
    );
    drop(port);
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(&config, "language='en'\n").unwrap();
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_radio"))
        .arg("--config")
        .arg(config)
        .arg("--data-dir")
        .arg(dir.path())
        .args(["podcast", "preview", &url])
        .env("NO_PROXY", "127.0.0.1")
        .env("no_proxy", "127.0.0.1")
        .output()
        .unwrap();
    assert!(!result.status.success());
    let message = String::from_utf8_lossy(&result.stderr);
    assert!(message.contains("connection refused"), "{message}");
    assert!(!message.contains("opaque-path"), "{message}");
    assert!(!message.contains("secret"), "{message}");
}

#[test]
fn legacy_episode_json_survives_listing_playback_and_feed_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = support::config(dir.path());
    config.settings.audio_device = Some("null".into());
    config.settings.mpv = std::env::var("RADIO_TEST_MPV").unwrap_or_else(|_| "mpv".into());
    let database = config.data_dir.join("radio.db");
    let store = Store::open(&database).unwrap();
    let audio = support::fixture("libmp3lame", "mp3");
    let server =
        support::Server::new(move |_, stream| support::response(stream, 200, "audio/mpeg", &audio));
    let mut p = preview(&server.url);
    let feed = store.podcast_subscribe(&p, &p.feed.url).unwrap();
    let episode = store.podcast_episodes(&feed.id).unwrap().remove(0);
    store
        .podcast_progress(&episode, 75, Some(200), false)
        .unwrap();
    let mut legacy = serde_json::to_value(&episode).unwrap();
    legacy.as_object_mut().unwrap().remove("transcripts");
    rusqlite::Connection::open(&database)
        .unwrap()
        .execute(
            "UPDATE podcast_episodes SET json=?1 WHERE id=?2",
            rusqlite::params![legacy.to_string(), episode.id],
        )
        .unwrap();
    drop(store);
    let store = Store::open(&database).unwrap();
    let listed = store.podcast_episodes(&feed.id).unwrap().remove(0);
    assert!(listed.transcripts.is_empty());
    assert_eq!(listed.id, episode.id);
    assert_eq!(listed.position_ms, 75);
    assert!(
        store
            .podcast_episode(&episode.id)
            .unwrap()
            .transcripts
            .is_empty()
    );
    if std::process::Command::new(&config.settings.mpv)
        .arg("--version")
        .output()
        .is_ok()
    {
        let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
        engine
            .podcast_action(Action::Play {
                id: episode.id.clone(),
                from_start: true,
                speed: None,
            })
            .unwrap();
        assert_eq!(engine.status().episode.as_ref().unwrap().id, episode.id);
        engine.shutdown().unwrap();
    }
    p.episodes[0].transcripts = vec![podcast::Transcript {
        url: format!("{}/captions.vtt", server.url),
        mime: "text/vtt".into(),
        language: Some("zh".into()),
    }];
    store.podcast_refresh_merge(&feed, Some(p)).unwrap();
    let refreshed = store.podcast_episode(&episode.id).unwrap();
    assert_eq!(refreshed.id, episode.id);
    assert_eq!(refreshed.transcripts.len(), 1);
    assert_eq!(refreshed.transcripts[0].mime, "text/vtt");
}

#[test]
fn episode_search_filters_before_sql_pagination_and_joins_progress() {
    let dir = tempfile::tempdir().unwrap();
    let c = support::config(dir.path());
    let store = Store::open(&c.data_dir.join("radio.db")).unwrap();
    let mut p = preview("https://example.test/rss");
    p.episodes = (0..101)
        .map(|i| Episode {
            guid: Some(i.to_string()),
            source_order: Some(100 - i),
            title: if i == 0 {
                "BJÖRK 第101集 100%_mix".into()
            } else {
                format!("Item {i}")
            },
            url: format!("https://example.test/{i}.mp3"),
            ..Default::default()
        })
        .collect();
    let feed = store.podcast_subscribe(&p, &p.feed.url).unwrap();
    let all = store.podcast_episodes(&feed.id).unwrap();
    let target = &all[100];
    store
        .podcast_progress(target, 12345, Some(60000), true)
        .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let page = radio_cli::cli::Page {
        offset: 0,
        limit: 100,
        all: false,
    };
    let result = podcast::manage(
        &c,
        &store,
        Action::Episodes {
            id: feed.id.clone(),
            query: "björk 第101集 100%_".into(),
            page: page.clone(),
        },
        &stop,
    )
    .unwrap();
    assert_eq!(result["total"], 1);
    assert_eq!(result["has_more"], false);
    assert_eq!(result["items"][0]["id"], target.id);
    assert_eq!(result["items"][0]["position_ms"], 12345);
    assert_eq!(result["items"][0]["observed_duration_ms"], 60000);
    assert_eq!(result["items"][0]["completed"], true);
    let result = podcast::manage(
        &c,
        &store,
        Action::Episodes {
            id: feed.id.clone(),
            query: "iTeM".into(),
            page: radio_cli::cli::Page {
                offset: 98,
                limit: 7,
                all: false,
            },
        },
        &stop,
    )
    .unwrap();
    assert_eq!(result["total"], 100);
    assert_eq!(result["items"].as_array().unwrap().len(), 2);
    assert_eq!(result["has_more"], false);
    let result = podcast::manage(
        &c,
        &store,
        Action::Episodes {
            id: feed.id,
            query: String::new(),
            page: radio_cli::cli::Page {
                offset: 100,
                all: true,
                ..page
            },
        },
        &stop,
    )
    .unwrap();
    assert_eq!(result["total"], 101);
    assert_eq!(result["items"][0]["id"], target.id);
    assert_eq!(result["limit"], serde_json::Value::Null);
}

#[test]
fn subscription_search_filters_before_pagination() {
    let dir = tempfile::tempdir().unwrap();
    let c = support::config(dir.path());
    let store = Store::open(&c.data_dir.join("radio.db")).unwrap();
    for title in ["Alpha", "Beta", "节目 Γάμμα"] {
        let mut p = preview(&format!("https://example.test/{title}"));
        p.feed.title = title.into();
        store.podcast_subscribe(&p, &p.feed.url).unwrap();
    }
    let result = podcast::manage(
        &c,
        &store,
        Action::List {
            query: "γάμμα".into(),
            page: radio_cli::cli::Page {
                offset: 0,
                limit: 1,
                all: false,
            },
        },
        &Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    assert_eq!(result["total"], 1);
    assert_eq!(result["items"][0]["title"], "节目 Γάμμα");
    assert_eq!(result["has_more"], false);
}

#[test]
fn subscriptions_merge_without_losing_identity_progress_or_missing_episodes() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("radio.db")).unwrap();
    let mut p = preview("https://example.test/rss");
    let f = store.podcast_subscribe(&p, &p.feed.url).unwrap();
    let e = store.podcast_episodes(&f.id).unwrap().remove(0);
    store
        .podcast_progress(&e, 12345, Some(60000), true)
        .unwrap();
    p.episodes[0].url = "https://cdn.test/replacement".into();
    store.podcast_refresh_merge(&f, Some(p.clone())).unwrap();
    assert_eq!(store.podcast_episodes(&f.id).unwrap().len(), 1);
    assert_eq!(store.podcast_episode(&e.id).unwrap().position_ms, 12345);
    p.episodes.clear();
    let current = store.podcast_feed(&f.id).unwrap();
    store.podcast_refresh_merge(&current, Some(p)).unwrap();
    assert_eq!(store.podcast_episodes(&f.id).unwrap().len(), 1);
    store.podcast_unsubscribe(&f.id).unwrap();
    store
        .podcast_refresh_merge(&f, Some(preview("https://example.test/rss")))
        .unwrap();
    assert!(store.podcast_feeds().unwrap().is_empty());
    assert!(store.podcast_episode(&e.id).unwrap().completed);
}
#[test]
fn opml_import_stops_on_cancellation_and_keeps_prior_successes() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for successful in [0, 4] {
        let stop = Arc::new(AtomicBool::new(false));
        let cancel = stop.clone();
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = requests.clone();
        let server = support::Server::new(move |_, stream| {
            if counter.fetch_add(1, Ordering::Relaxed) == successful {
                cancel.store(true, Ordering::Relaxed);
            }
            support::response(stream, 200, "application/rss+xml",
                b"<rss><channel><title>Feed</title><item><title>Episode</title><enclosure url='/audio.mp3' type='audio/mpeg'/></item></channel></rss>");
        });
        let dir = tempfile::tempdir().unwrap();
        let config = support::config(dir.path());
        let store = Store::open(&config.data_dir.join("radio.db")).unwrap();
        let file = dir.path().join("feeds.opml");
        let outlines: String = (0..8)
            .map(|i| format!("<outline xmlUrl='{}/{i}'/>", server.url))
            .collect();
        std::fs::write(&file, format!("<opml><body>{outlines}</body></opml>")).unwrap();
        for _ in 0..2 {
            let error = podcast::manage(
                &config,
                &store,
                Action::Import {
                    file: file.clone(),
                    dry_run: false,
                },
                &stop,
            )
            .unwrap_err();
            assert_eq!(
                error
                    .downcast_ref::<radio_cli::error::DomainError>()
                    .unwrap()
                    .code,
                radio_cli::error::ErrorCode::Cancelled
            );
            assert!((successful + 1..=successful + 4).contains(&requests.load(Ordering::Relaxed)));
            assert_eq!(store.podcast_feeds().unwrap().len(), successful);
        }
    }
}

#[test]
fn opml_roundtrip_queue_and_marks_are_independent_of_stations() {
    let dir = tempfile::tempdir().unwrap();
    let c = support::config(dir.path());
    let s = Store::open(&c.data_dir.join("radio.db")).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let p = preview("https://example.test/rss?a=1&b=2");
    let f = s.podcast_subscribe(&p, &p.feed.url).unwrap();
    let e = s.podcast_episodes(&f.id).unwrap().remove(0);
    s.podcast_queue_set(&[e.id.clone(), e.id.clone()]).unwrap();
    assert_eq!(s.podcast_queue().unwrap().len(), 1);
    podcast::manage(
        &c,
        &s,
        Action::Mark {
            id: e.id.clone(),
            state: "played".into(),
        },
        &stop,
    )
    .unwrap();
    assert!(s.podcast_episode(&e.id).unwrap().completed);
    podcast::manage(
        &c,
        &s,
        Action::Mark {
            id: e.id.clone(),
            state: "unplayed".into(),
        },
        &stop,
    )
    .unwrap();
    assert!(!s.podcast_episode(&e.id).unwrap().completed);
    let file = dir.path().join("feeds.opml");
    podcast::manage(
        &c,
        &s,
        Action::Export {
            file: file.clone(),
            overwrite: false,
        },
        &stop,
    )
    .unwrap();
    let v = podcast::manage(
        &c,
        &s,
        Action::Import {
            file: file.clone(),
            dry_run: true,
        },
        &stop,
    )
    .unwrap();
    assert_eq!(v["urls"][0], p.feed.url);
    assert!(
        podcast::manage(
            &c,
            &s,
            Action::Export {
                file,
                overwrite: false
            },
            &stop
        )
        .is_err()
    );
    assert_eq!(s.count().unwrap(), 0);
}
#[test]
fn cancelled_refresh_propagates_without_changing_feed_failures_or_timestamps() {
    let stop = Arc::new(AtomicBool::new(false));
    let cancel = stop.clone();
    let server = support::Server::new(move |_, stream| {
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        support::response(
            stream,
            200,
            "application/rss+xml",
            b"<rss><channel><title>Updated</title></channel></rss>",
        );
    });
    let dir = tempfile::tempdir().unwrap();
    let c = support::config(dir.path());
    let s = Store::open(&c.data_dir.join("radio.db")).unwrap();
    let mut p = preview(&server.url);
    p.feed.failures = 2;
    p.feed.error = Some("previous failure".into());
    p.feed.refreshed_at = Some("2026-01-01T00:00:00Z".into());
    let f = s.podcast_subscribe(&p, &p.feed.url).unwrap();
    let error = podcast::refresh(&c, &s, None, &stop).unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<radio_cli::error::DomainError>()
            .unwrap()
            .code,
        radio_cli::error::ErrorCode::Cancelled
    );
    assert_eq!(s.podcast_feed(&f.id).unwrap(), f);

    // Cancellation also applies when no feeds would otherwise be fetched.
    s.podcast_unsubscribe(&f.id).unwrap();
    let error = podcast::refresh(&c, &s, None, &stop).unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<radio_cli::error::DomainError>()
            .unwrap()
            .code,
        radio_cli::error::ErrorCode::Cancelled
    );
}

#[test]
fn conditional_fetch_discovery_and_failed_refresh_keep_cached_entries() {
    let server = support::Server::new(|request, stream| {
        if request.starts_with("GET /page ") {
            support::response(stream,200,"text/html",b"<html><head><link rel='alternate' type='application/rss+xml' href='/rss'/></head></html>");
        } else if request.starts_with("GET /rss ")
            && request
                .to_ascii_lowercase()
                .contains("if-none-match: \"feed\"")
        {
            let _ = stream.write_all(b"HTTP/1.1 304 Not Modified\r\nConnection: close\r\n\r\n");
        } else if request.starts_with("GET /rss ") {
            let xml=b"<rss><channel><title>Test</title><item><title>One</title><enclosure url='/audio.mp3' type='audio/mpeg'/></item></channel></rss>";
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nETag: \"feed\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                xml.len()
            );
            let _ = stream.write_all(xml);
        } else {
            support::response(stream, 404, "text/plain", b"missing");
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let c = support::config(dir.path());
    let s = Store::open(&c.data_dir.join("radio.db")).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let url = format!("{}/rss", server.url);
    assert_eq!(
        podcast::discover(&c, &format!("{}/page", server.url), &stop).unwrap(),
        vec![url.clone()]
    );
    let p = podcast::fetch(&c, &url, None, &stop).unwrap().unwrap();
    let f = s.podcast_subscribe(&p, &url).unwrap();
    assert!(podcast::fetch(&c, &url, Some(&f), &stop).unwrap().is_none());
    let mut invalid = f.clone();
    invalid.url = format!("{}/missing", server.url);
    s.podcast_refresh_merge(
        &f,
        Some(Preview {
            feed: invalid,
            episodes: vec![],
        }),
    )
    .unwrap();
    let result = podcast::refresh(&c, &s, Some(&f.id), &stop).unwrap();
    assert_eq!(result[0]["ok"], false);
    assert_eq!(s.podcast_episodes(&f.id).unwrap().len(), 1);
}
#[test]
fn finite_mp3_ends_once_without_radio_history_or_retry() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = support::config(dir.path());
    c.settings.audio_device = Some("null".into());
    c.settings.mpv = std::env::var("RADIO_TEST_MPV").unwrap_or_else(|_| "mpv".into());
    if std::process::Command::new(&c.settings.mpv)
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let audio = support::fixture("libmp3lame", "mp3");
    let server =
        support::Server::new(move |_, stream| support::response(stream, 200, "audio/mpeg", &audio));
    let mut engine = Engine::new(c, Arc::new(AtomicBool::new(false))).unwrap();
    let p = preview(&server.url);
    let f = engine.store().podcast_subscribe(&p, &p.feed.url).unwrap();
    let e = engine.store().podcast_episodes(&f.id).unwrap().remove(0);
    engine.play_episode(&e.id, false, None).unwrap();
    let deadline = Instant::now() + Duration::from_secs(12);
    while engine.playback_active() {
        engine.tick().unwrap();
        assert!(Instant::now() < deadline, "{:?}", engine.status());
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!engine.playback_failed(), "{:?}", engine.status());
    assert!(engine.store().podcast_episode(&e.id).unwrap().completed);
    let conn = rusqlite::Connection::open(engine.config().data_dir.join("radio.db")).unwrap();
    assert_eq!(
        conn.query_row("SELECT count(*) FROM history", [], |r| r.get::<_, usize>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row("SELECT count(*) FROM podcast_history", [], |r| r
            .get::<_, usize>(0))
            .unwrap(),
        1
    );
    engine.shutdown().unwrap();
    assert!(engine.store().podcast_episode(&e.id).unwrap().completed);
}
#[test]
fn stale_downloads_are_skipped_for_playback_and_replaced_on_download() {
    let old_audio = support::fixture("libmp3lame", "mp3");
    let output = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=880:duration=8",
            "-c:a",
            "libmp3lame",
            "-f",
            "mp3",
            "pipe:1",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let new_audio = output.stdout;
    for changed in ["url", "bytes", "duration", "legacy", "missing"] {
        let serve_new = Arc::new(AtomicBool::new(false));
        let flag = serve_new.clone();
        let old = old_audio.clone();
        let new = new_audio.clone();
        let server = support::Server::new(move |request, stream| {
            assert!(!request.to_ascii_lowercase().contains("range:"));
            support::response(
                stream,
                200,
                "audio/mpeg",
                if flag.load(std::sync::atomic::Ordering::Relaxed) {
                    &new
                } else {
                    &old
                },
            );
        });
        let dir = tempfile::tempdir().unwrap();
        let mut config = support::config(dir.path());
        config.settings.mpv = support::fake_mpv(dir.path()).to_string_lossy().into_owned();
        let mut engine = Engine::new(config.clone(), Arc::new(AtomicBool::new(false))).unwrap();
        let mut p = preview(&server.url);
        p.episodes[0].bytes = Some(old_audio.len() as u64);
        p.episodes[0].duration_ms = Some(4000);
        let feed = engine.store().podcast_subscribe(&p, &p.feed.url).unwrap();
        let episode = engine.store().podcast_episodes(&feed.id).unwrap().remove(0);
        let stop = Arc::new(AtomicBool::new(false));
        let old_download = podcast::download(&config, &episode.id, &stop).unwrap();
        assert_eq!(std::fs::read(&old_download.path).unwrap(), old_audio);
        let downloaded = podcast::LibraryQuery {
            downloaded: true,
            ..Default::default()
        };
        let rows = engine.store().podcast_library(&downloaded).unwrap();
        assert_eq!(rows["total"], 1);
        assert_eq!(rows["items"][0]["download_state"], "complete");
        serve_new.store(true, std::sync::atomic::Ordering::Relaxed);
        match changed {
            "url" => p.episodes[0].url = format!("{}/new.mp3", server.url),
            "bytes" => p.episodes[0].bytes = Some(new_audio.len() as u64),
            "duration" => p.episodes[0].duration_ms = Some(8000),
            "legacy" => {
                let mut value = serde_json::to_value(&old_download).unwrap();
                value.as_object_mut().unwrap().remove("media");
                engine
                    .store()
                    .podcast_save_download(&serde_json::from_value(value).unwrap())
                    .unwrap();
            }
            "missing" => std::fs::remove_file(&old_download.path).unwrap(),
            _ => unreachable!(),
        }
        engine
            .store()
            .podcast_refresh_merge(&feed, Some(p))
            .unwrap();
        let current = engine.store().podcast_episode(&episode.id).unwrap();
        let rows = engine.store().podcast_library(&downloaded).unwrap();
        assert_eq!(rows["total"], 0, "{changed}");
        assert_eq!(rows["items"], serde_json::json!([]), "{changed}");
        let rows = engine.store().podcast_library(&Default::default()).unwrap();
        assert_eq!(rows["items"][0]["download_state"], "stale", "{changed}");
        let downloads = podcast::manage(
            &config,
            engine.store(),
            Action::Downloads {
                action: podcast::DownloadAction::List,
            },
            &stop,
        )
        .unwrap();
        assert_eq!(downloads[0]["state"], "complete", "{changed}");
        assert_eq!(downloads[0]["download_state"], "stale", "{changed}");
        assert!(podcast::text_output(&downloads).contains("\tstale\t"));
        engine.play_episode(&episode.id, false, None).unwrap();
        assert_eq!(engine.status().source_url, current.url, "{changed}");
        engine.stop_playback().unwrap();
        let new_download = podcast::download(&config, &episode.id, &stop).unwrap();
        assert_eq!(new_download.state, "complete");
        let rows = engine.store().podcast_library(&downloaded).unwrap();
        assert_eq!(rows["total"], 1, "{changed}");
        assert_eq!(rows["items"][0]["download_state"], "complete", "{changed}");
        assert_eq!(
            std::fs::read(&new_download.path).unwrap(),
            new_audio,
            "{changed}"
        );
        engine.play_episode(&episode.id, false, None).unwrap();
        assert_eq!(engine.status().source_url, new_download.path, "{changed}");
        engine.shutdown().unwrap();
    }
}

#[test]
fn download_restarts_stale_partial_media_even_when_the_url_is_unchanged() {
    let audio = support::fixture("libmp3lame", "mp3");
    let expected = audio.clone();
    let server = support::Server::new(move |request, stream| {
        assert!(!request.to_ascii_lowercase().contains("range:"));
        support::response(stream, 200, "audio/mpeg", &audio);
    });
    let dir = tempfile::tempdir().unwrap();
    let config = support::config(dir.path());
    let store = Store::open(&config.data_dir.join("radio.db")).unwrap();
    let mut p = preview(&server.url);
    p.episodes[0].duration_ms = Some(4000);
    let feed = store.podcast_subscribe(&p, &p.feed.url).unwrap();
    let episode = store.podcast_episodes(&feed.id).unwrap().remove(0);
    let path = podcast::download_path(&config, &episode.id);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path.with_extension("part"), &expected[..expected.len() / 2]).unwrap();
    store
        .podcast_save_download(&podcast::Download {
            episode_id: episode.id.clone(),
            state: "paused".into(),
            url: episode.url.clone(),
            media: Some(episode.media()),
            bytes: expected.len() as u64 / 2,
            etag: Some("\"same-url\"".into()),
            path: path.to_string_lossy().into_owned(),
            ..Default::default()
        })
        .unwrap();
    p.episodes[0].duration_ms = Some(8000);
    store.podcast_refresh_merge(&feed, Some(p)).unwrap();
    let download =
        podcast::download(&config, &episode.id, &Arc::new(AtomicBool::new(false))).unwrap();
    assert_eq!(
        download.media,
        Some(store.podcast_episode(&episode.id).unwrap().media())
    );
    assert_eq!(std::fs::read(path).unwrap(), expected);
}

#[test]
fn download_keeps_its_original_media_snapshot_when_refreshed_in_flight() {
    let audio = support::fixture("libmp3lame", "mp3");
    let (ready, wait) = std::sync::mpsc::channel();
    let (release, go) = std::sync::mpsc::channel();
    let go = std::sync::Mutex::new(go);
    let server = support::Server::new(move |_, stream| {
        ready.send(()).unwrap();
        go.lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(3))
            .unwrap();
        support::response(stream, 200, "audio/mpeg", &audio);
    });
    let dir = tempfile::tempdir().unwrap();
    let mut config = support::config(dir.path());
    config.settings.mpv = support::fake_mpv(dir.path()).to_string_lossy().into_owned();
    let mut engine = Engine::new(config.clone(), Arc::new(AtomicBool::new(false))).unwrap();
    let mut p = preview(&server.url);
    p.episodes[0].duration_ms = Some(4000);
    let feed = engine.store().podcast_subscribe(&p, &p.feed.url).unwrap();
    let old = engine.store().podcast_episodes(&feed.id).unwrap().remove(0);
    let id = old.id.clone();
    let task = std::thread::spawn(move || {
        podcast::download(&config, &id, &Arc::new(AtomicBool::new(false))).unwrap()
    });
    wait.recv_timeout(Duration::from_secs(3)).unwrap();
    p.episodes[0].duration_ms = Some(8000);
    engine
        .store()
        .podcast_refresh_merge(&feed, Some(p))
        .unwrap();
    release.send(()).unwrap();
    let download = task.join().unwrap();
    assert_eq!(download.media, Some(old.media()));
    assert_eq!(download.state, "complete");
    engine.play_episode(&old.id, false, None).unwrap();
    assert_eq!(engine.status().source_url, old.url);
    assert!(!engine.store().podcast_episode(&old.id).unwrap().completed);
    engine.shutdown().unwrap();
}

#[test]
fn download_publishes_verified_media_and_reuses_completed_file() {
    let audio = support::fixture("libmp3lame", "mp3");
    let bytes = audio.len();
    let server =
        support::Server::new(move |_, stream| support::response(stream, 200, "audio/mpeg", &audio));
    let dir = tempfile::tempdir().unwrap();
    let c = support::config(dir.path());
    let s = Store::open(&c.data_dir.join("radio.db")).unwrap();
    let p = preview(&server.url);
    let f = s.podcast_subscribe(&p, &p.feed.url).unwrap();
    let e = s.podcast_episodes(&f.id).unwrap().remove(0);
    let stop = Arc::new(AtomicBool::new(false));
    let d = podcast::download(&c, &e.id, &stop).unwrap();
    assert_eq!(d.state, "complete");
    assert_eq!(d.bytes, bytes as u64);
    assert!(std::path::Path::new(&d.path).is_file());
    assert_eq!(podcast::download(&c, &e.id, &stop).unwrap(), d);
}

#[test]
fn m4a_resume_seek_pause_and_queue_completion_use_episode_progress() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fixture.m4a");
    let result = std::process::Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=4",
            "-c:a",
            "aac",
            "-movflags",
            "+faststart",
        ])
        .arg(&path)
        .status()
        .unwrap();
    assert!(result.success());
    let bytes = std::fs::read(path).unwrap();
    let server = support::Server::new(move |request, stream| {
        let offset = request
            .lines()
            .find_map(|l| {
                l.to_ascii_lowercase()
                    .strip_prefix("range: bytes=")
                    .map(str::to_owned)
            })
            .and_then(|v| v.split('-').next()?.parse::<usize>().ok())
            .unwrap_or(0)
            .min(bytes.len());
        if offset > 0 {
            let _ = write!(
                stream,
                "HTTP/1.1 206 Partial Content\r\nContent-Type: audio/mp4\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                offset,
                bytes.len() - 1,
                bytes.len(),
                bytes.len() - offset
            );
            let _ = stream.write_all(&bytes[offset..]);
        } else {
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: audio/mp4\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            );
            let _ = stream.write_all(&bytes);
        }
    });
    let mut c = support::config(dir.path());
    c.settings.mpv = std::env::var("RADIO_TEST_MPV").unwrap_or_else(|_| "mpv".into());
    c.settings.audio_device = Some("null".into());
    if std::process::Command::new(&c.settings.mpv)
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let mut engine = Engine::new(c, Arc::new(AtomicBool::new(false))).unwrap();
    let p = preview(&server.url);
    let f = engine.store().podcast_subscribe(&p, &p.feed.url).unwrap();
    let e = engine.store().podcast_episodes(&f.id).unwrap().remove(0);
    engine
        .store()
        .podcast_progress(&e, 1200, None, false)
        .unwrap();
    engine
        .store()
        .podcast_queue_set(std::slice::from_ref(&e.id))
        .unwrap();
    engine.play_podcast_queue(false).unwrap();
    let deadline = Instant::now() + Duration::from_secs(12);
    while engine.status().episode.as_ref().unwrap().position_ms < 1600 {
        engine.tick().unwrap();
        assert!(Instant::now() < deadline, "{:?}", engine.status());
        std::thread::sleep(Duration::from_millis(15));
    }
    engine.pause().unwrap();
    engine.seek_episode(2200).unwrap();
    engine.podcast_speed(1.5).unwrap();
    for _ in 0..10 {
        engine.tick().unwrap();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(engine.status().seekable);
    assert!(
        engine.status().paused(),
        "seeking while paused must retain pause state"
    );
    engine.pause().unwrap();
    while engine.playback_active() {
        engine.tick().unwrap();
        assert!(Instant::now() < deadline, "{:?}", engine.status());
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(engine.store().podcast_episode(&e.id).unwrap().completed);
    assert!(engine.store().podcast_queue().unwrap().is_empty());
    engine.shutdown().unwrap();
}

fn seed_restarting_download(
    config: &radio_cli::config::Config,
    store: &Store,
    url: &str,
    old: &[u8],
) -> (Episode, std::path::PathBuf, podcast::Download) {
    let p = preview(url);
    let feed = store.podcast_subscribe(&p, &p.feed.url).unwrap();
    let episode = store.podcast_episodes(&feed.id).unwrap().remove(0);
    let path = podcast::download_path(config, &episode.id);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path.with_extension("part"), old).unwrap();
    let download = podcast::Download {
        episode_id: episode.id.clone(),
        state: "paused".into(),
        etag: Some("\"old\"".into()),
        modified: Some("Thu, 01 Jan 2026 00:00:00 GMT".into()),
        url: episode.url.clone(),
        media: Some(episode.media()),
        bytes: old.len() as u64,
        path: path.to_string_lossy().into_owned(),
        ..Default::default()
    };
    store.podcast_save_download(&download).unwrap();
    (episode, path, download)
}

#[test]
fn rejected_download_resume_keeps_old_validators_when_truncation_fails() {
    let bytes = support::fixture("libmp3lame", "mp3");
    let expected = bytes.clone();
    let mut old = bytes[..bytes.len() / 2].to_vec();
    let changed = old.len() / 2;
    old[changed] ^= 1;
    let dir = tempfile::tempdir().unwrap();
    let config = support::config(dir.path());
    let first = Arc::new(AtomicBool::new(true));
    let part_slot = Arc::new(std::sync::Mutex::new(std::path::PathBuf::new()));
    let slot = part_slot.clone();
    let server = support::Server::new(move |request, stream| {
        let request = request.to_ascii_lowercase();
        let offset = request
            .lines()
            .find_map(|line| {
                line.strip_prefix("range: bytes=")?
                    .strip_suffix('-')?
                    .parse::<usize>()
                    .ok()
            })
            .unwrap_or(0);
        if first.swap(false, std::sync::atomic::Ordering::Relaxed) {
            assert!(offset > 0);
            assert!(request.contains("if-range: \"old\""));
            // Block opening the part on all users, including root, without
            // changing the saved bytes. Restore the old file before retrying.
            let part = slot.lock().unwrap();
            std::fs::rename(&*part, part.with_extension("saved")).unwrap();
            std::fs::create_dir(&*part).unwrap();
        }
        let append = offset > 0 && request.contains("if-range: \"new\"");
        if append {
            write!(
                stream,
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {}-{}/{}\r\n",
                offset,
                bytes.len() - 1,
                bytes.len()
            )
            .unwrap();
        } else {
            write!(stream, "HTTP/1.1 200 OK\r\n").unwrap();
        }
        let body = if append { &bytes[offset..] } else { &bytes[..] };
        write!(stream, "ETag: \"new\"\r\nLast-Modified: Mon, 02 Feb 2026 00:00:00 GMT\r\nContent-Type: audio/mpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
        let _ = stream.write_all(body);
    });
    let store = Store::open(&config.data_dir.join("radio.db")).unwrap();
    let (episode, path, download) = seed_restarting_download(&config, &store, &server.url, &old);
    let part = path.with_extension("part");
    *part_slot.lock().unwrap() = part.clone();
    let stop = Arc::new(AtomicBool::new(false));
    assert!(podcast::download(&config, &episode.id, &stop).is_err());
    std::fs::remove_dir(&part).unwrap();
    std::fs::rename(part.with_extension("saved"), &part).unwrap();
    assert_eq!(std::fs::read(&part).unwrap(), old);
    let failed = store.podcast_download(&episode.id).unwrap().unwrap();
    assert_eq!(failed.state, "failed");
    assert!(failed.error.is_some());
    assert_eq!(failed.etag, download.etag);
    assert_eq!(failed.modified, download.modified);
    assert_eq!(failed.bytes, download.bytes);
    assert_eq!(
        podcast::download(&config, &episode.id, &stop)
            .unwrap()
            .state,
        "complete"
    );
    assert_eq!(std::fs::read(path).unwrap(), expected);
}

#[test]
fn download_restart_recovers_if_metadata_publication_rolls_back() {
    let bytes = support::fixture("libmp3lame", "mp3");
    let expected = bytes.clone();
    let old = vec![0; bytes.len() / 2];
    let server = support::Server::new(move |request, stream| {
        assert!(!request.to_ascii_lowercase().contains("if-range: \"new\""));
        write!(stream, "HTTP/1.1 200 OK\r\nETag: \"new\"\r\nContent-Type: audio/mpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).unwrap();
        let _ = stream.write_all(&bytes);
    });
    let dir = tempfile::tempdir().unwrap();
    let config = support::config(dir.path());
    let db = config.data_dir.join("radio.db");
    let store = Store::open(&db).unwrap();
    let (episode, path, download) = seed_restarting_download(&config, &store, &server.url, &old);
    let connection = rusqlite::Connection::open(db).unwrap();
    // Interrupt both metadata publication and failure reporting. This leaves
    // the same disk/database boundary as a crash before the transaction commits.
    connection
        .execute_batch(
            r#"CREATE TRIGGER reject_new_download_validators
        BEFORE UPDATE ON podcast_downloads WHEN json_extract(NEW.json,'$.etag')='"new"'
        BEGIN SELECT RAISE(ABORT,'metadata publication interrupted'); END;"#,
        )
        .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    assert!(podcast::download(&config, &episode.id, &stop).is_err());
    assert_eq!(
        std::fs::metadata(path.with_extension("part"))
            .unwrap()
            .len(),
        0
    );
    let interrupted = store.podcast_download(&episode.id).unwrap().unwrap();
    assert_eq!(interrupted.etag, download.etag);
    assert_eq!(interrupted.modified, download.modified);
    assert_eq!(interrupted.state, "downloading");
    connection
        .execute_batch("DROP TRIGGER reject_new_download_validators")
        .unwrap();
    podcast::download(&config, &episode.id, &stop).unwrap();
    assert_eq!(std::fs::read(path).unwrap(), expected);
}

#[test]
fn download_restart_recovers_after_sigkill_following_validator_publication() {
    use std::process::{Command, Stdio};
    struct DownloadProcess(std::process::Child);
    impl Drop for DownloadProcess {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let bytes = support::fixture("libmp3lame", "mp3");
    let expected = bytes.clone();
    let old = vec![0; bytes.len() / 2];
    let first = AtomicBool::new(true);
    let (release, go) = std::sync::mpsc::channel();
    let go = std::sync::Mutex::new(go);
    let server = support::Server::new(move |request, stream| {
        if first.swap(false, std::sync::atomic::Ordering::Relaxed) {
            assert!(request.to_ascii_lowercase().contains("if-range: \"old\""));
            write!(stream, "HTTP/1.1 200 OK\r\nETag: \"new\"\r\nContent-Type: audio/mpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).unwrap();
            stream.flush().unwrap();
            // Keep the body unavailable until the downloading process is dead.
            let _ = go.lock().unwrap().recv_timeout(Duration::from_secs(5));
        } else {
            assert!(!request.to_ascii_lowercase().contains("range:"));
            support::response(stream, 200, "audio/mpeg", &bytes);
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let config = support::config(dir.path());
    let store = Store::open(&config.data_dir.join("radio.db")).unwrap();
    let (episode, path, _) = seed_restarting_download(&config, &store, &server.url, &old);
    let mut child = DownloadProcess(
        Command::new(env!("CARGO_BIN_EXE_radio"))
            .arg("--data-dir")
            .arg(&config.data_dir)
            .arg("--config")
            .arg(config.data_dir.join("test-default.toml"))
            .args(["podcast", "download", &episode.id])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let download = store.podcast_download(&episode.id).unwrap().unwrap();
        if download.etag.as_deref() == Some("\"new\"") {
            assert_eq!(download.bytes, 0);
            assert_eq!(
                std::fs::metadata(path.with_extension("part"))
                    .unwrap()
                    .len(),
                0
            );
            break;
        }
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "download exited before initialization"
        );
        assert!(
            Instant::now() < deadline,
            "new validators were never published"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    child.0.kill().unwrap();
    assert!(!child.0.wait().unwrap().success());
    release.send(()).unwrap();
    let finished =
        podcast::download(&config, &episode.id, &Arc::new(AtomicBool::new(false))).unwrap();
    assert_eq!(finished.state, "complete");
    assert_eq!(std::fs::read(path).unwrap(), expected);
}

#[test]
fn download_rejects_a_valid_partial_range_until_the_full_resource_is_present() {
    let bytes = support::fixture("libmp3lame", "mp3");
    let expected = bytes.clone();
    let initial = bytes[..bytes.len() / 4].to_vec();
    let first = Arc::new(AtomicBool::new(true));
    let server = support::Server::new(move |request, stream| {
        let start = request
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("range: bytes=")
                    .and_then(|v| v.strip_suffix('-'))?
                    .parse::<usize>()
                    .ok()
            })
            .expect("download must resume the retained part");
        let end = if first.swap(false, std::sync::atomic::Ordering::Relaxed) {
            bytes.len() / 2
        } else {
            bytes.len()
        };
        let _ = write!(
            stream,
            "HTTP/1.1 206 Partial Content\r\nETag: \"resource\"\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            start,
            end - 1,
            bytes.len(),
            end - start
        );
        let _ = stream.write_all(&bytes[start..end]);
    });
    let dir = tempfile::tempdir().unwrap();
    let c = support::config(dir.path());
    let s = Store::open(&c.data_dir.join("radio.db")).unwrap();
    let p = preview(&server.url);
    let f = s.podcast_subscribe(&p, &p.feed.url).unwrap();
    let e = s.podcast_episodes(&f.id).unwrap().remove(0);
    let path = podcast::download_path(&c, &e.id);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path.with_extension("part"), &initial).unwrap();
    s.podcast_save_download(&podcast::Download {
        episode_id: e.id.clone(),
        state: "paused".into(),
        etag: Some("\"resource\"".into()),
        url: e.url.clone(),
        media: Some(e.media()),
        bytes: initial.len() as u64,
        ..Default::default()
    })
    .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    assert!(podcast::download(&c, &e.id, &stop).is_err());
    assert!(!path.exists());
    assert_eq!(s.podcast_download(&e.id).unwrap().unwrap().state, "failed");
    assert_eq!(
        std::fs::read(path.with_extension("part")).unwrap(),
        expected[..expected.len() / 2]
    );
    assert_eq!(
        podcast::download(&c, &e.id, &stop).unwrap().state,
        "complete"
    );
    assert_eq!(std::fs::read(path).unwrap(), expected);
}

#[test]
fn download_range_resume_validates_the_resource() {
    let bytes = support::fixture("libmp3lame", "mp3");
    let partial = bytes[..bytes.len() / 2].to_vec();
    let expected = bytes.clone();
    let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let used = observed.clone();
    let server = support::Server::new(move |request, stream| {
        let start = request
            .lines()
            .find_map(|l| {
                l.to_ascii_lowercase()
                    .strip_prefix("range: bytes=")
                    .map(str::to_owned)
            })
            .and_then(|v| v.split('-').next()?.parse::<usize>().ok())
            .unwrap_or(0);
        if start > 0 {
            used.store(true, std::sync::atomic::Ordering::Relaxed);
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("if-range: \"resource\"")
            );
            let _ = write!(
                stream,
                "HTTP/1.1 206 Partial Content\r\nETag: \"resource\"\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                start,
                bytes.len() - 1,
                bytes.len(),
                bytes.len() - start
            );
            let _ = stream.write_all(&bytes[start..]);
        } else {
            support::response(stream, 200, "audio/mpeg", &bytes);
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let c = support::config(dir.path());
    let s = Store::open(&c.data_dir.join("radio.db")).unwrap();
    let p = preview(&server.url);
    let f = s.podcast_subscribe(&p, &p.feed.url).unwrap();
    let e = s.podcast_episodes(&f.id).unwrap().remove(0);
    let path = podcast::download_path(&c, &e.id);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path.with_extension("part"), &partial).unwrap();
    s.podcast_save_download(&podcast::Download {
        episode_id: e.id.clone(),
        state: "paused".into(),
        etag: Some("\"resource\"".into()),
        url: e.url.clone(),
        media: Some(e.media()),
        bytes: partial.len() as u64,
        ..Default::default()
    })
    .unwrap();
    podcast::download(&c, &e.id, &Arc::new(AtomicBool::new(false))).unwrap();
    assert!(observed.load(std::sync::atomic::Ordering::Relaxed));
    assert_eq!(std::fs::read(path).unwrap(), expected);
}

#[test]
fn unseekable_resume_preserves_position_and_waits_for_explicit_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = support::config(dir.path());
    c.settings.mpv = support::fake_mpv(dir.path()).to_string_lossy().into_owned();
    c.settings.playback_timeout_secs = 1;
    let mut engine = Engine::new(c, Arc::new(AtomicBool::new(false))).unwrap();
    let mut p = preview("https://example.test/rss");
    p.episodes[0].url = "https://example.test/noseek".into();
    let f = engine.store().podcast_subscribe(&p, &p.feed.url).unwrap();
    let e = engine.store().podcast_episodes(&f.id).unwrap().remove(0);
    engine
        .store()
        .podcast_progress(&e, 15000, None, false)
        .unwrap();
    engine.play_episode(&e.id, false, None).unwrap();
    let deadline = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < deadline {
        engine.tick().unwrap();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(engine.status().paused());
    assert!(!engine.playback_failed());
    assert!(engine.pause().is_err());
    engine.stop_playback().unwrap();
    assert_eq!(
        engine.status().playback_state,
        radio_cli::engine::PlaybackState::Idle
    );
    for _ in 0..3 {
        engine.tick().unwrap();
        assert_eq!(
            engine.status().playback_state,
            radio_cli::engine::PlaybackState::Idle
        );
        assert!(!engine.playback_active());
    }
    engine.shutdown().unwrap();
    assert_eq!(
        engine.store().podcast_episode(&e.id).unwrap().position_ms,
        15000
    );
}

#[test]
fn replay_after_media_refresh_uses_the_preserved_database_position() {
    for changed in ["duration", "bytes", "title"] {
        let dir = tempfile::tempdir().unwrap();
        let mut config = support::config(dir.path());
        config.settings.mpv = support::fake_mpv(dir.path()).to_string_lossy().into_owned();
        let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
        let mut p = preview("https://example.test/rss");
        p.episodes[0].url = "https://example.test/episode-eof".into();
        p.episodes[0].duration_ms = Some(20000);
        p.episodes[0].bytes = Some(100);
        let feed = engine.store().podcast_subscribe(&p, &p.feed.url).unwrap();
        let episode = engine.store().podcast_episodes(&feed.id).unwrap().remove(0);
        engine
            .store()
            .podcast_progress(&episode, 15000, Some(20000), false)
            .unwrap();
        engine.play_episode(&episode.id, false, None).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while engine.playback_active() {
            engine.tick().unwrap();
            assert!(Instant::now() < deadline, "{:?}", engine.status());
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            engine
                .store()
                .podcast_episode(&episode.id)
                .unwrap()
                .completed
        );
        match changed {
            "duration" => p.episodes[0].duration_ms = Some(30000),
            "bytes" => p.episodes[0].bytes = Some(200),
            "title" => p.episodes[0].title = "Updated title".into(),
            _ => unreachable!(),
        }
        engine
            .store()
            .podcast_refresh_merge(&feed, Some(p))
            .unwrap();
        let refreshed = engine.store().podcast_episode(&episode.id).unwrap();
        assert_eq!(refreshed.position_ms, 15000);
        assert_eq!(refreshed.completed, changed == "title");
        engine.play_episode(&episode.id, false, None).unwrap();
        let replay = engine.status().episode.as_ref().unwrap();
        assert_eq!(
            replay.position_ms,
            if changed == "title" { 0 } else { 15000 },
            "{changed}"
        );
        assert!(!replay.completed);
        engine.shutdown().unwrap();
    }
}

#[test]
fn queue_auto_next_consumes_only_explicit_entries() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = support::config(dir.path());
    c.settings.mpv = support::fake_mpv(dir.path()).to_string_lossy().into_owned();
    let mut engine = Engine::new(c, Arc::new(AtomicBool::new(false))).unwrap();
    let mut p = preview("https://example.test/rss");
    p.episodes[0].url = "https://example.test/episode-eof".into();
    let mut second = p.episodes[0].clone();
    second.guid = Some("second".into());
    p.episodes.push(second);
    let f = engine.store().podcast_subscribe(&p, &p.feed.url).unwrap();
    let episodes = engine.store().podcast_episodes(&f.id).unwrap();
    let ids: Vec<_> = episodes.iter().map(|e| e.id.clone()).collect();
    engine.store().podcast_queue_set(&ids).unwrap();
    engine.play_podcast_queue(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while engine.playback_active() {
        engine.tick().unwrap();
        assert!(Instant::now() < deadline, "{:?}", engine.status());
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(engine.store().podcast_queue().unwrap().is_empty());
    for id in ids {
        assert!(engine.store().podcast_episode(&id).unwrap().completed);
    }
    engine.shutdown().unwrap();
}

#[test]
fn missing_guids_deduplicate_by_url_and_stale_refresh_cannot_resubscribe() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("radio.db")).unwrap();
    let mut p = preview("https://example.test/rss");
    p.episodes[0].guid = None;
    p.episodes.push(p.episodes[0].clone());
    let f = s.podcast_subscribe(&p, &p.feed.url).unwrap();
    assert_eq!(s.podcast_episodes(&f.id).unwrap().len(), 1);
    let episode = s.podcast_episodes(&f.id).unwrap().remove(0);
    let id = episode.id.clone();
    s.podcast_progress(&episode, 321, Some(1000), true).unwrap();
    p.episodes[0].guid = Some("new-identity".into());
    s.podcast_refresh_merge(&f, Some(p.clone())).unwrap();
    assert_eq!(s.podcast_episodes(&f.id).unwrap().len(), 2);
    s.podcast_unsubscribe(&f.id).unwrap();
    let resubscribed = s.podcast_subscribe(&p, &p.feed.url).unwrap();
    let mut stale = p;
    stale.feed.title = "obsolete".into();
    s.podcast_refresh_merge(&f, Some(stale)).unwrap();
    assert_eq!(s.podcast_feed(&resubscribed.id).unwrap().title, "节目");
    assert_eq!(s.podcast_episode(&id).unwrap().position_ms, 321);
}

#[test]
fn migration_from_schema_nine_keeps_podcasts_and_enables_the_library() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("radio.db");
    let store = Store::open(&path).unwrap();
    let p = preview("https://example.test/legacy");
    let feed = store.podcast_subscribe(&p, &p.feed.url).unwrap();
    let episode = store.podcast_episodes(&feed.id).unwrap().remove(0);
    store
        .podcast_progress(&episode, 12345, Some(60000), false)
        .unwrap();
    store
        .podcast_queue_set(std::slice::from_ref(&episode.id))
        .unwrap();
    store
        .podcast_save_download(&podcast::Download {
            episode_id: episode.id.clone(),
            url: episode.url.clone(),
            state: "paused".into(),
            ..Default::default()
        })
        .unwrap();
    store.set_meta("pref_last_episode", &episode.id).unwrap();
    drop(store);
    let conn = rusqlite::Connection::open(&path).unwrap();
    support::downgrade_podcast_schema_to_v9(&conn);
    drop(conn);

    let store = Store::open(&path).unwrap();
    assert_eq!(store.podcast_feed(&feed.id).unwrap().title, feed.title);
    assert_eq!(
        store.podcast_by_url(&p.feed.url).unwrap().unwrap().id,
        feed.id
    );
    let resumed = store.podcast_episode(&episode.id).unwrap();
    assert_eq!(resumed.position_ms, 12345);
    assert_eq!(resumed.observed_duration_ms, Some(60000));
    assert!(!resumed.completed);
    assert_eq!(store.podcast_queue().unwrap()[0].id, episode.id);
    assert_eq!(
        store.podcast_download(&episode.id).unwrap().unwrap().state,
        "paused"
    );
    assert_eq!(
        store.meta("pref_last_episode").unwrap().as_deref(),
        Some(episode.id.as_str())
    );
    assert_eq!(
        store.podcast_library(&Default::default()).unwrap()["total"],
        1
    );
    assert!(store.podcast_bookmarks(&episode.id).unwrap().is_empty());
    let revision = store.podcast_revision().unwrap();
    store.podcast_save_episode(&episode.id, false).unwrap();
    assert!(store.podcast_revision().unwrap() > revision);
    for (view, total) in [("saved", 1), ("inbox", 0)] {
        assert_eq!(
            store
                .podcast_library(&podcast::LibraryQuery {
                    view: view.into(),
                    ..Default::default()
                })
                .unwrap()["total"],
            total
        );
    }
    let revision = store.podcast_revision().unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.podcast_revision().unwrap(), revision);
    let conn = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        11
    );
}

#[test]
fn failed_schema_ten_migration_rolls_back_before_retry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("radio.db");
    let store = Store::open(&path).unwrap();
    let p = preview("https://example.test/legacy");
    let feed = store.podcast_subscribe(&p, &p.feed.url).unwrap();
    drop(store);
    let conn = rusqlite::Connection::open(&path).unwrap();
    support::downgrade_podcast_schema_to_v9(&conn);
    conn.execute_batch("CREATE VIEW podcast_revision AS SELECT 0 AS n")
        .unwrap();
    assert!(Store::open(&path).is_err());
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        9
    );
    let new_tables: usize = conn.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name IN ('podcast_saved','podcast_inbox','podcast_bookmarks')",
        [], |r| r.get(0),
    ).unwrap();
    assert_eq!(new_tables, 0);
    conn.execute_batch("DROP VIEW podcast_revision").unwrap();
    let store = Store::open(&path).unwrap();
    assert_eq!(store.podcast_feed(&feed.id).unwrap().title, feed.title);
    assert!(store.podcast_library(&Default::default()).is_ok());
}

#[test]
fn migration_from_schema_eight_keeps_radio_data_and_creates_empty_podcasts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("radio.db");
    let s = Store::open(&path).unwrap();
    s.set_meta("existing", "retained").unwrap();
    drop(s);
    let conn = rusqlite::Connection::open(&path).unwrap();
    for table in [
        "podcast_queue",
        "podcast_downloads",
        "podcast_history",
        "podcast_progress",
        "podcast_episodes",
        "podcast_aliases",
        "podcast_feeds",
    ] {
        conn.execute_batch(&format!("DROP TABLE {table}")).unwrap();
    }
    conn.execute_batch("PRAGMA user_version=8").unwrap();
    drop(conn);
    let s = Store::open(&path).unwrap();
    assert_eq!(s.meta("existing").unwrap().as_deref(), Some("retained"));
    assert!(s.podcast_feeds().unwrap().is_empty());
}

#[test]
fn caption_cli_import_is_atomic_and_reports_json_contract() {
    let dir = tempfile::tempdir().unwrap();
    let config = support::config(dir.path());
    let store = Store::open(&config.data_dir.join("radio.db")).unwrap();
    let p = preview("https://example.test/rss");
    let feed = store.podcast_subscribe(&p, &p.feed.url).unwrap();
    let episode = store.podcast_episodes(&feed.id).unwrap().remove(0);
    let file = dir.path().join("caption.srt");
    std::fs::write(&file, "1\n00:00:00,000 --> 00:00:02,000\nCaption").unwrap();
    let command = || {
        let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_radio"));
        c.arg("--data-dir").arg(dir.path()).args([
            "--config",
            "/dev/null",
            "--json-v2",
            "podcast",
            "subtitles",
            &episode.id,
        ]);
        c
    };
    let output = command().arg("--file").arg(&file).output().unwrap();
    assert!(output.status.success(), "{:?}", output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["data"],
        serde_json::json!({"id":episode.id,"segments":1})
    );
    std::fs::write(&file, "invalid").unwrap();
    let output = command().arg("--file").arg(&file).output().unwrap();
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(!output.status.success());
    assert_eq!(value["error"]["code"], "podcast_failed");
    std::fs::remove_file(file).unwrap();
    let output = command().output().unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["data"]["segments"], 1,
        "bad import kept previous cache"
    );
    let output = command().arg("--transcribe").output().unwrap();
    assert!(!output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["code"], "subtitle_runtime");
    assert!(store.podcast_download(&episode.id).unwrap().is_none());
}

#[test]
fn redirect_conflicts_are_isolated_and_temporary_urls_are_not_saved() {
    let server = support::Server::new(|request, stream| {
        if request.starts_with("GET /old ") || request.starts_with("GET /temporary ") {
            let status = if request.starts_with("GET /old ") {
                301
            } else {
                302
            };
            write!(stream, "HTTP/1.1 {status} Redirect\r\nLocation: /target\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        } else {
            support::response(stream, 200, "application/rss+xml", b"<rss><channel><title>Updated</title><item><guid>a</guid><title>Episode</title><enclosure url='/audio' type='audio/mpeg'/></item></channel></rss>");
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let config = support::config(dir.path());
    let store = Store::open(&config.data_dir.join("radio.db")).unwrap();
    let old = format!("{}/old", server.url);
    let target = format!("{}/target", server.url);
    let temporary = format!("{}/temporary", server.url);
    let a = store.podcast_subscribe(&preview(&old), &old).unwrap();
    let b = store.podcast_subscribe(&preview(&target), &target).unwrap();
    let c = store
        .podcast_subscribe(&preview(&temporary), &temporary)
        .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let result = podcast::refresh(&config, &store, None, &stop).unwrap();
    assert_eq!(result.as_array().unwrap().len(), 3);
    let failed = store.podcast_feed(&a.id).unwrap();
    assert_eq!(failed.failures, 1);
    assert!(failed.error.is_some());
    assert_eq!(failed.url, old);
    assert_eq!(store.podcast_feed(&b.id).unwrap().title, "Updated");
    assert_eq!(store.podcast_feed(&c.id).unwrap().url, temporary);
}

#[test]
fn library_filters_before_paging_and_preserves_initial_inbox_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("radio.db")).unwrap();
    let mut p = preview("https://library.test/feed");
    let f = store.podcast_subscribe(&p, &p.feed.url).unwrap();
    let mut q = podcast::LibraryQuery {
        view: "inbox".into(),
        ..Default::default()
    };
    assert_eq!(store.podcast_library(&q).unwrap()["total"], 0);
    p.episodes.push(Episode {
        guid: Some("new".into()),
        title: "New episode".into(),
        url: "https://library.test/new.mp3".into(),
        ..Default::default()
    });
    store.podcast_refresh_merge(&f, Some(p)).unwrap();
    let new = store.podcast_library(&q).unwrap();
    assert_eq!(new["total"], 1);
    let id = new["items"][0]["id"].as_str().unwrap();
    store.podcast_save_episode(id, false).unwrap();
    q.view = "saved".into();
    q.query = "New".into();
    assert_eq!(store.podcast_library(&q).unwrap()["total"], 1);
    q.page.offset = 1;
    assert_eq!(
        store.podcast_library(&q).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    store.podcast_dismiss_inbox(id).unwrap();
    q.view = "inbox".into();
    q.page.offset = 0;
    assert_eq!(store.podcast_library(&q).unwrap()["total"], 0);
}

#[test]
fn settings_bookmarks_and_automatic_downloads_survive_reopening() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("radio.db");
    let store = Store::open(&path).unwrap();
    let mut p = preview("https://settings.test/feed");
    let f = store.podcast_subscribe(&p, &p.feed.url).unwrap();
    assert!(store.podcast_set_setting(&f.id, "speed", "9").is_err());
    assert!(store.podcast_set_setting(&f.id, "sort", "random").is_err());
    store
        .podcast_set_setting(&f.title, "speed", "1.25")
        .unwrap();
    store
        .podcast_set_setting(&f.id, "auto_download", "true")
        .unwrap();
    let e = store.podcast_episodes(&f.id).unwrap().remove(0);
    let bookmark = store.podcast_add_bookmark(&e.id, 12500, "A note").unwrap();
    store
        .podcast_edit_bookmark(&bookmark.id, Some("Edited"))
        .unwrap();
    assert!(store.podcast_downloads().unwrap().is_empty());
    p.episodes.push(Episode {
        guid: Some("new-id".into()),
        url: "https://settings.test/new.mp3".into(),
        ..Default::default()
    });
    store.podcast_refresh_merge(&f, Some(p)).unwrap();
    assert_eq!(store.podcast_downloads().unwrap().len(), 1);
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.podcast_settings(&f.id).unwrap().speed, Some(1.25));
    assert_eq!(store.podcast_settings(&f.title).unwrap().speed, Some(1.25));
    assert_eq!(store.podcast_bookmarks(&e.id).unwrap()[0].note, "Edited");
    store.podcast_edit_bookmark(&bookmark.id, None).unwrap();
    assert!(store.podcast_bookmarks(&e.id).unwrap().is_empty());
}

#[test]
fn transient_download_failures_have_a_persistent_retry_budget() {
    let server = support::Server::new(|_, stream| {
        support::response(stream, 503, "text/plain", b"Try later")
    });
    let dir = tempfile::tempdir().unwrap();
    let config = support::config(dir.path());
    let store = Store::open(&config.data_dir.join("radio.db")).unwrap();
    let p = preview(&server.url);
    let f = store.podcast_subscribe(&p, &p.feed.url).unwrap();
    let e = store.podcast_episodes(&f.id).unwrap().remove(0);
    let stop = Arc::new(AtomicBool::new(false));
    for attempt in 1..=4 {
        assert!(podcast::download(&config, &e.id, &stop).is_err());
        let d = store.podcast_download(&e.id).unwrap().unwrap();
        assert_eq!(d.retries, attempt.min(3));
        assert_eq!(d.state, if attempt <= 3 { "queued" } else { "failed" });
    }
}

#[test]
fn queue_start_and_relative_seeks_respect_pause_and_sleep_at_end() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = support::config(dir.path());
    config.settings.audio_device = Some("null".into());
    config.settings.mpv = std::env::var("RADIO_TEST_MPV").unwrap_or_else(|_| "mpv".into());
    let audio = support::fixture("pcm_s16le", "wav");
    let server =
        support::Server::new(move |_, stream| support::response(stream, 200, "audio/wav", &audio));
    let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
    let mut p = preview(&server.url);
    p.episodes = (0..3)
        .map(|i| Episode {
            guid: Some(i.to_string()),
            title: format!("Episode {i}"),
            url: format!("{}/audio", server.url),
            ..Default::default()
        })
        .collect();
    let f = engine.store().podcast_subscribe(&p, &p.feed.url).unwrap();
    let rows = engine.store().podcast_episodes(&f.id).unwrap();
    let ids: Vec<_> = rows.iter().map(|e| e.id.clone()).collect();
    engine.store().podcast_queue_set(&ids).unwrap();
    podcast::download(engine.config(), &ids[1], &Arc::new(AtomicBool::new(false))).unwrap();
    engine
        .podcast_action(Action::Queue {
            action: podcast::QueueAction::Start {
                id: ids[1].clone(),
                auto_next: true,
            },
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(12);
    while !engine.status().seekable
        || engine.status().episode.as_ref().unwrap().position_ms < 200
        || !matches!(
            engine.status().playback_state,
            radio_cli::engine::PlaybackState::Playing
        )
    {
        engine.tick().unwrap();
        assert!(Instant::now() < deadline, "{:?}", engine.status());
        std::thread::sleep(Duration::from_millis(10));
    }
    engine.pause().unwrap();
    while !engine.status().paused() {
        engine.tick().unwrap();
        assert!(Instant::now() < deadline, "{:?}", engine.status());
        std::thread::sleep(Duration::from_millis(10));
    }
    for seconds in [0.8, -0.4] {
        let before = engine.status().episode.as_ref().unwrap().position_ms;
        engine.podcast_action(Action::Relative { seconds }).unwrap();
        // Wait for mpv to report each position before issuing another seek.
        loop {
            engine.tick().unwrap();
            let position = engine.status().episode.as_ref().unwrap().position_ms;
            let moved = if seconds > 0. {
                position.saturating_sub(before)
            } else {
                before.saturating_sub(position)
            };
            if moved >= 200 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "relative seek by {seconds}s from {before}ms: {:?}",
                engine.status()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(engine.status().paused());
    }
    engine
        .podcast_action(Action::Queue {
            action: podcast::QueueAction::Next {
                id: ids[0].clone(),
                after: None,
            },
        })
        .unwrap();
    while engine.poll_podcast().is_none() {
        engine.tick().unwrap();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        engine
            .store()
            .podcast_queue()
            .unwrap()
            .iter()
            .map(|e| &e.id)
            .collect::<Vec<_>>(),
        vec![&ids[1], &ids[0], &ids[2]]
    );
    engine
        .podcast_action(Action::Sleep {
            value: "end".into(),
        })
        .unwrap();
    engine.pause().unwrap();
    while engine.playback_active() || engine.store().podcast_queue().unwrap().len() == 3 {
        engine.tick().unwrap();
        assert!(Instant::now() < deadline, "{:?}", engine.status());
        std::thread::sleep(Duration::from_millis(10));
    }
    let queue = engine.store().podcast_queue().unwrap();
    assert_eq!(
        queue.iter().map(|e| &e.id).collect::<Vec<_>>(),
        vec![&ids[0], &ids[2]]
    );
    assert_eq!(engine.status().episode.as_ref().unwrap().id, ids[1]);
    assert!(engine.store().podcast_episode(&ids[1]).unwrap().completed);
    engine.shutdown().unwrap();
}

#[test]
fn empty_subscriptions_receive_new_episodes_and_cli_titles_keep_json_contracts() {
    let dir = tempfile::tempdir().unwrap();
    let config = support::config(dir.path());
    let store = Store::open(&config.data_dir.join("radio.db")).unwrap();
    let mut p = preview("https://empty.test/feed");
    let episode = p.episodes.pop().unwrap();
    let feed = store.podcast_subscribe(&p, &p.feed.url).unwrap();
    p.episodes.push(episode);
    store.podcast_refresh_merge(&feed, Some(p)).unwrap();
    let q = podcast::LibraryQuery {
        view: "inbox".into(),
        feed: Some(feed.title.clone()),
        ..Default::default()
    };
    assert_eq!(store.podcast_library(&q).unwrap()["total"], 1);
    let run = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_radio"))
            .arg("--data-dir")
            .arg(dir.path())
            .args(["--config", "/dev/null", "podcast"])
            .args(args)
            .output()
            .unwrap()
    };
    let output = run(&["episodes", &feed.title]);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.starts_with("title\t"));
    assert!(text.contains("单集"));
    assert!(!text.contains("\"items\""));
    let output = run(&["library", "inbox", "--feed", &feed.title, "--json-v2"]);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ok"], true);
    assert_eq!(value["data"]["total"], 1);
    let output = run(&["save", "节目::单集", "--json-v2"]);
    assert!(output.status.success(), "{:?}", output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["saved"], true);
}

#[test]
fn foreground_download_retries_and_cancelling_backoff_requires_manual_resume() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let audio = support::fixture("libmp3lame", "mp3");
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let server = support::Server::new(move |request, stream| {
        if request.starts_with("GET /cancel ") || count.fetch_add(1, Ordering::SeqCst) == 0 {
            support::response(stream, 503, "text/plain", b"Later");
        } else {
            support::response(stream, 200, "audio/mpeg", &audio);
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let config = support::config(dir.path());
    let store = Store::open(&config.data_dir.join("radio.db")).unwrap();
    let mut p = preview(&server.url);
    p.episodes.push(Episode {
        guid: Some("cancel".into()),
        url: format!("{}/cancel", server.url),
        ..Default::default()
    });
    let feed = store.podcast_subscribe(&p, &p.feed.url).unwrap();
    let rows = store.podcast_episodes(&feed.id).unwrap();
    let e = rows
        .iter()
        .find(|e| e.guid.as_deref() == Some("opaque-guid"))
        .unwrap();
    let value = podcast::manage(
        &config,
        &store,
        Action::Download { id: e.id.clone() },
        &Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    assert_eq!(value["state"], "complete");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let id = rows
        .iter()
        .find(|e| e.guid.as_deref() == Some("cancel"))
        .unwrap()
        .id
        .clone();
    let target = id.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let cancel = stop.clone();
    let task = std::thread::spawn(move || {
        let store = Store::open(&config.data_dir.join("radio.db")).unwrap();
        podcast::manage(&config, &store, Action::Download { id: target }, &cancel)
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    while store
        .podcast_download(&id)
        .unwrap()
        .is_none_or(|d| d.retry_at.is_none())
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    stop.store(true, Ordering::SeqCst);
    assert!(task.join().unwrap().is_err());
    let d = store.podcast_download(&id).unwrap().unwrap();
    assert_eq!(d.state, "paused");
    assert!(d.retry_at.is_none());
}
