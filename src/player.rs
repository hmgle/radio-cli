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
    Muted(bool),
}

pub struct Player {
    child: Child,
    stream: UnixStream,
    socket: PathBuf,
    buffer: Vec<u8>,
}

fn metadata_title(metadata: &Value) -> String {
    let Some(title) = metadata["icy-title"].as_str() else {
        return crate::clean_text(metadata["title"].as_str().unwrap_or(""));
    };
    // Some ICY sources re-encode UTF-8 as Latin-1. Reverse this only when
    // the entire title maps to bytes that form valid UTF-8. This heuristic
    // deliberately leaves mixed encodings and ordinary file tags alone.
    // Repair before clean_text removes C1 characters needed as UTF-8 bytes.
    if !title.is_ascii() && title.chars().all(|c| c <= '\u{ff}') {
        let bytes = title.chars().map(|c| c as u8).collect();
        if let Ok(repaired) = String::from_utf8(bytes) {
            return crate::clean_text(&repaired);
        }
    }
    crate::clean_text(title)
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
                // Keep recent packets until playback observes timed ICY metadata.
                // Disabling the back buffer can lose one-shot title updates.
                "--demuxer-max-back-bytes=524288",
                "--demuxer-donate-buffer=no",
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
        for (id, property) in [(1, "metadata"), (2, "pause"), (3, "volume"), (4, "mute")] {
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
    pub fn toggle_mute(&mut self) -> Result<()> {
        self.command(json!(["cycle", "mute"]))
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
                        out.push(Event::Title(metadata_title(&value["data"])));
                    }
                    "pause" => out.push(Event::Paused(value["data"].as_bool().unwrap_or(false))),
                    "mute" => out.push(Event::Muted(value["data"].as_bool().unwrap_or(false))),
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

#[cfg(test)]
mod tests {
    use super::metadata_title;
    use serde_json::json;

    #[test]
    fn icy_titles_repair_latin1_mojibake_before_sanitizing() {
        // UTF-8 bytes for "邝美云 - 雪花" were decoded as Latin-1 and then
        // encoded as UTF-8. The C1 characters must survive until repaired.
        let title = "é\u{82}\u{9d}ç¾\u{8e}äº\u{91} - é\u{9b}ªè\u{8a}±\x1b\n";
        assert_eq!(
            metadata_title(&json!({"icy-title": title})),
            "邝美云 - 雪花"
        );
    }

    #[test]
    fn icy_titles_keep_text_without_a_lossless_latin1_repair() {
        for title in [
            "Radio Swiss Jazz",
            "张柏芝 - 乱了感觉",
            "Café à côté",
            "BeyoncÃ© - Café",
            "CafÃ© — Live",
            "â€™",
        ] {
            assert_eq!(metadata_title(&json!({"icy-title": title})), title);
        }
        assert_eq!(metadata_title(&json!({"icy-title": "中文\x1b\n"})), "中文");
    }

    #[test]
    fn metadata_titles_preserve_file_tags_and_explicit_empty_icy_titles() {
        // A literal Latin-1-looking sequence in a file tag is not ICY data.
        assert_eq!(metadata_title(&json!({"title": "Â©\x1b\n"})), "Â©");
        assert_eq!(
            metadata_title(&json!({"icy-title": "", "title": "File title"})),
            ""
        );
        assert_eq!(
            metadata_title(&json!({"icy-title": 1, "title": "File title"})),
            "File title"
        );
        assert_eq!(metadata_title(&json!(null)), "");
    }

    #[test]
    fn icy_titles_are_limited_after_repair() {
        let title = "ä¸\u{ad}".repeat(1100);
        assert_eq!(
            metadata_title(&json!({"icy-title": title})),
            "中".repeat(1024)
        );
    }
}
