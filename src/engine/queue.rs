//! Retain EOF events until their queue transactions commit off the media thread.
use crate::{
    podcast::Episode,
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
    removed: bool,
    earlier: Vec<String>,
}

pub(super) struct Finished {
    pub generation: u64,
    pub advance: bool,
    pub next: Option<Episode>,
}

#[derive(Default)]
pub(super) struct Completions {
    pending: VecDeque<Completion>,
    task: Option<Background<Candidate>>,
}
type Candidate = (Option<Episode>, u64, Vec<String>);

impl Completions {
    pub fn push(&mut self, id: String, generation: u64, advance: bool) {
        self.pending.push_back(Completion {
            id,
            generation,
            advance,
            retry_at: Instant::now(),
            reported: false,
            removed: false,
            earlier: vec![],
        });
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub fn active(&self, generation: u64) -> bool {
        self.pending.iter().any(|p| p.generation == generation)
    }

    pub fn poll(
        &mut self,
        path: &Path,
        lock: Arc<MediaLock>,
        store: &Store,
    ) -> Result<Option<Finished>> {
        if let Some(result) = self.task.as_ref().and_then(|task| task.poll()) {
            self.task = None;
            match result {
                Ok((next, revision, earlier)) => {
                    let pending = self.pending.front_mut().unwrap();
                    pending.removed = true;
                    pending.earlier = earlier;
                    // Queue edits can commit after the completion task finishes.
                    // Refresh only the candidate; removal must not run twice.
                    if store.podcast_queue_revision()? == revision {
                        let completed = self.pending.pop_front().unwrap();
                        return Ok(Some(Finished {
                            generation: completed.generation,
                            advance: completed.advance,
                            next,
                        }));
                    }
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
            let removed = pending.removed;
            let earlier = pending.earlier.clone();
            self.task = Some(Background::start("radio-podcast-complete", move |_| {
                let _lock = lock;
                let store = Store::open_writer(&path)?;
                let (items, revision, earlier) = if removed {
                    let (items, revision) = store.podcast_queue_snapshot()?;
                    (items, revision, earlier)
                } else {
                    store.podcast_queue_complete(&id)?
                };
                Ok((
                    items
                        .into_iter()
                        .find(|e| e.id != id && !earlier.contains(&e.id)),
                    revision,
                    earlier,
                ))
            })?);
        }
        Ok(None)
    }
}
