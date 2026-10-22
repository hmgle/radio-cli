# Configuration reference

**English** | [简体中文](configuration.zh-CN.md)

The configuration file is optional. Omitted settings use the defaults below.
[`config.example.toml`](../config.example.toml) contains an annotated starting
point. Command-line options take precedence over the file.

## Paths and precedence

| Item | Default on Linux | Default on macOS |
| --- | --- | --- |
| Config file | `${XDG_CONFIG_HOME:-~/.config}/radio-cli/config.toml` | `~/Library/Application Support/radio-cli/config.toml` |
| Data directory | `${XDG_DATA_HOME:-~/.local/share}/radio-cli/` | `~/Library/Application Support/radio-cli/` |
| Catalog database | `<data>/radio.db` | `<data>/radio.db` |
| Recordings | `<data>/recordings/` | `<data>/recordings/` |
| Command history | `<data>/command-history.json` | `<data>/command-history.json` |
| Artwork cache | `<data>/artwork/` | `<data>/artwork/` |
| Runtime lock | `<data>/run/media.lock` | `<data>/run/media.lock` |

Resolution rules:

- `--config FILE` selects an explicit config file. A missing explicit file is an
  error; a missing default config file is accepted and defaults are used.
- `--data-dir DIR` replaces both the default data directory and the database
  location beneath it.
- `--api-url URL` overrides the `api_url` setting.
- Relative paths, including `recording_dir`, are resolved against the working
  directory at startup. The effective recording directory is stored as an
  absolute path.
- The config file is read when a command starts and is never rewritten. Restart
  radio-cli after editing it.

## Playback settings

