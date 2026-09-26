import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createPoller } from './poller.js';

const flush = () => new Promise(resolve => setImmediate(resolve));
const snapshot = (cursor = 1, epoch = 'server-one') => ({ protocol: 1, epoch, cursor, subtitle_revision: 'subs-one', snapshot: { pending_cards: [] } });
function harness(options = {}) {
  const requests = [], applied = [], statuses = [], scheduled = [];
  const state = { revision: 0, mutations: 0 };
  const poller = createPoller({
    request: path => new Promise((resolve, reject) => requests.push({ path, resolve, reject })),
    apply: (result, restarted) => applied.push({ result, restarted }),
    status: error => statuses.push(error),
    requestState: () => ({ ...state }), interval: () => 750,
    schedule: (fn, delay) => { const timer = { fn, delay }; scheduled.push(timer); return timer; },
    cancel: timer => { if (timer) timer.cancelled = true; },
    ...options,
  });
  return { poller, requests, applied, statuses, scheduled, state };
}

test('fallback waits on the server and immediately listens again after card delivery', async () => {
  const h = harness({ longPoll: true }); h.poller.start();
  assert.equal(new URL(h.requests[0].path, 'https://nagare.test').searchParams.get('wait_ms'), '750');
  h.requests[0].resolve(snapshot()); await flush();
  assert.equal(h.scheduled[0].delay, 0);
  h.poller.stop();
});

test('snapshots overlapping a confirmation are discarded without consuming the event cursor', async () => {
  const h = harness(); h.poller.start();
  h.state.revision++;
  h.requests[0].resolve(snapshot()); await flush();
  assert.equal(h.applied.length, 0);
  h.scheduled[0].fn();
  assert.equal(h.requests[1].path.includes('after='), false);
  h.requests[1].resolve(snapshot(2)); await flush();
  assert.equal(h.applied.length, 1);
  h.poller.stop();
});

test('changing servers cannot apply an older in-flight reply or create a second polling loop', async () => {
  const h = harness(); h.poller.start(); h.poller.start();
  h.requests[0].resolve(snapshot()); await flush();
  assert.equal(h.applied.length, 0);
  assert.equal(h.scheduled.length, 0);
  h.requests[1].resolve(snapshot(4, 'new-server')); await flush();
  assert.equal(h.applied.length, 1);
  assert.equal(h.scheduled.length, 1);
  h.poller.stop(); assert.equal(h.scheduled[0].cancelled, true);
});

test('successful polls send cursors and subtitle revisions, and detect a backend restart', async () => {
  const h = harness(); h.poller.start();
  h.requests[0].resolve(snapshot(10)); await flush();
  h.scheduled[0].fn();
  const query = new URL(h.requests[1].path, 'https://nagare.test').searchParams;
  assert.equal(query.get('after'), '10');
  assert.equal(query.get('subtitle_revision'), 'subs-one');
  h.requests[1].resolve(snapshot(1, 'server-two')); await flush();
  assert.equal(h.applied[1].restarted, true);
  h.poller.stop();
});

test('connection failures back off and closing the companion ignores late errors', async () => {
  const h = harness(); h.poller.start();
  h.requests[0].reject(new Error('offline')); await flush();
  assert.deepEqual(h.statuses, ['offline']);
  assert.equal(h.scheduled[0].delay, 3000);
  h.scheduled[0].fn(); h.poller.stop();
  h.requests[1].reject(new Error('late')); await flush();
  assert.deepEqual(h.statuses, ['offline']);
});
