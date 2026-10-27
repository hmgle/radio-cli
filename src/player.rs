use crate::error::{DomainError, ErrorCode};
use crate::{
    USER_AGENT,
    config::{Config, validate_url},
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    borrow::Cow,
    collections::HashMap,
    fs,
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex, OnceLock,
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
    Failed(DomainError),
    Paused(bool),
    Volume(u8),
    Muted(bool),
}

pub struct Player {
    child: Child,
    stream: UnixStream,
    socket: PathBuf,
    buffer: Vec<u8>,
    audio_device: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct AudioDevice {
    pub name: String,
    pub description: String,
}

fn parse_audio_devices(output: &str) -> Vec<AudioDevice> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim().strip_prefix('\'')?;
            let (name, description) = line.rsplit_once("' (")?;
            Some(AudioDevice {
                name: name.into(),
                description: description.strip_suffix(')')?.into(),
            })
        })
        .collect()
}

pub fn audio_devices(config: &Config, stop: &Arc<AtomicBool>) -> Result<Vec<AudioDevice>> {
    let output = crate::process::output(
        Command::new(&config.settings.mpv).args(["--no-config", "--audio-device=help"]),
        Duration::from_secs(3),
        stop,
    )?;
    let devices = parse_audio_devices(&String::from_utf8_lossy(&output));
    if devices.is_empty() {
        bail!(DomainError::new(ErrorCode::AudioDevicesUnavailable));
    }
    Ok(devices)
}

fn supports_normalization(config: &Config, stop: &Arc<AtomicBool>) -> Result<bool> {
    type Probe = Arc<Mutex<Option<bool>>>;
    static PROBES: OnceLock<Mutex<HashMap<String, Probe>>> = OnceLock::new();
    let probe = PROBES
        .get_or_init(Mutex::default)
        .lock()
        .unwrap()
        .entry(config.settings.mpv.clone())
        .or_default()
        .clone();
    // Serialize only probes for this executable, including concurrent starts.
    // Failed or cancelled subprocesses remain retryable on the next start.
    let mut cached = probe.lock().unwrap();
    if let Some(supported) = *cached {
        return Ok(supported);
    }
    let filters = crate::process::output(
        Command::new(&config.settings.mpv).args(["--no-config", "--af=help"]),
        Duration::from_secs(3),
        stop,
    )?;
    let supported = String::from_utf8_lossy(&filters)
        .lines()
        .any(|line| line.split_whitespace().next() == Some("dynaudnorm"));
    *cached = Some(supported);
    Ok(supported)
}

fn metadata_title(metadata: &Value) -> String {
    let Some(title) = metadata["icy-title"].as_str() else {
        return crate::clean_text(metadata["title"].as_str().unwrap_or(""));
    };
    icy_title(&repair_mpv_gbk_runs(title))
}

fn repair_mpv_gbk_runs(title: &str) -> String {
    let mut repaired = String::with_capacity(title.len());
    let mut start = 0;
    for (end, character) in title
        .char_indices()
        .chain(std::iter::once((title.len(), ' ')))
    {
        if character.is_ascii() || is_cjk(character) || !character.is_alphabetic() {
            let run = &title[start..end];
            // Some mpv builds turn invalid bytes into Latin-1 while retaining
            // valid UTF-8 subsequences (C5 B9 -> Ź inside GBK 张国荣).
            // Reconstruct that mixed run only when the normal conservative
            // GBK heuristic accepts it. Complete Unicode elsewhere is intact.
            if run.chars().any(|c| c > '\u{ff}') && run.chars().any(|c| c <= '\u{ff}') {
                let mut bytes = Vec::new();
                for c in run.chars() {
                    if c <= '\u{ff}' {
                        bytes.push(c as u8);
                    } else {
                        bytes.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
                    }
                }
                let latin1: String = bytes.iter().map(|&byte| char::from(byte)).collect();
                if let Cow::Owned(decoded) = repair_latin1_run(&latin1, false)
                    && decoded.chars().all(is_cjk)
                {
                    repaired.push_str(&decoded);
                } else {
                    repaired.push_str(run);
                }
            } else {
                repaired.push_str(run);
            }
            if end < title.len() {
                repaired.push(character);
                start = end + character.len_utf8();
            }
        }
    }
    repaired
}

