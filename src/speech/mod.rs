//! Optional local speech recognition. Network installation is always explicit.
mod live;
mod models;
pub use live::{Controller, Segment, Snapshot};
pub use models::{Action, manage};
