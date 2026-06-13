import { get } from 'svelte/store';
import { activeHistoryItemId, sessionState, pendingCards, connected, ankiStatus, enhancementQueue, syncPositionFromSessionState, isSeekLocked, isPlayLocked, applySubtitlePayload, applyAudioTracksPayload, showErrorToast, showToast, forceResync } from './stores.js';

let ws = null;
let reconnectTimer = null;
let lastMessageAt = 0;
let resyncWatchdog = null;

export function connectWebSocket() {
  if (ws && ws.readyState === WebSocket.OPEN) return;

  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  const url = `${proto}//${location.host}/ws`;

  ws = new WebSocket(url);

  ws.onopen = () => {
    connected.set(true);
    console.log('WebSocket connected');
    if (reconnectTimer) {
      clearTimeout(reconnectTimer);
      reconnectTimer = null;
    }
  };

  ws.onclose = () => {
    connected.set(false);
    ankiStatus.set({ state: 'unknown', message: null });
    enhancementQueue.set([]);
    // Drop the stale playback clock so the fresh `init` snapshot re-anchors cleanly.
    forceResync();
    console.log('WebSocket disconnected, reconnecting in 2s...');
    reconnectTimer = setTimeout(connectWebSocket, 2000);
  };

  ws.onerror = (e) => {
    console.error('WebSocket error:', e);
  };

  ws.onmessage = (event) => {
    lastMessageAt = Date.now();
    try {
      const msg = JSON.parse(event.data);
      handleMessage(msg);
    } catch (e) {
      console.error('Failed to parse WS message:', e);
    }
  };
}

/** Tear down the current socket and reconnect immediately, skipping the backoff timer. */
function reconnectNow() {
  if (reconnectTimer) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
  if (ws) {
    // Detach handlers so the imminent close doesn't schedule a duplicate reconnect.
    ws.onclose = null;
    ws.onerror = null;
    try { ws.close(); } catch (_) { /* ignore */ }
    ws = null;
  }
  connectWebSocket();
}

/**
 * Called when the page returns to the foreground (e.g. mobile tab resumed).
 * Mobile browsers freeze timers and can leave the WebSocket in a "zombie" state
 * that no longer delivers messages, so the local playback clock drifts out of
 * sync. Re-anchor on the next server snapshot and revive the socket if needed.
 */
export function resyncFromBackground() {
  // Re-anchor to whatever the server reports next, discarding drifted projection.
  forceResync();

  // Dead or closing socket: reconnect right away instead of waiting out the backoff.
  if (!ws || ws.readyState === WebSocket.CLOSING || ws.readyState === WebSocket.CLOSED) {
    reconnectNow();
    return;
  }

  // Socket claims to be open. It may be a zombie after backgrounding — watch for a
  // fresh message and force a reconnect if none arrives shortly (the server pushes
  // state every 50ms while connected).
  if (ws.readyState === WebSocket.OPEN) {
    const seenAt = lastMessageAt;
    clearTimeout(resyncWatchdog);
    resyncWatchdog = setTimeout(() => {
      if (lastMessageAt === seenAt) {
        console.log('No WS traffic after resume, reconnecting');
        reconnectNow();
      }
    }, 1500);
  }
}

function handleMessage(msg) {
  if (msg.anki_status) {
    ankiStatus.set(msg.anki_status);
  }
  if (Object.prototype.hasOwnProperty.call(msg, 'enhancement_queue')) {
    enhancementQueue.set(msg.enhancement_queue || []);
  }

  switch (msg.type) {
    case 'init':
    case 'full_update':
      if (msg.state) {
        sessionState.set(msg.state);
        syncPositionFromSessionState(msg.state);
      }
      if (msg.subtitles && !get(activeHistoryItemId)) {
        applySubtitlePayload(msg.subtitles);
      }
      if (msg.audio_tracks) {
        applyAudioTracksPayload(msg.audio_tracks);
      }
      break;

    case 'position':
      if (msg.state) {
        // During a play-lock, preserve the optimistic is_paused value
        if (isPlayLocked() && msg.state.now_playing) {
          const cur = get(sessionState);
          if (cur.now_playing) {
            msg.state.now_playing.is_paused = cur.now_playing.is_paused;
          }
        }
        sessionState.set(msg.state);

        // During a seek-lock, don't overwrite position or activeLineIndex
        // so rapid remote-control clicks accumulate correctly.
        if (!isSeekLocked()) {
          syncPositionFromSessionState(msg.state);
        }
      }
      break;

    case 'new_card':
      if (msg.new_card) {
        pendingCards.update(cards => {
          const noteId = msg.new_card.event.note_id;
          if (cards.some(c => c.event.note_id === noteId)) return cards;
          return [...cards, msg.new_card];
        });
      }
      break;

    case 'enhancement_result':
      if (msg.enhancement_result) {
        const r = msg.enhancement_result;
        console.log('[WS] enhancement_result:', r);
        if (r.success) {
          showToast('success', r.message);
        } else {
          showErrorToast(r.message);
        }
      }
      break;

    case 'remote_result':
      if (msg.remote_result && !msg.remote_result.success) {
        console.warn('[WS] remote_result error:', msg.remote_result);
        showErrorToast(msg.remote_result.error || 'Remote control command failed');
      }
      break;
  }
}

export function disconnect() {
  if (ws) {
    ws.close();
    ws = null;
  }
  enhancementQueue.set([]);
  if (reconnectTimer) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
  if (resyncWatchdog) {
    clearTimeout(resyncWatchdog);
    resyncWatchdog = null;
  }
}
