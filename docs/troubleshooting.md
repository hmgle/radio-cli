# Troubleshooting

**English** | [简体中文](troubleshooting.zh-CN.md)

Start with the environment and the built-in checks:

```sh
radio --version
radio doctor
radio --json doctor
```

`radio doctor` checks the configured mpv, FFmpeg, and ffprobe executables, the
recording directory's writability, and free space. The JSON form also reports the
effective config and database paths. It does not test a station stream, the
audio output, or general internet access.

## Missing media tools

Install mpv and FFmpeg, including ffprobe, through your package manager. Then
run `radio doctor` again. If the tools are installed in a nonstandard location,
set `mpv`, `ffmpeg`, or `ffprobe` to the executable path in the config file. A
missing required executable exits with status 3.

```toml
mpv = "/opt/homebrew/bin/mpv"
ffmpeg = "/opt/homebrew/bin/ffmpeg"
ffprobe = "/opt/homebrew/bin/ffprobe"
```

## Catalog is empty or stale

With no local catalog, the TUI and `radio random` bootstrap the 1,000 most popular
stations. Run `radio sync` to fetch the full catalog:

```sh
radio sync
radio countries --prefix CN
```

A failed or cancelled sync keeps the previous catalog. If a sync succeeds but
reduces the catalog by more than 25%, radio-cli treats the result as suspicious
and asks you to confirm with `radio sync --accept-shrink`. Use that option only
when you expect the upstream catalog to shrink substantially.

In the TUI, run `:sync`; use `:sync-cancel` to stop a running sync without
replacing the current catalog.

## Sync or artwork requests cannot reach the network

Catalog and artwork requests use the standard `HTTP_PROXY`, `HTTPS_PROXY`,
`ALL_PROXY`, and `NO_PROXY` environment variables. Check those variables if
other HTTP clients work but `radio sync` does not.

If the default Radio Browser discovery is unavailable, select a compatible
server explicitly:

```sh
radio --api-url https://de1.api.radio-browser.info sync
```

or set `api_url` in the config file. Audio does not use the catalog HTTP client:
mpv connects directly to the station and follows its own proxy settings. A
working `radio sync` therefore does not prove that a stream is reachable.

## Playback fails or there is no sound

Check mute, pause, and volume first, then list the devices exposed by mpv:

```sh
radio devices
```

Set `audio_device` to one of those names, or remove it to use automatic
selection. Device names can change after hardware is reconnected.

Test the station URL directly in mpv. If the stream works in mpv but not inside
radio-cli, check `radio doctor`, the configured `mpv` path, and whether
`normalize_loudness` is enabled. A missing normalization filter is reported
separately; disable the setting or install an mpv build that supports
`dynaudnorm`.

Local player failures are not repaired by switching stations. Missing
executables, player startup timeouts, unavailable normalization filters, and
configured audio-device failures end the current request without trying backup
URLs, random stations, or a directory refresh. Resolve the local problem and
start playback again.

For network stream failures, radio-cli tries backup addresses, bounded live
reconnects, and a catalog refresh as appropriate. Reconnects can leave gaps in
the audio even when the request eventually recovers. A finite or flapping source
may still fail after retries are exhausted.

## Another media session owns the data directory

Only one playback, recording, or TUI media session can use a data directory at a
time. This prevents competing app processes from controlling the same runtime
lock and recording state. The failure uses exit status 4.

Close the other session, or isolate a concurrent manual session with another
data directory:

```sh
radio --data-dir /tmp/radio-second tui
```

One-shot catalog queries and favorite operations can still open the database
while a media session is running.

## Configuration changes do not apply

Check the effective path:

```sh
radio --json doctor
```

Then check that:

- `--config` points to the intended file. An explicitly selected file must exist.
- The file is valid TOML.
- Every key is known. radio-cli rejects unknown fields so misspellings do not
  silently do nothing.
- Values are within their documented ranges.

