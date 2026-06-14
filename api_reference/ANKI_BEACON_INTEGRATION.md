# AnkiBeacon Integration Guide

This document is for apps, services, and coding agents that want to consume
events from the AnkiBeacon add-on without polling Anki continuously.

The current release emits `note_added` and `heartbeat` events. The add-on
package/folder name remains `new_card_created`, but the public project name is
AnkiBeacon so the protocol can grow to more event types later.

## Goal

Use push delivery when Anki is alive.
Use polling only as a fallback when heartbeats disappear.

## Transport

The add-on sends HTTP `POST` requests with JSON bodies.

- Each configured operation owns its own `urls`.
- Note-created events go to the `note_added` operation URLs.
- Heartbeats go to the `heartbeat` operation URLs.
- If the `heartbeat` operation has empty `urls`, it uses `fallback_operation`
  to borrow URLs from another operation, normally `note_added`.

## Event Types

Receivers should inspect `event`.

- `heartbeat`: Anki is running and this add-on is ready to emit events.
- `note_added`: A new note has been created and has a real `note_id`.

## Recommended Consumer Behavior

1. Expose an HTTP endpoint that accepts JSON `POST` requests.
2. Accept both `heartbeat` and `note_added` payloads on the same endpoint unless you have a reason to split them.
3. When a `heartbeat` arrives, record `last_heartbeat_at`, `session_id`, and `heartbeat_interval_seconds`.
4. Disable polling while the heartbeat is fresh.
5. If no heartbeat has arrived for about `2-3` heartbeat intervals, re-enable polling as a fallback.
6. When a `note_added` event arrives, process it immediately instead of polling Anki.
7. Deduplicate note events by `note_id`. If you want tighter scoping, use `(session_id, note_id)`.

## Fallback Strategy (ASK BEFORE IMPLEMENTING)

Recommended rule:

- Push mode is active when the latest heartbeat is newer than `max(30 seconds, 3 * heartbeat_interval_seconds)`.
- Poll mode is active when the heartbeat is older than that threshold or has never been seen.

This lets your app stop doing constant `findNotes "added:1"` polling while Anki is healthy, but still recover if Anki closes, the addon is disabled, or the local receiver restarts.

## Payload Shape

All payloads include:

- `addon`: package identifier, currently `new_card_created`
- `addon_name`: display/project name, currently `AnkiBeacon`
- `protocol_version`: currently `1`
- `session_id`: a per-Anki-session identifier
- `event`: `heartbeat` or `note_added`

`note_added` always includes:

- `note_id`
- `source`
- `created_at`

If the `note_added` operation uses `payload_mode: "note"`, it also includes:

- `note_type_id`
- `note_type_name`
- `tags`
- `fields`
- `card_ids`
- `cards`

`heartbeat` includes:

- `status`
- `sent_at`
- `heartbeat_interval_seconds`
- `payload_mode`
- `capabilities`

## Receiver Rules For Agents

If you are an agent integrating another app with this addon, follow these rules:

- Do not build a steady-state polling loop around `findNotes "added:1"` if heartbeat is available.
- Prefer push handling from this addon first.
- Keep a polling fallback only for startup gaps and heartbeat expiry.
- Accept both `heartbeat` and `note_added` on the receiver side.
- Treat `note_id` as the canonical identifier for follow-up Anki queries.
- If `payload_mode` is `note`, use the included fields directly when possible to avoid unnecessary follow-up requests.

## Minimal Receiver Pseudocode

```text
on POST payload:
  if payload.event == "heartbeat":
    state.last_heartbeat_at = now()
    state.session_id = payload.session_id
    state.heartbeat_interval_seconds = payload.heartbeat_interval_seconds
    state.push_ready = true
    return 204

  if payload.event == "note_added":
    state.push_ready = heartbeat_is_fresh()
    if already_processed(payload.session_id, payload.note_id):
      return 200
    process_note(payload)
    return 202

background loop:
  if heartbeat_is_fresh():
    disable_polling()
  else:
    enable_polling_fallback()
```

## Suggested Endpoint Design

One endpoint is enough:

- `POST /anki/events`

Or two endpoints if you want routing separation:

- `POST /anki/new-card`
- `POST /anki/heartbeat`

The addon supports either model through operation-specific config.
