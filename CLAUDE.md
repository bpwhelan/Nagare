# Nagare

Subtitle mining tool for Emby / Jellyfin / Plex. A Rust (axum + tokio) backend
watches media-server playback sessions and AnkiConnect, then enriches Anki cards
with audio, screenshots, and sentence/translation context. A Svelte frontend
provides the player-side UI and configuration.

## Layout

- `src/` — Rust backend
  - `api.rs` — axum HTTP/WebSocket routes, enrichment pipeline (`perform_enrichment`)
  - `config.rs` — `Config` and all persisted settings (serde). Stored as JSON in SQLite (`app_config`)
  - `anki.rs` — AnkiConnect client + the poller/AnkiBeacon push loop (`run_anki_poller`)
  - `mining.rs` — SQLite persistence (config, media history, mined notes)
  - `session.rs` — media-server session tracking, history entries, subtitle loading
  - `media.rs` — ffmpeg audio/screenshot/AVIF generation
  - `subtitle.rs` — subtitle parsing + sentence-to-line matching
  - `media_server/` — per-server adapters: `mediabrowser.rs` (Emby/Jellyfin), `plex.rs`, shared types in `mod.rs`
- `frontend/src/` — Svelte app
  - `lib/ConfigPage.svelte` — settings UI (tabbed: Server / Anki & Media / Frontend)
  - `lib/EnrichDialog.svelte` — the card enrichment dialog
  - `lib/stores.js`, `lib/api.js` — client state and backend calls
- `api_reference/` — **third-party media-server OpenAPI specs** (see below)

## API reference

When working with the media-server adapters in `src/media_server/`, consult the
upstream OpenAPI specifications in [`api_reference/`](api_reference/) rather than
guessing endpoint shapes or field names:

- `api_reference/emby_openapi.json` — Emby
- `api_reference/jellyfin-openapi-stable.json` — Jellyfin
- `api_reference/plex_openapi.json` — Plex

These are large; search them for the specific endpoint/field instead of reading
them whole.

## Build & check

- Backend: `cargo check` / `cargo build` / `cargo test` (run from repo root)
- Frontend: `npm run build` (or `npm run dev`) from `frontend/`

## Notes

- Config is persisted via SQLite, not a flat file; `Config::save_to` is legacy.
  The `mining.auto_approve` field is a migrated client-local setting and is
  stripped server-side.
- Series tagging (`anki.series_tag_parent`): when set, enrichment adds a
  `parent::Series_Name` tag in `perform_enrichment`. The series name comes from
  the media server's metadata (`NowPlaying.series_name`), plumbed through
  `NowPlayingState` / `HistoryEntry` (persisted in the `media_history` table) and
  `MediaContext`; movies fall back to the bare title.
