mod support;
use radio_cli::{model::Station, store::Store};
use std::{
    fs,
    io::{Read, Write},
    os::fd::{AsRawFd, FromRawFd},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn radio(dir: &std::path::Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_radio"));
    c.arg("--data-dir").arg(dir);
    c
}

#[test]
fn cli_roundtrip_and_invalid_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let add = radio(dir.path())
        .args([
            "--json",
            "station",
            "add",
            "上海电台",
            "https://example.invalid/live",
            "--backup-url",
            "http://example.invalid/backup",
        ])
        .output()
        .unwrap();
    assert!(
        add.status.success(),
        "{}",
        String::from_utf8_lossy(&add.stderr)
    );
    let station: Station = serde_json::from_slice(&add.stdout).unwrap();
    assert!(
        radio(dir.path())
            .args(["favorites", "add", &station.stationuuid])
            .status()
            .unwrap()
            .success()
    );
    let search = radio(dir.path())
        .args(["--json", "search", "上海"])
        .output()
        .unwrap();
    let rows: Vec<Station> = serde_json::from_slice(&search.stdout).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].favorite);
    let invalid = radio(dir.path())
        .args(["station", "add", "Bad", "file:///etc/passwd"])
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    assert!(
        !radio(dir.path())
            .args(["record", &station.stationuuid, "--duration", "0"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(
        !radio(dir.path())
            .args(["tui"])
            .stdin(Stdio::null())
            .output()
            .unwrap()
            .status
            .success()
    );
    let help = radio(dir.path()).stdin(Stdio::null()).output().unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("Usage"));
}

#[test]
fn bad_config_is_reported_without_panicking() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("bad.toml");
    fs::write(&config, "volume = 200\n").unwrap();
    let output = radio(dir.path())
        .arg("--config")
        .arg(&config)
        .arg("doctor")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("0–100"));
}

#[test]
fn favorite_mutations_confirm_names_and_reject_missing_stations() {
    let dir = tempfile::tempdir().unwrap();
    let _config = support::config(dir.path());
    let store = Store::open(&dir.path().join("radio.db")).unwrap();
    let station = Station {
        stationuuid: uuid::Uuid::new_v4().to_string(),
        name: "Jazz 上海\x1b\n".into(),
        active: true,
        ..Station::default()
    };
    store.upsert(&station).unwrap();
    for action in ["add", "remove"] {
        let output = radio(dir.path())
            .args(["favorites", action, &station.stationuuid])
            .output()
            .unwrap();
        assert!(output.status.success());
        let confirmation = String::from_utf8(output.stdout).unwrap();
        assert!(confirmation.contains("Jazz 上海"));
        assert!(!confirmation.contains('\x1b'));
        let json = radio(dir.path())
            .args(["--json", "favorites", action, &station.stationuuid])
            .output()
            .unwrap();
        assert!(json.status.success());
        let result: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
        assert_eq!(result["stationuuid"], station.stationuuid);
        assert_eq!(result["favorite"], action == "add");
        assert_eq!(
            store.station(&station.stationuuid).unwrap().favorite,
            action == "add"
        );
        let missing = radio(dir.path())
            .args(["favorites", action, &uuid::Uuid::new_v4().to_string()])
            .output()
            .unwrap();
        assert!(!missing.status.success());
        assert!(missing.stdout.is_empty());
        assert!(!missing.stderr.is_empty());
    }
}