The config is read at process startup and is never rewritten. Restart radio-cli
after editing it. TUI changes made with `:set language` or `:set mouse` apply only
to the current session. Runtime volume and the last-station preference live in
SQLite, not in the config file.

See the [configuration reference](configuration.md) for all keys and constraints.

## Recording fails or remains incomplete

Confirm that FFmpeg and ffprobe are available and that the destination is on a
writable filesystem with enough free space:

```sh
radio doctor
radio recordings list
```

radio-cli records only MP3, AAC, Vorbis, and Opus streams by stream copy. An
unsupported codec fails cleanly. A live source that ends early, exhausted
reconnects, a disk-full condition, or an invalid audio track can leave the
recording `interrupted` or `failed` while retaining the `.part` file and error
diagnostics.

For pending finalization, keep all files in place and run:

```sh
radio recordings recover
```

On macOS, publication requires a filesystem with hard-link support. Do not
delete the `.part` file before recovery. If you recover it manually, copy it
first and then stream-copy or remux it according to the original codec.

## Artwork is missing or disappears

Run `radio sync` so existing catalog rows receive `favicon` metadata. Station
artwork is optional, and custom stations may not have a logo. Track artwork also
requires a nonempty current title and a usable ICY `StreamUrl` image URL.

Images are hidden for the monochrome theme, terminals smaller than 100×24, or
when the details panel has insufficient space below its text. tmux always uses
Unicode half blocks. Some Sixel terminals, including xterm, lose an image after a
popup or text overwrites it; close and reopen details to redraw it.

See [Artwork](configuration.md#artwork) for cache and protocol details.

## Text selection, colors, or terminal layout is wrong

Set `mouse = false` or run `:set mouse false` to stop capturing mouse events and
use the terminal's native text selection. A nonempty `NO_COLOR` environment
variable forces the mono theme. Use `theme = "light"` on a light terminal
background if the default theme has poor contrast.

The TUI adapts to narrow and short terminals. When space is very limited, some
panels collapse or details move to a shared area. The built-in `?` help lists the
keys available in the current context. Use a larger terminal if controls or
labels do not fit.

## macOS blocks the downloaded executable

The macOS archives are not Developer ID-signed or notarized. Verify the archive
against `SHA256SUMS`, extract it, and only then remove quarantine from the
binary if you trust the release:

```sh
xattr -d com.apple.quarantine ./radio
```

See [Apple's guidance](https://support.apple.com/en-us/102445). Building from
source is also supported with Rust 1.88 or newer.

## Language is not the expected one

Language precedence is `--lang`, `RADIO_LANG`, config `language`, `LC_ALL`,
`LANG`, then English. An unsupported value produces a notice and falls back to
English. Use `--lang en`, `--lang zh-CN`, or `radio --lang zh-CN tui` to test a
specific locale. Inside the TUI, `:set language en|zh-CN` changes the current
session.

Country/region names in the Chinese interface use a pinned CLDR table. Missing or
unrecognized upstream codes fall back to the catalog name or code. Storage and
JSON keep the upstream values.

## Scripts receive unexpected output

Runtime status and diagnostics go to stderr. `radio info` always produces JSON.
Use `--json` for legacy shapes or `--json-v2` when exactly one success or error
envelope is required:

```sh
radio --json-v2 search jazz
radio --json-v2 info <UUID>
```

Do not parse localized message text; use the stable error code in the JSON
envelope. See the [JSON contract](json-contract.md).

| Exit | Meaning |
| --- | --- |
| 0 | Success, including help and version |
| 1 | Operation, configuration, network, storage, or media failure |
| 2 | Invalid command-line usage |
| 3 | A required executable could not be started |
| 4 | Another media session owns the runtime lock |

## Still stuck

Collect `radio --version`, operating-system details, `radio doctor` output, the
exact command or TUI keys, and relevant stderr. Redact stream credentials or
private tokens before opening an issue. Security problems should use
[SECURITY.md](../SECURITY.md), not a public issue.
