import { get } from 'svelte/store';
import { activeHistoryItemId, sessionState, pendingCards, connected, ankiStatus, enhancementQueue, syncPositionFromSessionState, isSeekLocked, isPlayLocked, applySubtitlePayload } from './stores.js';

let ws = null;
let reconnectTimer = null;

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
    console.log('WebSocket disconnected, reconnecting in 2s...');
    reconnectTimer = setTimeout(connectWebSocket, 2000);
  };

  ws.onerror = (e) => {
    console.error('WebSocket error:', e);
  };

  ws.onmessage = (event) => {
    try {
      const msg = JSON.parse(event.data);
      handleMessage(msg);
    } catch (e) {
      console.error('Failed to parse WS message:', e);
    }
  };
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
}
