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
    assert!(String::from_utf8_lossy(&result.stderr).contains("另一个 radio"));
}

#[cfg(target_os = "linux")]
#[test]
fn owned_children_exit_after_frontend_sigkill() {
    if let Some(path) = std::env::var_os("RADIO_TEST_CHILD_PID") {
        let mut command = Command::new("/bin/sleep");
        command.arg("60");
        let mut child = radio_cli::process::owned_child(&mut command)
            .spawn()
            .unwrap();
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
