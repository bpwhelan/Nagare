import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createPushConnection } from './push.js';

function harness(blocked = false) {
  const sockets = [], timers = [], messages = [];
  let starts = 0, stops = 0;
  const connection = createPushConnection({
    url: () => 'wss://nagare.test/prefix/ws',
    socketFactory: url => {
      if (blocked) throw new Error('Mixed content blocked');
      const socket = { url, close() { this.closed = true; } }; sockets.push(socket); return socket;
    },
    fallback: { start() { starts++; }, stop() { stops++; } },
    status() {}, apply: message => messages.push(message),
    schedule: (fn, delay) => { const timer = { fn, delay }; timers.push(timer); return timer; },
    cancel: timer => { if (timer) timer.cancelled = true; },
  });
  return { connection, sockets, timers, messages, get starts() { return starts; }, get stops() { return stops; } };
}
const send = (socket, message) => socket.onmessage({ data: JSON.stringify(message) });

test('a connected Companion receives cards immediately without HTTP polling', () => {
  const h = harness(); h.connection.start();
  assert.equal(h.sockets[0].url, 'wss://nagare.test/prefix/ws');
  send(h.sockets[0], { type: 'init', pending_cards: [] });
  send(h.sockets[0], { type: 'new_card', new_card: { event: { note_id: 12 } } });
  assert.equal(h.messages[1].new_card.event.note_id, 12);
  assert.equal(h.starts, 0);
  h.connection.stop();
});

test('blocked WebSockets use the event-woken fallback and stopping cancels reconnection', () => {
  const h = harness(true); h.connection.start();
  assert.equal(h.starts, 1);
  assert.equal(h.timers[0].delay, 30_000);
  h.connection.stop();
  assert.equal(h.timers[0].cancelled, true);
});

test('disconnects use one fallback and reconnecting shuts it down before applying the snapshot', () => {
  const h = harness(); h.connection.start();
  const first = h.sockets[0];
  send(first, { type: 'init' });
  const lateMessage = first.onmessage;
  first.onclose();
  assert.equal(h.starts, 1);
  h.timers.find(t => t.delay === 10_000).fn();
  send(h.sockets[1], { type: 'init', pending_cards: [] });
  lateMessage({ data: JSON.stringify({ type: 'new_card' }) });
  assert.equal(h.messages.length, 2);
  assert.equal(h.starts, 1);
  assert.ok(h.stops >= 2);
  h.connection.stop();
});

test('a silent connection falls back and resumes receipt without a frozen socket', () => {
  const h = harness(); h.connection.start();
  send(h.sockets[0], { type: 'init' });
  h.timers.find(t => t.delay === 5000).fn();
  assert.equal(h.starts, 1);
  assert.equal(h.sockets[0].closed, true);
  h.connection.stop();
});
