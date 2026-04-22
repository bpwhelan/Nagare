import { writable, derived, get } from 'svelte/store';

/**
 * A writable store that persists its value in localStorage.
 * @template T
 * @param {string} key
 * @param {T} defaultValue
 */
function localStorageStore(key, defaultValue) {
  const stored = localStorage.getItem(key);
  const initial = stored !== null ? JSON.parse(stored) : defaultValue;
  const store = writable(initial);
  store.subscribe(value => localStorage.setItem(key, JSON.stringify(value)));
  return store;
}

/** @typedef {{ id: string, server_kind: 'emby'|'jellyfin'|'plex', client: string, device_name: string, user_name: string|null, title: string|null, is_target_language: boolean }} SessionSummary */
/** @typedef {{ history_id: string, server_kind: 'emby'|'jellyfin'|'plex', item_id: string, title: string, position_ms: number, duration_ms: number|null, is_paused: boolean, supports_remote_control: boolean, subtitle_stream_index: number|null, subtitle_candidate_id: string|null, subtitle_selection_mode: 'auto'|'manual', media_source_id: string, file_path: string|null }} NowPlayingState */
/** @typedef {{ sessions: SessionSummary[], active_session_id: string|null, now_playing: NowPlayingState|null }} SessionState */
/** @typedef {{ index: number, start_ms: number, end_ms: number, text: string }} SubtitleLine */
/** @typedef {{ id: string, source: 'server'|'sidecar', stream_index: number|null, language: string|null, label: string, codec: string|null, is_default: boolean, is_external: boolean, is_selected_in_session: boolean }} SubtitleCandidate */
/** @typedef {{ lines: SubtitleLine[], count: number, candidates: SubtitleCandidate[], selected_candidate_id: string|null, selection_mode: 'auto'|'manual' }} SubtitlePayload */
/** @typedef {{ note_id: number, sentence: string, fields: Object, model_name: string, tags: string[] }} NewCardEvent */
/** @typedef {{ event: NewCardEvent, matched_line_index: number|null, history_id?: string|null, start_ms?: number|null, end_ms?: number|null, generate_avif?: boolean|null, included_line_first?: number|null, included_line_last?: number|null, card_ids?: number[], source?: 'pending'|'mining_history', updated_at?: string|null }} NewCardWithMatch */
/** @typedef {{ state: 'unknown'|'connected'|'disconnected', message: string|null }} AnkiStatus */
/** @typedef {{ note_id: number, state: 'queued'|'running', message: string }} EnhancementQueueItem */

export const sessionState = writable(/** @type {SessionState} */ ({
  sessions: [],
  active_session_id: null,
  now_playing: null,
}));

export const subtitles = writable(/** @type {SubtitleLine[]} */ ([]));
export const subtitleCandidates = writable(/** @type {SubtitleCandidate[]} */ ([]));
export const selectedSubtitleCandidateId = writable(/** @type {string|null} */ (null));
export const subtitleSelectionMode = writable(/** @type {'auto'|'manual'} */ ('auto'));
export const activeLineIndex = writable(/** @type {number|null} */ (null));
export const pendingCards = writable(/** @type {NewCardWithMatch[]} */ ([]));
export const connected = writable(false);
export const ankiStatus = writable(/** @type {AnkiStatus} */ ({
  state: 'unknown',
  message: null,
}));
export const enhancementQueue = writable(/** @type {EnhancementQueueItem[]} */ ([]));
export const currentView = writable('timeline'); // 'timeline' | 'history' | 'config'
export const pauseOnHover = localStorageStore('opt_pauseOnHover', false);
export const pauseOnSeek = localStorageStore('opt_pauseOnSeek', false);
export const yomitanPause = localStorageStore('opt_yomitanPause', false);
export const yomitanPopupVisible = writable(false);
export const audioStartOffset = writable(100);
export const audioEndOffset = writable(500);
export const defaultGenerateAvif = writable(true);
export const autoApprove = localStorageStore('opt_autoApprove', false);

// History
export const historyItems = writable(/** @type {any[]} */ ([]));
export const minedHistoryItems = writable(/** @type {any[]} */ ([]));
/// When mining from history, the active history item_id
export const activeHistoryItemId = writable(/** @type {string|null} */ (null));
export const dialogCard = writable(/** @type {NewCardWithMatch|null} */ (null));

// Audio tracks
/** @typedef {{ index: number, codec: string|null, language: string|null, display_title: string|null, title: string|null, is_default: boolean, channels: string|null }} AudioTrack */
/** @typedef {{ tracks: AudioTrack[], selected_index: number|null, resolution: 'single'|'auto_language'|'manual'|'needs_selection' }} AudioTracksPayload */
export const audioTracks = writable(/** @type {AudioTrack[]} */ ([]));
export const selectedAudioTrackIndex = writable(/** @type {number|null} */ (null));
export const audioTrackResolution = writable(/** @type {'single'|'auto_language'|'manual'|'needs_selection'} */ ('single'));
export const showAudioTrackModal = writable(false);

