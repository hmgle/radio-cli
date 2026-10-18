mod support;
use radio_cli::{
    engine::Engine,
    model::Station,
    player::{Event, Player},
    recorder::{self, Recorder},
    store::Store,
};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::Command,
    sync::{Arc, atomic::AtomicBool},
    thread,
    time::{Duration, Instant},
};
use support::{Server, response};

fn sample(url: &str) -> Station {
    Station {
        stationuuid: uuid::Uuid::new_v4().to_string(),
        name: "Test / 电台".into(),
        url: url.into(),
        url_resolved: url.into(),
        custom: true,
        active: true,
        ..Station::default()
    }
}

#[test]
fn native_formats_are_copied_and_finalized_with_sidecars() {
    for (encoder, format, codec, ext) in [
        ("libmp3lame", "mp3", "mp3", "mp3"),
        ("aac", "adts", "aac", "aac"),
        ("libvorbis", "ogg", "vorbis", "ogg"),
        ("libopus", "opus", "opus", "opus"),
    ] {
        let bytes = support::fixture(encoder, format);
        let server = Server::new(move |_, s| response(s, 200, "application/octet-stream", &bytes));
        let dir = tempfile::tempdir().unwrap();
        let config = support::config(dir.path());
        let store = Store::open(&dir.path().join("radio.db")).unwrap();
        let station = sample(&server.url);
        store.upsert(&station).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let mut recorder = Recorder::start(
            &config,
            &store,
            &station,
            &server.url,
            &config.recording_dir,
            Some(Duration::from_secs(2)),
            &stop,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !recorder.poll().unwrap() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(20));
        }
        let saved = recorder.finish(&config, &store, None).unwrap();
        assert_eq!(saved.status, "complete", "{:?}", saved);
        assert_eq!(saved.codec, codec);
        assert!(saved.path.ends_with(ext));
        assert!(saved.size_bytes > 0);
        assert!(saved.media_duration_ms.unwrap() > 1000);
        assert!(PathBuf::from(format!("{}.json", saved.path)).exists());
        assert!(!saved.path.contains("/电台/"));
        let probe = recorder::probe(&config, &saved.path, &stop).unwrap();
        assert_eq!(probe["streams"][0]["codec_name"], codec);
        assert_eq!(store.recordings(1).unwrap()[0].path, saved.path);
        let listing: Vec<_> = fs::read_dir(&config.recording_dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(
            listing.len(),
            2,
            "temporary files should be removed after success"
        );
    }
}

