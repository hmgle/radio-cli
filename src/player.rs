use crate::error::{DomainError, ErrorCode};
use crate::{
    USER_AGENT,
    config::{Config, validate_url},
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Debug)]
pub enum Event {
    Playing,
    Title(String),
    End(String),
    Paused(bool),
    Volume(u8),
}

pub struct Player {
    child: Child,
    stream: UnixStream,
    socket: PathBuf,
    buffer: Vec<u8>,
}

impl Player {
    pub fn start(config: &Config, url: &str, stop: &Arc<AtomicBool>) -> Result<Self> {
        validate_url(url)?;
        Self::start_input(config, url, stop)
    }

    pub fn start_local(
        config: &Config,
        path: &std::path::Path,
        stop: &Arc<AtomicBool>,
    ) -> Result<Self> {
        let path = fs::canonicalize(path)?;
        let url = reqwest::Url::from_file_path(path)
            .map_err(|_| DomainError::new(ErrorCode::InvalidUrl))?;
        Self::start_input(config, url.as_str(), stop)
    }

    fn start_input(config: &Config, url: &str, stop: &Arc<AtomicBool>) -> Result<Self> {
        let socket = config
            .runtime_dir
            .join(format!("mpv-{}.sock", uuid::Uuid::new_v4().simple()));
        if socket.as_os_str().len() > 100 {
            bail!(DomainError::new(ErrorCode::RuntimePathTooLong));
        }
        let mut command = Command::new(&config.settings.mpv);
        command
            .args([
                "--no-config",
                "--load-scripts=no",
                "--no-video",
                "--no-audio-display",
                "--terminal=no",
                "--input-default-bindings=no",
                "--idle=yes",
                "--keep-open=no",
                "--cache=yes",
                "--cache-secs=3",
                "--demuxer-max-bytes=2097152",
                "--demuxer-max-back-bytes=0",
                "--audio-buffer=0.2",
            ])
            .arg(format!("--user-agent={USER_AGENT}"))
            .arg(format!("--volume={}", config.settings.volume))
            .arg(format!(
                "--network-timeout={}",
                config.settings.playback_timeout_secs
            ))
            .arg(format!("--input-ipc-server={}", socket.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = crate::process::spawn_owned(command)
            .context(DomainError::new(ErrorCode::MissingDependency).with("program", "mpv"))?;
        let deadline = Instant::now() + Duration::from_secs(3);
        let stream = loop {
            if let Ok(stream) = UnixStream::connect(&socket) {
                break stream;
            }
            if child.try_wait()?.is_some()
                || Instant::now() >= deadline
                || stop.load(Ordering::Relaxed)
            {
                let _ = child.kill();
                let _ = child.wait();
                let _ = fs::remove_file(&socket);
                bail!(DomainError::new(ErrorCode::PlayerStartup));
            }
            thread::sleep(Duration::from_millis(20));
        };
        stream.set_read_timeout(Some(Duration::from_millis(1)))?;
        stream.set_write_timeout(Some(Duration::from_secs(1)))?;
        let mut player = Self {
            child,
            stream,
            socket,
            buffer: vec![],
        };
        for (id, property) in [(1, "metadata"), (2, "pause"), (3, "volume")] {
            player.command(json!(["observe_property", id, property]))?;
        }
        player.command(json!(["loadfile", url, "replace"]))?;
        Ok(player)
    }

    pub fn command(&mut self, command: Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(&json!({"command":command}))?;
        bytes.push(b'\n');
        self.stream.write_all(&bytes)?;
        Ok(())
    }

    pub fn toggle_pause(&mut self) -> Result<()> {
        self.command(json!(["cycle", "pause"]))
    }
    pub fn volume(&mut self, delta: i32) -> Result<()> {
        self.command(json!(["add", "volume", delta]))
    }

    pub fn poll(&mut self) -> Result<Vec<Event>> {
        let mut out = vec![];
        let mut buf = [0u8; 8192];
        loop {
            match self.stream.read(&mut buf) {
                Ok(0) => bail!(DomainError::new(ErrorCode::PlayerClosed)),
                Ok(n) => self.buffer.extend_from_slice(&buf[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break;
                }
                Err(e) => return Err(e.into()),
            }
            if self.buffer.len() > 1024 * 1024 {
                bail!(DomainError::new(ErrorCode::PlayerMessageTooLarge));
            }
            if self.buffer.len() >= 8192 {
                break;
            }
        }
        while let Some(end) = self.buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<_> = self.buffer.drain(..=end).collect();
            let Ok(value) = serde_json::from_slice::<Value>(&line) else {
                continue;
            };
            match value["event"].as_str().unwrap_or("") {
                "playback-restart" => out.push(Event::Playing),
                "end-file" => out.push(Event::End(
                    value["reason"].as_str().unwrap_or("unknown").into(),
                )),
                "property-change" => match value["name"].as_str().unwrap_or("") {
                    "metadata" => {
                        let data = &value["data"];
                        let title = data["icy-title"]
                            .as_str()
                            .or_else(|| data["title"].as_str())
                            .unwrap_or("");
                        out.push(Event::Title(crate::clean_text(title)));
                    }
                    "pause" => out.push(Event::Paused(value["data"].as_bool().unwrap_or(false))),
                    "volume" => out.push(Event::Volume(
                        value["data"].as_f64().unwrap_or(80.).clamp(0., 100.) as u8,
                    )),
                    _ => {}
                },
                _ => {}
            }
        }
        if self.child.try_wait()?.is_some() {
            bail!(DomainError::new(ErrorCode::PlayerExited));
        }
        Ok(out)
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.command(json!(["quit"]));
        let deadline = Instant::now() + Duration::from_millis(800);
        while Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_file(&self.socket);
    }
}