/**
 * @param {AudioTracksPayload | null | undefined} payload
 */
export function applyAudioTracksPayload(payload) {
  audioTracks.set(payload?.tracks || []);
  selectedAudioTrackIndex.set(payload?.selected_index ?? null);
  const resolution = payload?.resolution || 'single';
  audioTrackResolution.set(resolution);
  // Auto-show modal when user must choose
  if (resolution === 'needs_selection') {
    showAudioTrackModal.set(true);
  }
}

function parseRoute(pathname = location.pathname) {
  const noteMatch = pathname.match(/^\/mine\/note\/(\d+)$/);
  if (noteMatch) {
    return { name: 'mine_note', noteId: Number(noteMatch[1]) };
  }

  const cardMatch = pathname.match(/^\/mine\/card\/(\d+)$/);
  if (cardMatch) {
    return { name: 'mine_card', cardId: Number(cardMatch[1]) };
  }

  return { name: 'home' };
}

export const route = writable(parseRoute());

export function syncRouteFromLocation() {
  route.set(parseRoute());
}

export function navigate(pathname) {
  history.pushState({}, '', pathname);
  syncRouteFromLocation();
}

export function replaceRoute(pathname) {
  history.replaceState({}, '', pathname);
  syncRouteFromLocation();
}

export function applyMiningConfig(mining = {}) {
  audioStartOffset.set(mining.audio_start_offset_ms ?? 100);
  audioEndOffset.set(mining.audio_end_offset_ms ?? 500);
  defaultGenerateAvif.set(mining.generate_avif ?? true);
}

/**
 * @param {SubtitlePayload | null | undefined} payload
 */
export function applySubtitlePayload(payload) {
  subtitles.set(payload?.lines || []);
  subtitleCandidates.set(payload?.candidates || []);
  selectedSubtitleCandidateId.set(payload?.selected_candidate_id ?? null);
  subtitleSelectionMode.set(payload?.selection_mode || 'auto');
}

// Toasts
let _toastId = 0;
export const toasts = writable(/** @type {{ id: number, type: 'success'|'error', message: string }[]} */ ([]));

/**
 * Show a toast notification that auto-dismisses after `duration` ms.
 * @param {'success'|'error'} type
 * @param {string} message
 * @param {number} [duration]
 */
export function showToast(type, message, duration = 4000) {
  const id = ++_toastId;
  toasts.update(ts => [...ts, { id, type, message }]);
  setTimeout(() => toasts.update(ts => ts.filter(t => t.id !== id)), duration);
}

// Derived
export const activeSessions = derived(sessionState, $s =>
  $s.sessions.filter(s => s.is_target_language)
);

// positionMs is writable so remote actions can update it optimistically
// without waiting for the next WebSocket poll.
export const positionMs = writable(0);

let _playbackAnchor = null;
let _lastServerObservation = null;

function midpointDistance(line, posMs) {
  return Math.abs(((line.start_ms + line.end_ms) / 2) - posMs);
}

function findActiveLineAtPosition(lines, posMs) {
  if (!lines.length) return null;

  let lo = 0;
  let hi = lines.length - 1;

  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    const line = lines[mid];

    if (posMs < line.start_ms) {
      hi = mid - 1;
    } else if (posMs > line.end_ms) {
      lo = mid + 1;
    } else {
      return mid;
    }
  }

  const candidates = [...new Set([hi - 1, hi, lo, lo + 1])]
    .filter(index => index >= 0 && index < lines.length);

  let bestIndex = candidates[0] ?? 0;
  let bestDistance = midpointDistance(lines[bestIndex], posMs);

  for (const index of candidates.slice(1)) {
    const distance = midpointDistance(lines[index], posMs);
    if (distance < bestDistance) {
      bestDistance = distance;
      bestIndex = index;
    }
  }

  return bestIndex;
}

function syncActiveLineWithPosition(posMs = get(positionMs)) {
  const lines = get(subtitles);
  if (!lines.length) {
    activeLineIndex.set(null);
    return;
  }

  activeLineIndex.set(findActiveLineAtPosition(lines, posMs));
}

function projectedPosition(anchor = _playbackAnchor) {
  if (!anchor) return null;
  if (anchor.paused) return anchor.positionMs;

  const elapsed = Date.now() - anchor.wallTimeMs;
  const projected = anchor.positionMs + elapsed;
  return Math.max(0, Math.min(anchor.durationMs, projected));
}

