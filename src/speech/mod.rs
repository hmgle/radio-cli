//! Optional local speech recognition. Network installation is always explicit.
mod chunks;
mod live;
mod models;
pub use live::{Controller, Segment, Snapshot};
pub(crate) use models::validate_model;
pub use models::{Action, manage};

pub(crate) use live::transcribe;

pub(crate) use chunks::{ChunkRecognizer, decode_chunk};
pub(crate) use models::check_available as check_transcription;
