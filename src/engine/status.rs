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
            Self::Idle => "待机",
            Self::Connecting => "连接中",
            Self::Reconnecting { .. } => "重连中",
            Self::Playing => "播放中",
            Self::Paused => "暂停",
            Self::Failed { .. } => "播放失败",
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
            Self::Idle => "录音待机",
            Self::Probing => "检测编码",
            Self::Recording => "正在录音",
            Self::Finalizing => "录音收尾中",
            Self::Done => "录音已保存",
            Self::Failed { .. } => "录音失败",
        }
    }
}