#[test]
fn failures_preserve_partial_files_and_recovery_never_claims_completion() {
    use std::os::unix::fs::PermissionsExt;
    let bytes = support::fixture("libmp3lame", "mp3");
    let server = Server::new(move |_, s| response(s, 200, "audio/mpeg", &bytes));
    let dir = tempfile::tempdir().unwrap();
    let mut config = support::config(dir.path());
    let store = Store::open(&dir.path().join("radio.db")).unwrap();
    let station = sample(&server.url);
    store.upsert(&station).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let fake = dir.path().join("failing-ffmpeg");
    fs::write(
        &fake,
        "#!/bin/sh\nprintf 'No space left on device\\n' >&2\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o700)).unwrap();
    config.settings.ffmpeg = fake.to_string_lossy().into();
    let mut r = Recorder::start(
        &config,
        &store,
        &station,
        &server.url,
        &config.recording_dir,
        None,
        &stop,
    )
    .unwrap();
    while !r.poll().unwrap() {
        thread::sleep(Duration::from_millis(10));
    }
    let saved = r.finish(&config, &store, None).unwrap();
    assert_eq!(saved.status, "failed");
    assert!(saved.path.ends_with(".part"));
    assert!(saved.error.unwrap().contains("No space"));
    config.settings.ffmpeg = "ffmpeg".into();
    let recorder = Recorder::start(
        &config,
        &store,
        &station,
        &server.url,
        &config.recording_dir,
        None,
        &stop,
    )
    .unwrap();
    let interrupted = recorder.record.clone();
    drop(recorder);
    recorder::recover(&store).unwrap();
    let recovered = store
        .recordings(100)
        .unwrap()
        .into_iter()
        .find(|r| r.id == interrupted.id)
        .unwrap();
    assert_eq!(recovered.status, "interrupted");
    assert!(recovered.stopped_at.is_some());
    assert!(PathBuf::from(recovered.path).exists());
}

#[test]
fn hls_aac_is_saved_as_native_aac() {
    let source = tempfile::tempdir().unwrap();
    let playlist = source.path().join("index.m3u8");
    let result = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=4",
            "-c:a",
            "aac",
            "-f",
            "hls",
            "-hls_time",
            "1",
            "-hls_list_size",
            "0",
        ])
        .arg(&playlist)
        .status()
        .unwrap();
    assert!(result.success());
    let root = source.path().to_path_buf();
    let server = Server::new(move |request, stream| {
        let path = request
            .split_whitespace()
            .nth(1)
            .unwrap_or("/")
            .trim_start_matches('/');
        if path.contains("..") {
            response(stream, 404, "text/plain", b"");
            return;
        }
        match fs::read(root.join(path)) {
            Ok(bytes) => response(
                stream,
                200,
                if path.ends_with("m3u8") {
                    "application/vnd.apple.mpegurl"
                } else {
                    "video/mp2t"
                },
                &bytes,
            ),
            Err(_) => response(stream, 404, "text/plain", b""),
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let config = support::config(dir.path());
    let store = Store::open(&dir.path().join("radio.db")).unwrap();
    let url = format!("{}/index.m3u8", server.url);
    let station = sample(&url);
    store.upsert(&station).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let mut recorder = Recorder::start(
        &config,
        &store,
        &station,
        &url,
        &config.recording_dir,
        Some(Duration::from_secs(2)),
        &stop,
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    while !recorder.poll().unwrap() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    let record = recorder.finish(&config, &store, None).unwrap();
    assert_eq!(record.status, "complete", "{record:?}");
    assert_eq!(record.codec, "aac");
}

#[test]
fn early_stream_eof_is_not_reported_as_complete() {
    let bytes = support::fixture("libmp3lame", "mp3");
    let server = Server::new(move |_, s| response(s, 200, "audio/mpeg", &bytes));
    let dir = tempfile::tempdir().unwrap();
    let config = support::config(dir.path());
    let store = Store::open(&dir.path().join("radio.db")).unwrap();
    let station = sample(&server.url);
    store.upsert(&station).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let mut recorder = Recorder::start(
        &config,
        &store,
        &station,
        &server.url,
        &config.recording_dir,
        Some(Duration::from_secs(30)),
        &stop,
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !recorder.poll().unwrap() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    let record = recorder.finish(&config, &store, None).unwrap();
    assert_eq!(record.status, "interrupted");
    assert!(record.path.ends_with(".part"));
}

fn mpv_config(dir: &std::path::Path) -> Option<radio_cli::config::Config> {
    use std::os::unix::fs::PermissionsExt;
    let mpv = std::env::var("RADIO_TEST_MPV").unwrap_or_else(|_| "mpv".into());
    if !Command::new(&mpv)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        eprintln!("mpv unavailable; set RADIO_TEST_MPV to run real IPC tests");
        return None;
    }
    let wrapper = dir.join("mpv-null");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nexec '{}' --ao=null \"$@\"\n",
            mpv.replace('\'', "'\\''")
        ),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
    let mut config = support::config(dir);
    config.settings.mpv = wrapper.to_string_lossy().into();
    Some(config)
}

#[test]
fn mpv_ipc_reads_icy_titles_and_reaps_child() {
    let dir = tempfile::tempdir().unwrap();
    let Some(config) = mpv_config(dir.path()) else {
        return;
    };
    let bytes = support::fixture("libmp3lame", "mp3");
    let server = Server::new(move |request, stream| {
        let icy = request.to_lowercase().contains("icy-metadata: 1");
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nConnection: close\r\n{}\r\n",
            if icy { "icy-metaint: 1024\r\n" } else { "" }
        );
        if stream.write_all(header.as_bytes()).is_err() {
            return;
        }
        for _ in 0..4 {
            for chunk in bytes.chunks(1024) {
                if stream.write_all(chunk).is_err() {
                    return;
                }
                if icy && chunk.len() == 1024 {
                    let mut metadata = b"StreamTitle='Local test';".to_vec();
                    metadata.resize(32, 0);
                    if stream.write_all(&[2]).is_err() || stream.write_all(&metadata).is_err() {
                        return;
                    }
                }
                // Keep ICY alignment across repeats by ending at a complete block.
                if chunk.len() != 1024 {
                    return;
                }
            }
        }
    });
    let stop = Arc::new(AtomicBool::new(false));
    let mut player = Player::start(&config, &server.url, &stop).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut title = false;
    let mut playing = false;
    while Instant::now() < deadline && !(title && playing) {
        for event in player.poll().unwrap() {
            match event {
                Event::Playing => playing = true,
                Event::Title(s) if s == "Local test" => title = true,
                _ => {}
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        title && playing,
        "must receive playback and ICY metadata events"
    );
    player.toggle_pause().unwrap();
    player.volume(-10).unwrap();
    drop(player);
    assert_eq!(fs::read_dir(&config.runtime_dir).unwrap().count(), 0);
}

#[test]
fn edited_custom_station_plays_the_new_url() {
    let dir = tempfile::tempdir().unwrap();
    let Some(config) = mpv_config(dir.path()) else {
        return;
    };
    let bytes = support::fixture("libmp3lame", "mp3");
    let server = Server::new(move |_, stream| response(stream, 200, "audio/mpeg", &bytes));
    let mut store = Store::open(&dir.path().join("radio.db")).unwrap();
    let station = radio_cli::service::add_station(
        &mut store,
        "Editable".into(),
        "http://127.0.0.1:1/old".into(),
        &[],
    )
    .unwrap();
    radio_cli::service::edit_station(
        &mut store,
        &station.stationuuid,
        None,
        Some(server.url.clone()),
        &[],
    )
    .unwrap();
    drop(store);
    let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
    engine.play(&station.stationuuid).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !engine.status.playing {
        engine.tick().unwrap();
        assert!(Instant::now() < deadline, "{}", engine.status.message);
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(engine.status.source_url, server.url);
    engine.shutdown().unwrap();
}

#[test]
fn live_stream_reconnects_after_a_ten_second_outage_without_duplicate_history() {
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    let dir = tempfile::tempdir().unwrap();
    let Some(config) = mpv_config(dir.path()) else {
        return;
    };
    let bytes = support::fixture("libmp3lame", "mp3");
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = requests.clone();
    let outage = Arc::new(Mutex::new(None::<Instant>));
    let server = Server::new(move |_, stream| {
        let n = seen.fetch_add(1, Ordering::Relaxed);
        let mut until = outage.lock().unwrap();
        if n == 0 {
            // First fixture plays for four seconds; refuse connections for
            // ten more seconds after its expected EOF.
            *until = Some(Instant::now() + Duration::from_secs(14));
            response(stream, 200, "audio/mpeg", &bytes);
        } else if until.is_some_and(|t| Instant::now() < t) {
            response(stream, 503, "text/plain", b"offline");
        } else {
            response(stream, 200, "audio/mpeg", &bytes);
        }
    });
    let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
    let station = sample(&server.url);
    engine.store.upsert(&station).unwrap();
    engine.play(&station.stationuuid).unwrap();
    let deadline = Instant::now() + Duration::from_secs(23);
    let mut first_played = false;
    let mut reconnecting = false;
    loop {
        engine.tick().unwrap();
        first_played |= engine.status.playing;
        reconnecting |= engine.status.message.contains("重连中");
        if first_played && reconnecting && engine.status.playing {
            break;
        }
        assert!(!engine.playback_failed, "{}", engine.status.message);
        assert!(Instant::now() < deadline, "{}", engine.status.message);
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        engine.status.station.as_ref().unwrap().stationuuid,
        station.stationuuid
    );
    let conn = rusqlite::Connection::open(engine.config.data_dir.join("radio.db")).unwrap();
    assert_eq!(
        conn.query_row("SELECT count(*) FROM history", [], |r| r.get::<_, usize>(0))
            .unwrap(),
        1
    );
    engine.stop_playback().unwrap();
    let before = requests.load(Ordering::Relaxed);
    for _ in 0..70 {
        engine.tick().unwrap();
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(requests.load(Ordering::Relaxed), before);
}

#[test]
fn runtime_preferences_survive_sessions_without_rewriting_config() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.toml");
    let original = "# handwritten\nvolume = 80\nresume = true\n";
    fs::write(&config_path, original).unwrap();
    let mut config = radio_cli::config::Config::load(
        Some(&config_path),
        Some(dir.path()),
        Some("http://127.0.0.1:1"),
    )
    .unwrap();
    config.prepare().unwrap();
    config.settings.mpv = support::fake_mpv(dir.path()).to_string_lossy().into();
    let stop = Arc::new(AtomicBool::new(false));
    let id;
    {
        let mut engine = Engine::new(config.clone(), stop.clone()).unwrap();
        let station = radio_cli::service::add_station(
            &mut engine.store,
            "Remembered".into(),
            "https://example.com/live".into(),
            &[],
        )
        .unwrap();
        id = station.stationuuid;
        engine.play(&id).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !engine.status.playing {
            engine.tick().unwrap();
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        engine.volume(-15).unwrap();
        engine.shutdown().unwrap();
    }
    let mut resumed = Engine::new(config.clone(), stop.clone()).unwrap();
    assert_eq!(resumed.status.volume, 65);
    assert_eq!(resumed.config.settings.volume, 65);
    resumed.resume_last_station().unwrap();
    assert_eq!(resumed.status.station.as_ref().unwrap().stationuuid, id);
    resumed.shutdown().unwrap();
    resumed.store.set_meta("pref_volume", "999").unwrap();
    drop(resumed);
    let fresh = Engine::new(config, stop).unwrap();
    assert_eq!(fresh.status.volume, 80);
    assert_eq!(fs::read_to_string(config_path).unwrap(), original);
}

#[test]
fn engine_uses_explicit_backup_without_merging_names() {
    let dir = tempfile::tempdir().unwrap();
    let Some(config) = mpv_config(dir.path()) else {
        return;
    };
    let bytes = support::fixture("libmp3lame", "mp3");
    let server = Server::new(move |_, s| response(s, 200, "audio/mpeg", &bytes));
    let stop = Arc::new(AtomicBool::new(false));
    let mut engine = Engine::new(config, stop).unwrap();
    let station = sample("http://127.0.0.1:1/unreachable");
    engine.store.upsert(&station).unwrap();
    engine
        .store
        .add_backup(&station.stationuuid, &server.url)
        .unwrap();
    engine.play(&station.stationuuid).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !engine.status.playing && Instant::now() < deadline {
        engine.tick().unwrap();
        thread::sleep(Duration::from_millis(20));
    }
    assert!(engine.status.playing, "{}", engine.status.message);
    assert!(!engine.playback_failed);
    engine.toggle_recording().unwrap();
    assert!(engine.status.recording);
    // Finalizing before FFmpeg captured any audio would correctly be
    // reported as a failed recording; wait until the partial file holds a
    // real audio track so the stop always finalizes a verifiable file.
    let partial = PathBuf::from(&engine.status.recording_path);
    let wait_stop = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let captured = recorder::probe(&engine.config, &partial.to_string_lossy(), &wait_stop)
            .ok()
            .and_then(|info| info["streams"].as_array().map(|s| !s.is_empty()))
            .unwrap_or(false);
        if captured {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "recording captured no audio track before finalizing"
        );
        thread::sleep(Duration::from_millis(50));
    }
    engine.pause().unwrap();
    assert!(engine.status.recording, "pause should not end recording");
    engine.shutdown().unwrap();
    assert!(!engine.status.recording);
    let record = engine.store.recordings(1).unwrap().remove(0);
    assert_eq!(record.status, "complete", "{record:?}");
}

#[test]
fn stalled_explicit_station_times_out_without_selecting_another() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut config) = mpv_config(dir.path()) else {
        return;
    };
    config.settings.playback_timeout_secs = 1;
    let server = Server::new(move |_, _| thread::sleep(Duration::from_secs(2)));
    let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
    let selected = sample(&server.url);
    engine.store.upsert(&selected).unwrap();
    engine
        .store
        .upsert(&sample("http://127.0.0.1:1/other"))
        .unwrap();
    engine.play(&selected.stationuuid).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !engine.playback_failed && Instant::now() < deadline {
        engine.tick().unwrap();
        thread::sleep(Duration::from_millis(20));
    }
    assert!(engine.playback_failed, "stalled input must time out");
    assert_eq!(
        engine.status.station.as_ref().unwrap().stationuuid,
        selected.stationuuid
    );
}

#[test]
fn progress_flood_does_not_lose_final_duration() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let mut config = support::config(dir.path());
    let backend = dir.path().join("progress-backend");
    let probe = dir.path().join("probe-backend");
    fs::write(&backend, "#!/bin/sh\nprintf audio\ni=1\nwhile [ $i -le 40 ]; do printf 'out_time_us=%s\\n' $((i * 50000)) >&2; i=$((i + 1)); done\n").unwrap();
    fs::write(&probe, "#!/bin/sh\nprintf '%s' '{\"streams\":[{\"codec_name\":\"mp3\"}],\"format\":{\"duration\":\"2.0\"}}'\n").unwrap();
    for path in [&backend, &probe] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    config.settings.ffmpeg = backend.to_string_lossy().into();
    config.settings.ffprobe = probe.to_string_lossy().into();
    let store = Store::open(&dir.path().join("radio.db")).unwrap();
    let station = sample("https://example.com/audio");
    let mut recorder = Recorder::start(
        &config,
        &store,
        &station,
        station.primary_url(),
        &config.recording_dir,
        Some(Duration::from_secs(2)),
        &Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    // Deliberately leave all progress unread until the producer exits.
    thread::sleep(Duration::from_millis(200));
    assert!(recorder.poll().unwrap());
    assert_eq!(recorder.elapsed_ms(), 2000);
    let record = recorder.finish(&config, &store, None).unwrap();
    assert_eq!(record.status, "complete", "{record:?}");
    assert_eq!(record.media_duration_ms, Some(2000));
}

#[test]
fn inaccessible_old_recordings_do_not_block_media_startup() {
    let dir = tempfile::tempdir().unwrap();
    let config = support::config(dir.path());
    let store = Store::open(&dir.path().join("radio.db")).unwrap();
    for i in 0..2 {
        let r: radio_cli::model::Recording = serde_json::from_value(serde_json::json!({
            "id":format!("old-{i}"), "stationuuid":"old", "station_name":"old", "source_url_resolved":"https://example.com", "started_at":"2026-01-01", "stopped_at":null,
            "media_duration_ms":null, "codec":"mp3", "container":"mp3", "path":dir.path().join(format!("missing-{i}/old.part")), "size_bytes":0,"status":"recording","error":null
        })).unwrap();
        store.save_recording(&r).unwrap();
    }
    drop(store);
    let engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
    assert!(engine.status.message.contains("old-0"));
    assert!(engine.status.message.contains("old-1"));
    for r in engine.store.recordings(10).unwrap() {
        assert_eq!(r.status, "interrupted");
        assert!(r.error.unwrap().contains(&r.path));
    }
}

#[test]
fn invalid_resolved_url_uses_original_for_playback_and_recording() {
    let dir = tempfile::tempdir().unwrap();
    let Some(config) = mpv_config(dir.path()) else {
        return;
    };
    let bytes = support::fixture("libmp3lame", "mp3");
    let server = Server::new(move |_, s| response(s, 200, "audio/mpeg", &bytes));
    let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
    let mut station = sample(&server.url);
    station.url_resolved = "invalid resolved URL".into();
    engine.store.upsert(&station).unwrap();
    engine.play(&station.stationuuid).unwrap();
    assert_eq!(engine.status.source_url, server.url);
    engine
        .standalone_record(&station.stationuuid, Some(Duration::from_secs(1)), None)
        .unwrap();
    assert_eq!(
        engine.store.recordings(1).unwrap()[0].source_url_resolved,
        server.url
    );
    engine.shutdown().unwrap();
}

#[test]
fn recording_resolves_backup_and_missing_backend_fails_fast() {
    let dir = tempfile::tempdir().unwrap();
    let config = support::config(dir.path());
    let bytes = support::fixture("libmp3lame", "mp3");
    let server = Server::new(move |_, s| response(s, 200, "audio/mpeg", &bytes));
    let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
    let station = sample("http://127.0.0.1:1/unavailable");
    engine.store.upsert(&station).unwrap();
    engine
        .store
        .add_backup(&station.stationuuid, &server.url)
        .unwrap();
    engine
        .standalone_record(&station.stationuuid, Some(Duration::from_secs(1)), None)
        .unwrap();
    assert_eq!(
        engine.store.recordings(1).unwrap()[0].source_url_resolved,
        server.url
    );
    engine.stop_recording(None).unwrap();
    engine.config.settings.ffprobe = dir.path().join("missing-ffprobe").to_string_lossy().into();
    let error = engine
        .standalone_record(&station.stationuuid, None, None)
        .unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<radio_cli::error::DomainError>()
            .unwrap()
            .code,
        radio_cli::error::ErrorCode::MissingDependency
    );
}

#[test]
fn repeated_adjustments_reach_the_player_ipc() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = support::config(dir.path());
    let fake = support::fake_mpv(dir.path());
    config.settings.mpv = fake.to_string_lossy().into();
    config.settings.volume = 80;
    let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
    let row = sample("https://example.com/live");
    engine.store.upsert(&row).unwrap();
    engine.play(&row.stationuuid).unwrap();
    for _ in 0..3 {
        engine.volume(5).unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        engine.tick().unwrap();
        let log = fs::read_to_string(fake.with_extension("commands")).unwrap();
        if log.matches("[\"add\",\"volume\",5]").count() == 3 {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(engine.status.volume, 95);
    engine.shutdown().unwrap();
    assert_eq!(
        fs::read_to_string(fake.with_extension("commands"))
            .unwrap()
            .matches("[\"add\",\"volume\",5]")
            .count(),
        3
    );
}

#[test]
fn recording_skips_a_source_without_an_audio_track() {
    let video = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=16x16:d=1",
            "-an",
            "-c:v",
            "mpeg4",
            "-f",
            "matroska",
            "pipe:1",
        ])
        .output()
        .unwrap();
    assert!(video.status.success());
    let audio = support::fixture("libmp3lame", "mp3");
    let server = Server::new(move |request, s| {
        response(
            s,
            200,
            "application/octet-stream",
            if request.contains("/video") {
                &video.stdout
            } else {
                &audio
            },
        )
    });
    let dir = tempfile::tempdir().unwrap();
    let config = support::config(dir.path());
    let mut engine = Engine::new(config, Arc::new(AtomicBool::new(false))).unwrap();
    let row = sample(&format!("{}/video", server.url));
    engine.store.upsert(&row).unwrap();
    let backup = format!("{}/audio", server.url);
    engine.store.add_backup(&row.stationuuid, &backup).unwrap();
    engine
        .standalone_record(&row.stationuuid, Some(Duration::from_secs(1)), None)
        .unwrap();
    assert_eq!(
        engine.store.recordings(1).unwrap()[0].source_url_resolved,
        backup
    );
    engine.shutdown().unwrap();
}
