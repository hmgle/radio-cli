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
    pub volume: u8,
    pub recording_dir: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
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
            volume: 80,
            recording_dir: None,
        }
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
        if settings.volume > 100
            || settings.connect_timeout_secs == 0
            || settings.request_timeout_secs == 0
            || settings.playback_timeout_secs == 0
            || settings.connect_timeout_secs > 300
            || settings.request_timeout_secs > 300
            || settings.playback_timeout_secs > 300
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
