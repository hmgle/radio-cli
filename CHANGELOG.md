# Changelog

## Unreleased

- Keep TUI shortcut key highlighting consistent in filter and confirmation
  dialogs and command/search hints, and clarify the English panel and search
  labels. Show the alternative detail-scrolling keys with a slash separator.

## 0.8.2 — 2026-10-23

- Highlight literal keys in the TUI's bottom shortcut hints and make the
  separate `[` and `]` source-switching keys visually distinct from the slash
  separator.
- Move detailed installation instructions into paired English and Chinese
  guides and keep the README quick start short.

## 0.8.1 — 2026-10-23

- Distribute the binary through npm as `@hmgle/radio-cli`, with per-platform
  packages that carry the prebuilt binary, published from GitHub Actions with
  trusted publishing and provenance. Install with `npm install -g
  @hmgle/radio-cli`.
- Publish the crate to crates.io from a GitHub Actions workflow through trusted
  publishing, so no long-lived API token is stored, and install the binary with
  `cargo install radio-cli --locked`.
- Show the `F` filters shortcut in the bottom shortcut row beside the station
  panels.
- Package only build inputs and user-facing files, keeping internal design
  notes, packaging scripts, and test fixtures out of the published crate.

## 0.8.0 — 2026-10-22

- Reorganize the public documentation into synchronized English and Chinese
  entry points, add detailed configuration, recording, troubleshooting, and JSON
  references, and remove version-pinned release download examples.
- Keep list rows and the current selection visible during background refreshes
  triggered by playback confirmation, including while navigating with j/k.
- Make Ctrl-e/Ctrl-y scroll the list viewport by one line, preserving selection
  unless the selected row would leave the screen, as in Vim.
- Keep small terminals usable, isolate deletion confirmations, and make list
  query failures visible with bounded retries and stale-result protection.
- Edit and measure complete Unicode grapheme clusters, handle bracketed paste,
  and ignore repeat events for one-shot actions when the terminal identifies them.
- Unify playback markers and recording timers, and allow locating the last
  requested station after playback fails without replaying it.
- Build localized keyboard help from structured sections, give diagnostic
  popups specific titles, and scroll long content through its measured extent.
- Expire ordinary notices, name copy and favorite targets, restore cancelled
  search positions, and remember each source's page and row for the session.
- Group and align station details, identify their source, show backup lookup
  states, and reuse wrapped content when calculating artwork space.
- Exclude commands containing URL credentials from command history with a
  notice. Redact credentials in TUI diagnostics and explain OSC 52 truncation.
- Anchor scrolled task logs, count new messages, and add H/M/L and zt/zz/zb
  viewport navigation with a one-second sequence timeout.
- Resolve interface theme styles before painting widgets, preserve artwork
  colors in light mode, and keep monochrome terminal output free of SGR.
- Build release binaries for size with opt-level s and fat LTO. The static
  Linux executable is about a third smaller, while a full catalog sync runs
  about ten percent slower.

## 0.7.0 — 2026-10-21

- Cache distinct station facets and index normalized stream URLs, removing
  catalog-wide token splitting and JSON decoding from interactive queries.
- Use FTS5 trigram candidates for selective substring searches, with exact
  literal checks and scan paths for short, special, or broad queries. Keep
  Chinese territory matching and existing sorting and pagination behavior.
- Run TUI catalog queries on a persistent reader thread so slow searches do
  not delay media controls. Coalesce requests, discard stale results, cache
  exact totals across pages, and interrupt queries during shutdown.
- Locate stations through indexed sort ranges and reduce text copying when
  publishing TUI logs and rendering station rows.
- Collect search-planning statistics after catalog publication without holding
  the main database write lock, then replace the hints in a short transaction.

### Compatibility

- Existing databases migrate atomically to schema v5 on first open. This
  builds search indexes and facet caches without requiring a catalog sync;
  first open takes longer and the database uses additional disk space.
  Older binaries cannot open the upgraded database; keep a pre-upgrade copy
  if downgrade support is needed. CLI options and JSON output are unchanged.
- For scale, the deterministic 200,000-station fixture occupies about 243 MiB
  with schema v5 (about 144 MiB for the equivalent v2 fixture). Its first-open
  v2-to-v5 migration took about 5.3 seconds on the development machine. The name
  and bitrate indexes together occupy about 32 MiB; they speed up those optional
  sorting modes.
  These are fixture measurements, not a fixed overhead: station text, catalog
  contents, SQLite free pages, and hardware affect disk use and migration time.

## 0.6.1 — 2026-10-20

- Display country/region names in Chinese in the TUI using a pinned Unicode
  CLDR 48 name table keyed by the upstream `countrycode`. Details, the filter
  popup and its candidates show localized names while storage, JSON output and
  `--country` keep the upstream codes, so existing catalogs need no resync.
