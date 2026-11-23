# Podcasts

**English** | [简体中文](podcasts.zh-CN.md)

radio-cli plays public RSS 2.0 and Atom audio enclosures. Podcasts have their
own subscriptions, history, progress and downloads, independent of the radio
catalog. No account is needed. Xiaoyuzhou shows work when they have a public
feed; sharing a platform page does not guarantee that a feed can be discovered.
Private account synchronization and paid-content access are not supported.

## Everyday use in the TUI

Choose Podcasts in Sources, open a show, and press Enter on an episode to play.
While listening, press **p** (or type `:podcast` / `:subtitles`) to open the
podcast action menu. It names the current episode and offers subtitles, local
caption generation, import, audio download, queue insertion and restart.
Choose with `j/k` and Enter, press the shown number, or click a row. Esc closes
it. You do not need to find or copy episode IDs.

**p operates on the playing episode, including while paused**, even if another
list row is selected. Use **P** explicitly for the selected episode in a
podcast episode, download or queue list. With no podcast playing, p can also
open the selected episode's actions. Targets stay fixed while a menu or import
prompt is open, even when the queue advances.
Choosing **Show subtitles** resumes that named episode if playback has moved
to another episode; showing captions for the current episode keeps its position
and pause state.

Common commands default to the current playing episode:

```text
:subtitles on
:subtitles transcribe
:subtitles import "episode captions.vtt"
:podcast download
:podcast queue add
:podcast play --from-start
:podcast mark played
```

The same default applies to `:podcast subtitles`, `:podcast queue remove`,
`:podcast mark unplayed`, and `:podcast downloads pause|resume|cancel|remove`.
Without a playing or paused podcast, these commands explain how to select one;
they never guess from listening history or apply to a radio station. A menu
import binds the path to the named episode; Escape cancels the prompt. Quote
paths containing spaces. Download lists show episode titles.

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

Press **D** in a podcast source, or run `:podcast browse`, to discover podcasts
without a search term. Choose popular shows, 19 categories, Surprise me, search,
change region or subscriptions. The p menu also exposes discovery during playback.
Categories load offline; Enter opens the region/category's Apple Podcasts chart.
Press **R** on a chart to sample its candidates, Enter to preview, and Esc to
return with the original results and cursor. Recognized local subscriptions are
labeled and open their episodes directly.

```text
:podcast browse
:podcast categories --country US
:podcast charts --country US
:podcast charts --country US --category technology
:podcast charts --country US --category 1318 --random --limit 10
```

CLI users can replace the colon with `radio `. Categories accept the IDs or
slugs listed by `categories`. Region precedence is the command option, then
`podcasts.country`, then CN for Chinese or US for English. Charts display authors,
categories and original ranks. Surprise me samples up to 100 chart candidates;
it is not a random sample of all podcasts or a personalized recommendation.
Charts are cached for 10 minutes in the current process. F5 reloads the current
directory and resamples random charts. Page Up/Down moves within directory rows.
Use `?` for network/rate-limit errors; Esc returns using session snapshots.

Use the command line for all podcast actions, for example:

```text
:podcast search 知行小酒馆
:podcast refresh
:podcast download
:podcast downloads list
:podcast queue add
:podcast queue play
:podcast seek 12:30
:podcast speed 1.5
:podcast auto-next on
:sync-cancel
```

Space pauses or resumes. Alt-Left/Right seeks backward/forward 15 seconds by default during
podcast playback. `:podcast seek` accepts seconds or `HH:MM:SS`; seeking requires
mpv support. Speeds range from 0.5 to 3.0 and persist independently of live radio.
`/` filters show or episode titles locally, F5 reloads the current view, and
`:podcast refresh [FEED_ID]` fetches feeds. `:sync-cancel` also cancels the current
podcast command and stops the view's periodic reload; F5 loads it again.
Pause/cancel individual downloads with the commands below.
In podcast lists, f toggles Save for later and F opens episode filtering.
Radio recording actions do not apply to episodes.

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
Downloaded media is preferred when its enclosure URL, declared size and
duration match the current episode. Missing or outdated downloads fall back to
the public enclosure. Downloads from older versions without a media fingerprint
also need downloading again before local playback. An explicit download replaces
an outdated file and restarts an outdated partial transfer. At least 200 MiB free
space is reserved. Optional per-podcast cleanup runs separately from refreshing
or unsubscribing; it is disabled by default.

Queue entries are explicit and deduplicated. Completion consumes the current
queue entry; automatic next playback is off by default. Enable it in the TUI
with `:podcast auto-next on`, or explicitly for a CLI session with `--auto-next`.
Errors and manual stops do not advance the queue. Playing an episode directly
does not consume a queued entry. The next episode is never added automatically.

## Synchronized subtitles

In the TUI, `:subtitles on` loads timed captions linked by the RSS
`podcast:transcript` tag. Supported formats are UTF-8 WebVTT and SRT; untimed
HTML/plain text cannot be synchronized. Refresh an existing subscription once
to discover its caption links. Captions follow mpv's media position, including
resume, pause, seeking and speed changes. No speech model is needed for supplied
captions. CLI playback remains audio-only.

