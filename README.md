# radio-cli

[![CI](https://github.com/hmgle/radio-cli/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/hmgle/radio-cli/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/hmgle/radio-cli.svg?logo=github)](https://github.com/hmgle/radio-cli/releases/latest)
[![Crates.io](https://img.shields.io/crates/v/radio-cli.svg?logo=rust)](https://crates.io/crates/radio-cli)
[![npm](https://img.shields.io/npm/v/@hmgle/radio-cli.svg?logo=npm)](https://www.npmjs.com/package/@hmgle/radio-cli)
[![Rust 1.88+](https://img.shields.io/badge/Rust-1.88%2B-dea584.svg?logo=rust)](https://github.com/hmgle/radio-cli/blob/main/Cargo.toml)
[![Linux and macOS](https://img.shields.io/badge/platforms-Linux%20%7C%20macOS-blue.svg)](#requirements)
[![MIT license](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)

**English** | [简体中文](README.zh-CN.md)

A lightweight terminal radio for Linux and macOS. Rust manages the local catalog
and interface, SQLite stores favorites and history, mpv plays audio, and FFmpeg
records streams without re-encoding. No account or API key is required.

- Browse a catalog from [Radio Browser](https://www.radio-browser.info/), search
  locally, filter by country, language, tag, and bitrate, and play a random match.
- Use the keyboard-first TUI or script the CLI, with legacy `--json` and stable
  `--json-v2` output.
- Keep favorites, history, custom stations, settings, and recordings on your
  machine.
- Enable optional local live subtitles with SenseVoice or Qwen3;
  see [subtitle setup](docs/configuration.md#live-subtitles).
- Play any HTTP(S) stream URL, or save custom stations with backup URLs.
- Record MP3, AAC, Vorbis, and Opus streams as stream copies with metadata.

Linux is the primary platform. macOS supports the core CLI/TUI workflow.
Native Windows is not supported because the code uses Unix APIs.

## Demo

[![Watch radio-cli browse CN stations and play live radio](assets/demo.svg)](https://asciinema.org/a/1269102)

[Watch the 90-second demo on asciinema](https://asciinema.org/a/1269102).
It shows the Chinese UI browsing a synced catalog, filtering by `CN`, searching,
playing, saving favorites, locating the playing station, and recording a short
clip. The recording is silent and shows an earlier interface; shortcut hints
have since been updated.

## Quick start

The fastest route is the npm package:

```sh
npm install -g @hmgle/radio-cli
radio doctor
radio sync
radio tui
```

Runtime dependencies are `mpv`, `ffmpeg`, and `ffprobe`. Alternatively, install
from crates.io:

```sh
cargo install radio-cli --locked
```

See the [installation guide](docs/installation.md) for platform archives and
checksum verification, crates.io installation, build-from-source details,
macOS Gatekeeper notes, and first-run behavior.

## Everyday use

### TUI shortcuts

| Context | Keys | Action |
| --- | --- | --- |
| General | `q` / `Ctrl-C` | Finalize recordings and quit |
| General | `?` | Keyboard help; with a pending failure, show its details first |
| General | `Esc` | Close a popup, cancel editing, or dismiss an error |
| General | `F5` | Refresh the current list |
| Panels | `Tab` / `Shift-Tab` | Next / previous panel |
| Panels | `1`–`5` | Focus Sources / List / Details / Player / Tasks |
| Panels | `h` / `l`, arrows | Move focus left / right or select and scroll |
| Panels | `Ctrl-h/j/k/l`, `Ctrl` + arrows | Move focus by screen direction |
| Sources | `[` / `]` | Previous / next source: stations, favorites, history, recordings |
| Sources | `j` / `k`, arrows | Next / previous source while the Sources panel is focused |
| Lists | `j` / `k`, arrows | Move the selection |
| Lists | `gg` / `G`, `Home` / `End` | First / last item |
| Lists | `Ctrl-u` / `Ctrl-d` | Move ten items |
| Lists | `H` / `M` / `L` | Select the top / middle / bottom visible row |
| Lists | `zt` / `zz` / `zb` | Put the selected row at the top / middle / bottom |
| Lists | `PgUp` / `PgDn`, `Ctrl-b` / `Ctrl-f` | Previous / next data page; scroll ten lines in other panels |
| Lists | `Ctrl-e` / `Ctrl-y` | Scroll the viewport down / up one line |
| Playback | `Enter` | Play the selected station or recording; in Sources, open the list; in Player, show the playing station's details |
| Playback | `Space` | Pause / resume |
| Playback | `m` | Mute / unmute |
| Playback | `+` or `=` / `-` | Raise / lower volume |
| Playback | `n` | Play a random station using the current filters |
| Playback | `f` | Favorite / unfavorite the selected station |
| Playback | `r` | Start / stop recording the playing station |
| Copy | `y` / `Y` | Copy the selected UUID / stream URL, or recording ID / path |
| Search | `/` | Search; `Enter` confirms, `Esc` restores the previous view |
| Search | `F` | Country, language, tag, and minimum-bitrate filters |
| Search | `Ctrl-U` | In the filter popup, clear search and filters |
| Recordings | `D` / `o` / `e` | Delete (`y` confirms; `Enter` / `n` / `Esc` / `q` cancel) / open folder / show error details |
| Navigation | Configurable | Locate the playing station; defaults to pressing `''` twice within one second |

In the bottom bar, highlighted tokens are literal keys. For example,
`[ / ]` means the separate `[` and `]` keys; the slash between them is only a
separator.

Mouse support is enabled by default: click a panel or row, double-click a row
within 500 ms to play it, and use the wheel to navigate.

The Recordings source does not support catalog search: `/`, `F`, and the
favorite keys are unavailable there; press `n` to switch back to stations and
play a random match. In tmux, `Ctrl-b` is consumed as the prefix, so use
`PgUp` / `PgDn` for page navigation. Popups, text input, and confirmation
prompts use their own keys; press `?` inside the TUI for the full
context-sensitive reference. A failed list query retries automatically with
exponential backoff from 500 ms to 30 seconds; `F5` bypasses the wait. Transient
notices clear after about four seconds. `y` and `Y` copy through OSC 52, whose
support depends on the terminal and multiplexer; requests over 4,096 bytes are
truncated and reported.

### In-TUI commands

| Command | Action |
| --- | --- |
| `:sync [--accept-shrink]` / `:sync-cancel` | Start or cancel a background catalog sync |
| `:refresh` | Refresh the current list |
| `:search [QUERY] [OPTIONS]` | Apply a search using the CLI search options |
| `:play [UUID\|URL]` | Play the selection, a UUID, or a temporary stream URL |
| `:random [OPTIONS]` | Play a random station; without options, reuse current filters |
| `:info [UUID]` / `:current` | Inspect a station / the playing station |
| `:favorites list\|add\|remove [UUID]` | Manage favorites |
| `:history` | Show recent successful plays |
| `:recordings list\|play ID\|remove ID [--index-only]` | Inspect, play, or remove recordings |
| `:open recordings` | Open the recording directory |
| `:station add\|list\|edit\|remove\|backups ...` | Manage custom stations and fallback URLs |
| `:record [UUID] [--duration D] [--output-dir DIR]` / `:record stop` | Start or finalize a recording |
| `:pause` / `:mute` / `:stop` / `:volume 70` | Control playback |
| `:help [COMMAND]` | Open keyboard help or a command's usage |
| `:set language en\|zh-CN` / `:set mouse true\|false` | Change session settings |
| `:doctor` / `:focus sources\|list\|details\|player\|activity` | Run diagnostics / move focus |
| `:subtitles on\|off\|toggle\|history\|clear\|retry` | Control live subtitles / inspect session history |
| `:set subtitles.model ID` | Change the subtitle model for this session |
| `:quit` / `:q` | Finalize recordings and quit |

`:help` shows the full context-sensitive keyboard reference, and `:` followed by
`Tab` completes commands. `:record UUID` only accepts the station that is
currently playing; use `radio record UUID` for an independent recording.

### Explore map

Open **Explore** in Sources or run `:explore`. On wide terminals the offline
world map appears beside the station list; on narrow terminals use `6` for the
map, `2` for results, and `3` for details. Click a map cell to list its
stations. With map focus, use arrows or `hjkl` to move, `z` / `Z` to zoom up to
8×, `0` to return to the world, and `Enter` to focus results. The wheel over the
map zooms around the pointer. Playback and volume shortcuts keep their usual
meaning.

The map preserves its fine Braille dot pattern, with up to eight small dots
in each terminal cell. Station density colors the existing dots while retaining
the coastline shape. Gray marks land; blue, teal, amber, and red indicate 1–9,
10–49, 50–199, and 200+ stations per cell, as shown in the legend. Stations in
empty sea cells use a single Braille dot. The selected cell uses a large circle
(`●`) while retaining its density color. Monochrome mode underlines station
cells and keeps the selected-cell count without colors.

Explore includes active catalog and custom stations; inactive favorites remain
available in Favorites. Clearing search and filters keeps the selected map cell;
press `0` with map focus to return to the world view.

Search and filters apply to the map and results. Press `c` to browse stations
without coordinates by country/region, then `Esc` to return to the map. Run
`radio sync` after upgrading to populate coordinates; older catalogs have none.
Geographic metadata comes from station submissions and may be inaccurate. The
bundled Natural Earth 110m land outline is public domain and uses no online
tiles. Small islands and enlarged coastlines may be simplified.

### CLI reference

Global options apply to every subcommand:

| Option | Description |
| --- | --- |
| `--lang <en\|zh-CN>` | Select interface and CLI language |
| `--config <FILE>` | Use a specific TOML configuration file |
| `--data-dir <DIR>` | Override the data directory |
| `--api-url <URL>` | Override Radio Browser server discovery |
| `--json` | Emit legacy JSON shapes |
| `--json-v2` | Emit one versioned success/error envelope |
| `-h`, `--help` | Print help |
| `-V`, `--version` | Print the version |

Catalog and search commands:

| Command | Purpose |
| --- | --- |
| `radio` / `radio tui` | Open the TUI |
| `radio sync [--accept-shrink]` | Fetch and replace the full catalog |
| `radio search [QUERY] [OPTIONS]` | Search the local catalog |
| `radio random [QUERY] [OPTIONS]` | Play one random matching station |
| `radio info <UUID>` | Print one station as JSON |
| `radio countries [--prefix TEXT]` | List country/region codes and counts |
| `radio languages [--prefix TEXT]` | List languages and counts |
| `radio tags [--prefix TEXT]` | List tags and counts |

`search` and `random` accept `--sort name|bitrate|votes|clickcount|random`,
`--country CODE`, `--language LANGUAGE`, `--tag TAG`, `--min-bitrate KBPS`,
`--limit N` (1–1000, default 100), and `--offset N`. Search defaults to
`clickcount` order. `info` always emits station JSON.

Favorites, history, and custom stations:

| Command | Purpose |
| --- | --- |
| `radio favorites list [PAGE]` | List favorites |
| `radio favorites add <UUID>` | Add a favorite |
| `radio favorites remove <UUID>` | Remove a favorite |
| `radio history [PAGE]` | Show successful playback history |
| `radio station list [PAGE]` | List custom stations |
| `radio station add <NAME> <URL> [--backup-url URL]...` | Add a custom station |
| `radio station edit <UUID> [--name NAME] [--url URL] [--backup-url URL]...` | Edit a custom station |
| `radio station remove <UUID> [--keep-references]` | Remove a custom station |
| `radio station backups <UUID> [--add URL]... [--remove URL]...` | Manage fallback URLs |

`PAGE` means `[--limit N] [--offset N] [--all]`. List commands accept a limit
from 1 to 1000 (default 100); `--all` exports every match from the offset.

Playback, recording, and diagnostics:

| Command | Purpose |
| --- | --- |
| `radio play <UUID\|URL>` | Play a saved station or audition an HTTP(S) stream |
| `radio record <UUID> [--duration D] [--output-dir DIR]` | Record a saved station until stopped or the duration expires |
| `radio recordings list [PAGE]` | List recording records |
| `radio recordings recover` | Retry pending finalization of interrupted recordings |
| `radio recordings play <ID>` | Play a recording |
| `radio recordings remove <ID> [--index-only]` | Delete a recording and its files, or only its index entry |
| `radio recordings open` | Open the recording directory |
| `radio devices` | List mpv audio output devices |
| `radio doctor` | Check media tools, recording path, and free space |

Shell integration:

| Command | Purpose |
| --- | --- |
| `radio completions <SHELL>` | Generate completion code for bash, elvish, fish, powershell, or zsh |
| `radio man` | Generate the `radio(1)` manual page |
| `radio help [COMMAND]` | Print command help |

Examples:

```sh
radio sync
radio search jazz --country FR --min-bitrate 128
radio search 新闻 --language chinese
radio random --country CN
radio play https://example.com/live.mp3
radio station add "My Station" https://example.com/live \
  --backup-url https://example.com/backup
radio record <UUID> --duration 30m --output-dir ./recordings
radio --json-v2 search jazz
radio completions zsh > _radio
```

Search is local and case-insensitive by default. Queries of two or more
characters also match Chinese country/region names, so `刚果` finds both Congos
and `法国` finds France. Single-character queries match station text only because
one character is shared by too many territories. Country/region, language, and
tag filters use their full catalog values; languages also accept catalog codes
such as `zh`. `--min-bitrate` excludes stations whose bitrate is unknown.
`random` samples from the entire filtered set, not just the current page.

Direct URLs are temporary: they are not added to the catalog or history and do
not replace the last saved station. Save a station before recording it.

Use legacy `--json` for existing scripts or `--json-v2` for atomic success/error
responses and stable error codes. See the [JSON contract](docs/json-contract.md).

## Configuration

Configuration is optional. The default paths are:

| Item | Linux | macOS |
| --- | --- | --- |
| Config | `${XDG_CONFIG_HOME:-~/.config}/radio-cli/config.toml` | `~/Library/Application Support/radio-cli/config.toml` |
| Data directory | `${XDG_DATA_HOME:-~/.local/share}/radio-cli/` | `~/Library/Application Support/radio-cli/` |
| Database | `<data>/radio.db` | `<data>/radio.db` |
| Recordings | `<data>/recordings/` | `<data>/recordings/` |

Use `--config <FILE>` and `--data-dir <DIR>` to isolate or relocate state.
`--api-url` overrides the configured catalog server. Command-line options take
precedence over the config file. Relative paths are resolved from the working
directory at startup.

| Area | Settings |
| --- | --- |
| Catalog and network | `api_url`, `connect_timeout_secs`, `request_timeout_secs`, `auto_sync_days` |
| Playback | `mpv`, `volume`, `normalize_loudness`, `asiafm_metadata_patch`, `audio_device`, `resume`, `playback_timeout_secs` |
| Recording | `ffmpeg`, `ffprobe`, `recording_dir`, `probe_timeout_secs`, `recording_min_free_mb` |
| Interface | `language`, `theme`, `artwork`, `mouse`, `keybindings.jump_to_playing` |

Start from [config.example.toml](config.example.toml). For valid values, defaults,
path precedence, language detection, resume modes, artwork behavior, and
keybinding syntax, see the [configuration reference](docs/configuration.md).

```toml
mpv = "mpv"
volume = 80
resume = "selection"
auto_sync_days = 7
theme = "default"
artwork = "station"

[keybindings]
jump_to_playing = "<F6>"
```

The handwritten config file is never rewritten. Runtime volume and the last
saved station are stored in SQLite. The TUI settings changed with `:set` apply to
the current session only.

## Recording

Recording is a stream copy: audio is not re-encoded or degraded. The actual
stream codec is probed before recording starts.

| Stream codec | Output |
| --- | --- |
| MP3 | `.mp3` |
| AAC, including common HLS AAC | `.aac` |
| Vorbis | `.ogg` |
| Opus | `.opus` |

Each recording starts as a unique `.part` file and is published after a normal
stop. A JSON sidecar records station, requested URL, timestamps, codec, duration,
completion state, and reconnect count. Existing files are never overwritten.
Interrupted recordings retain their partial file and can be inspected with
`radio recordings list`; `radio recordings recover` retries a pending
publication.

Pausing playback does not pause recording. Switching stations ends the current
recording. `--duration` accepts seconds, `s`, `m`, and `h`, from 1 second to 365
days. For formats, space checks, reconnect behavior, and recovery steps, see the
[recording guide](docs/recording.md).

## Troubleshooting

| Symptom | First action |
| --- | --- |
| Missing mpv, FFmpeg, or ffprobe | Install the tools, then run `radio doctor` |
| Empty or stale catalog | Run `radio sync`; check network and proxy settings |
| Stream fails or there is no sound | Try the URL in mpv, check volume/mute, then run `radio devices` |
| Another session owns the data directory | Close the other TUI/playback/recording session or use another `--data-dir` |
| Config changes are ignored | Check the config path, TOML syntax, and unknown keys; restart radio-cli |
| Recording is incomplete or cannot be published | Keep the `.part` file, inspect `radio recordings list`, then run `radio recordings recover` |
| Artwork is missing | Run `radio sync`, use a supported terminal/protocol, and check the details panel size |

See [Troubleshooting](docs/troubleshooting.md) for proxy, playback-device,
exit-code, macOS Gatekeeper, terminal, and language details.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --all-features --locked
cargo test --release --test storage catalog_benchmark -- --ignored --nocapture
```

Tests use local HTTP/ICY/HLS fixtures rather than public stations. Recording
tests require FFmpeg; mpv tests use a null audio output, skip when mpv is absent,
and accept `RADIO_TEST_MPV=/absolute/path/to/mpv`. See
[CONTRIBUTING.md](CONTRIBUTING.md) for release packaging, platform requirements,
and the deterministic catalog benchmark.

## Documentation

| Document | Contents |
| --- | --- |
| [Installation](docs/installation.md) | Requirements, npm/crates/source installs, release verification |
| [Configuration](docs/configuration.md) | Every setting, paths, language, resume, artwork, keybindings |
| [Recording](docs/recording.md) | Formats, files, recovery, reconnects, disk checks |
| [Troubleshooting](docs/troubleshooting.md) | Common failures, proxies, devices, exit codes |
| [JSON contract](docs/json-contract.md) | JSON shapes and process exit codes |
| [Changelog](CHANGELOG.md) | Release notes and compatibility changes |
| [Contributing](CONTRIBUTING.md) | Development, tests, and release archives |
| [Security](SECURITY.md) | Supported versions and vulnerability reporting |

The paired guides under `docs/` include English and Simplified Chinese versions.

## License

MIT. See [LICENSE](LICENSE). The bundled Chinese country/region name table is
covered by the [Unicode License V3](locales/LICENSE.unicode).