fn parse_ipc_message(line: &[u8]) -> serde_json::Result<Value> {
    if let Ok(valid) = std::str::from_utf8(line) {
        return serde_json::from_str(valid);
    }
    // Repair whole non-ASCII runs before UTF-8 chunking: GBK bytes can contain
    // valid UTF-8 subsequences, which would otherwise hide recoverable bytes.
    // Do not sanitize JSON here; other fields must retain their Unicode.
    let decoded = decode_metadata_bytes(line);
    serde_json::from_str(&decoded)
}

fn push_metadata_bytes(bytes: &[u8], text: &mut String) {
    let mut offset = 0;
    for chunk in bytes.utf8_chunks() {
        text.push_str(chunk.valid());
        offset += chunk.valid().len() + chunk.invalid().len();
        // A lone three/four-byte UTF-8 lead after Han text and before ASCII
        // suggests a truncated character. Detect this in the raw bytes, so actual
        // Unicode accents such as "中文é?" remain intact.
        if matches!(chunk.invalid(), [0xe0..=0xf4])
            && chunk.valid().chars().next_back().is_some_and(is_cjk)
            && bytes.get(offset).is_none_or(u8::is_ascii)
        {
            text.push('\u{fffd}');
        } else {
            text.extend(chunk.invalid().iter().map(|&byte| char::from(byte)));
        }
    }
}

pub(crate) fn icy_title(title: &str) -> String {
    // Repair Latin-1 byte runs before sanitizing: C1 characters can be
    // UTF-8 continuation bytes. ASCII and existing Unicode delimit runs,
    // so GBK guesses cannot consume ASCII letters or span separate words.
    // Latin-1 and wrapped UTF-8 remain ambiguous: a genuine accent followed
    // by malformed continuation bytes can be reinterpreted as Han text.
    // A complete UTF-8 Han prefix is repaired even without Han elsewhere.
    let mut repaired = String::with_capacity(title.len());
    let utf8_han = title.chars().any(is_cjk);
    let mut start = 0;
    for (index, character) in title.char_indices() {
        if !matches!(character, '\u{80}'..='\u{ff}') {
            repaired.push_str(&repair_latin1_run(&title[start..index], utf8_han));
            repaired.push(character);
            start = index + character.len_utf8();
        }
    }
    repaired.push_str(&repair_latin1_run(&title[start..], utf8_han));
    crate::clean_text(&repaired)
}

pub(crate) fn icy_title_bytes(bytes: &[u8]) -> String {
    icy_title(&decode_metadata_bytes(bytes))
}

fn decode_metadata_bytes(bytes: &[u8]) -> String {
    // Decode each non-ASCII run independently. Some GBK bytes happen to form
    // short UTF-8 sequences, so try a complete run before the byte fallback.
    let mut title = String::with_capacity(bytes.len());
    let mut start = 0;
    for (index, &byte) in bytes.iter().enumerate() {
        if byte.is_ascii() {
            decode_icy_run(&bytes[start..index], &mut title);
            title.push(char::from(byte));
            start = index + 1;
        }
    }
    decode_icy_run(&bytes[start..], &mut title);
    title
}

fn decode_icy_run(bytes: &[u8], title: &mut String) {
    if let Ok(valid) = std::str::from_utf8(bytes) {
        title.push_str(valid);
        return;
    }
    // Existing UTF-8 Han text is stronger evidence than a GBK guess over
    // the entire run. Preserve it even when adjacent bytes are malformed.
    let has_utf8_han = bytes
        .utf8_chunks()
        .any(|chunk| chunk.valid().chars().any(is_cjk));
    if !has_utf8_han {
        let latin1: String = bytes.iter().map(|&byte| char::from(byte)).collect();
        if let Cow::Owned(repaired) = repair_latin1_run(&latin1, false) {
            title.push_str(&repaired);
            return;
        }
    }
    push_metadata_bytes(bytes, title);
}

