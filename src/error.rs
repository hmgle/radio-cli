use std::{collections::BTreeMap, fmt};

// Variants and their stable public codes are defined below.
macro_rules! codes {
    ($($variant:ident => $code:literal),+ $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum ErrorCode { $($variant),+ }
        impl ErrorCode {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            pub fn as_str(self) -> &'static str { match self { $(Self::$variant => $code),+ } }
        }
    }
}

codes! {
    InvalidUrl => "invalid_url",
    MediaBusy => "media_busy",
    StationNotFound => "station_not_found",
    InvalidStation => "invalid_station",
    InvalidUuid => "invalid_uuid",
    DatabaseTooNew => "database_too_new",
    Cancelled => "cancelled",
    EmptyCatalog => "empty_catalog",
    NoDirectoryServers => "no_directory_servers",
    DirectoryRequest => "directory_request",
    DirectoryResponseTooLarge => "directory_response_too_large",
    InvalidPagination => "invalid_pagination",
    CatalogTooLarge => "catalog_too_large",
    CatalogShrink => "catalog_shrink",
    DirectorySync => "directory_sync",
    DataDirectory => "data_directory",
    InvalidConfig => "invalid_config",
    ReadConfig => "read_config",
    InvalidSettings => "invalid_settings",
    UnsupportedCodec => "unsupported_codec",
    FinalPathMissing => "final_path_missing",
    RecordingPublish => "recording_publish",
    RecordingSizeMismatch => "recording_size_mismatch",
    NoAudio => "no_audio",
    MissingDependency => "missing_dependency",
    InvalidDuration => "invalid_duration",
    DurationUnit => "duration_unit",
    DurationOverflow => "duration_overflow",
    DurationRange => "duration_range",
    NoStream => "no_stream",
    RandomDisabled => "random_disabled",
    NoMatchingStation => "no_matching_station",
    NotPlaying => "not_playing",
    WaitForPlayback => "wait_for_playback",
    RecordingBusy => "recording_busy",
    NoRecordingStream => "no_recording_stream",
    RecordingSourcesFailed => "recording_sources_failed",
    RuntimePathTooLong => "runtime_path_too_long",
    PlayerStartup => "player_startup",
    PlayerClosed => "player_closed",
    PlayerMessageTooLarge => "player_message_too_large",
    PlayerExited => "player_exited",
    ProcessTimeout => "process_timeout",
    ProcessFailed => "process_failed",
    BackgroundFailed => "background_failed",
    EmptyStationName => "empty_station_name",
    DuplicateStation => "duplicate_station",
    NotCustom => "not_custom",
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
            }
            rest = &rest[start + end + 1..];
        }
        f.write_str(rest)
    }
}
impl std::error::Error for DomainError {}
