# JSON output and process exits

Use global `--json-v2` for a consistent machine-readable response. On success,
stdout contains exactly one JSON value: `{"ok":true,"data":...}`. Commands
without a result use `null`. On failure it contains
`{"ok":false,"error":{"code":"...","message":"...","params":{...}}}`.
Progress goes to stderr. Error codes and parameter names are stable across
English and Chinese; messages follow the selected language. Unclassified I/O
errors use `general_error`. Do not parse the message text.

`--json-v2` requires an explicit noninteractive command. It conflicts with
`--json`. Explicit `--help` and `--version` retain their standard text output.
Usage failures, including unknown commands, return the error envelope when
`--json-v2` is present. Failed commands never emit a success envelope first.

| Exit | Meaning |
| --- | --- |
| 0 | Success, including help and version |
| 1 | Operation, configuration, network, storage or media failure |
| 2 | Invalid command-line usage |
| 3 | Required executable could not be started |
| 4 | Another media session owns the runtime lock |

Legacy `--json` remains available in 0.7.0. Its success shapes are:

| Command | Legacy data shape (also the `data` of v2) |
| --- | --- |
| `search` | Station array; preserved for existing scripts |
| `info`, `station add`, `station edit` | Station object (`info`/`edit` always JSON) |
| `favorites list`, `history`, `station list`, `recordings list` | Page object: `items`, `total`, `offset`, `limit`, `has_more` |
| `countries`, `languages`, `tags` | Array of `{value,count}` |
| `favorites add/remove` | `{stationuuid,name,favorite}` |
| `station remove` | `{stationuuid,removed,placeholder_kept}` |
| `station backups` | URL array |
| `sync` | `{stations,last_sync}` |
| `record` | Array containing the most recent recording |
| `recordings recover` | `{warnings}` |
| `recordings remove` | `{id,removed,index_only}` |
| `devices` | Array of `{name,description}` from the configured mpv |
| `doctor` | Object with `checks`, `config`, `database` |
| `play`, `random`, `recordings play/open` | No legacy JSON result; v2 `data:null` |

Page `limit` is `null` with `--all`; otherwise it is the requested page size.
`offset` is zero-based. JSON preserves user strings, including Unicode;
terminal control characters are sanitized only for human-facing output.
Legacy failures print a diagnostic on stderr and use the same exit codes.
Legacy doctor/recovery/recording may print their diagnostic data before a
nonzero exit. Prefer v2 when an atomic success/error response is required.

Examples:

```sh
radio --json search jazz                  # legacy array
radio --json-v2 search jazz               # {"ok":true,"data":[...]}
radio --lang en --json-v2 info missing    # station_not_found; exit 1
```

`completions <shell>` and `man` return generated text normally; with `--json`
it is a JSON string, and with `--json-v2` it is the envelope's `data` string.
Generation reads configuration only to select the language; it does not create
data directories or open SQLite.

Recording objects include `reconnects`, the number of HTTP retry attempts
reported by FFmpeg. A positive count means live audio may have gaps even when
`status` is `complete`; old metadata without this field reads as zero.

Station objects include `favicon`, the catalog's optional station-logo URL.
It defaults to an empty string for older stored rows and custom stations.
Refresh the catalog with `sync` to populate existing catalog stations.

`info`, `station edit/remove/backups`, and `recordings remove` always produce
JSON, even without a JSON flag. For station removal, `removed:true` means the
custom station was removed; `placeholder_kept:true` explicitly indicates that
an inactive name-only row remains for references or `--keep-references`.
Malformed equals forms such as `--json-v2=true` are usage errors and also
receive the v2 error envelope (the option itself is a valueless flag).

Recording objects may include `track`, the title captured when recording was
requested during playback. It is omitted when unknown, including old records
and standalone CLI recordings. It remains unchanged when the track changes.

Configured audio-output failures return `audio_device_failed` with a `device`
parameter (exit 1). Local player startup errors retain their original codes,
such as `missing_dependency` (exit 3), `player_startup` (exit 1), or
`normalization_unavailable` (exit 1). These failures end the play request without
stream fallback or directory refresh; the error code survives normal shutdown.