export function syncPositionFromSessionState(state) {
  const np = state?.now_playing;
  if (!np) {
    _playbackAnchor = null;
    _lastServerObservation = null;
    positionMs.set(0);
    activeLineIndex.set(null);
    return;
  }

  const nextAnchor = {
    itemId: np.history_id,
    positionMs: np.position_ms,
    durationMs: np.duration_ms ?? Infinity,
    paused: np.is_paused,
    wallTimeMs: Date.now(),
  };

  const projected = projectedPosition();
  const itemChanged = !_playbackAnchor || _playbackAnchor.itemId !== nextAnchor.itemId;
  const pauseChanged = !_playbackAnchor || _playbackAnchor.paused !== nextAnchor.paused;
  const serverObservationChanged = !_lastServerObservation
    || _lastServerObservation.itemId !== nextAnchor.itemId
    || _lastServerObservation.paused !== nextAnchor.paused
    || Math.abs(_lastServerObservation.positionMs - nextAnchor.positionMs) > 250;
  const serverJumpedBackward = serverObservationChanged
    && _lastServerObservation
    && _lastServerObservation.itemId === nextAnchor.itemId
    && nextAnchor.positionMs < _lastServerObservation.positionMs - 1500;
  const serverAhead = projected == null || nextAnchor.positionMs > projected + 750;
  const serverCloseToProjection = projected != null && Math.abs(nextAnchor.positionMs - projected) <= 1500;

  // Plex Web can report stale viewOffset values for ~10s at a time.
  // Keep a local playback clock running between coarse server updates and
  // only re-anchor when Plex reports a real state change.
  if (
    itemChanged
    || pauseChanged
    || serverJumpedBackward
    || serverAhead
    || (serverObservationChanged && serverCloseToProjection)
  ) {
    _playbackAnchor = nextAnchor;
  } else if (_playbackAnchor) {
    _playbackAnchor.durationMs = nextAnchor.durationMs;
  }

  _lastServerObservation = {
    itemId: nextAnchor.itemId,
    positionMs: nextAnchor.positionMs,
    paused: nextAnchor.paused,
  };

  const nextPosition = Math.round(projectedPosition() ?? nextAnchor.positionMs);
  positionMs.set(nextPosition);
  syncActiveLineWithPosition(nextPosition);
}

setInterval(() => {
  if (isSeekLocked()) return;

  const projected = projectedPosition();
  if (projected != null) {
    const nextPosition = Math.round(projected);
    positionMs.set(nextPosition);
    syncActiveLineWithPosition(nextPosition);
  }
}, 100);

subtitles.subscribe(() => {
  syncActiveLineWithPosition();
});

// --- Seek lock: after an optimistic seek, suppress incoming WS position
//     updates so rapid clicks accumulate correctly. ---
let _seekLockUntil = 0;
const SEEK_LOCK_MS = 1500;

export function isSeekLocked() {
  return Date.now() < _seekLockUntil;
}

/** Call after any seek/skip to reflect the new position before the server responds. */
export function setOptimisticPosition(ms) {
  const pos = Math.max(0, ms);
  positionMs.set(pos);
  _seekLockUntil = Date.now() + SEEK_LOCK_MS;

  const np = get(sessionState).now_playing;
  if (np) {
    _playbackAnchor = {
      itemId: np.history_id,
      positionMs: pos,
      durationMs: np.duration_ms ?? Infinity,
      paused: np.is_paused,
      wallTimeMs: Date.now(),
    };
  }

  syncActiveLineWithPosition(pos);
}

// --- Play-state lock: prevents WS from reverting optimistic play/pause. ---
let _playLockUntil = 0;

export function isPlayLocked() {
  return Date.now() < _playLockUntil;
}

export function setOptimisticPlayState(paused) {
  sessionState.update(s => {
    if (s.now_playing) {
      return { ...s, now_playing: { ...s.now_playing, is_paused: paused } };
    }
    return s;
  });
  _playLockUntil = Date.now() + SEEK_LOCK_MS;

  const np = get(sessionState).now_playing;
  if (np) {
    _playbackAnchor = {
      itemId: np.history_id,
      positionMs: get(positionMs),
      durationMs: np.duration_ms ?? Infinity,
      paused,
      wallTimeMs: Date.now(),
    };
  }
}

export const durationMs = derived(sessionState, $s =>
  $s.now_playing?.duration_ms ?? 0
);

export const isPlaying = derived(sessionState, $s =>
  $s.now_playing != null && !$s.now_playing.is_paused
);

export const nowPlayingTitle = derived(sessionState, $s =>
  $s.now_playing?.title ?? 'Nothing playing'
);
