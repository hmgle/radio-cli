# Changelog

## 0.2.0 — 2026-10-18

### Fixes and improvements

- Keep Ctrl-W deletion on UTF-8 boundaries; honor Ctrl-C in every TUI mode and refresh terminal dimensions before interpreting keys.
- Apply rapid volume and favorite shortcuts to current state. Serialize favorite toggles across database connections.
- Retry queries after queue backpressure; refresh history, external database changes, and station details consistently.
- Add pagination and complete exports for favorites, history, and recordings. Keep recording order stable and prevent paging beyond known results.
- Preserve the latest recording progress and recover interrupted finalization without overwriting existing files.
- Isolate stale-recording recovery failures and add `recordings recover` for explicit retries.
- Keep playback controls responsive during network requests, probes, diagnostics, and recording finalization; bound shutdown waits and reclaim media subprocesses.
- Record the current TUI playback station with explicit behavior for pause, station changes, and shutdown.
- Share validated stream candidates between playback and standalone recording; fall back from invalid resolved URLs.
- Fetch each full directory from one server, restart failed snapshots on failover, and reject unexpected catalog shrinkage without repeatedly fetching replicas. Preserve diagnostics for all attempts.

### Compatibility

- `favorites list`, `history`, and `recordings list` JSON now returns `{items,total,limit,offset,has_more}`. With `--all`, `limit` is `null`. `search --json` keeps its array format.
- SQLite databases automatically migrate to schema v2. Explicit `recovery_pending` state replaces diagnostic-based retry selection; legacy pending recoveries are preserved. Recording JSON and sidecars include the new boolean field. Older binaries cannot open the migrated database.
- Catalog shrinkage greater than 25% requires `sync --accept-shrink`. A rejected snapshot preserves the existing catalog and stops the sync attempt.
- TUI recording belongs to the current playback session. Use `radio record UUID` for independent recording.
- Local finalization probing remains limited to 2 seconds, FFmpeg graceful stop to 3 seconds, and TUI finalization waiting to 6 seconds. Slow-disk failures preserve files and diagnostics.

### Distribution

The Linux x86_64 release archive contains a statically linked musl executable, documentation, and the MIT license. Playback and recording still require external `mpv` and `ffmpeg` / `ffprobe` executables. Checksums accompany the archive.

All issues #1–#13 are addressed. Detailed evidence and validation are recorded in [the issue report](docs/issues-2026-10-18.md) and [the review follow-up](docs/review-followup-2026-10-18.md).