| Setting | Default | Valid values and behavior |
| --- | --- | --- |
| `mpv` | `"mpv"` | Executable name or path used for playback, device enumeration, and filter checks. |
| `volume` | `80` | Integer from 0 to 100. The runtime volume is stored in SQLite and reused on later sessions. |
| `normalize_loudness` | `false` | Enable playback-only loudness normalization. Recording remains a stream copy. |
| `audio_device` | automatic | Optional mpv output device name. Omit or use `""` for automatic selection. |
| `resume` | `false` | `false`, `true`, or `"selection"`. See [Resume modes](#resume-modes). |
| `playback_timeout_secs` | `12` | Integer from 1 to 300. Deadline for playback to begin. |

Changing `volume` in the config affects sessions that have no remembered runtime
volume. A saved value outside the valid range falls back to the config value.

### Resume modes

| Value | Behavior at TUI startup |
| --- | --- |
| `false` | Open the all-stations source without playing. |
| `true` | Restore the last source and last successfully played station, then start playback. |
| `"selection"` | Restore the last source and select the last station, including its page, without starting audio. |

An unavailable last station is skipped or falls back to the first row as
appropriate. The recordings source has no station selection to restore. Volume
is remembered independently and is always restored.

### Loudness normalization

`normalize_loudness = true` asks mpv to apply `dynaudnorm` with the short window
`f=150:g=5`. It affects playback only, adds a small amount of buffering, and
never modifies a recording. Support is checked once per configured mpv executable
for the lifetime of the process. If the filter is unavailable, disable the
setting or install an mpv build that provides it.

## Recording settings

| Setting | Default | Valid values and behavior |
| --- | --- | --- |
| `ffmpeg` | `"ffmpeg"` | Executable name or path used to record and finalize media. |
| `ffprobe` | `"ffprobe"` | Executable name or path used to inspect streams before recording. |
| `recording_dir` | `<data>/recordings` | Destination directory. Relative values are resolved at startup. |
| `probe_timeout_secs` | `5` | Integer from 1 to 300. Independent codec-probe budget before recording. |
| `recording_min_free_mb` | `16` | Positive MiB reserve. It is added to the estimated recording size when one can be calculated. |

The space preflight is a guard, not a reservation. If duration or bitrate is
unknown, radio-cli checks only the configured reserve and reports that the total
size cannot be estimated. See the [recording guide](recording.md).

## Catalog and network settings

| Setting | Default | Valid values and behavior |
| --- | --- | --- |
| `api_url` | automatic discovery | HTTP(S) URL for a Radio Browser-compatible service. `--api-url` takes precedence. |
| `connect_timeout_secs` | `5` | Integer from 1 to 300. Connection timeout for catalog, artwork, and related HTTP requests. |
| `request_timeout_secs` | `20` | Integer from 1 to 300. Overall request timeout for catalog and artwork requests. |
| `auto_sync_days` | `7` | Unsigned 16-bit integer (0–65535). Refresh a previously synced catalog when it is at least this many days old; `0` disables automatic refresh. |

### Catalog source and synchronization

The station catalog is third-party data from
[Radio Browser](https://www.radio-browser.info/) and is cached in the local
SQLite database. The repository and release archives do not ship a catalog
database.

Radio Browser servers are discovered automatically with bounded failover. Set
`api_url` or pass `--api-url` to use a specific compatible service. A full
`radio sync` fetches every available page. If no catalog exists, the TUI and
`radio random` bootstrap the 1,000 most popular stations first.

If an otherwise successful sync would reduce the previous catalog by more than
25%, radio-cli rejects it unless `radio sync --accept-shrink` is used. Failed,
cancelled, or rejected syncs leave the previous catalog in place. TUI syncs can
also be cancelled with `:sync-cancel`.

`auto_sync_days` refreshes only a catalog that has completed a full sync before.
Automatic sync runs in the background in the TUI and uses the same cancellation
and shrink protection.

Successful playback of a catalog station sends one click to Radio Browser.
Reconnects do not repeat it. Custom stations and temporary direct streams do not
report clicks.

### Proxies

Catalog and artwork HTTP requests honor the standard `HTTP_PROXY`, `HTTPS_PROXY`,
`ALL_PROXY`, and `NO_PROXY` environment variables. mpv and the audio backend use
their own proxy configuration. A reachable catalog therefore does not guarantee
that an audio stream is reachable.

## Interface settings

| Setting | Default | Valid values and behavior |
| --- | --- | --- |
| `language` | detected | `"en"` or `"zh-CN"`; an unknown value falls back to English with one notice. |
| `theme` | `"default"` | `"default"`, `"mono"`, or `"light"`. A nonempty `NO_COLOR` forces mono. |
| `artwork` | `"off"` | `"off"`, `"station"`, or `"track"`. See [Artwork](#artwork). |
| `mouse` | `true` | Whether the TUI captures mouse events. `false` allows native terminal text selection. |
| `keybindings.jump_to_playing` | `"''"` | Shortcut that locates the playing station. See [Keybindings](#keybindings). |

### Language

Language selection uses the first nonempty value in this order:

1. `--lang`
2. `RADIO_LANG`
3. `language` in the config file
4. `LC_ALL`
5. `LANG`
6. English

`en`, `C`, `POSIX`, and `en-*` select English. `zh`, `zh-CN`, and `zh-Hans`
select Simplified Chinese. Other values produce a one-time notice and fall back
to English. `radio --lang ...`, `RADIO_LANG`, and the config setting affect a
new process; `:set language en|zh-CN` changes the current TUI session only.

In the Chinese interface, country/region names come from a pinned Unicode CLDR 48
table keyed by the catalog's `countrycode`. Unrecognized or missing codes fall
back to the upstream catalog name, then the code, then unknown. Storage and JSON
preserve the upstream `country` and `countrycode` values, and `--country` keeps
using upstream codes.

### Audio devices

`audio_device` selects an mpv output device. Run `radio devices` to list the
names and descriptions exposed by the configured mpv executable. Names depend on
the mpv audio backend and local hardware and can change after devices are
reconnected. An empty value uses automatic selection.

Failures to start the configured local player or output device end the current
play request. They do not try backup stations or URLs because changing the
stream cannot repair a local media configuration problem.

## Artwork

Artwork is optional and appears in the TUI details panel. It does not affect
audio playback or recordings.

| Value | Source | Fallback |
| --- | --- | --- |
| `"off"` | No images | Text-only details |
| `"station"` | Catalog `favicon` for the displayed station | Text-only details |
| `"track"` | Current track's ICY `StreamUrl` image | Station logo, then text |

Station logos come from catalog metadata, not from the audio stream. Run
`radio sync` after upgrading an older catalog to populate `favicon` values for
existing rows.

Track artwork is requested only when the station shown in details is playing and
publishes a usable image URL in ICY `StreamUrl` together with a nonempty current
title. Probing starts at 25-second intervals and can count as another listener.
It stops on pause, stop, station change, or exit. HLS streams and local recording
playback are excluded. During recording, artwork probing yields to the codec
probe and recording connection; details fall back to the station logo or text.

Terminal rendering uses Kitty, Sixel, or iTerm2 image protocols when available,
with Unicode half blocks as the fallback. tmux always uses half blocks. Images
are centered in the space below station details without moving the text. Details
stay text-only for the monochrome theme, terminals smaller than 100×24, or when
there is not enough room below the text.

Downloads are limited to JPEG/PNG files of at most 256 KiB and are decoded with
size limits. The cache lives under `artwork/` in the data directory and is pruned
to 128 entries and 32 MiB. A changed image URL is a new cache entry. Cached
files larger than 1 MiB are discarded before decoding.

Some Sixel terminals, including xterm, may lose an image after a popup or text
overwrites it. Closing and reopening the details usually redraws it.

## Keybindings

`[keybindings] jump_to_playing` changes the normal-mode shortcut that locates the
playing station and focuses its list row.

Accepted forms:

- One to four printable, non-whitespace characters, for example `"gp"`.
- One named key in angle brackets, optionally with modifiers, for example
  `"<F6>"`, `"<Ctrl-p>"`, `"<Alt-p>"`, or `"<Shift-P>"`.
  Named keys are `F1`–`F12`, `Space`, `Home`, `End`, `Enter`, `Tab`, and the
  arrow keys, or any single character. `"<Shift-Tab>"` is accepted as BackTab.
- `""` disables the shortcut.

The default is `"''"`: press two single quotes within one second. Ctrl-C remains
reserved for quitting. A configured binding takes precedence over existing
normal-mode shortcuts. If the target is on the current page, source, filters,
and sort order are preserved; otherwise the view switches to all stations,
clears filters, and loads the target page. Locating never replays audio.

The built-in help panel shows the configured key and omits the entry when the
binding is disabled.

## Validation and data ownership

Unknown TOML keys are rejected so typos do not silently pass. Invalid URLs fail
with `invalid_url`; out-of-range timeouts, a zero free-space reserve, an invalid
keybinding, or a volume above 100 fails with `invalid_settings`.

radio-cli does not rewrite the handwritten config file or `config.example.toml`.
Mutable state belongs to the selected data directory:

- `radio.db` stores the catalog, favorites, history, recording index, runtime
  volume, last-station preference, and sync metadata. History retains the last
  1,000 successful plays.
- `command-history.json` keeps up to 100 TUI commands. Commands containing URL
  credentials are not saved.
- `artwork/` is a bounded, disposable image cache.
- `run/media.lock` serializes playback, recording, and TUI media sessions.

One data directory allows one media session at a time. Other one-shot queries and
favorite operations can still open the database. Use separate `--data-dir`
values for concurrent manual sessions.
