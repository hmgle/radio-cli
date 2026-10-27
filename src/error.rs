use std::{collections::BTreeMap, fmt};

// Variants and their stable public codes are defined below.
macro_rules! codes {
    ($($variant:ident => ($code:literal, $exit:literal)),+ $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum ErrorCode { $($variant),+ }
        impl ErrorCode {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            pub fn exit_code(self) -> i32 { match self { $(Self::$variant => $exit),+ } }
            pub fn as_str(self) -> &'static str { match self { $(Self::$variant => $code),+ } }
        }
    }
}

codes! {
    SubtitleSettings => ("subtitle_settings", 1),
    SubtitleModel => ("subtitle_model", 1),
    SubtitleIntegrity => ("subtitle_integrity", 1),
    SubtitleRuntime => ("subtitle_runtime", 1),
    SubtitlePlatform => ("subtitle_platform", 1),
    SubtitleBusy => ("subtitle_busy", 1),
    SubtitleFailed => ("subtitle_failed", 1),
    UsageError => ("usage_error", 2),
    InvalidUrl => ("invalid_url", 1),
    MediaBusy => ("media_busy", 4),
    StationNotFound => ("station_not_found", 1),
    InvalidStation => ("invalid_station", 1),
    InvalidUuid => ("invalid_uuid", 2),
    DatabaseTooNew => ("database_too_new", 1),
    Cancelled => ("cancelled", 1),
    EmptyCatalog => ("empty_catalog", 1),
    NoDirectoryServers => ("no_directory_servers", 1),
    DirectoryRequest => ("directory_request", 1),
    DirectoryResponseTooLarge => ("directory_response_too_large", 1),
    InvalidPagination => ("invalid_pagination", 1),
    CatalogTooLarge => ("catalog_too_large", 1),
    CatalogShrink => ("catalog_shrink", 1),
    DirectorySync => ("directory_sync", 1),
    DataDirectory => ("data_directory", 1),
    InvalidConfig => ("invalid_config", 1),
    ReadConfig => ("read_config", 1),
    InvalidSettings => ("invalid_settings", 1),
    UnsupportedCodec => ("unsupported_codec", 1),
    FinalPathMissing => ("final_path_missing", 1),
    RecordingPublish => ("recording_publish", 1),
    RecordingSizeMismatch => ("recording_size_mismatch", 1),
    NoAudio => ("no_audio", 1),
    MissingDependency => ("missing_dependency", 3),
    InvalidDuration => ("invalid_duration", 2),
    DurationUnit => ("duration_unit", 2),
    DurationOverflow => ("duration_overflow", 2),
    DurationRange => ("duration_range", 2),
    NoStream => ("no_stream", 1),
    RandomDisabled => ("random_disabled", 1),
    NoMatchingStation => ("no_matching_station", 1),
    NotPlaying => ("not_playing", 1),
    WaitForPlayback => ("wait_for_playback", 1),
    RecordingBusy => ("recording_busy", 1),
    NoRecordingStream => ("no_recording_stream", 1),
    RecordingSourcesFailed => ("recording_sources_failed", 1),
    RuntimePathTooLong => ("runtime_path_too_long", 1),
    PlayerStartup => ("player_startup", 1),
    AudioDeviceFailed => ("audio_device_failed", 1),
    AudioDevicesUnavailable => ("audio_devices_unavailable", 1),
    NormalizationUnavailable => ("normalization_unavailable", 1),
    PlayerClosed => ("player_closed", 1),
    PlayerMessageTooLarge => ("player_message_too_large", 1),
    PlayerExited => ("player_exited", 1),
    ProcessTimeout => ("process_timeout", 1),
    ArtworkHandoffTimeout => ("artwork_handoff_timeout", 1),
    ProcessFailed => ("process_failed", 1),
    BackgroundFailed => ("background_failed", 1),
    EmptyStationName => ("empty_station_name", 2),
    DuplicateStation => ("duplicate_station", 1),
    NotCustom => ("not_custom", 1),
    RecordingNotFound => ("recording_not_found", 1),
    RecordingInUse => ("recording_in_use", 1),
    InsufficientSpace => ("insufficient_space", 1),
    LocalRecording => ("local_recording", 1),
    EphemeralRecording => ("ephemeral_recording", 1),
}

#[derive(Debug, Clone)]
pub struct DomainError {
    pub code: ErrorCode,
    pub params: BTreeMap<String, String>,
}

impl DomainError {
    pub fn new(code: ErrorCode) -> Self {
        Self {
            code,
            params: BTreeMap::new(),
        }
    }
    pub fn with(mut self, key: &str, value: impl ToString) -> Self {
        self.params.insert(key.into(), value.to_string());
        self
    }
}
impl fmt::Display for DomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let template = crate::messages::text(self.code.as_str());
        // Substitute once so user-provided values cannot become placeholders.
        let mut rest = template;
        while let Some(start) = rest.find('{') {
            f.write_str(&rest[..start])?;
            let Some(end) = rest[start..].find('}') else {
                return f.write_str(&rest[start..]);
            };
            let key = &rest[start + 1..start + end];
            if let Some(value) = self.params.get(key) {
                f.write_str(value)?;
            } else {
                write!(f, "{{{key}}}")?;
            }
            rest = &rest[start + end + 1..];
        }
        f.write_str(rest)
    }
}
impl std::error::Error for DomainError {}

pub fn exit_code(error: &anyhow::Error) -> i32 {
    error
        .downcast_ref::<DomainError>()
        .map_or(1, |e| e.code.exit_code())
}

pub fn json_error(error: &anyhow::Error) -> serde_json::Value {
    let domain = error.downcast_ref::<DomainError>();
    serde_json::json!({"ok":false,"error":{
        "code":domain.map(|e| e.code.as_str()).unwrap_or("general_error"),
        "message":crate::clean_text(&format!("{error:#}")),
        "params":domain.map(|e| e.params.clone()).unwrap_or_default()
    }})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn domain_exit_classes_are_explicit() {
        for (code, expected) in [
            (ErrorCode::UsageError, 2),
            (ErrorCode::InvalidDuration, 2),
            (ErrorCode::DurationRange, 2),
            (ErrorCode::MissingDependency, 3),
            (ErrorCode::MediaBusy, 4),
            (ErrorCode::InvalidSettings, 1),
            (ErrorCode::StationNotFound, 1),
        ] {
            assert_eq!(exit_code(&DomainError::new(code).into()), expected);
        }
        assert_eq!(exit_code(&anyhow::anyhow!("I/O")), 1);
    }

    #[test]
    fn runtime_path_too_long_reports_the_offending_path() {
        let error = DomainError::new(ErrorCode::RuntimePathTooLong)
            .with("path", "/deep/dir/run/mpv-abc.sock");
        assert!(error.to_string().contains("/deep/dir/run/mpv-abc.sock"));
    }
}
