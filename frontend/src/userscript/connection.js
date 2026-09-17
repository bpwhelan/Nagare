import { get } from 'svelte/store';
import { handleMessage } from '../lib/websocket.js';
import { applyMiningConfig, connected, pendingCards, forceResync, enhancementQueue, ankiStatus } from '../lib/stores.js';
import { getSettings, requestJson, requestState } from './runtime.js';
import { createPoller } from './poller.js';

export function connectCompanion(status) {
  let audioKey;
  const poller = createPoller({
    request: requestJson,
    requestState,
    interval: () => document.visibilityState === 'hidden' ? 3000 : getSettings().pollIntervalMs,
    status: error => {
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
      for (const event of result.events || []) handleMessage(event);
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
  const resume = () => {
    if (document.visibilityState !== 'hidden') { forceResync(); poller.start(); }
  };
  const suspend = () => { poller.stop(); connected.set(false); };
  document.addEventListener('visibilitychange', resume);
  window.addEventListener('pageshow', resume);
  window.addEventListener('pagehide', suspend);
  poller.start();
  return () => {
    poller.stop();
    connected.set(false);
    document.removeEventListener('visibilitychange', resume);
    window.removeEventListener('pageshow', resume);
    window.removeEventListener('pagehide', suspend);
  };
}
