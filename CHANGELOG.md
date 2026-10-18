# Changelog

## 0.3.0 — 2026-10-19

Complete roadmap stages S1–S3 (#36–#57); S4 (#58–#64) remains backlog.

- Fix whitespace search, favorite confirmations, full-catalog SQL random selection,
  atomic bootstrap, wide-character input cursors and source-specific hints.
- Introduce stable localized domain error codes and language selection. TUI static
  text and Clap help remain Chinese pending their separately scoped migration.
- Add transactional custom station editing, duplicate URL detection, backup URL
  management, remembered volume, opt-in resume, local filter facets, sorting,
  automatic stale-catalog refresh and one-action filter reset.
- Reconnect established streams with bounded retries while preserving history.
- Play and manage local recordings, confirm TUI deletion, open the recording
  directory, inspect full FFmpeg errors and refuse insufficient disk space.
- Add atomic `--json-v2` responses and exit codes; preserve legacy JSON shapes.
  Document the contract in `docs/json-contract.md`.
- Generate shell completions and `radio(1)`; include both in release archives.
- Improve 80×24 layouts; add default/mono/light themes and strict `NO_COLOR`.
  Show independent playback/recording stages and retain a visible exit hint.
- Share view snapshots, refresh the active source lazily, throttle progress,
  index history pruning and separate playback/recording resource ownership.
- Reuse Unicode editing in filters; add context completion, private persistent
  command history, command-specific help, runtime settings, mute and copying.
- Upgrade audited dependencies and check stable/all-features, Rust 1.88 and
  static x86_64 musl in CI. No advisory exceptions are configured.

### Compatibility

- Existing configuration and schema-v2 databases continue to work. New settings:
  `auto_sync_days = 7` (0 disables), `resume = false`, `probe_timeout_secs = 5`,
  `recording_min_free_mb = 16`, `theme = "default"`, `mouse = true`, and optional
  `language`. Volume is always remembered; resume is opt-in.
- `--json-v2` is opt-in; `search --json` remains an array throughout 0.3.x.
- Exit codes: 0 success, 1 operation failure, 2 usage, 3 missing executable,
  4 competing media session. Scripts that previously assumed every error was
  exit 1 should handle these specific codes.
- The supported release target is Linux x86_64. External mpv and FFmpeg/ffprobe
  remain required. NO_COLOR forces monochrome; clipboard copying uses OSC 52.

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
