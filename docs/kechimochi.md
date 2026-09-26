# Kechimochi sync

Nagare can automatically mirror its saved media history into a running
[Kechimochi HTTP API](https://github.com/Morgawr/kechimochi/blob/main/docs/http-api.md).
The sync runs on the Nagare server, so the Nagare browser page can be closed.

## Setup

1. Enable **HTTP API** in Kechimochi's profile settings. The `automation` scope
   provides the media and activity endpoints Nagare uses.
2. In Nagare, open **Settings → Kechimochi** and enter the API base URL.
   `http://127.0.0.1:3031` works when both applications run on the same computer.
   When Nagare runs in Docker or on another computer, enable Kechimochi's LAN
   access and use the Kechimochi computer's LAN address instead.
3. Use **Test connection**, then enable automatic sync. Settings save automatically.

The first enabled check backfills the entire saved history. Subsequent checks
run every five minutes by default; choose an interval from 1–1440 minutes or a
daily time. The selected IANA time zone controls both activity dates and daily
scheduling. Daylight saving transitions are handled automatically. A missed
daily run catches up after Nagare restarts. You can also use **Sync now** while
automatic sync is paused.

Both Nagare and Kechimochi must be running and able to reach each other for a
sync to complete. If Kechimochi closes or becomes unreachable, Nagare retains
successful writes and retries after 30 seconds, then increases the delay up to
one hour. The next successful pass catches up without manual review.

## What is mirrored

Every entry in Nagare's saved history is included, regardless of its age,
language, completion percentage, or Tadoku approval/decline state. Nothing is
sent to Tadoku by this integration.

| Nagare data | Kechimochi representation |
| --- | --- |
| Series, media server, media category, language | A grouped media entry in a Nagare variant |
| Individual episode, movie, or audiobook with positive recorded progress | One activity log per watch, including confirmed rewatches |
| History without recorded playback | Stored with the media; creates an activity when playback is recorded |
| Saved playback position | Activity duration, capped at runtime and rounded to the nearest whole minute, with a one-minute minimum for positive progress |
| Latest observed playback timestamp | Activity date in the configured time zone |
| Video / AudioBookShelf | Watching / Listening activity |
| Full history metadata, exact position, runtime, subtitle count, mined-note count | Media's `extra_data.nagare.history` |
| Title, progress in seconds, mined-note count | Managed section of the activity's notes |

**Playback progress is not elapsed watch time.** Nagare observes all permitted
clients, independently of the player selected for subtitle mining. A paused or
stalled tab does not overwrite another client's advancing playback. Each watch
has its own saved position and latest playback timestamp. Sync mirrors those
values; it does not measure daily immersion time. Kechimochi
requires positive whole-minute durations for these activities, so positive
sub-minute progress becomes one minute. Items with zero progress remain in the
media metadata without inventing activity time. If progress resets to zero,
the previous managed log is removed and its media history remains.

After a completed item is restarted below 80% and more than a minute behind its
saved position, **five consecutive real minutes of advancing playback** confirm
a rewatch. Playback speed cannot shorten those five minutes. Pauses, backward
seeks, implausible forward jumps, and gaps of more than 25 seconds without
progress restart qualification. Repeated server positions between normal
check-ins do not add credit by themselves. Client changes and Nagare restarts
cannot combine separate qualification streaks.

Until confirmation, the completed watch keeps its progress and date. Once
confirmed, the new watch gets a separate activity log and the previous watch is
preserved. The new watch must reach the usual completion threshold before
Tadoku offers it as a completed episode. Qualified watch history survives
restarts; watches from before this feature cannot be reconstructed.

Media identity is separated by server, category, and language to avoid merging
different records that happen to share a title. Video paths containing `anime`
are categorized as Anime; other series as TV Series and one-off videos as Movie.
AudioBookShelf entries are Listening / Audiobook. A matching target audio language
is preferred, then the first reported audio language; missing tags fall back to
Nagare's target language. These classifications do not exclude any history.

Anki notes, audio clips, screenshots, and subtitle text remain in Nagare/Anki.
Kechimochi receives immersion records and their metadata, including subtitle and
mining counts; it does not have Nagare's mining or subtitle-review interface.

## Keeping records in sync

Each run reads the current Nagare snapshot and the current Kechimochi media/log
lists before changing anything. Unchanged objects are left alone. Progress,
dates, titles, language/category, and mining counts update existing records.
A series rename keeps its media identity and cover when its remaining history
still forms a single group.

Remote records carry a stable identifier for this Nagare database. This allows
Nagare to recover a successful creation even if the HTTP response was lost or
Nagare restarted before recording the result. Concurrent manual and scheduled
requests share a single run. Successful items remain synced when another item
fails; subsequent passes retry the remaining differences.

- Removing an entry from Nagare's saved history removes its managed activity log.
- Deleting a managed record in Kechimochi recreates it on the next pass while it
  still exists in Nagare. Pause automatic sync to stop this behavior.
- User-created media/logs and records owned by another Nagare database are never
  adopted or removed just because their titles match.
- Covers, descriptions, status, milestones, and custom media metadata are
  preserved during normal updates. Personal log notes can be appended after
  the `[/nagare]` marker.
- Empty former Nagare groups are deleted only after a successful reconciliation,
  and only if they contain no remaining activity or milestones. Other groups are
  retained and counted in the status report.
- Changing the API URL uses a separate scheduling/retry history and discovers
  ownership afresh. Old destination records remain there. Numeric IDs from one
  destination are never applied to another.

Keep the Nagare database when upgrading or moving machines: its stable instance
ID is what identifies its remote records. Two processes should not run against
the same Nagare database at once. Keep ownership markers intact. A conflict or
malformed remote record is reported in **Sync status** rather than overwritten
through a title-based guess.

## Status and API

The settings page reports the last successful sync, next scheduled check,
history count, additions/updates/deletions, and any errors. Status and retry
state persist in SQLite. Timestamps in the status display use the browser's
local time; the configured sync time zone still controls the schedule and
activity dates.

Nagare exposes these endpoints:

- `GET /api/kechimochi/status`: current run state, report, and next check.
- `POST /api/kechimochi/test`: read-only version/media/log compatibility check.
- `POST /api/kechimochi/sync`: start a background sync, or return the existing
  running state. Poll the status endpoint for completion.
- `GET` / `PUT /api/config`: the `kechimochi` section contains `enabled`,
  `api_url`, `sync_mode` (`automatic` or `daily`), `interval_minutes`,
  `daily_hour`, `daily_minute`, and `timezone`.

The defaults leave this integration disabled on an unconfigured installation.
There is no Kechimochi password to store for the local automation API.
