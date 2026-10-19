use crate::error::{DomainError, ErrorCode};
use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub keybindings: Keybindings,
    pub mouse: bool,
    pub theme: Theme,
    pub recording_min_free_mb: u64,
    pub auto_sync_days: u16,
    pub resume: bool,
    pub language: Option<String>,
    pub api_url: Option<String>,
    pub mpv: String,
    pub ffmpeg: String,
    pub ffprobe: String,
    pub connect_timeout_secs: u64,
    pub request_timeout_secs: u64,
    pub playback_timeout_secs: u64,
    pub probe_timeout_secs: u64,
    pub volume: u8,
    pub recording_dir: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            keybindings: Keybindings::default(),
            mouse: true,
            theme: Theme::Default,
            recording_min_free_mb: 16,
            auto_sync_days: 7,
            resume: false,
            language: None,
            api_url: None,
            mpv: "mpv".into(),
            ffmpeg: "ffmpeg".into(),
            ffprobe: "ffprobe".into(),
            connect_timeout_secs: 5,
            request_timeout_secs: 20,
            playback_timeout_secs: 12,
            probe_timeout_secs: 5,
            volume: 80,
            recording_dir: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Keybindings {
    pub jump_to_playing: String,
}
impl Default for Keybindings {
    fn default() -> Self {
        Self {
            jump_to_playing: "''".into(),
        }
    }
}