- Match Chinese country/region names in search without a database migration.
  Queries of two or more characters resolve to territory codes at query time;
  single-character queries keep matching station text only, because one
  character is shared by too many territories.
- Fall back to the upstream catalog name, then the code, then unknown when a
  country code is missing or absent from the CLDR table, so details no longer
  discard usable catalog metadata.
- Render the catalog source notice in the active interface language instead of
  always in Chinese, and include the Unicode data license in release archives.
- Label the `state` field as state/province/area and clarify third-party
  catalog provenance and local caching in the guides.

### Compatibility

- Existing configuration and schema-v2 databases remain supported. Upstream
  `country` and `countrycode` values are preserved in local storage and JSON
  output; `--country` and `countries` keep using upstream codes. Geographic
  names and codes serve station search and display only. The CLDR name table is
  covered by the Unicode License V3 and is regenerable with
  `uv run scripts/update_territories.py`.

## 0.6.0 — 2026-10-20

- Add optional station logos and current-track covers in the TUI details panel
  through `artwork = "logo"` or `artwork = "track"`; the default remains `"off"`.
  Use Kitty, Sixel or iTerm2 graphics when detected and Unicode half blocks as
  fallback. tmux uses half blocks; monochrome and small layouts remain text-only.
- Center artwork in the space below station details without moving the text.
  Fix clipped graphics in Kitty/Ghostty and use a larger half-block area in tmux.
  Encode outside the render loop and preserve startup keys during capability
  detection, including keys arriving before terminal replies.
- Fetch station logos from catalog `favicon` URLs independently of audio.
  Bound JPEG/PNG downloads and decoding, reuse a bounded disk cache, and prune
  stale temporary files. Sync existing catalogs to populate favicon fields.
- Read track covers from validated ICY `StreamUrl` metadata with track/station
  association checks and logo fallback. Keep the two-upstream-audio-connection
  budget during playback and recording, cancel probes promptly, and bound
  connection handoffs before starting playback or recording. HLS and stations
  without usable image metadata retain the logo or text details.
- Add experimental native macOS Apple Silicon and Intel release archives,
  alongside the primary static Linux x86_64 musl build. Validate both macOS
  architectures in CI, smoke-test extracted packages, include `BUILDINFO`, shell
  completions and the manual, and provide a combined `SHA256SUMS`.
- Isolate pseudo-terminal tests from user configuration.

### Compatibility

- Existing configuration and schema-v2 databases remain supported. Artwork is
  opt-in and existing station rows need `radio sync` to obtain catalog logos.
  Track probing only runs for the visible, playing station when recording does
  not use the second audio connection; it may count as a listener upstream.
- External mpv and FFmpeg/ffprobe remain required on every platform. macOS
  downloads are not Developer ID-signed or notarized; see the READMEs for
  checksum verification and quarantine handling. Linux remains the primary
  platform; abrupt media-child cleanup is still not guaranteed on macOS.

## 0.5.0 — 2026-10-20

- Play HTTP(S) stream URLs directly from the CLI or TUI without saving stations,
  history, or resume preferences. Temporary streams support inspection; save
  them as custom stations before recording. Bound display names to 60 characters.
- Play a station by double-clicking its list row within 500 ms. Preserve
  single-click selection, including border clicks, and cancel gestures after
  movement, keyboard input, popups, or row changes.
- Select an mpv output with `audio_device` and enumerate current devices with
  `radio devices`, including legacy JSON and versioned JSON responses.
  Preserve `audio_device_failed` and its device parameter through CLI shutdown.
- Add opt-in `normalize_loudness` using a short-window mpv dynaudnorm filter.
  Probe support once per configured executable per process, sharing concurrent
  probes; keep recordings as unmodified stream copies.
- Accept `resume = "selection"` to restore the source and last station page
  without starting audio. Share the startup query with the TUI worker and consume
  the restore target after the first matching page, even when it is absent.
- Freeze the known track title when recording is requested and include it in
  the filename and optional `track` metadata. Collapse track-word whitespace to
  underscores and bound UTF-8 filename bytes; keep old names when titles are unknown.
- End local player startup and configured-device failures without retrying
  stream URLs, random stations, or directory refreshes. This includes transient
  IPC/socket startup timeouts: retrying playback requires a new play request.
  Stream connection failures retain candidate fallback and bounded reconnection.
- Support basic CLI/TUI operation on macOS, with a macOS CI job and source
  installation instructions. Use the native directory opener and make terminal
  tests portable. Linux remains the primary platform and release target;
  macOS does not guarantee media-child cleanup after abrupt frontend death.
