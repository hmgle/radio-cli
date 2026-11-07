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
fn subscriptions_merge_without_losing_identity_progress_or_missing_episodes() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("radio.db")).unwrap();
    let mut p = preview("https://example.test/rss");
    let f = store.podcast_subscribe(&p, &p.feed.url).unwrap();
    let e = store.podcast_episodes(&f.id).unwrap().remove(0);
    store
        .podcast_progress(&e.id, 12345, Some(60000), true)
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
        .podcast_progress(&e.id, 1200, None, false)
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
        .podcast_progress(&e.id, 15000, None, false)
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
    let id = s.podcast_episodes(&f.id).unwrap()[0].id.clone();
    s.podcast_progress(&id, 321, Some(1000), true).unwrap();
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
