import { get } from 'svelte/store';
import { handleMessage } from '../lib/websocket.js';
import { applyMiningConfig, connected, pendingCards, forceResync, enhancementQueue, ankiStatus } from '../lib/stores.js';
import { getSettings, requestJson, requestState } from './runtime.js';
import { createPoller } from './poller.js';
import { createPushConnection } from './push.js';
import { logConnection, logTransportEvent } from '../lib/connectionLog.js';

export function connectCompanion(status) {
  let audioKey;
  let lastHttpError;
  const poller = createPoller({
    request: requestJson,
    requestState,
    longPoll: true,
    interval: () => document.visibilityState === 'hidden' ? 3000 : getSettings().pollIntervalMs,
    status: error => {
      if (error !== lastHttpError) {
        logConnection('companion', error ? 'http_fallback_error' : 'http_fallback_ready', `${getSettings().serverUrl}/api/companion`, {
          transport: 'http_fallback', error: error || null,
        }, error ? 'warn' : 'info');
        lastHttpError = error;
      }
      if (error) {
        connected.set(false);
        forceResync();
        ankiStatus.set({ state: 'unknown', message: null });
        enhancementQueue.set([]);
      } else { connected.set(true); }
      status(error);
    },
    apply: (result, restarted) => {
      if (restarted) { forceResync(); audioKey = undefined; }
      // Feed the existing event handlers first, then the authoritative pending
      // list. Replayed receipts must not resurrect already-consumed cards.
      for (const event of result.events || []) {
        logTransportEvent('companion', 'http_fallback', `${getSettings().serverUrl}/api/companion`, event);
        handleMessage(event);
      }
      const snapshot = { ...result.snapshot };
      if (snapshot.state) {
        handleMessage({ type: 'position', state: snapshot.state });
        delete snapshot.state;
      }
      const nextAudioKey = JSON.stringify(snapshot.audio_tracks);
      if (nextAudioKey === audioKey) delete snapshot.audio_tracks;
      else audioKey = nextAudioKey;
      // Keep local retry markers when the POST failed before reaching Nagare.
      const retries = new Set(get(pendingCards).filter(c => c.source === 'retry').map(c => c.event.note_id));
      snapshot.pending_cards = (snapshot.pending_cards || []).map(c => retries.has(c.event.note_id) ? { ...c, source: 'retry' } : c);
      if (JSON.stringify(snapshot.pending_cards) === JSON.stringify(get(pendingCards))) delete snapshot.pending_cards;
      handleMessage(snapshot);
      applyMiningConfig(result.mining);
    },
  });
  const connection = createPushConnection({
    url: () => `${getSettings().serverUrl.replace(/^http/, 'ws')}/ws`,
    fallback: poller,
    status: error => { connected.set(!error); status(error); },
    apply: message => {
      if (message.type === 'init') {
        forceResync();
        // Commands already dispatched by this tab own its pending list until
        // the server replies; reconnecting must not resurrect those cards.
        if (requestState().mutations) delete message.pending_cards;
        else if (message.pending_cards) {
          const retries = new Set(get(pendingCards).filter(c => c.source === 'retry').map(c => c.event.note_id));
          message.pending_cards = message.pending_cards.map(c => retries.has(c.event.note_id) ? { ...c, source: 'retry' } : c);
        }
        requestJson('/api/config').then(config => applyMiningConfig(config.mining)).catch(() => {});
      }
      handleMessage(message);
    },
  });
  const resume = () => {
    if (document.visibilityState !== 'hidden') { forceResync(); connection.start(); }
  };
  const suspend = () => { connection.stop(); connected.set(false); };
  document.addEventListener('visibilitychange', resume);
  window.addEventListener('pageshow', resume);
  window.addEventListener('pagehide', suspend);
  connection.start();
  return () => {
    connection.stop();
    connected.set(false);
    document.removeEventListener('visibilitychange', resume);
    window.removeEventListener('pageshow', resume);
    window.removeEventListener('pagehide', suspend);
  };
}
