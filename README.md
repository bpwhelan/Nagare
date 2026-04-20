# Nagare (流れ)

Pronounced "Nah-gah-reh". Subtitle mining companion for Emby, Jellyfin, and Plex. Watches live playback sessions, displays a subtitle timeline in the browser, and enriches Anki cards with sentence audio, screenshots, and source metadata.

NOTE: This project is my most vibe-coded yet, so YMMV. It's really a problem that I seeked out to solve for myself, but I believe/hope it can be useful for others.

## Features

- Live subtitle timeline synced to playback
- Sentence audio extraction and animated AVIF screenshot clips
- AnkiConnect integration with automatic card matching
- Playback controls (seek, pause, resume) from the browser.
- Yomitan-aware pause behavior. (Must turn off Secure Popup in Yomitan) 
- Watch history for mining after playback ends
- Multi-server support (Emby + Jellyfin + Plex simultaneously)


## Roadmap

- [x] Initial prototype with Emby support
- [x] Add Jellyfin support
- [x] Add Plex support
- [x] AnkiConnect integration
- [x] Support for subtitles even when player has none (listening practice while maintaining mineability).
- [ ] Automatic Subtitle Sync? IDK if this is even feasible, the ability to press a button, Nagare syncs with alass or subplz, and then sends the updated sub to the media server would be the idea.
- [ ] More Active Subtitle Sync? If you change subtitle timing in media player, Nagare will not adjust.
- [ ] More options for audio/ss formats


## Installation

### Docker (recommended)

1. Run with Docker Compose:

```yaml
# docker-compose.yml
services:
  nagare:
    image: ghcr.io/bpwhelan/nagare:latest
    container_name: nagare
    ports:
      - "9470:9470"
    volumes:
      - ./data:/app/data
      # Optional: mount media library for disk-mode access
      # - /path/to/anime:/media/Anime:ro
    extra_hosts:
      - "host.docker.internal:host-gateway"
    restart: unless-stopped
```

```sh
docker compose up -d
```

2. Open `http://localhost:9470` and configure Nagare from the web UI Config page.

### Binary release

Download the latest binary for your platform from [GitHub Releases](https://github.com/bpwhelan/Nagare/releases).

Requirements:
- `ffmpeg` on `PATH`
- Anki with [AnkiConnect](https://ankiweb.net/shared/info/2055492159)

```sh
./nagare
```

The web UI is served at `http://localhost:9470`.

### Build from source

```sh
cd frontend && npm ci && npm run build && cd ..
cargo build --release
```

## Configuration

All configuration is managed through the web UI Config page and stored in `data/nagare.sqlite`. On first run, configure:

1. **Media server** — URL and API key (Emby/Jellyfin) or token (Plex)
2. **AnkiConnect** — URL and field mappings (`Sentence`, `SentenceAudio`, `Picture`)
3. **Media access** — `auto`, `disk`, or `api` mode; add path mappings if server and Nagare see different file paths

## How it works

1. Nagare polls your media servers for active playback sessions
2. Select a session in the web UI to load its subtitle track
3. Create a card in Anki — Nagare matches it to the current subtitle context
4. Confirm the match, preview audio/screenshot, and enrich the card

## Project structure

```
src/            Rust backend (Axum + Tokio)
frontend/       Svelte frontend (Vite)
Dockerfile      Multi-stage container build
```

Data is stored in `data/nagare.sqlite`. Generated Anki media files are prefixed with `nagare_`.
