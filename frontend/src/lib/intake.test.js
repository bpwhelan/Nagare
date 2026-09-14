import assert from 'node:assert/strict';
import { test, after } from 'node:test';
import { get } from 'svelte/store';
globalThis.localStorage = { getItem: () => null, setItem() {} };
globalThis.location = { pathname: '/', protocol: 'http:', host: 'localhost' };
const interval = globalThis.setInterval;
globalThis.setInterval = () => 0;
const stores = await import('./stores.js');
globalThis.setInterval = interval;
let socket;
globalThis.WebSocket = class { static OPEN = 1; readyState = 1; constructor() { socket = this; } close() {} };
const { connectWebSocket, disconnect } = await import('./websocket.js');
connectWebSocket(); after(disconnect);
const card = (id, source = 'pending') => ({event:{note_id:id,sentence:'文'}, source});
const send = msg => socket.onmessage({data:JSON.stringify(msg)});

test('reconnect restores pending cards from the same websocket snapshot', () => {
  stores.pendingCards.set([card(1)]);
  send({type:'init',pending_cards:[card(2)]});
  assert.deepEqual(get(stores.pendingCards).map(c=>c.event.note_id),[2]);
});
test('retry updates replace the existing pending event instead of being deduplicated away', () => {
  send({type:'new_card',new_card:card(2,'retry')});
  assert.equal(get(stores.pendingCards)[0].source,'retry');
});
test('cards that already have media confirm receipt without an enhancement popup', () => {
  send({type:'new_card',new_card:card(3,'mining_history')});
  assert.equal(get(stores.ankiNotice),3);
  assert.deepEqual(get(stores.pendingCards).map(c=>c.event.note_id),[2]);
});
