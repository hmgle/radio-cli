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
| `asiafm_metadata_patch` | `false` | Optional cleanup of AsiaFM Cantonese track titles. See [AsiaFM titles](#asiafm-titles). |
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

### AsiaFM titles

Set the top-level `asiafm_metadata_patch = true` to clean up damaged titles from
AsiaFM Cantonese, UUID `fb1a81c7-cdf2-4ed8-912c-796918d0fa08`. The same option
works for direct/custom playback of `http://yyt.asiafm.net:8000/asiafm` (or HTTPS
on the same host, port and path). Restart radio-cli after editing the config.

The patch removes the known trailing `(AsiaFM亚洲粤语台)` watermark, including
its damaged forms, collapses whitespace, and shows missing text as `�` rather
than `�?`. For example, `张国荣  (AsiaFM亚洲粤语�?)` becomes `张国荣`, and
`陈小�?- ? (AsiaFM亚洲粤语�?)` becomes `陈小� - ?`. The normal decoder attempts
GBK repair with or without this option; the patch never guesses an artist or
song name.

Bare station names outside the trailing parentheses are preserved because they
may be part of a program title, so some watermark text can remain. After a known
watermark is matched, every `�-` is normalized to `� -`, including any intentional
occurrence within a title.

On this station, an entire field containing only a Latin-1 UTF-8 lead such as
`ç` is treated as truncated text and displayed as `�`. This rule covers only
three/four-byte leads (`E0`–`F4`); two-byte leads (`C2`–`DF`, such as `Ã`) are
preserved because they overlap with genuine Latin-1 letters. This is a
station-specific assumption: a genuine single accented letter in that position
also becomes `�`; words such as `Café` and `Jóga` are preserved. Other stations,
unknown watermark formats, and local file tags are left alone.

Cover matching uses the same cleaned title. Recordings started during playback
capture that title in their filename and `track` metadata; existing recordings
are not rewritten. The option adds no network requests and cannot recover
bytes already discarded upstream.

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
with Unicode half blocks as the fallback. Native Sixel in tmux requires a build
reporting `sixel_support=1` (tmux 3.6+), an attached client whose
`client_termfeatures` includes `sixel`, and a pixel cell size. Older versions and
failed detection use half blocks. `allow-passthrough` is not required or changed.
Detection runs once at startup, with one retry for a failed query. The result is
fixed until radio restarts.
With multiple attached clients, the selected client's cell size determines image
geometry. If it differs from the cell size tmux uses for the window, artwork can
be clipped or leave gaps. Use one attached client or matching cell sizes, and
restart radio after switching clients or changing font size. Clients without
Sixel support may show a `SIXEL IMAGE` text placeholder or shade characters,
depending on the tmux version. Images are centered below station details without
moving the text. Details stay text-only for the monochrome theme,
terminals smaller than 100×24, or when there is not enough room below the text.

Downloads are limited to JPEG/PNG files of at most 256 KiB and are decoded with
size limits. The cache lives under `artwork/` in the data directory and is pruned
to 128 entries and 32 MiB. A changed image URL is a new cache entry. Cached
files larger than 1 MiB are discarded before decoding.

Some Sixel terminals, including xterm, may lose an image after a popup or text
overwrites it. Closing and reopening the details usually redraws it.
Sixel encoding pads images to whole cells and may add a narrow black edge at the
right or bottom, including on light themes.

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

## Live subtitles

Subtitles are optional, disabled by default, and available only in the TUI for
live HTTP(S) streams. Recognition stays on your machine. Install
[uv](https://docs.astral.sh/uv/getting-started/installation/) first, then run:

```sh
radio subtitles setup
radio subtitles models list
radio subtitles models install sensevoice-small-int8
radio subtitles doctor
radio tui
```

When upgrading from 0.9.0, an omitted `subtitles.model` now selects
`sensevoice-small-int8`, even if only the previous Cantonese preset is
installed. Stop subtitle sessions, run
`radio subtitles models install sensevoice-small-int8`, then
`radio subtitles doctor` before enabling subtitles again. Explicit model
selections are preserved. To keep the previous model, set
`model = "sensevoice-small-yue-int8"` in your `[subtitles]` configuration.
Upgrading does not download models or rewrite your configuration.

In the TUI, use `:subtitles on` to start, `:subtitles off` to stop, and
`:subtitles history` to open scrollable history. `j/k`, arrows and Page Up/Down
scroll; `Home` shows the oldest retained text, `End` resumes following new
subtitles, and `Esc` closes the popup. `:subtitles clear` clears the history;
`:subtitles retry` retries a failed recognizer. The history holds at most 500
segments and is discarded on station changes or program exit.

The subtitle strip uses three rows in terminals at least 24 rows tall, or one
row showing the status and latest segment in shorter terminals. It sits above
the command bar and reduces panel height without changing the layout mode.
The three-row strip reserves one row for each of the two latest segments;
status messages wrap within the remaining rows. Segment text and the one-row
strip are clipped to the terminal width; open history to read full segments.
Press `?` to read persistent recognition failures in full.

`setup` explicitly installs managed Python 3.12 and hash-locked sherpa-onnx
2.0.1 / NumPy 2.2.6 wheels in `<data>/speech/`. Linux glibc x86_64 and macOS
x86_64/arm64 are supported; the optional runtime is unavailable on pure musl
systems. Python is isolated from the system environment. Normal recognition
never downloads packages or models. Installation needs network access to the
Python distributions, PyPI, Hugging Face and GitHub.

The upstream SenseVoice preset downloads approximately 240 MB including its
vocabulary and Silero VAD. The Cantonese-only `sensevoice-small-yue-int8`
preset downloads approximately 242 MB including the shared VAD.
`qwen3-asr-0.6b-int8` downloads approximately
988 MB plus VAD and supports broader multilingual recognition. These sizes
describe files, not peak RAM. The upstream `sensevoice-small-int8` weights use
the [FunASR Model Open Source License Agreement](https://github.com/modelscope/FunASR/blob/main/MODEL_LICENSE),
separately from the toolkit's MIT code license. The `sensevoice-small-yue-int8`
and Qwen3 presets list Apache-2.0; Silero VAD uses MIT.
`models list` includes license source links.
Downloads resume `.part` files and verify SHA-256 before publishing. Use
`models verify ID` to check installed files, `models install ID` to repair
them, or `models remove ID` to remove a model. Stop subtitle sessions before
installation or removal.

Both SenseVoice presets use the same vocabulary and graph layout. Each preset
keeps its own copy of its files under `<data>/speech/models/<id>/`; only the
Silero VAD files are shared between presets.
`sensevoice-small-int8` is the default and covers Cantonese, Mandarin,
English, Japanese, and Korean; it emits sentence punctuation and applies
inverse text normalization, so spoken numbers are written as digits.
`sensevoice-small-yue-int8` is the WenetSpeech-Yue fine-tune, retained for
existing configurations; use the upstream preset for new installations. It
recognizes Cantonese only, without punctuation or inverse text normalization.
Its `language` setting does not restore multilingual recognition. All presets
already use Silero VAD to split audio at pauses; this does not add punctuation
or guarantee grammatical sentence boundaries.

Within one TUI session, recognition reuses successful model verification for
unchanged files when reconnecting. Changed or replaced files are checked again;
a new TUI session starts with full verification. `models verify`, `doctor`, and
installation always perform full SHA-256 checks.

Model downloads have no total time limit while data keeps arriving. Each
network wait aborts after 20 seconds without progress, and downloads can be
cancelled.

Before downloading, installation reports the remaining bytes and checks free
disk space, allowing for resumable files and a 64 MiB reserve. An optional
`model_mirror` HTTP(S) base URL serves files as
`<base>/<model-id>/<file-path>`, for example
`https://mirror.example.com/radio-models/silero-vad/silero_vad.onnx`.
The official source is tried if the mirror fails. The mirror must provide the
exact files pinned by the built-in manifest: size and SHA-256 verification
remain mandatory. This setting covers model and VAD downloads, not Python or
package installation. Query strings and fragments are not supported.

```toml
[subtitles]
enabled = false
model = "sensevoice-small-int8"
language = "auto"
threads = 0
max_segment_secs = 8
uv = "uv"
# model_mirror = "https://mirror.example.com/radio-models/"
```

| Setting | Default | Behavior |
| --- | --- | --- |
| `enabled` | `false` | Start subtitles when TUI live playback begins. |
| `model` | `"sensevoice-small-int8"` | This model, `"sensevoice-small-yue-int8"`, or `"qwen3-asr-0.6b-int8"`; must already be installed. |
| `language` | `"auto"` | SenseVoice: `auto`, `zh`, `yue`, `en`, `ja`, `ko`; only `sensevoice-small-int8` follows this setting. Qwen3 requires `auto`. |
| `threads` | `0` | Automatic: half the logical cores, limited to 1–4; explicit values: 1–32. |
| `max_segment_secs` | `8` | VAD speech target, 2–20 seconds; a short trailing pause may be included. |
| `uv` | `"uv"` | Executable name/path used only by `setup`. |
| `model_mirror` | unset | Optional model download base URL, with official fallback and fixed SHA-256 checks. |

`:set subtitles.model ID` changes the session model and restarts recognition;
`:subtitles toggle` changes the session switch. Neither rewrites your config.

Subtitles use a separate FFmpeg connection to the current playback URL. Audio
is not delayed to wait for recognition, and the subtitle time ranges refer to
capture, not mpv's playback position. HLS offsets, network buffering and pauses
can make captions describe a different live position. Pause stops capture;
resume reconnects at the live edge even if mpv resumes buffered audio. Muting
keeps recognition running. Reconnection boundaries appear in history.

Speech queues are bounded; overload skips old audio and text rather than
accumulating unlimited delay. Results more than 30 seconds behind capture are
discarded. Network failures retry three times with 1, 3 and 7 second waits.
The budget resets when a connection produces another segment at least 60
seconds after its first segment, so brief productive connections still exhaust
retries. Model/runtime failures require
repair or `:subtitles retry`. Invalid PCM frames are skipped and marked as gaps.
Subtitle failure does not stop playback or recording. Recording adds its usual
connection; some stations restrict listener connections. Track artwork probing
yields while subtitles are enabled, including loading, retry waits and failures.

This first version provides session-only transcription, without translation,
recording transcription, export, or cloud recognition. ASR text can be wrong,
especially over music or noise; model benchmarks are not guarantees for radio.
