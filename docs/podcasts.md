# Podcasts

**English** | [简体中文](podcasts.zh-CN.md)

radio-cli plays public RSS 2.0 and Atom audio enclosures. Podcasts have their
own subscriptions, history, progress and downloads, independent of the radio
catalog. No account is needed. Xiaoyuzhou shows work when they have a public
feed; sharing a platform page does not guarantee that a feed can be discovered.
Private account synchronization and paid-content access are not supported.

## Subscribe and listen

```sh
radio podcast discover https://example.org/show
radio podcast preview https://example.org/feed.xml
radio podcast add https://example.org/feed.xml
radio podcast list
radio podcast episodes FEED_ID
radio podcast play EPISODE_ID
radio podcast play EPISODE_ID --from-start --speed 1.25
radio podcast refresh
radio podcast unsubscribe FEED_ID
```

`discover` returns public feed URLs declared in a page, or resolves an Apple
Podcasts show link. With multiple results, choose a URL explicitly. `add`
subscribes without playing; `preview` makes no subscription changes. Lists use
`--limit` (1–1000, default 100), `--offset` and `--all`. Use
`radio podcast episodes FEED_ID --query "title"` or
`radio podcast list --query "show"` to filter local titles before pagination.
The TUI `/` search also searches all episodes in the current feed.

Search Apple Podcasts with `radio podcast search QUERY [--country CN]
[--limit 25]`. The country is the store region, not the feed language. Results
without `feedUrl` cannot be subscribed to. Directory availability never blocks
local subscriptions or downloaded playback. Artwork and audio come from the
feed after subscribing, rather than Apple media previews.

Natural completion marks a single episode played. Interrupted playback saves
its position; finished episodes replay from the beginning. Progress is submitted
every five seconds and on pause, switch, stop or shutdown. A persistence failure
is reported; abnormal process termination can lose the latest updates. Playback
resumes only when mpv supports seeking. If it cannot seek, playback stays paused
with an explanation; choose `--from-start` or download the episode first.

`radio podcast mark EPISODE_ID played|unplayed` changes the listening state.
Marking unplayed resets position to zero. Feed refresh retains episodes missing
from a newer feed. Changing an episode's declared length or duration clears its
completion flag while retaining its position.

## TUI

Sources includes Podcasts, Podcast downloads and Podcast queue. Enter opens a
show, then plays the selected episode. Esc returns to the show list. In a search
or discovery result, Enter previews the feed; Enter on the preview confirms the
subscription. `:podcast add URL` opens that same preview first.

Use the command line for all podcast actions, for example:

```text
:podcast search 知行小酒馆
:podcast refresh
:podcast download EPISODE_ID
:podcast downloads list
:podcast queue add EPISODE_ID
:podcast queue play
:podcast seek 12:30
:podcast speed 1.5
:podcast auto-next on
:sync-cancel
```

Space pauses or resumes. Alt-Left/Right seeks backward/forward 15 seconds during
podcast playback. `:podcast seek` accepts seconds or `HH:MM:SS`; seeking requires
mpv support. Speeds range from 0.5 to 3.0 and persist independently of live radio.
`/` filters show or episode titles locally, F5 reloads the current view, and
`:podcast refresh [FEED_ID]` fetches feeds. `:sync-cancel` also cancels the current
podcast command; pause/cancel individual downloads with the commands below.
Radio favorite and recording actions do not apply to episodes.

The existing `resume` setting also governs podcasts. `selection` restores the
last episode and its page without starting playback; `playback` resumes the last episode;
`disabled` starts without restoring playback.

## Downloads and queue

```sh
radio podcast download EPISODE_ID
radio podcast downloads list
radio podcast downloads pause EPISODE_ID
radio podcast downloads resume EPISODE_ID
radio podcast downloads cancel EPISODE_ID
radio podcast downloads remove EPISODE_ID
radio podcast queue add EPISODE_ID
radio podcast queue list
radio podcast queue move EPISODE_ID 1
radio podcast queue remove EPISODE_ID
radio podcast queue clear
radio podcast queue play --auto-next
```

CLI downloads run in the foreground. In the TUI, up to two downloads run in the
background; unfinished tasks resume at the next TUI launch. Paused, cancelled
and failed tasks need an explicit resume. Interrupted transfers resume only
with a resource validator and a valid Range response; otherwise they restart.
Completed files are checked with ffprobe and published from a `.part` file.
Downloaded media is preferred for playback; a missing file falls back to the
public enclosure. At least 200 MiB free space is reserved. No downloads or files
are removed automatically, including when unsubscribing.

Queue entries are explicit and deduplicated. Completion consumes the current
queue entry; automatic next playback is off by default. Enable it in the TUI
with `:podcast auto-next on`, or explicitly for a CLI session with `--auto-next`.
Errors and manual stops do not advance the queue. Playing an episode directly
does not consume a queued entry. The next episode is never added automatically.

## OPML and configuration

```sh
radio podcast import subscriptions.opml --dry-run
radio podcast import subscriptions.opml
radio podcast export subscriptions.opml
```

Import validates the complete OPML before fetching each source and reports each
result. Existing subscriptions are deduplicated. Partial failures do not remove
successful subscriptions. Cancellation stops the import immediately and keeps
subscriptions already imported. Export contains active subscriptions only, without
progress or downloads. Existing export files require `--overwrite`.

Optional TOML settings:

```toml
[podcasts]
# directory = "/path/to/podcasts"
refresh_hours = 6
```

Downloads default to `<data-dir>/podcasts`. Relative directories resolve from
the startup working directory. TUI refresh runs only while radio is running;
set `refresh_hours = 0` to disable automatic refresh. Failed sources back off
between retries, up to one day. No system service is installed.

Feed requests have bounded redirects, timeouts, document size (10 MiB after
decompression), nesting depth (64) and episode count (10,000). Oversized or
invalid responses retain cached episodes. Only HTTP(S) resources are supported;
XML DTDs are rejected. Dates, duration and enclosure length are optional feed
hints; duration shown during playback comes from mpv when available.

Use `--json-v2` for scripts; see the [JSON contract](json-contract.md).
