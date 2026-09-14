import assert from 'node:assert/strict';
import { after, beforeEach, test } from 'node:test';
import { get } from 'svelte/store';

// The playback stores only need storage, a route, and a clock in these tests.
globalThis.localStorage = { getItem: () => null, setItem() {} };
globalThis.location = { pathname: '/', protocol: 'http:', host: 'localhost' };
const realSetInterval = globalThis.setInterval;
globalThis.setInterval = () => 0;
const stores = await import('./stores.js');
globalThis.setInterval = realSetInterval;
const realNow = Date.now;
let now = 100_000;
Date.now = () => now;
after(() => { Date.now = realNow; });

let socket;
globalThis.WebSocket = class {
  static OPEN = 1;
  readyState = 1;
  constructor() { socket = this; }
};
const { connectWebSocket } = await import('./websocket.js');
connectWebSocket();

function playback(device, position = 10_000, paused = false) {
  return {
    sessions: [],
    active_session_id: `jellyfin|${device}`,
    now_playing: {
      history_id: 'jellyfin|episode-1',
      server_kind: 'jellyfin',
      position_ms: position,
      duration_ms: 1_500_000,
      is_paused: paused,
    },
  };
}

function receive(state, type = 'position') {
  socket.onmessage({ data: JSON.stringify({ type, state }) });
}

beforeEach(() => {
  now += 10_000;
  stores.forceResync();
  stores.sessionState.set({ sessions: [], active_session_id: null, now_playing: null });
});

test('switching devices reanchors the clock even with the same episode and position', () => {
  receive(playback('tv'));
  now += 8_000;
  receive(playback('browser'));
  assert.equal(get(stores.positionMs), 10_000);
});

test('repeated progress for the same device preserves its projected clock', () => {
  receive(playback('tv'));
  now += 8_000;
  receive(playback('tv'));
  assert.equal(get(stores.positionMs), 18_000);
});

test('a device switch drops optimistic seek and pause from the previous device', () => {
  receive(playback('tv'));
  stores.setOptimisticPosition(50_000);
  stores.setOptimisticPlayState(true);
  receive(playback('browser', 12_000));
  assert.equal(get(stores.positionMs), 12_000);
  assert.equal(get(stores.sessionState).now_playing.is_paused, false);
  assert.equal(stores.isSeekLocked(), false);
  assert.equal(stores.isPlayLocked(), false);
});

test('optimistic commands still survive stale updates from the same device', () => {
  receive(playback('tv'));
  stores.setOptimisticPosition(50_000);
  stores.setOptimisticPlayState(true);
  receive(playback('tv'));
  assert.equal(get(stores.positionMs), 50_000);
  assert.equal(get(stores.sessionState).now_playing.is_paused, true);
  assert.equal(stores.isSeekLocked(), true);
});

test('a full update clears the previous device command locks too', () => {
  receive(playback('tv'));
  stores.setOptimisticPosition(50_000);
  receive(playback('browser', 12_000), 'full_update');
  assert.equal(get(stores.positionMs), 12_000);
  assert.equal(stores.isSeekLocked(), false);
});
