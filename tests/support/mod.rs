#![allow(dead_code)]
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

pub struct Server {
    pub url: String,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}
impl Server {
    pub fn new(handler: impl Fn(&str, &mut TcpStream) + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let handler = Arc::new(handler);
        let handle = thread::spawn(move || {
            let mut connections = vec![];
            while !worker_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let handler = handler.clone();
                        connections.push(thread::spawn(move || {
                            // Darwin inherits the listener's nonblocking flag;
                            // fixture handlers need blocking reads with timeouts.
                            stream.set_nonblocking(false).unwrap();
                            stream
                                .set_read_timeout(Some(Duration::from_secs(2)))
                                .unwrap();
                            stream
                                .set_write_timeout(Some(Duration::from_secs(2)))
                                .unwrap();
                            let mut request = Vec::new();
                            let mut buf = [0; 1024];
                            while request.len() < 8192 {
                                let Ok(n) = stream.read(&mut buf) else {
                                    return;
                                };
                                if n == 0 {
                                    return;
                                }
                                request.extend_from_slice(&buf[..n]);
                                if request.windows(4).any(|s| s == b"\r\n\r\n") {
                                    break;
                                }
                            }
                            handler(&String::from_utf8_lossy(&request), &mut stream);
                        }));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(_) => break,
                }
            }
            for handle in connections {
                let _ = handle.join();
            }
        });
        Self {
            url,
            stop,
            handle: Some(handle),
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
pub fn response(stream: &mut TcpStream, status: u16, content_type: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    if stream.write_all(head.as_bytes()).is_ok() {
        let _ = stream.write_all(body);
    }
}
pub fn config(dir: &std::path::Path) -> radio_cli::config::Config {
    let path = dir.join("test-default.toml");
    std::fs::write(&path, "").unwrap();
    let config =
        radio_cli::config::Config::load(Some(&path), Some(dir), Some("http://127.0.0.1:1"))
            .unwrap();
    config.prepare().unwrap();
    config
}

pub fn terminal_local_flags(termios: &libc::termios) -> libc::tcflag_t {
    // macOS can set PENDIN while restoring canonical input. It describes
    // pending input to retype, rather than an application terminal setting.
    let state = if cfg!(target_os = "macos") {
        libc::PENDIN
    } else {
        0
    };
    termios.c_lflag & !state
}

pub fn fixture(codec: &str, format: &str) -> Vec<u8> {
    let result = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=4",
            "-c:a",
            codec,
            "-f",
            format,
            "pipe:1",
        ])
        .output()
        .expect("FFmpeg required");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    result.stdout
}

pub fn fake_mpv(dir: &std::path::Path) -> std::path::PathBuf {
    let source = dir.join("fake-mpv.rs");
    std::fs::write(&source, include_str!("fake_mpv.rs")).unwrap();
    let binary = dir.join("fake-mpv");
    let output = std::process::Command::new("rustc")
        .arg("--edition=2024")
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    binary
}
