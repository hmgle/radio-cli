# Recording guide

**English** | [简体中文](recording.zh-CN.md)

radio-cli records the selected audio stream with FFmpeg using stream copy. It does
not re-encode, transcode, or intentionally alter audio quality. Recording works
from the TUI or as a standalone CLI command.

## Starting and stopping

```sh
radio record <UUID>
radio record <UUID> --duration 30m --output-dir ./recordings
```

The standalone command runs until `Ctrl-C` or the requested duration. Progress
and the output path are written to stderr. In the TUI, press `r` to start or stop
recording the currently playing station. Playback must have reached the playing
or paused state first; wait for a connecting stream to start. Direct temporary
URLs cannot be recorded until saved as a custom station.

`--duration` accepts plain seconds and values with `s`, `m`, or `h`, from 1
second to 365 days. A duration is a stop deadline, not a promise that the source
will supply that much audio. Cut points follow audio packet boundaries, so an
exact song or program boundary is not guaranteed.

Pausing playback does not pause recording. Switching stations in the TUI ends the
current recording; press `r` again to record the new station.

## Formats

radio-cli probes the actual audio track with ffprobe before starting FFmpeg. It
does not blindly trust the catalog's codec field.

| Probed codec | Container / extension |
| --- | --- |
| MP3 | MP3 / `.mp3` |
| AAC, including common HLS AAC | ADTS AAC / `.aac` |
| Vorbis | Ogg Vorbis / `.ogg` |
| Opus | Ogg Opus / `.opus` |

An unsupported codec fails before a recording is started. Recording remains a
stream copy even when `normalize_loudness` is enabled; that setting affects
playback only.

## Files and naming

Every recording is written first to a unique `.part` file in the target
directory. When the recording ends normally, radio-cli verifies the audio track,
publishes the final file, and writes a JSON sidecar. Final files never overwrite
an existing file.

The default directory is `recordings/` under the data directory. Override it with
`recording_dir` or `--output-dir`. Relative paths are resolved from the working
directory at startup.

The filename includes the station name and, when recording started from an active
playback session, the track title known at that moment. Later track changes never
rename the file. Unknown titles use the station-only naming form. Unsafe filename
characters are removed, whitespace between track words becomes underscores, and
components are bounded by both character and byte length.

Each completed or failed recording has a sidecar next to its media file. It
contains the station, the address that radio-cli requested, timestamps, codec,
media duration, completion state, reconnect count, and error diagnostics when
available. A source-side redirect may hide the final CDN address.

## Status and recovery

The recording database tracks states such as `recording`, `complete`,
`interrupted`, `failed`, and `finalizing`. Use the following commands to inspect
and manage them:

```sh
radio recordings list
radio recordings play <ID>
radio recordings remove <ID>
radio recordings remove <ID> --index-only
radio recordings open
```

`recordings remove` deletes the indexed media file and sidecar unless
`--index-only` is used. `recordings open` opens the configured recording
directory in the platform file manager.

An interrupted or failed recording keeps its `.part` file and diagnostic state.
`radio recordings recover` retries publication of recordings left in a pending
finalization state. It does not turn an interrupted live capture into a complete
recording.

On macOS, publication requires a filesystem that supports hard links. If
publication is interrupted, keep the files in place and run
`radio recordings recover` before trying manual cleanup.

To recover a `.part` file manually, copy it to a new filename first, then remux
or stream-copy it with FFmpeg according to the original codec. Do not overwrite
the only copy of the partial file.

## Reconnects and incomplete captures

Truncated HTTP/ICY connections are retried with bounded backoff, and HLS segment
re-fetch failures count as reconnects. A reconnect can leave a gap in live audio.
A completed recording retains the reconnect count and displays a warning because
the media may contain gaps.

If retries are exhausted, radio-cli retains the partial file and failure
diagnostics. A live source that ends early is not treated as a complete
recording, and a finite source is never replayed in a loop to fill the requested
duration.

Listening and recording use at most two concurrent upstream audio connections.
The codec-probing connection is closed before the recording connection starts.
One HLS media task may request multiple segments. Track-artwork ICY probes share
this audio-connection budget and yield before recording starts. Artwork image
downloads and catalog HTTP requests are outside the budget.

## Disk space

Before recording, radio-cli checks the destination filesystem. It prefers the
probed audio bitrate, then the container bitrate, then the catalog bitrate to
estimate the required size. The configured `recording_min_free_mb` reserve is
added to that estimate.

If duration or bitrate is unknown, only the reserve can be checked and the total
size is reported as unknown. The preflight cannot guarantee that the disk will
not fill during a long recording, so monitor the destination when free space is
tight. See the [configuration reference](configuration.md#recording-settings)
for the related settings.

## Privacy and compliance

Recordings stay on the local machine. Check the station's terms and the rules
that apply to recording broadcasts in your jurisdiction before redistributing a
recording.