- Recover recording publication interrupted between creating a hard link and
  removing the temporary name. Verify file identity before removing the partial
  name; unrelated files and symlink destinations remain protected.
- Stop playback from failing when the mpv IPC socket path exceeds the
  100-byte Unix socket limit: use a shorter socket name under the runtime
  directory and fall back to the system temp directory with the full UUID
  for deep paths. The default macOS per-user application-support directory
  came close enough that ordinary user names could trip this on every start;
  rejections now include the offending path.
- Preserve one-shot ICY song-title updates with a bounded 512 KiB back buffer;
  keep its memory budget separate from the forward cache.
- Repair ICY titles whose UTF-8 bytes were re-encoded as Latin-1, when the
  entire conversion produces valid UTF-8. Apply this before filtering control
  characters; leave ordinary file-title tags unchanged.

### Compatibility

- Existing schema-v2 databases and boolean `resume` settings remain supported;
  `false` remains the default. `normalize_loudness` defaults to false, and absent
  or empty `audio_device` preserves mpv automatic output selection.
- Recording metadata adds an optional `track`; old records remain readable.
  JSON device listing adds a new command without changing existing success shapes.
- Prebuilt downloads remain Linux x86_64 musl; macOS is supported from source.
  External mpv and FFmpeg/ffprobe remain required on both platforms.

## 0.4.0 — 2026-10-19

- Retry truncated HTTP/ICY recording connections with bounded backoff, including
  disconnects before AAC startup parameters are available. Preserve finite-source
  EOF behavior and partial files when retries fail.
- Retain the latest 8 KiB of recording diagnostics with line breaks, including
  messages containing equals signs and errors accompanying premature stream EOF.
- Preserve recording reconnect counts through finalization and recovery; warn
  that audio may have gaps even when the recording completes successfully.
  Print full diagnostic tails in the CLI text list as well as JSON and sidecars.
- Check recording space before creating destination directories; clear inactive
  playback state after stop or failure and cancel stale candidate retry timers.
- Localize top-level CLI help descriptions and playback progress; validate
  message keys in direct and explicit-locale lookups and integration tests.
- Retry failed TUI backup queries with backoff, suppress repeated errors and
  retain station details; invalidate cached results when station rows refresh.
- Share case-insensitive facet matching between navigation and rendering;
  debounce queries for the active field and prefix, with at most 100 candidates.
- Add a configurable normal-mode shortcut to locate the playing station without
  restarting playback. Accept an empty binding to disable it, reject incomplete
  named keys and normalize shifted control keys consistently.
- Generate release manuals in English regardless of packaging host language.
- Count HLS segment re-fetch failures as reconnects so HLS recordings also warn
  about possible audio gaps.
- Keep tab characters in CLI recording diagnostic lines so column alignment
  survives; other control characters remain filtered.
- Ignore commented-out message lookups when validating catalog keys, covering
  nested block comments and comment-like text inside strings and raw strings.
- Generate release shell completions in English as well as the manual, with
  tests guarding the packaging script and locale-invariant completions.
- Mark reconnected recordings in the TUI recording list so potential audio
  gaps are visible without opening the details panel.

### Compatibility

- Existing configuration and schema-v2 databases continue to work. The new
  `keybindings.jump_to_playing` setting defaults to `''` and can be set to an
  empty string to disable the shortcut.
- The supported release target remains Linux x86_64. External mpv and
  FFmpeg/ffprobe remain required.

## 0.3.1 — 2026-10-19

Post-release review fixes tracked in #83.

- Renew reconnect budgets after confirmed playback; cap flapping streams at six
  retries per minute and clear completed recording status when switching stations.
- Request audio/format bitrate from ffprobe for recording preflight. Prefer audio
  bitrate and explicitly report reserve-only checks for unknown duration/bitrate.
  `recording_min_free_mb` must be positive and convertible to bytes.
- Coalesce TUI list refreshes, avoid unchanged preference writes, and load backup
  URLs only for selected or explicitly inspected stations.
- Filter facet choices by the typed prefix and display up to eight candidates.
- Show observed mute state, restore narrow list/details focus navigation, retain
  titles in short wide windows, and keep recording-source actions consistent.
- Return JSON usage envelopes for malformed v2 flags, declare domain exit codes
  explicitly, and report `placeholder_kept` when station removal retains a row.
  Legacy always-JSON commands and the existing `removed` field remain compatible.
- Localize CLI operation progress and validate message keys across all modules.
- Package only user-facing guides from fresh staging, check nonempty files, and
  run the packager in CI. Internal review notes are excluded from new archives.
- Repeat paired catalog benchmarks to record comparative evidence and limits;
  retain the 100 ms absolute search budget.

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

All issues #1–#13 are addressed.
