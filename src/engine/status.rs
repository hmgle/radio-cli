#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum PlaybackState {
    #[default]
    Idle,
    Connecting,
    Reconnecting {
        attempt: usize,
    },
    Playing,
    Paused,
    Failed {
        reason: String,
    },
}
impl PlaybackState {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Idle => crate::t!("state_playback_idle"),
            Self::Connecting => crate::t!("state_playback_connecting"),
            Self::Reconnecting { .. } => crate::t!("state_playback_reconnecting"),
            Self::Playing => crate::t!("state_playback_playing"),
            Self::Paused => crate::t!("state_playback_paused"),
            Self::Failed { .. } => crate::t!("state_playback_failed"),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RecordingState {
    #[default]
    Idle,
    Probing,
    Recording,
    Finalizing,
    Done,
    Failed {
        reason: String,
    },
}
impl RecordingState {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Idle => crate::t!("state_recording_idle"),
            Self::Probing => crate::t!("state_recording_probing"),
            Self::Recording => crate::t!("state_recording_recording"),
            Self::Finalizing => crate::t!("state_recording_finalizing"),
            Self::Done => crate::t!("state_recording_done"),
            Self::Failed { .. } => crate::t!("state_recording_failed"),
        }
    }
}
