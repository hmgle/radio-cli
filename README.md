# radio-cli

**English** | [简体中文](README.zh-CN.md)

A lightweight, Linux-first terminal radio. Rust manages the local catalog and interaction, SQLite stores favorites and history, mpv plays audio, and FFmpeg records streams losslessly. No account or API key required.

Homepage: <https://github.com/hmgle/radio-cli>

## Installation & Quick Start

Requires Rust 1.88+, mpv, and FFmpeg (including ffprobe). SQLite is compiled into the binary, so no SQLite command-line tools are needed.

Prebuilt Linux x86_64 binaries and checksums are available on the [releases page](https://github.com/hmgle/radio-cli/releases). The static binary needs no Rust toolchain; install mpv and FFmpeg separately. See the [changelog](CHANGELOG.md) for upgrade compatibility notes.

```sh
# Debian / Ubuntu
sudo apt install mpv ffmpeg
cargo build --release --locked
./target/release/radio doctor
./target/release/radio sync
./target/release/radio tui

# or install to ~/.cargo/bin
cargo install --path . --locked
```

Running `radio` directly opens the TUI in an interactive terminal; when output is redirected it prints help instead. With no catalog present, the TUI and `random` first fetch the top 1,000 stations; the full catalog is fetched with `sync`. A failed sync keeps the previous catalog.

## Commands

```sh
radio search jazz --country FR --min-bitrate 128
radio search 新闻 --language chinese
radio search --tag 'public radio' --limit 100 --offset 100
radio --json search jazz
radio info <UUID>
radio play <UUID>
radio random --country CN
radio favorites add <UUID>
radio favorites list
radio favorites remove <UUID>
radio history
radio station add 'My Station' 'https://example.com/live' \
  --backup-url 'https://example.com/backup'
radio record <UUID> --duration 30m --output-dir ./recordings
radio recordings list
```

Name, country, language, and tag searches run locally; country code, language, and tag filters use full values (languages also accept the codes provided by the catalog, e.g. `zh`). The minimum-bitrate filter excludes records with unknown bitrate. `random` samples across the entire matching catalog and is not limited by search pagination. `info` always outputs JSON; query commands accept `--json`. Runtime status and diagnostics go to stderr.

Stations are identified by their full Radio Browser UUID; custom stations get a local UUID. Favorites, recently played entries, and custom addresses are never overwritten by a catalog sync. A favorite that has disappeared from the catalog can still be played. History keeps the last 1,000 successful playbacks; the interface aggregates it by station to show recent listening.

TUI keys:

| Key | Action |
|---|---|
| `:` | Command mode; Enter executes, Esc returns |
| `Tab` / `Shift-Tab` | Next / previous panel |
| `1`–`5` | Source / list / details / player / tasks panel |
| `Ctrl` + arrows / `Ctrl`+`h/j/k/l` | Switch panels by spatial position |
| Arrows / `j`, `k` | Select, navigate, or scroll the focused panel |
| `Enter` | Play in lists; view current station details in the player |
| `Space` | Pause / resume playback; recording continues |
| `n` | Random next station under the current filter |
| `f` | Add / remove the selected station from favorites |
| `r` | Start / stop recording the current station |
| `+`, `-` | Volume |
| `/` | Incremental search; Enter confirms, Esc restores the previous search |
| `F` | Country, language, tag, and minimum-bitrate filters |
| `[` / `]` | Stations / favorites / recent / recordings categories |
| `gg` / `G` | First / last item in the list |
| `PgUp` / `PgDn` | Page through the station list |
| `?` | Open help |
| `q` / `Ctrl-C` | Finalize recordings and quit |

The focused panel uses a bright cyan bold border, a `▶` marker, and a number; the bottom bar shows the mode and the focused panel's actions. Mouse clicks switch panels and the wheel navigates. The top bar continuously shows the current station, codec, bitrate, volume, and recording status; the details panel shows country, language, tags, UUID, address, and more. Bitrate and codec are labeled as catalog data and unknown values are explicitly shown as unknown; the current playback info does not change with list selection.

Inside the TUI, `:sync` runs a background sync, `:sync-cancel` cancels the task, `:current` shows full details of the current station, and `:record --duration 30m` schedules a timed recording of the current station. Commands support quoting, Tab completion, and history; see the [TUI guide](docs/tui.md) for more commands and interaction design (in Chinese).

## Configuration & Data

Default paths on Linux:

Select the CLI error and list language with `--lang en` or `--lang zh-CN`.
Precedence is `--lang`, `RADIO_LANG`, config `language`, then `LC_ALL`/`LANG`;
unknown languages fall back to English with one notice. TUI text and clap
help migration follow separately.

Manage custom stations with `station list --all`, `station edit UUID --name NAME
--url URL`, and `station remove UUID`. Removing a referenced station preserves
its name for favorites, history, and recordings and clears its stream URLs;
`--keep-references` also keeps an unreferenced placeholder. Use `station backups
UUID --add URL --remove URL` to manage fallback URLs. Normalized duplicate URLs
are rejected with the existing UUID. TUI removal asks for a second confirmation.

Established live streams reconnect after unexpected EOF/errors at most three
times, after 1, 3, and 7 seconds. A reconnect keeps the same station and URL
and does not duplicate history or click reports. Stop, switching stations,
and exit cancel pending retries. Exhausted retries use the normal fallback
chain; initial connection failures retain their existing fallback behavior.

Runtime volume is remembered in SQLite. Set `resume = true` to restore the
last successfully played station and TUI source on startup (default: false).
The handwritten config file is never rewritten. Invalid saved volume values
fall back to the validated config setting; unavailable last stations are skipped.

Inspect local filter values with `countries`, `languages --prefix zh`, or
`tags --prefix jazz`; each value includes its station count. Search accepts
`--sort name|bitrate|votes|clickcount|random` (default: clickcount), combined
with all filters. Random ordering is reshuffled per query. The TUI `F` popup
loads local candidates in the background; arrows select and fill a value.

- Config: `${XDG_CONFIG_HOME:-~/.config}/radio-cli/config.toml`
- Data: `${XDG_DATA_HOME:-~/.local/share}/radio-cli/radio.db`
- Recordings: `recordings/` under the data directory

The config file is optional; see [config.example.toml](config.example.toml), pass it with `--config <file>`, and use `--data-dir <dir>` to isolate test data. Relative directories are resolved against the working directory at startup; the recording database stores absolute paths.

Radio Browser servers are discovered automatically with limited retries. Set `api_url`, or use `--api-url https://server` which takes precedence. HTTP requests honor the standard `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, and `NO_PROXY` environment variables; audio backends follow their own proxy support, so a reachable catalog does not imply a reachable audio stream. A catalog older than seven days triggers a refresh hint, never a silent full sync.

A single data directory allows only one playback / recording / TUI session; other CLI queries and favorite operations still work. Leaving the foreground stops the audio; for long-running use, run it inside tmux or systemd.

## Recording Behavior

- MP3 → `.mp3`; AAC (including common HLS AAC) → `.aac`; Vorbis → `.ogg`; Opus → `.opus`. The actual audio track is probed; the catalog's codec field is not trusted blindly.
- Stream copy is used: no re-encoding, no quality loss. Recording an unsupported codec fails with a clear error.
- Each recording has a JSON sidecar with the station, actually requested address, timestamps, codec, media duration, and completion status. The final CDN address after a source-side redirect may not be visible.
- A unique `.part` file is written first; the final file is produced after a normal stop. Interruptions / failures keep the `.part` file and its status, and unfinished records are recovered when a media session starts next. Use `recordings list` to inspect results; media duration may be an estimate.
- Switching stations in the TUI ends the current recording first; the new station requires pressing `r` again. Pausing playback does not pause recording. A live source ending early is not treated as a complete recording.
- `--duration` accepts seconds, `s`, `m`, and `h`, from 1 second to 365 days. Cut points follow audio packet boundaries; exact song boundaries cannot be guaranteed.
- Listening and recording use at most two concurrent upstream connections; the codec-probing connection is closed before the recording connection starts. One HLS media task may request multiple segments.
- Files stay local only; please comply with station terms and applicable personal-recording rules.

To recover a `.part` file, copy it to a new file first, then use FFmpeg stream copy according to the original codec. Do not overwrite the original file.

## Development & Validation

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
cargo test --release --test storage catalog_benchmark -- --ignored --nocapture
uv run scripts/measure_resources.py --binary target/release/radio
```

Tests use local HTTP/ICY/HLS sources and do not depend on public stations. Recording tests require FFmpeg; mpv IPC tests use a null audio output and can be pointed at a specific binary with `RADIO_TEST_MPV=/absolute/path/to/mpv`, skipping when mpv is not found. CI installs and runs the real mpv tests. Resource targets and results are in the [validation log](docs/validation.md) (in Chinese).

The first release does not provide a background service, time-shift caching, song recognition, lyrics, maps, or background full-catalog probing. The catalog comes from [Radio Browser](https://www.radio-browser.info/); audio connects directly to stations. RoamDial's private services are not used, and backup links are never auto-merged by name. See the [implementation notes](docs/implementation.md) for more design details (in Chinese).

Ctrl-C quits and finalizes recordings from every TUI mode. Esc cancels editing or closes a popup.
