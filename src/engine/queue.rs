//! Retain EOF events until their queue transactions commit off the media thread.
use crate::{
    podcast::{Episode, QueueAction},
    process::{Background, MediaLock},
    store::Store,
};
use anyhow::Result;
use std::{
    collections::VecDeque,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

struct Completion {
    id: String,
    generation: u64,
    advance: bool,
    retry_at: Instant,
    reported: bool,
}

pub(super) struct Finished {
    pub generation: u64,
    pub advance: bool,
    pub next: Option<Episode>,
}

#[derive(Default)]
pub(super) struct Completions {
    pending: VecDeque<Completion>,
    task: Option<Background<Option<Episode>>>,
}

impl Completions {
    pub fn push(&mut self, id: String, generation: u64, advance: bool) {
        self.pending.push_back(Completion {
            id,
            generation,
            advance,
            retry_at: Instant::now(),
            reported: false,
        });
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub fn active(&self, generation: u64) -> bool {
        self.pending.iter().any(|p| p.generation == generation)
    }

    pub fn poll(&mut self, path: &Path, lock: Arc<MediaLock>) -> Result<Option<Finished>> {
        if let Some(result) = self.task.as_ref().and_then(|task| task.poll()) {
            self.task = None;
            match result {
                Ok(next) => {
                    let completed = self.pending.pop_front().unwrap();
                    return Ok(Some(Finished {
                        generation: completed.generation,
                        advance: completed.advance,
                        next,
                    }));
                }
                Err(error) => {
                    let busy = error.downcast_ref::<rusqlite::Error>().is_some_and(|e| {
                        matches!(e, rusqlite::Error::SqliteFailure(code, _)
                            if matches!(code.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked))
                    });
                    let pending = self.pending.front_mut().unwrap();
                    pending.retry_at = Instant::now()
                        + if busy {
                            Duration::from_millis(50)
                        } else {
                            Duration::from_secs(5)
                        };
                    if !busy && !pending.reported {
                        pending.reported = true;
                        return Err(error);
                    }
                }
            }
        }
        if self.task.is_none()
            && let Some(pending) = self.pending.front()
            && Instant::now() >= pending.retry_at
        {
            let path = path.to_owned();
            let id = pending.id.clone();
            self.task = Some(Background::start("radio-podcast-complete", move |_| {
                let _lock = lock;
                let store = Store::open_writer(&path)?;
                Ok(store
                    .podcast_queue_update(QueueAction::Remove { id })?
                    .into_iter()
                    .next())
            })?);
        }
        Ok(None)
    }
}