fn repair_latin1_run(run: &str, utf8_han: bool) -> Cow<'_, str> {
    if run.is_empty() {
        return Cow::Borrowed(run);
    }
    let bytes: Vec<u8> = run.chars().map(|c| c as u8).collect();
    let error = match std::str::from_utf8(&bytes) {
        Ok(repaired) => return Cow::Owned(repaired.to_owned()),
        Err(error) => error,
    };
    let incomplete_prefix = error
        .error_len()
        .is_none()
        .then(|| std::str::from_utf8(&bytes[..error.valid_up_to()]).unwrap());
    // Prefer a complete UTF-8 Han prefix over a GBK guess, as decode_icy_run
    // does for valid Han islands. Mark only the incomplete trailing character;
    // this evidence comes from the run itself, independently of utf8_han.
    if let Some(prefix) = incomplete_prefix
        && prefix.chars().any(is_cjk)
    {
        return Cow::Owned(format!("{prefix}\u{fffd}"));
    }

    // GBK and genuine Latin-1 text overlap. Require a contiguous run of at
    // least two double-byte Han characters and some evidence beyond letters
    // (e.g. the symbols in "Íõ·Æ" or "ºÓÍ¼"). Letter-only runs such as "ää"
    // and "éééé" stay untouched. This remains a conservative heuristic, not
    // encoding detection; ambiguous titles may need a source-specific policy.
    if bytes.len() >= 4
        && run
            .chars()
            .any(|c| !c.is_alphabetic() && !c.is_control() && !c.is_whitespace())
        && let Some(decoded) =
            encoding_rs::GB18030.decode_without_bom_handling_and_without_replacement(&bytes)
        && decoded.chars().all(is_cjk)
        && decoded.chars().count() * 2 == bytes.len()
    {
        return Cow::Owned(decoded.into_owned());
    }

    // With Han elsewhere in the title, mark a UTF-8 lead and continuation
    // that remain incomplete after a lossless GBK repair was ruled out.
    // A lone byte is ambiguous with a genuine accent and stays unchanged.
    if let Some(prefix) = incomplete_prefix
        && utf8_han
        && bytes.len() - prefix.len() >= 2
    {
        return Cow::Owned(format!("{prefix}\u{fffd}"));
    }
    Cow::Borrowed(run)
}

fn is_cjk(character: char) -> bool {
    matches!(character, '\u{3400}'..='\u{9fff}' | '\u{f900}'..='\u{faff}')
}

/// `sockaddr_un.sun_path` holds 104 bytes on macOS (108 on Linux) including
/// the terminating NUL; stay at the lowest common denominator. The default
/// per-user data directory (`~/Library/Application Support` on macOS) plus a
/// full UUID socket name could exceed it, so the runtime-directory name keeps
/// only 12 hex characters; when the runtime directory runs deeper, fall back
/// to the system temp directory with the full UUID, which stays unguessable
/// in shared temp dirs. Do not lengthen the runtime-directory name again: the
/// default macOS path leaves room for little more than the truncation.
/// Session uniqueness comes from the random suffix; the application allows a
/// single media session per data directory anyway.
const SOCKET_PATH_LIMIT: usize = 100;

fn ipc_socket_path(runtime_dir: &Path) -> PathBuf {
    let id = uuid::Uuid::new_v4().simple().to_string();
    let primary = runtime_dir.join(format!("mpv-{}.sock", &id[..12]));
    if primary.as_os_str().len() <= SOCKET_PATH_LIMIT {
        return primary;
    }
    std::env::temp_dir().join(format!("mpv-{id}.sock"))
}

