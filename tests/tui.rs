mod support;

use radio_cli::{
    model::{Filter, Station},
    store::Store,
};
use std::{
    fs,
    io::{Read, Write},
    os::fd::{AsRawFd, FromRawFd},
    process::{Child, Command},
    thread,
    time::{Duration, Instant},
};
use support::{Server, response};

struct Tui {
    child: Child,
    master: fs::File,
    slave: fs::File,
    original: libc::termios,
}
impl Tui {
    fn start(dir: &std::path::Path, api: &str) -> Self {
        Self::start_with_config(dir, api, None)
    }
    fn start_with_config(
        dir: &std::path::Path,
        api: &str,
        config: Option<&std::path::Path>,
    ) -> Self {
        Self::start_with_height(dir, api, config, 30)
    }
    fn start_with_height(
        dir: &std::path::Path,
        api: &str,
        config: Option<&std::path::Path>,
        height: u16,
    ) -> Self {
        let (master, slave, original) = unsafe {
            let (mut master, mut slave) = (0, 0);
            let mut size = libc::winsize {
                ws_row: height,
                ws_col: 120,
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
            let mut original = std::mem::zeroed();
            assert_eq!(libc::tcgetattr(slave, &mut original), 0);
            assert_ne!(libc::fcntl(master, libc::F_SETFL, libc::O_NONBLOCK), -1);
            (
                fs::File::from_raw_fd(master),
                fs::File::from_raw_fd(slave),
                original,
            )
        };
        let mut command = Command::new(env!("CARGO_BIN_EXE_radio"));
        let default_config = dir.join("tui-default.toml");
        if config.is_none() {
            fs::write(&default_config, "").unwrap();
        }
        command
            .arg("--config")
            .arg(config.unwrap_or(&default_config));
        let child = command
            .arg("--data-dir")
            .arg(dir)
            .args(["--api-url", api, "tui"])
            .env("TERM", "xterm-256color")
            .env_remove("NO_COLOR")
            .env_remove("TMUX")
            .stdin(slave.try_clone().unwrap())
            .stdout(slave.try_clone().unwrap())
            .stderr(slave.try_clone().unwrap())
            .spawn()
            .unwrap();
        Self {
            child,
            master,
            slave,
            original,
        }
    }
    fn resize(&mut self, width: u16, height: u16) {
        let size = libc::winsize {
            ws_row: height,
            ws_col: width,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        unsafe {
            assert_eq!(
                libc::ioctl(self.slave.as_raw_fd(), libc::TIOCSWINSZ, &size),
                0
            );
            assert_eq!(
                libc::kill(self.child.id() as libc::pid_t, libc::SIGWINCH),
                0
            );
        }
    }
    fn send(&mut self, text: &str) {
        self.master.write_all(text.as_bytes()).unwrap();
    }
    fn wait(&mut self, mut condition: impl FnMut() -> bool) {
        self.wait_output(|_| condition());
    }
    fn wait_output(&mut self, mut condition: impl FnMut(&[u8]) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = vec![];
        let mut buffer = [0; 16384];
        loop {
            while let Ok(n) = self.master.read(&mut buffer) {
                if n == 0 {
                    break;
                }
                output.extend_from_slice(&buffer[..n]);
            }
            if condition(&output) {
                return;
            }
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "TUI exited: {}",
                String::from_utf8_lossy(&output)
            );
            assert!(
                Instant::now() < deadline,
                "timed out: {}",
                String::from_utf8_lossy(&output)
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
    fn quit(&mut self) {
        self.send(":q\r");
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut buffer = [0; 8192];
        loop {
            while let Ok(n) = self.master.read(&mut buffer) {
                if n == 0 {
                    break;
                }
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(20));
        }
        unsafe {
            let mut restored = std::mem::zeroed::<libc::termios>();
            assert_eq!(libc::tcgetattr(self.slave.as_raw_fd(), &mut restored), 0);
            assert_eq!(
                support::terminal_local_flags(&restored),
                support::terminal_local_flags(&self.original)
            );
        }
    }
}
impl Drop for Tui {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

#[test]
fn artwork_capability_timeout_keeps_real_terminal_input_and_exit_working() {
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(32, 32)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let server = Server::new(move |_, stream| response(stream, 200, "image/png", png.get_ref()));
    let dir = tempfile::tempdir().unwrap();
    let config = support::config(dir.path());
    let store = Store::open(&config.data_dir.join("radio.db")).unwrap();
    store
        .upsert(&Station {
            stationuuid: uuid::Uuid::new_v4().to_string(),
            name: "Artwork input".into(),
            url: "http://localhost/live".into(),
            favicon: format!("{}/logo.png", server.url),
            active: true,
            ..Station::default()
        })
        .unwrap();
    let path = dir.path().join("artwork.toml");
    fs::write(&path, "artwork = 'station'\ntheme = 'default'\n").unwrap();
    let mut tui = Tui::start_with_height(dir.path(), "http://127.0.0.1:1", Some(&path), 60);
    // This PTY never replies to terminal queries. Once fallback has drawn the
    // interface, no query reader may compete for these input bytes.
    tui.wait_output(|output| {
        String::from_utf8_lossy(output).contains("Artwork input")
            && output.windows(4).any(|bytes| bytes == b"\x1b[5n")
    });
    tui.wait(|| dir.path().join("artwork").is_dir());
    tui.send(":station add 'Input after artwork probe' http://localhost/new\r");
    tui.wait(|| store.count().unwrap() == 2);
    tui.quit();
}

#[test]
fn artwork_probe_preserves_commands_sent_before_the_first_frame() {
    let dir = tempfile::tempdir().unwrap();
    let config = support::config(dir.path());
    let store = Store::open(&config.data_dir.join("radio.db")).unwrap();
    let path = dir.path().join("early-input.toml");
    fs::write(&path, "artwork = 'track'\ntheme = 'default'\n").unwrap();
    let mut tui = Tui::start_with_config(dir.path(), "http://127.0.0.1:1", Some(&path));
    // A query is emitted after raw mode starts and before the first frame.
    tui.wait_output(|output| output.windows(4).any(|bytes| bytes == b"\x1b[5n"));
    // Mouse/paste reports must not become shortcuts, and terminal replies
    // arriving after the command prefix must not become editor characters.
    tui.send("\x1b[<0;20;7M\x1b[200~\x1b[201~:station \x1b_Gi=31;OK\x1b\\\x1b[6;16;8t\x1b[0nadd '启动输入台标' http://localhost/early\r");
    tui.wait(|| store.count().unwrap() == 1);
    assert_eq!(
        store.search(&Filter::default()).unwrap()[0].name,
        "启动输入台标"
    );
    tui.quit();
}

#[test]
fn double_click_station_row_starts_playback_in_real_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let config = support::config(dir.path());
    let store = Store::open(&config.data_dir.join("radio.db")).unwrap();
    let station = Station {
        stationuuid: uuid::Uuid::new_v4().to_string(),
        name: "Mouse station".into(),
        url: "https://example.com/live".into(),
        active: true,
        custom: true,
        ..Station::default()
    };
    store.upsert(&station).unwrap();
    let mpv = support::fake_mpv(dir.path());
    let config_path = dir.path().join("config.toml");
    fs::write(
        &config_path,
        toml::to_string(&serde_json::json!({"mpv": mpv})).unwrap(),
    )
    .unwrap();
    let mut tui = Tui::start_with_config(dir.path(), "http://127.0.0.1:1", Some(&config_path));
    tui.wait_output(|output| output.windows(5).any(|s| s == b"Mouse"));
    // At 120x30, the first station row is at (19, 6), zero based.
    tui.send("\x1b[<0;20;7M\x1b[<0;20;7m\x1b[<0;20;7M\x1b[<0;20;7m");
    tui.wait(|| {
        store.meta("pref_last_station").unwrap().as_deref() == Some(station.stationuuid.as_str())
    });
    tui.quit();
}

#[test]
fn command_mode_sync_add_search_favorite_and_quit_work_in_real_terminal() {
    let remote = Station {
        stationuuid: uuid::Uuid::new_v4().to_string(),
        name: "Remote station".into(),
        active: true,
        ..Station::default()
    };
    let payload = serde_json::to_vec(std::slice::from_ref(&remote)).unwrap();
    let server = Server::new(move |_, stream| response(stream, 200, "application/json", &payload));
    let dir = tempfile::tempdir().unwrap();
    let config = support::config(dir.path());
    let store = Store::open(&config.data_dir.join("radio.db")).unwrap();
    store
        .upsert(&Station {
            stationuuid: uuid::Uuid::new_v4().to_string(),
            name: "Seed".into(),
            active: true,
            custom: true,
            ..Station::default()
        })
        .unwrap();
    let mut tui = Tui::start(dir.path(), &server.url);
    // The lock is created before raw mode; wait until input is actually ready.
    let terminal_fd = tui.slave.as_raw_fd();
    tui.wait(|| unsafe {
        let mut mode = std::mem::zeroed::<libc::termios>();
        libc::tcgetattr(terminal_fd, &mut mode) == 0 && mode.c_lflag & libc::ICANON == 0
    });
    tui.send(":sync\r");
    tui.wait(|| store.meta("last_sync").unwrap().is_some());
    assert_eq!(
        store.station(&remote.stationuuid).unwrap().name,
        "Remote station"
    );
    // q/r/n in quoted input must never trigger normal-mode quit/record/random keys.
    tui.send(":station add 'quoted q radio' https://example.com/live\r");
    let filter = Filter {
        query: "quoted q".into(),
        limit: 100,
        ..Filter::default()
    };
    tui.wait(|| !store.search(&filter).unwrap().is_empty());
    let id = store.search(&filter).unwrap()[0].stationuuid.clone();
    tui.send(":search 'quoted q'\r");
    // Selecting a UUID explicitly avoids depending on terminal repaint timing.
    tui.send(&format!(":favorites add {id}\r"));
    tui.wait(|| store.station(&id).unwrap().favorite);
    tui.send(":focus details\r");
    tui.send(":help\r");
    tui.send("?");
    tui.quit();
}

#[test]
fn direct_url_command_plays_and_inspects_without_saving_a_station() {
    let dir = tempfile::tempdir().unwrap();
    let config = support::config(dir.path());
    let store = Store::open(&config.data_dir.join("radio.db")).unwrap();
    let mpv = support::fake_mpv(dir.path());
    let config_path = dir.path().join("config.toml");
    fs::write(
        &config_path,
        toml::to_string(&serde_json::json!({"mpv": mpv})).unwrap(),
    )
    .unwrap();
    let mut tui = Tui::start_with_config(dir.path(), "http://127.0.0.1:1", Some(&config_path));
    tui.wait_output(|output| output.windows(6).any(|s| s == b"NORMAL"));
    tui.send(":play https://example.com/audition\r");
    let log = mpv.with_extension("commands");
    tui.wait(|| {
        fs::read_to_string(&log)
            .unwrap_or_default()
            .contains("/audition")
    });
    tui.send(":current\r");
    tui.wait_output(|output| String::from_utf8_lossy(output).contains("example.com/audition"));
    assert_eq!(store.count().unwrap(), 0);
    assert!(store.meta("pref_last_station").unwrap().is_none());
    tui.quit();
}

#[test]
fn shrinking_cancels_deletion_and_blocks_invisible_commands() {
    let dir = tempfile::tempdir().unwrap();
    let config = support::config(dir.path());
    let store = Store::open(&config.data_dir.join("radio.db")).unwrap();
    let station = Station {
        stationuuid: uuid::Uuid::new_v4().to_string(),
        name: "Keep this station".into(),
        url: "https://example.com/keep".into(),
        custom: true,
        active: true,
        ..Station::default()
    };
    store.upsert(&station).unwrap();
    let config_path = dir.path().join("config.toml");
    fs::write(&config_path, "language = \"en\"\nauto_sync_days = 0\n").unwrap();
    let mut tui = Tui::start_with_config(dir.path(), "http://127.0.0.1:1", Some(&config_path));
    tui.wait_output(|o| String::from_utf8_lossy(o).contains("NORMAL"));
    tui.send(&format!(":station remove {}\r", station.stationuuid));
    tui.wait_output(|o| String::from_utf8_lossy(o).contains("Confirm deletion"));
    tui.resize(39, 16);
    tui.wait_output(|o| String::from_utf8_lossy(o).contains("40×16"));
    tui.send("y:station add hidden https://example.com/hidden\r");
    // q is a FIFO barrier: all preceding invisible input is handled before exit.
    tui.send("q");
    tui.quit();
    assert_eq!(
        store.station(&station.stationuuid).unwrap().name,
        station.name
    );
    assert_eq!(store.count().unwrap(), 1);
}