#[test]
fn cli_language_precedence_and_unknown_locale_notice() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    fs::write(&config, "language = 'zh-CN'\n").unwrap();
    for (args, radio_lang, lc_all, expected) in [
        (
            vec!["--lang", "en"],
            "zh-CN",
            "zh_CN.UTF-8",
            "Station not found",
        ),
        (vec![], "en", "zh_CN.UTF-8", "Station not found"),
        (vec![], "", "en_US.UTF-8", "本地未找到电台"),
        (vec!["--lang", "zh-CN"], "en", "C", "本地未找到电台"),
        (
            vec!["--lang", "unknown"],
            "zh-CN",
            "zh_CN.UTF-8",
            "Station not found",
        ),
    ] {
        let result = radio(dir.path())
            .arg("--config")
            .arg(&config)
            .args(args)
            .args(["info", &uuid::Uuid::new_v4().to_string()])
            .env("RADIO_LANG", radio_lang)
            .env("LC_ALL", lc_all)
            .output()
            .unwrap();
        assert!(!result.status.success());
        let error = String::from_utf8(result.stderr).unwrap();
        assert!(error.contains(expected), "{error}");
        assert!(error.matches("unsupported language").count() <= 1);
    }
    let result = radio(dir.path())
        .args(["info", &uuid::Uuid::new_v4().to_string()])
        .env_remove("RADIO_LANG")
        .env("LC_ALL", "zh_CN.UTF-8")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&result.stderr).contains("本地未找到电台"));
}