fn playback_command(config: &Config, socket: &Path) -> Command {
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
    if config.settings.normalize_loudness {
        command.arg("--af=dynaudnorm=f=150:g=5");
    }
    if let Some(device) = config
        .settings
        .audio_device
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        command.arg(format!("--audio-device={device}"));
    }
    command
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
        let socket = ipc_socket_path(&config.runtime_dir);
        if socket.as_os_str().len() > SOCKET_PATH_LIMIT {
            // The runtime directory and the system temp directory are both
            // too deep; surface the path so the JSON report shows the culprit.
            bail!(DomainError::new(ErrorCode::RuntimePathTooLong).with("path", socket.display()));
        }
        if config.settings.normalize_loudness && !supports_normalization(config, stop)? {
            bail!(DomainError::new(ErrorCode::NormalizationUnavailable));
        }
        let command = playback_command(config, &socket);
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
            audio_device: config.settings.audio_device.clone(),
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
        let mut closed = false;
        loop {
            match self.stream.read(&mut buf) {
                Ok(0) => {
                    closed = true;
                    break;
                }
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
            let Ok(value) = parse_ipc_message(&line) else {
                continue;
            };
            match value["event"].as_str().unwrap_or("") {
                "playback-restart" => out.push(Event::Playing),
                "end-file" => {
                    if value["file_error"]
                        .as_str()
                        .is_some_and(|error| error.contains("audio output"))
                        && let Some(device) = self
                            .audio_device
                            .as_deref()
                            .filter(|s| !s.trim().is_empty())
                    {
                        out.push(Event::Failed(
                            DomainError::new(ErrorCode::AudioDeviceFailed).with("device", device),
                        ));
                        continue;
                    }
                    out.push(Event::End(
                        value["reason"].as_str().unwrap_or("unknown").into(),
                    ));
                }
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
        // Deliver buffered end-file errors even when mpv closes IPC or exits
        // immediately afterward; otherwise their structured reason is lost.
        if out.is_empty() && closed {
            bail!(DomainError::new(ErrorCode::PlayerClosed));
        }
        if out.is_empty() && self.child.try_wait()?.is_some() {
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
mod socket_path_tests {
    use super::{SOCKET_PATH_LIMIT, ipc_socket_path};
    use std::path::Path;

    #[test]
    fn prefers_runtime_dir_with_a_short_name() {
        let dir = Path::new("/tmp/radio-test-run");
        let path = ipc_socket_path(dir);
        assert!(path.starts_with(dir));
        assert!(path.as_os_str().len() <= SOCKET_PATH_LIMIT);
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("mpv-") && name.ends_with(".sock"));
    }

    #[test]
    fn falls_back_to_temp_dir_for_deep_runtime_dirs() {
        let deep = std::env::temp_dir().join("a".repeat(160));
        let path = ipc_socket_path(&deep);
        // The function only guarantees the primary path fits; a fallback path
        // that is still too long is rejected by the caller's length check.
        assert!(path.starts_with(std::env::temp_dir()));
        assert!(!path.starts_with(&deep));
    }
}

#[cfg(test)]
mod tests {
    use super::{
        icy_title_bytes, metadata_title, parse_audio_devices, parse_ipc_message, playback_command,
    };
    use serde_json::json;

    #[test]
    fn device_names_preserve_backend_syntax_and_optional_arguments() {
        let parsed = parse_audio_devices(
            "List of detected audio devices:\n  'auto' (Autoselect device)\n  'alsa/plughw:CARD=USB,DEV=0' (Listener's USB (stereo))\nignored\n",
        );
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[1].name, "alsa/plughw:CARD=USB,DEV=0");
        assert_eq!(parsed[1].description, "Listener's USB (stereo)");
        let dir = tempfile::tempdir().unwrap();
        let mut config = crate::test_support::config(dir.path());
        let socket = std::path::Path::new("/tmp/radio.sock");
        let original: Vec<_> = playback_command(&config, socket)
            .get_args()
            .map(|a| a.to_owned())
            .collect();
        config.settings.audio_device = Some(String::new());
        assert_eq!(
            playback_command(&config, socket)
                .get_args()
                .collect::<Vec<_>>(),
            original
        );
        config.settings.audio_device = Some(parsed[1].name.clone());
        let command = playback_command(&config, socket);
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(&args[..original.len()], original);
        assert_eq!(
            args.last().unwrap(),
            &"--audio-device=alsa/plughw:CARD=USB,DEV=0"
        );
    }

    #[test]
    fn normalization_changes_only_the_opt_in_filter_argument() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = crate::test_support::config(dir.path());
        let socket = std::path::Path::new("/tmp/radio-test.sock");
        let command = playback_command(&config, socket);
        let original: Vec<_> = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            original,
            vec![
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
                "--demuxer-max-back-bytes=524288",
                "--demuxer-donate-buffer=no",
                "--audio-buffer=0.2",
                &format!("--user-agent={}", crate::USER_AGENT),
                "--volume=80",
                "--network-timeout=12",
                "--input-ipc-server=/tmp/radio-test.sock",
            ]
        );
        config.settings.normalize_loudness = true;
        let command = playback_command(&config, socket);
        let enabled: Vec<_> = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(&enabled[..original.len()], original);
        assert_eq!(&enabled[original.len()..], ["--af=dynaudnorm=f=150:g=5"]);
    }

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
    fn ipc_metadata_keeps_unicode_and_recovers_invalid_gbk_bytes() {
        let mut line =
            br#"{"event":"property-change","name":"metadata","data":{"icy-title":""#.to_vec();
        line.extend_from_slice(b"\xcd\xf5\xb7\xc6 - ");
        line.extend_from_slice("中文电台".as_bytes());
        line.extend_from_slice(br#"","title":"File tag"}}"#);
        let message = parse_ipc_message(&line).unwrap();
        assert_eq!(message["event"], "property-change");
        assert_eq!(metadata_title(&message["data"]), "王菲 - 中文电台");
        assert_eq!(message["data"]["title"], "File tag");
        assert!(parse_ipc_message(b"{\"data\":\"\xff\"").is_err());
    }

    #[test]
    fn ipc_metadata_repairs_gbk_runs_with_valid_utf8_subsequences() {
        for (raw, expected) in [
            (b"\xd5\xc5\xb9\xfa\xc8\xd9".as_slice(), "张国荣"),
            (b"\xba\xd3\xcd\xbc".as_slice(), "河图"),
        ] {
            let mut line = br#"{"icy-title":""#.to_vec();
            line.extend_from_slice(raw);
            line.extend_from_slice(r#" - Björk（中文）","title":"Café"}"#.as_bytes());
            let message = parse_ipc_message(&line).unwrap();
            assert_eq!(
                metadata_title(&message),
                format!("{expected} - Björk（中文）")
            );
            assert_eq!(message["title"], "Café");
        }
        // A valid accent alongside unrelated invalid bytes stays Unicode.
        let message = parse_ipc_message(b"{\"icy-title\":\"Caf\xc3\xa9\xff\"}").unwrap();
        assert_eq!(metadata_title(&message), "Caféÿ");

        // mpv versions which wrap only invalid bytes already send valid JSON.
        for (title, expected) in [("ÕŹúÈÙ", "张国荣"), ("ºÓͼ", "河图")] {
            assert_eq!(metadata_title(&json!({"icy-title": title})), expected);
        }
    }

    #[test]
    fn icy_titles_repair_gbk_segments_in_mixed_mojibake() {
        // AsiaFM sends GBK song text before UTF-8 station text. The final
        // metadata byte is already a literal question mark and cannot be
        // recovered from the stream, so only the recoverable part is repaired.
        let title = "\u{cd}\u{f5}\u{b7}\u{c6} -  (AsiaFM亚洲粤语\u{e5}\u{8f}?)";
        assert_eq!(
            metadata_title(&json!({"icy-title": title})),
            "王菲 -  (AsiaFM亚洲粤语�?)"
        );
        // A lone GBK pair decodes to a single character; guessing one
        // character from two ambiguous bytes is not worth the risk, so such
        // runs stay untouched. Rejecting a single-character guess is the safe
        // side: the raw bytes are shown instead of a wrong character.
        let lone_pair = "\u{b7}\u{c6} -  (AsiaFM亚洲粤语)";
        assert_eq!(
            metadata_title(&json!({"icy-title": lone_pair})),
            "\u{b7}\u{c6} -  (AsiaFM亚洲粤语)"
        );
        let another_lone_pair = "\u{cd}\u{f5} -  (AsiaFM亚洲粤语)";
        assert_eq!(
            metadata_title(&json!({"icy-title": another_lone_pair})),
            "\u{cd}\u{f5} -  (AsiaFM亚洲粤语)"
        );
        // Ordinary text and controls elsewhere must not veto a good segment.
        assert_eq!(
            metadata_title(&json!({"icy-title": "Íõ·Æ\n - Café（中文电台）"})),
            "王菲 - Café（中文电台）"
        );
    }

    #[test]
    fn icy_titles_repair_latin1_wrapped_gbk() {
        // A Zeno stream wraps GBK title bytes in Latin-1 and then UTF-8.
        let title = "ºÓÍ¼ - ×Ï´¨Â¼";
        assert_eq!(
            metadata_title(&json!({"icy-title": title})),
            "河图 - 紫川录"
        );
        // GBK can overlap with a valid UTF-8 character and a truncated prefix.
        let title = "Ã©æ± - 中文电台";
        assert_eq!(
            metadata_title(&json!({"icy-title": title})),
            "茅姹 - 中文电台"
        );
        assert_eq!(icy_title_bytes(title.as_bytes()), "茅姹 - 中文电台");
    }

    #[test]
    fn icy_titles_prefer_utf8_han_prefixes_over_gbk_without_external_han_context() {
        // UTF-8 for 王 followed by an incomplete E4 lead, wrapped in Latin-1.
        // The same bytes form two valid GBK Han characters (鐜嬩), so this
        // fixes the priority of the UTF-8 prefix rather than the GBK guess.
        let title = "ç\u{8e}\u{8b}ä";
        assert_eq!(metadata_title(&json!({"icy-title": title})), "王�");
        assert_eq!(icy_title_bytes(title.as_bytes()), "王�");
    }

    #[test]
    fn icy_titles_keep_text_without_a_lossless_latin1_repair() {
        for title in [
            "Radio Swiss Jazz",
            "张柏芝 - 乱了感觉",
            "Café à côté",
            "Björk - Jóga",
            "Motörhead - Ace of Spades",
            "Sigur Rós - Hoppípolla",
            "ΩΩ - ŹŹ - Ελλάδα - Łódź",
            "Chloéë",
            "Chloéë - 中文电台",
            "Café - 中文电台",
            "Björk - Jóga（中文）",
            "Café? - 中文电台",
            "中文é?",
            "Lääke - 中文电台",
            "Lääke - Jää（中文电台）",
            "éééé - 中文电台",
            "éé - àà（中文电台）",
            "Íõ· - 中文电台",
            "ÖÐÎÄ", // Also ambiguous with letter-only Latin-1 text.
            "â€™",
        ] {
            assert_eq!(metadata_title(&json!({"icy-title": title})), title);
            assert_eq!(icy_title_bytes(title.as_bytes()), title);
        }
        assert_eq!(metadata_title(&json!({"icy-title": "中文\x1b\n"})), "中文");
    }

    #[test]
    fn icy_titles_repair_utf8_runs_before_removing_c1_bytes() {
        for (title, expected) in [
            ("BeyoncÃ© - Café", "Beyoncé - Café"),
            ("CafÃ© — Live", "Café — Live"),
            (
                "é\u{82}\u{9d}ç¾\u{8e}äº\u{91} - 中文电台\x1b\n",
                "邝美云 - 中文电台",
            ),
        ] {
            assert_eq!(metadata_title(&json!({"icy-title": title})), expected);
        }
    }

    #[test]
    fn icy_title_bytes_preserve_utf8_islands_and_latin1_fallbacks() {
        let mut raw = b"\xcd\xf5\xb7\xc6 - ".to_vec();
        raw.extend_from_slice("中文电台".as_bytes());
        assert_eq!(icy_title_bytes(&raw), "王菲 - 中文电台");
        let mut adjacent = b"\xcd\xf5\xb7\xc6".to_vec();
        adjacent.extend_from_slice("中文".as_bytes());
        assert_eq!(icy_title_bytes(&adjacent), "王菲中文");
        assert_eq!(icy_title_bytes(b"Beyonc\xe9"), "Beyoncé");
        assert_eq!(
            icy_title_bytes(b"\xba\xd3\xcd\xbc - \xd7\xcf\xb4\xa8\xc2\xbc"),
            "河图 - 紫川录"
        );
        assert_eq!(icy_title_bytes(b"Track\x1b\n\xff"), "Trackÿ");
    }

    #[test]
    fn damaged_utf8_uses_replacement_characters_without_losing_gbk_or_accents() {
        let mut raw = "李克".as_bytes().to_vec();
        raw.extend_from_slice(b"\xe5\x8b?- ");
        raw.extend_from_slice("飞".as_bytes());
        raw.extend_from_slice(b"\xe8? (AsiaFM");
        raw.extend_from_slice("亚洲粤语".as_bytes());
        raw.extend_from_slice(b"\xe5\x8f?)");
        let expected = "李克�?- 飞�? (AsiaFM亚洲粤语�?)";
        assert_eq!(icy_title_bytes(&raw), expected);
        let mut line = br#"{"icy-title":""#.to_vec();
        line.extend_from_slice(&raw);
        line.extend_from_slice(r#"","title":"Björk - Jóga"}"#.as_bytes());
        let message = parse_ipc_message(&line).unwrap();
        assert_eq!(metadata_title(&message), expected);
        assert_eq!(message["title"], "Björk - Jóga");

        let mut adjacent = b"\xcd\xf5\xb7\xc6".to_vec();
        adjacent.extend_from_slice("中文".as_bytes());
        adjacent.extend_from_slice(b"\xe8?");
        assert_eq!(icy_title_bytes(&adjacent), "王菲中文�?");
        // A two-byte lead is ambiguous with Latin-1 and remains untouched.
        let mut two_byte_lead = "中".as_bytes().to_vec();
        two_byte_lead.extend_from_slice(b"\xc3?");
        assert_eq!(icy_title_bytes(&two_byte_lead), "中Ã?");
        assert_eq!(
            metadata_title(&json!({"icy-title": "Björk - Jóga（中文）å\u{8f}?"})),
            "Björk - Jóga（中文）�?"
        );
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
