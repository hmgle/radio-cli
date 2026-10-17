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
    engine.pause().unwrap();
    assert!(engine.status.recording, "pause should not end recording");
    engine.shutdown().unwrap();
    assert!(!engine.status.recording);
    assert_eq!(engine.store.recordings(1).unwrap()[0].status, "complete");
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