#[test]
fn station_cli_supports_edit_backups_list_and_remove() {
    let dir = tempfile::tempdir().unwrap();
    let output = radio(dir.path())
        .args([
            "--json",
            "station",
            "add",
            "Before",
            "https://example.com/live",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let station: Station = serde_json::from_slice(&output.stdout).unwrap();
    let id = station.stationuuid.as_str();
    for args in [
        vec![
            "station",
            "edit",
            id,
            "--name",
            "After",
            "--url",
            "https://example.com/changed",
        ],
        vec![
            "station",
            "backups",
            id,
            "--add",
            "https://example.com/backup",
        ],
    ] {
        let output = radio(dir.path()).args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let output = radio(dir.path())
        .args(["--json", "station", "list", "--all"])
        .output()
        .unwrap();
    let page: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(page["total"], 1);
    assert_eq!(page["items"][0]["name"], "After");
    let duplicate = radio(dir.path())
        .args([
            "station",
            "add",
            "Again",
            "https://EXAMPLE.com:443/changed#ignored",
        ])
        .output()
        .unwrap();
    assert!(!duplicate.status.success());
    assert!(String::from_utf8_lossy(&duplicate.stderr).contains("already exists"));
    let output = radio(dir.path())
        .args(["station", "remove", id, "--keep-references"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let removed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(removed["placeholder_kept"], true);
    let output = radio(dir.path()).args(["info", id]).output().unwrap();
    let row: Station = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(row.name, "After");
    assert!(!row.active);
}

#[test]
fn facet_cli_reports_real_local_values() {
    let dir = tempfile::tempdir().unwrap();
    let _config = support::config(dir.path());
    let store = Store::open(&dir.path().join("radio.db")).unwrap();
    store
        .upsert(&Station {
            stationuuid: uuid::Uuid::new_v4().to_string(),
            name: "Fixture".into(),
            active: true,
            countrycode: "CN".into(),
            languagecodes: "zh,en".into(),
            tags: "jazz".into(),
            ..Station::default()
        })
        .unwrap();
    for (command, prefix, expected) in [
        ("countries", "c", "CN"),
        ("languages", "zh", "zh"),
        ("tags", "ja", "jazz"),
    ] {
        let output = radio(dir.path())
            .args(["--json", command, "--prefix", prefix])
            .output()
            .unwrap();
        assert!(output.status.success());
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result.as_array().unwrap().len(), 1);
        assert_eq!(result[0]["value"], expected);
        assert_eq!(result[0]["count"], 1);
    }
}

#[test]
fn media_lock_blocks_only_competing_media_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let config = support::config(dir.path());
    let _lock = radio_cli::process::MediaLock::acquire(&config.runtime_dir).unwrap();
    assert!(
        radio(dir.path())
            .args(["--json", "search"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let result = radio(dir.path())
        .args(["play", &uuid::Uuid::new_v4().to_string()])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("Another radio"));
}

#[cfg(target_os = "linux")]
#[test]
fn finished_session_releases_lock_even_while_a_fork_inherits_its_descriptor() {
    let dir = tempfile::tempdir().unwrap();
    let config = support::config(dir.path());
    let lock = radio_cli::process::MediaLock::acquire(&config.runtime_dir).unwrap();
    unsafe {
        let mut pipe = [0; 2];
        assert_eq!(libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC), 0);
        let pid = libc::fork();
        assert!(pid >= 0);
        if pid == 0 {
            // Only async-signal-safe calls after forking the test process.
            libc::close(pipe[1]);
            let mut byte = 0u8;
            libc::read(pipe[0], (&mut byte as *mut u8).cast(), 1);
            libc::_exit(0);
        }
        libc::close(pipe[0]);
        drop(lock);
        let next = radio_cli::process::MediaLock::acquire(&config.runtime_dir);
        let byte = 1u8;
        libc::write(pipe[1], (&byte as *const u8).cast(), 1);
        libc::close(pipe[1]);
        libc::waitpid(pid, std::ptr::null_mut(), 0);
        assert!(
            next.is_ok(),
            "inherited descriptor prolonged the finished session"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn owned_children_exit_after_frontend_sigkill() {
    if let Some(path) = std::env::var_os("RADIO_TEST_CHILD_PID") {
        let mut command = Command::new("/bin/sleep");
        command.arg("60");
        let mut child = radio_cli::process::spawn_owned(command).unwrap();
        fs::write(path, child.id().to_string()).unwrap();
        let _ = child.wait();
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("child.pid");
    let mut parent = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "owned_children_exit_after_frontend_sigkill"])
        .env("RADIO_TEST_CHILD_PID", &path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        if Instant::now() > deadline {
            parent.kill().unwrap();
            parent.wait().unwrap();
            panic!("child never started");
        }
        thread::sleep(Duration::from_millis(20));
    }
    let pid = fs::read_to_string(path).unwrap().parse::<i32>().unwrap();
    parent.kill().unwrap();
    parent.wait().unwrap();
    let state_path = format!("/proc/{pid}/stat");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let state = fs::read_to_string(&state_path);
        if state.is_err()
            || state
                .unwrap()
                .rsplit(')')
                .next()
                .unwrap()
                .trim_start()
                .starts_with('Z')
        {
            break;
        }
        if Instant::now() > deadline {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
            panic!("owned child survived frontend SIGKILL");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn tui_restores_terminal_on_quit_ctrl_c_and_sigterm() {
    for mode in ["q", "ctrl-c", "sigterm"] {
        let dir = tempfile::tempdir().unwrap();
        let config = support::config(dir.path());
        let store = Store::open(&dir.path().join("radio.db")).unwrap();
        store
            .upsert(&Station {
                stationuuid: uuid::Uuid::new_v4().to_string(),
                name: "本地测试".into(),
                active: true,
                custom: true,
                ..Station::default()
            })
            .unwrap();
        let (mut master, slave, before) = unsafe {
            let mut master = 0;
            let mut slave = 0;
            let mut size = libc::winsize {
                ws_row: 24,
                ws_col: 100,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            assert_eq!(
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &raw mut size
                ),
                0
            );
            let mut termios = std::mem::zeroed::<libc::termios>();
            assert_eq!(libc::tcgetattr(slave, &mut termios), 0);
            let flags = libc::fcntl(master, libc::F_GETFL);
            assert_ne!(
                libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK),
                -1
            );
            (
                fs::File::from_raw_fd(master),
                fs::File::from_raw_fd(slave),
                termios,
            )
        };
        let mut child = radio(dir.path())
            .arg("tui")
            .env("TERM", "xterm-256color")
            .env("NO_COLOR", if mode == "q" { "1" } else { "" })
            .stdin(slave.try_clone().unwrap())
            .stdout(slave.try_clone().unwrap())
            .stderr(slave.try_clone().unwrap())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = String::new();
        let mut buffer = [0; 8192];
        while !output.contains("radio") && Instant::now() < deadline {
            if let Ok(n) = master.read(&mut buffer) {
                output.push_str(&String::from_utf8_lossy(&buffer[..n]));
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(output.contains("radio"), "TUI did not render: {output}");
        if mode == "q" {
            for sequence in output.split("\x1b[").skip(1) {
                let end = sequence.bytes().find(|b| (0x40..=0x7e).contains(b));
                assert_ne!(end, Some(b'm'), "NO_COLOR emitted SGR: {output}");
            }
        }
        // Exercise resize handling before leaving.
        unsafe {
            let size = libc::winsize {
                ws_row: 12,
                ws_col: 50,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &size);
        }
        match mode {
            "q" => master.write_all(b"q").unwrap(),
            "ctrl-c" => master.write_all(&[3]).unwrap(),
            _ => unsafe {
                assert_eq!(libc::kill(child.id() as i32, libc::SIGTERM), 0);
            },
        }
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "{mode}: {output}");
                break;
            }
            if Instant::now() > deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("TUI did not exit: {mode}");
            }
            let _ = master.read(&mut buffer);
            thread::sleep(Duration::from_millis(20));
        }
        unsafe {
            let mut after = std::mem::zeroed::<libc::termios>();
            assert_eq!(libc::tcgetattr(slave.as_raw_fd(), &mut after), 0);
            assert_eq!(
                support::terminal_local_flags(&before),
                support::terminal_local_flags(&after),
                "terminal flags not restored on {mode}"
            );
        }
        assert_eq!(
            fs::read_dir(&config.runtime_dir).unwrap().count(),
            1,
            "only media.lock should remain"
        );
    }
}

#[test]
fn recordings_list_keeps_tabs_in_diagnostic_lines() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("radio.db")).unwrap();
    let record: radio_cli::model::Recording = serde_json::from_value(serde_json::json!({
        "id":"tabs", "stationuuid":"s", "station_name":"Recorded stream",
        "source_url_resolved":"https://example.com", "started_at":"2026-01-01",
        "stopped_at":null, "media_duration_ms":null, "codec":"mp3", "container":"mp3",
        "path":"/tmp/sample", "size_bytes":1, "status":"complete", "reconnects":0,
        "error":"frame\tpts\tsize\n1\t0\t64"
    }))
    .unwrap();
    store.save_recording(&record).unwrap();
    let output = radio(dir.path())
        .args(["recordings", "list"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("frame\tpts\tsize"), "{text}");
    assert!(text.contains("1\t0\t64"), "{text}");
}

#[test]
fn recordings_list_preserves_diagnostic_tails_without_terminal_controls() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("radio.db")).unwrap();
    let body = "x".repeat(7000);
    let record: radio_cli::model::Recording = serde_json::from_value(serde_json::json!({
        "id":"diagnostics", "stationuuid":"s", "station_name":"Recorded stream",
        "source_url_resolved":"https://example.com", "started_at":"2026-01-01",
        "stopped_at":null, "media_duration_ms":null, "codec":"mp3", "container":"mp3",
        "path":"/tmp/sample", "size_bytes":1, "status":"complete", "reconnects":2,
        "error":format!("first line\n{body}\n\u{1b}[31mFINAL_DIAGNOSTIC\u{1b}[0m")
    }))
    .unwrap();
    store.save_recording(&record).unwrap();
    for (lang, warning) in [("en", "audio may have gaps"), ("zh-CN", "音频可能有缺口")] {
        let output = radio(dir.path())
            .args(["--lang", lang, "recordings", "list"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains(&body));
        assert!(text.contains("\n  first line\n"));
        assert!(text.contains("FINAL_DIAGNOSTIC"));
        assert!(text.contains(warning));
        assert!(!text.contains('\u{1b}'));
    }
}

#[test]
fn large_lists_expose_pages_totals_and_full_exports() {
    let dir = tempfile::tempdir().unwrap();
    let _config = support::config(dir.path());
    let store = Store::open(&dir.path().join("radio.db")).unwrap();
    for i in 0..105 {
        let station = Station {
            stationuuid: uuid::Uuid::new_v4().to_string(),
            name: format!("station {i:03}"),
            active: true,
            ..Station::default()
        };
        store.upsert(&station).unwrap();
        store.favorite(&station.stationuuid, true).unwrap();
        store.played(&station.stationuuid).unwrap();
        let r: radio_cli::model::Recording = serde_json::from_value(serde_json::json!({
            "id":format!("r-{i:03}"),"stationuuid":station.stationuuid,"station_name":station.name,"source_url_resolved":"https://example.com","started_at":format!("2026-01-01T00:{:02}:{:02}Z", i/60,i%60),"stopped_at":null,"media_duration_ms":null,"codec":"mp3","container":"mp3","path":"/tmp/test","size_bytes":1,"status":"complete","error":null
        })).unwrap();
        store.save_recording(&r).unwrap();
    }
    // Updating an old recording must not move it ahead of newer starts.
    let first = store.recordings_page(1, 104).unwrap().remove(0);
    store.save_recording(&first).unwrap();
    assert_eq!(store.recordings_page(1, 104).unwrap()[0].id, first.id);
    for command in [
        vec!["favorites", "list"],
        vec!["history"],
        vec!["recordings", "list"],
    ] {
        for (options, len, more) in [
            (vec![], 100, true),
            (vec!["--offset", "100"], 5, false),
            (vec!["--offset", "105"], 0, false),
            (vec!["--all"], 105, false),
            (vec!["--all", "--limit", "10", "--offset", "7"], 98, false),
        ] {
            let output = radio(dir.path())
                .arg("--json")
                .args(&command)
                .args(options)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let page: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(page["items"].as_array().unwrap().len(), len);
            assert_eq!(page["total"], 105);
            assert_eq!(page["has_more"], more);
            let ids: std::collections::HashSet<_> = page["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["id"].as_str().or_else(|| item["stationuuid"].as_str()))
                .collect();
            assert_eq!(ids.len(), len, "pages repeated entries");
        }
    }
}

#[test]
fn json_v2_is_atomic_preserves_legacy_and_has_stable_exit_codes() {
    let dir = tempfile::tempdir().unwrap();
    let _config = support::config(dir.path());
    let read_json = |output: std::process::Output, code: i32| {
        assert_eq!(
            output.status.code(),
            Some(code),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    let station = read_json(
        radio(dir.path())
            .args([
                "--json-v2",
                "station",
                "add",
                "JSON station",
                "https://example.com/live",
            ])
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(station["ok"], true);
    let legacy = read_json(
        radio(dir.path())
            .args(["--json", "search", "JSON"])
            .output()
            .unwrap(),
        0,
    );
    assert!(legacy.is_array());
    let modern = read_json(
        radio(dir.path())
            .args(["--json-v2", "search", "JSON"])
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(modern["data"], legacy);
    for lang in ["en", "zh-CN"] {
        let failure = read_json(
            radio(dir.path())
                .args(["--lang", lang, "--json-v2", "info", "missing"])
                .output()
                .unwrap(),
            1,
        );
        assert_eq!(failure["ok"], false);
        assert_eq!(failure["error"]["code"], "station_not_found");
        assert!(failure["error"]["params"].is_object());
    }
    let usage = read_json(
        radio(dir.path())
            .args(["--json-v2", "no-such-command"])
            .output()
            .unwrap(),
        2,
    );
    assert_eq!(usage["error"]["code"], "usage_error");
    let no_command = read_json(radio(dir.path()).arg("--json-v2").output().unwrap(), 2);
    assert_eq!(no_command["error"]["code"], "usage_error");
    let config = support::config(dir.path());
    let lock = radio_cli::process::MediaLock::acquire(&config.runtime_dir).unwrap();
    let busy = read_json(
        radio(dir.path())
            .args([
                "--json-v2",
                "play",
                station["data"]["stationuuid"].as_str().unwrap(),
            ])
            .output()
            .unwrap(),
        4,
    );
    assert_eq!(busy["error"]["code"], "media_busy");
    drop(lock);
    let missing_config = dir.path().join("missing-player.toml");
    fs::write(&missing_config, "mpv = '/does/not/exist'\n").unwrap();
    let dependency = read_json(
        radio(dir.path())
            .arg("--config")
            .arg(missing_config)
            .args([
                "--json-v2",
                "play",
                station["data"]["stationuuid"].as_str().unwrap(),
            ])
            .output()
            .unwrap(),
        3,
    );
    assert_eq!(dependency["error"]["code"], "missing_dependency");
}

#[test]
fn completions_and_manual_are_generated_without_preparing_data() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("uncreated");
    for (args, expected) in [
        (vec!["completions", "zsh"], "#compdef radio"),
        (vec!["completions", "bash"], "_radio()"),
        (vec!["completions", "fish"], "complete -c radio"),
        (vec!["man"], ".TH radio 1"),
    ] {
        let output = radio(&data).args(&args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let content = String::from_utf8(output.stdout).unwrap();
        assert!(
            content.contains(expected),
            "{}",
            &content[..content.len().min(300)]
        );
        assert!(content.contains("recordings"));
        assert!(!data.exists());
    }
    let v2 = radio(&data)
        .args(["--json-v2", "completions", "zsh"])
        .output()
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&v2.stdout).unwrap();
    assert!(value["data"].as_str().unwrap().contains("#compdef radio"));
    let english = radio(&data).args(["--lang", "en", "man"]).output().unwrap();
    let chinese_host = radio(&data)
        .env("LANG", "zh_CN.UTF-8")
        .env("LC_ALL", "zh_CN.UTF-8")
        .env("RADIO_LANG", "zh-CN")
        .args(["--lang", "en", "man"])
        .output()
        .unwrap();
    assert!(english.status.success());
    assert!(chinese_host.status.success());
    assert_eq!(english.stdout, chinese_host.stdout);
}

#[test]
fn completions_ignore_the_ambient_language() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("uncreated");
    for shell in ["bash", "zsh", "fish"] {
        let english = radio(&data).args(["completions", shell]).output().unwrap();
        let chinese_host = radio(&data)
            .env("LANG", "zh_CN.UTF-8")
            .env("LC_ALL", "zh_CN.UTF-8")
            .env("RADIO_LANG", "zh-CN")
            .args(["completions", shell])
            .output()
            .unwrap();
        assert!(english.status.success());
        assert!(chinese_host.status.success());
        assert_eq!(english.stdout, chinese_host.stdout, "{shell}");
    }
}

#[test]
fn release_script_pins_english_generated_files() {
    let script = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/package-release.sh"),
    )
    .unwrap();
    // Generated release files must not follow the packaging host language.
    assert!(
        script.contains(r#""$release_binary" --lang en man"#),
        "{script}"
    );
    assert!(
        script.contains(r#""$release_binary" --lang en completions "$release_shell""#),
        "{script}"
    );
}

#[test]
fn doctor_missing_dependencies_return_one_error_envelope_with_exit_three() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("doctor.toml");
    fs::write(
        &config,
        "mpv='/missing/mpv'\nffmpeg='/missing/ffmpeg'\nffprobe='/missing/ffprobe'\n",
    )
    .unwrap();
    let output = radio(dir.path())
        .arg("--config")
        .arg(config)
        .args(["--json-v2", "doctor"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["ok"], false);
    assert_eq!(result["error"]["code"], "missing_dependency");
    assert!(result.get("data").is_none());
}

#[test]
fn malformed_json_v2_flags_still_receive_usage_envelopes() {
    let dir = tempfile::tempdir().unwrap();
    for flag in ["--json-v2=true", "--json-v2=false", "--json-v2=invalid"] {
        let output = radio(dir.path()).args([flag, "search"]).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["error"]["code"], "usage_error");
    }
}

#[test]
fn operation_messages_follow_the_selected_cli_language() {
    let dir = tempfile::tempdir().unwrap();
    for (lang, expected) in [("en", "Recovery complete"), ("zh-CN", "恢复完成")] {
        let output = radio(dir.path())
            .args(["--lang", lang, "recordings", "recover"])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains(expected));
    }
}

#[test]
fn completed_recording_summaries_follow_cli_language() {
    let bytes = support::fixture("libmp3lame", "mp3");
    let server =
        support::Server::new(move |_, stream| support::response(stream, 200, "audio/mpeg", &bytes));
    for (language, expected) in [("en", "Recording complete:"), ("zh-CN", "录音 complete:")] {
        let dir = tempfile::tempdir().unwrap();
        let add = radio(dir.path())
            .args(["--json", "station", "add", "Summary", &server.url])
            .output()
            .unwrap();
        assert!(add.status.success());
        let station: Station = serde_json::from_slice(&add.stdout).unwrap();
        let output = radio(dir.path())
            .args([
                "--lang",
                language,
                "record",
                &station.stationuuid,
                "--duration",
                "1s",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains(expected));
    }
}

#[test]
fn help_about_uses_the_language_before_clap_exits() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("help.toml");
    fs::write(&config, "language = 'zh-CN'\n").unwrap();
    for (args, expected) in [
        (vec!["--lang", "en", "--help"], "Lightweight terminal radio"),
        (vec!["--lang=zh-CN", "--help"], "轻量终端收音机"),
        (vec!["--help", "--lang", "zh-CN"], "轻量终端收音机"),
        (vec!["--help", "--lang", "en"], "Lightweight terminal radio"),
        (vec!["--help"], "轻量终端收音机"),
    ] {
        let output = radio(dir.path())
            .env("RADIO_LANG", "")
            .env("LC_ALL", "C")
            .arg("--config")
            .arg(&config)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(expected),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
    assert!(!dir.path().join("radio.db").exists());
}

#[test]
fn doctor_space_detail_follows_the_cli_language() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("doctor.toml");
    fs::write(
        &config,
        "mpv='/missing/mpv'\nffmpeg='/missing/ffmpeg'\nffprobe='/missing/ffprobe'\n",
    )
    .unwrap();
    for (language, expected) in [("en", "bytes available"), ("zh-CN", "可用")] {
        let output = radio(dir.path())
            .arg("--config")
            .arg(&config)
            .args(["--lang", language, "--json", "doctor"])
            .output()
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let space = value["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|check| check["name"] == "recording_space")
            .unwrap();
        assert!(space["detail"].as_str().unwrap().contains(expected));
        assert!(space["available_bytes"].is_u64() && space["minimum_bytes"].is_u64());
    }
}

#[test]
fn play_and_random_progress_follow_the_cli_language() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("play.toml");
    let mpv = support::fake_mpv(dir.path());
    fs::write(&config, format!("mpv = {:?}\n", mpv.to_str().unwrap())).unwrap();
    let store = Store::open(&dir.path().join("radio.db")).unwrap();
    let station = Station {
        stationuuid: uuid::Uuid::new_v4().to_string(),
        name: "Progress".into(),
        url: "https://example.com/fail".into(),
        custom: true,
        active: true,
        ..Station::default()
    };
    store.upsert(&station).unwrap();
    for (language, expected, unexpected) in [
        ("en", "Playback failed:", "播放失败"),
        ("zh-CN", "播放失败:", "Playback failed:"),
    ] {
        for command in [vec!["play", station.stationuuid.as_str()], vec!["random"]] {
            let output = radio(dir.path())
                .arg("--config")
                .arg(&config)
                .args(["--lang", language])
                .args(command)
                .output()
                .unwrap();
            assert!(!output.status.success());
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains(expected), "{stderr}");
            assert!(!stderr.contains(unexpected), "{stderr}");
        }
    }
}