For the current podcast, press p and choose **Generate subtitles**. This checks
the local runtime/model, then generates captions in 15-second audio windows.
Each recognized segment appears immediately, without waiting for a full download
or whole-episode recognition. A complete, matching download is reused; otherwise
the audio URL is read directly in bounded windows. The action is explicit;
`:subtitles on` only displays supplied or cached captions and never starts a
large audio download or recognition job. Set up the runtime and model first;
see [subtitle setup](configuration.md#live-subtitles).

```text
:subtitles transcribe
:subtitles import "episode captions.vtt"
:subtitles fetch
:subtitles cancel
```

`transcribe` prioritizes the playing position and then works ahead. Seeking
changes the next window to recognize; the current window finishes first. The
model stays loaded across windows. Completed windows, including silence, are
saved separately as partial coverage and reused after cancellation or restart.
Earlier gaps are filled after reaching the end. Only complete coverage replaces
the full cache; previous supplied/imported captions stay on disk until then.

The first captions still need model loading and recognition. A slow CPU, faster
playback, or a server without efficient seeking can leave captions behind.
Generation does not pause playback. If remote window reads fail, cancel the
caption task, download the audio, then retry. There is no automatic full download
in the TUI. The separate CLI `--transcribe` command keeps its full-download,
whole-episode workflow.

`import` copies a local UTF-8 VTT/SRT file, which is no longer needed afterwards. `fetch` uses cached
captions or fetches the feed's supported captions. ASR timestamps describe
speech segments, not words; accuracy depends on the model and audio.

Caption work runs separately from browsing. Switching podcast pages leaves it
running and playback controls stay responsive. `:subtitles cancel` cancels only
the caption task; `:sync-cancel` cancels caption and ordinary podcast commands.
Completion names the processed episode and reloads its timeline if it is still
playing. A task for an earlier episode never replaces the current episode's
captions.

Subtitles appear in **Details** with wrapped text and the current segment in
bold. Press `3` to open Details on a narrow terminal. With Details focused,
`v` switches between captions and ordinary information; `j/k`, arrows and
Page Up/Down scroll without following playback. `End` resumes following the
current segment. The panel and bottom strip indicate manual reading; press `3`
then `End`, or run `:subtitles follow`, to resume automatic scrolling.
Alt+Left/Right changes playback time while preserving the current following
or manual reading mode. Backfilled captions keep your manual reading position;
changing episodes or starting a new playback session restores following.
`:subtitles panel` switches views, and `:subtitles follow`
opens captions and resumes following. The bottom strip uses one status row.
Live radio uses the same panel but follows the latest recognized speech; its
capture timestamps remain independent of playback.

`:subtitles history` opens the whole caption timeline with media timestamps.
`:subtitles clear` clears the displayed timeline without deleting the saved
cache; `:subtitles retry` reloads it. Caches live under
`<data-dir>/podcasts/transcripts`, independent of the audio download directory.
Partial ASR files use `.partial.json` and record recognized windows separately
from complete captions. Generation resumes these windows only when the media,
model, language and segment settings still match.
They are tied to the enclosure URL, declared size and duration, and supplied
captions are also tied to their feed URL. Changed metadata invalidates a cache.
A server replacing audio or inserting ads without changing those hints cannot
be detected reliably: import matching captions or regenerate from the download.
Files are limited to 10 MiB and 100,000 segments. Captions are not translated,
and playback does not wait for recognition or fetches.

## OPML and configuration

```sh
radio podcast import subscriptions.opml --dry-run
radio podcast import subscriptions.opml
radio podcast export subscriptions.opml
```

Import validates the complete OPML before fetching each source and reports each
result. Existing subscriptions are deduplicated. Partial failures do not remove
successful subscriptions. Cancellation stops subsequent commits and keeps
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

## Identifying episodes in scripts

For CLI commands that target an episode, supply its ID or the exact titles as
`"Show Title::Episode Title"`. The same forms work for an explicit target in
the TUI.

```sh
radio podcast episodes FEED_ID --json-v2
radio podcast subtitles EPISODE_ID --transcribe
radio podcast subtitles EPISODE_ID --file episode.vtt
```

Use the `items[].id` field from `podcast episodes` for scripts. Feed IDs come
from `podcast list` or `podcast add`. Interactive use should prefer the menus
and current-episode commands above.

## Navigation and local collections

Press `'` twice (or your configured `jump_to_playing` binding), or run
`:podcast current`, to open the playing or paused episode in its podcast and
select it in the list. On a narrow terminal, the list replaces Details until
you press Esc or switch panels. Esc restores the previous focus and browsing
context, including search and pagination. The chosen subtitles or information
view and subtitle reading position carry across the jump. The information
view's scroll position resets on navigation.
Esc from a podcast also restores the subscription list's previous
position. Online results and previews are kept for the session, so returning
restores their cursor and scroll without networking.
Playback markers remain visible on the parent podcast as well as the episode,
independently of the cursor, including paused, connecting and failed states.

The p menu targets playback; P targets the selected episode. Menus scroll with
j/k or the mouse; Enter selects an action, and numbers select the first nine.
In podcast sources without playback, p opens podcast management and collection
entries, including search, RSS subscription and OPML import/export. Unsubscribe,
audio deletion and clearing the queue require confirmation.

```text
:podcast library all
:podcast library inbox
:podcast library continue
:podcast library history
:podcast library saved
:podcast library all --feed FEED_ID --status unplayed --sort oldest
:podcast library all --downloaded --query title
:podcast save
:podcast save --selected
:podcast save EPISODE_ID --remove
:podcast dismiss EPISODE_ID
```

The inbox contains episodes discovered after subscription; initial subscription
does not import the entire backlog. Dismissing an inbox entry leaves progress
unchanged. History lists each episode once. Status accepts
`all|unplayed|progress|played`; sorting accepts `newest|oldest|title`. Filtering
and sorting precede pagination.

Omitted episode IDs retain their current-playback meaning. TUI `--selected`
explicitly targets the selected row. Press x to select episodes across pages,
X to mark them played, or U to mark them unplayed, then confirm the fixed target
list. Unplayed resets progress. Scripts can use
`radio podcast mark-batch ID1 ID2 [--unplayed]`.

Podcasts accept exact titles in place of IDs. Episodes accept the quoted form
`Podcast title::Episode title`; ambiguous matches list candidate IDs.
`radio podcast resume` resumes the last podcast without falling back to radio.

## Queue, chapters, bookmarks and sleep

Enter in the queue starts queue playback from the selected episode, preserving
earlier entries. Continuous playback remains off by default. The menu exposes
Play Next, removal, movement and continuous playback. Natural completion consumes
the queue entry; errors and manual stops do not advance. Alt-Left/Right uses
relative seeking so repeated presses accumulate.

```text
:podcast queue next EPISODE_ID
:podcast queue start EPISODE_ID
:podcast auto-next on
:podcast chapters
:podcast bookmarks
:podcast bookmark --note "Listen again"
:podcast bookmark EPISODE_ID --time 12:30 --note "Key point"
:podcast bookmark-edit BOOKMARK_ID --note "Updated note"
:podcast bookmark-edit BOOKMARK_ID
:podcast sleep 30
:podcast sleep +15
:podcast sleep end
:podcast sleep off
```

Chapters support Podcasting 2.0 JSON with fallback to embedded Podlove Simple
Chapters. Enter seeks to a chapter or bookmark; seeking within the paused episode
keeps it paused. Bookmarks survive download removal and changes to played status.

Sleep timers use elapsed wall time, including time spent paused. Expiry pauses
and saves progress; `end` prevents queue advancement at episode completion.
Switching media, stopping or quitting cancels the timer; it is not restored.

## Per-podcast settings and download policies

Use the podcast menu or `radio podcast settings FEED_ID [KEY VALUE]`. Omitting
key and value shows settings. Nullable settings accept `inherit`.

| Setting | Default and behavior |
|---|---|
| `speed` | Global podcast speed; range 0.5–3.0 |
| `sort` | `newest`; also `oldest` or `title` |
| `skip_back` / `skip_forward` | 15 seconds each |
| `skip_intro` / `skip_outro` | 0 seconds, disabled |
| `completion_seconds` | 0, disables near-end marking |
| `auto_download` | `false`; downloads subsequently discovered episodes |
| `auto_cleanup` | `false`; removes played managed audio |

```text
:podcast settings FEED_ID speed 1.25
:podcast settings FEED_ID sort oldest
:podcast settings FEED_ID skip_forward 30
:podcast settings FEED_ID completion_seconds 30
:podcast settings FEED_ID auto_download true
:podcast downloads stats
```

Settings apply on subsequent playback or entry to a podcast. Change current
speed with the menu or `:podcast speed`. Explicit playback speed overrides the
podcast setting, which overrides the global default. Intro skipping does not
replace a resume position; explicit restart begins at zero. Outro skipping
requires a known duration and acts as natural completion. Optional near-end
marking requires actual playback through the tail; a seek alone does not mark
an episode or consume its queue entry.

Automatic downloads do not backfill or enqueue episodes. Cleanup preserves
progress and bookmarks and defers files used by playback, transcription or the
queue. Automation runs only while the client is running.

Downloads use FIFO scheduling with two workers. Transient network errors get up
to three additional attempts, delayed 5, 30 and 120 seconds, honoring a longer
server retry delay. Paused, cancelled, insufficient-space and invalid-media
failures need manual intervention. Percentage and transfer speed appear when
known; unknown totals remain byte counts. `podcast downloads stats` reports
recorded bytes and available space.

Refresh and OPML import use at most four concurrent network requests. Browsing
does not cancel them. Per-podcast failures do not block other subscriptions and
are visible in the list, details and activity log. Temporary redirects retain
the subscription entry URL; conflicting migrations report an error without
merging listening records. Directory rate limits fail promptly with a delay
notice, leaving local functions available.

CLI output without `--json` is readable text; interactive download progress uses
stderr. Directory region precedence is explicit `--country`, `[podcasts].country`,
then CN for Chinese or US for English.
