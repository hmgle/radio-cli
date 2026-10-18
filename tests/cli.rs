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
    assert!(
        radio(dir.path())
            .args(["station", "remove", id, "--keep-references"])
            .output()
            .unwrap()
            .status
            .success()
    );
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
            let size = libc::winsize {
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
                    std::ptr::null(),
                    &size
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
                before.c_lflag, after.c_lflag,
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