/// Printable sequences (up to four keys), or a named key with optional modifiers.
pub(crate) fn parse_keybinding(
    value: &str,
) -> Option<Vec<(crossterm::event::KeyCode, crossterm::event::KeyModifiers)>> {
    use crossterm::event::{KeyCode, KeyModifiers};
    if let Some(named) = value.strip_prefix('<').and_then(|v| v.strip_suffix('>')) {
        let mut parts = named.split('-');
        let mut modifiers = KeyModifiers::NONE;
        while parts.clone().count() > 1 {
            modifiers |= match parts.next()?.to_ascii_lowercase().as_str() {
                "ctrl" => KeyModifiers::CONTROL,
                "alt" => KeyModifiers::ALT,
                "shift" => KeyModifiers::SHIFT,
                _ => return None,
            };
        }
        let name = parts.next()?;
        let code = match name.to_ascii_lowercase().as_str() {
            "space" => KeyCode::Char(' '),
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "enter" => KeyCode::Enter,
            "tab" => KeyCode::Tab,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            s if s.starts_with('f') && s.len() > 1 => {
                let number = s[1..].parse::<u8>().ok()?;
                if !(1..=12).contains(&number) {
                    return None;
                }
                KeyCode::F(number)
            }
            _ if name.chars().count() == 1 => KeyCode::Char(name.chars().next()?),
            _ => return None,
        };
        if matches!(code, KeyCode::Char('c' | 'C')) && modifiers.contains(KeyModifiers::CONTROL) {
            return None;
        }
        if code == KeyCode::Tab && modifiers.contains(KeyModifiers::SHIFT) {
            modifiers.remove(KeyModifiers::SHIFT);
            return Some(vec![(KeyCode::BackTab, modifiers)]);
        }
        if let KeyCode::Char(c) = code {
            let c = if modifiers.contains(KeyModifiers::SHIFT) {
                c.to_ascii_uppercase()
            } else if modifiers.contains(KeyModifiers::CONTROL) {
                c.to_ascii_lowercase()
            } else {
                c
            };
            modifiers.remove(KeyModifiers::SHIFT);
            return Some(vec![(KeyCode::Char(c), modifiers)]);
        }
        Some(vec![(code, modifiers)])
    } else if (1..=4).contains(&value.chars().count())
        && value.chars().all(|c| !c.is_control() && !c.is_whitespace())
    {
        Some(
            value
                .chars()
                .map(|c| (KeyCode::Char(c), KeyModifiers::NONE))
                .collect(),
        )
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Default,
    Mono,
    Light,
}

impl Theme {
    pub fn effective(self, no_color: bool) -> Self {
        if no_color { Self::Mono } else { self }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub settings: Settings,
    pub config_path: PathBuf,
    pub data_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub recording_dir: PathBuf,
}

impl Config {
    pub fn load(
        config_path: Option<&Path>,
        data_dir: Option<&Path>,
        api: Option<&str>,
    ) -> Result<Self> {
        let dirs = ProjectDirs::from("", "", "radio-cli")
            .context(DomainError::new(ErrorCode::DataDirectory))?;
        let path = config_path
            .map(PathBuf::from)
            .unwrap_or_else(|| dirs.config_dir().join("config.toml"));
        let mut settings: Settings = match fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).with_context(|| {
                DomainError::new(ErrorCode::InvalidConfig).with("path", path.display())
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && config_path.is_none() => {
                Settings::default()
            }
            Err(e) => {
                return Err(e).with_context(|| {
                    DomainError::new(ErrorCode::ReadConfig).with("path", path.display())
                });
            }
        };
        if let Some(api) = api {
            settings.api_url = Some(api.to_owned());
        }
        if parse_keybinding(&settings.keybindings.jump_to_playing).is_none()
            || settings.recording_min_free_mb == 0
            || settings
                .recording_min_free_mb
                .checked_mul(1024 * 1024)
                .is_none()
            || settings.volume > 100
            || settings.connect_timeout_secs == 0
            || settings.request_timeout_secs == 0
            || settings.playback_timeout_secs == 0
            || settings.probe_timeout_secs == 0
            || settings.connect_timeout_secs > 300
            || settings.request_timeout_secs > 300
            || settings.playback_timeout_secs > 300
            || settings.probe_timeout_secs > 300
        {
            bail!(DomainError::new(ErrorCode::InvalidSettings));
        }
        if let Some(api) = &settings.api_url {
            validate_url(api)?;
        }
        let data_dir = data_dir
            .map(PathBuf::from)
            .unwrap_or_else(|| dirs.data_dir().to_path_buf());
        let data_dir = std::path::absolute(data_dir)?;
        let recording_dir = settings
            .recording_dir
            .clone()
            .unwrap_or_else(|| data_dir.join("recordings"));
        let recording_dir = std::path::absolute(recording_dir)?;
        let runtime_dir = data_dir.join("run");
        Ok(Self {
            settings,
            config_path: path,
            data_dir,
            runtime_dir,
            recording_dir,
        })
    }

    pub fn prepare(&self) -> Result<()> {
        fs::create_dir_all(&self.data_dir)?;
        fs::create_dir_all(&self.runtime_dir)?;
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&self.runtime_dir, fs::Permissions::from_mode(0o700))?;
        Ok(())
    }
}

pub fn validate_url(value: &str) -> Result<()> {
    let url = reqwest::Url::parse(value).context(DomainError::new(ErrorCode::InvalidUrl))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        bail!(DomainError::new(ErrorCode::InvalidUrl));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jump_bindings_accept_sequences_and_named_keys_but_reserve_exit() {
        use crossterm::event::{KeyCode, KeyModifiers};
        for binding in [
            "''",
            "gp",
            "<F6>",
            "<Ctrl-p>",
            "<Alt-p>",
            "<Shift-P>",
            "<Space>",
        ] {
            assert!(parse_keybinding(binding).is_some(), "{binding}");
        }
        for binding in [
            "",
            "abcde",
            "a b",
            "\n",
            "<F13>",
            "<Hyper-p>",
            "<Ctrl-c>",
            "<Ctrl-C>",
        ] {
            assert!(parse_keybinding(binding).is_none(), "{binding}");
        }
        assert_eq!(
            parse_keybinding("<Ctrl-p>"),
            Some(vec![(KeyCode::Char('p'), KeyModifiers::CONTROL)])
        );
        assert_eq!(parse_keybinding("<Ctrl-P>"), parse_keybinding("<Ctrl-p>"));
        assert_eq!(
            parse_keybinding("<Shift-Tab>"),
            Some(vec![(KeyCode::BackTab, KeyModifiers::NONE)])
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.toml");
        fs::write(&path, "[keybindings]\njump_to_playing = '<F6>'\n").unwrap();
        assert_eq!(
            Config::load(Some(&path), Some(dir.path()), None)
                .unwrap()
                .settings
                .keybindings
                .jump_to_playing,
            "<F6>"
        );
        fs::write(&path, "[keybindings]\njump_to_playing = '<Ctrl-c>'\n").unwrap();
        assert!(Config::load(Some(&path), Some(dir.path()), None).is_err());
    }

    #[test]
    fn recording_reserve_rejects_zero_and_byte_overflow() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        for value in [0_u64, 1_u64 << 44] {
            fs::write(&path, format!("recording_min_free_mb = {value}")).unwrap();
            let error = Config::load(Some(&path), Some(dir.path()), None).unwrap_err();
            assert_eq!(
                error.downcast_ref::<DomainError>().unwrap().code,
                ErrorCode::InvalidSettings
            );
        }
        fs::write(&path, "recording_min_free_mb = 1").unwrap();
        assert_eq!(
            Config::load(Some(&path), Some(dir.path()), None)
                .unwrap()
                .settings
                .recording_min_free_mb,
            1
        );
    }
}
