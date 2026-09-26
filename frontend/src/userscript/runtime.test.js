import assert from 'node:assert/strict';
import { test } from 'node:test';
const requests = [];
const storage = new Map([['nagare_userscript_settings', { serverUrl: 'http://nagare.test:9470/prefix/' }]]);
globalThis.GM_getValue = (key, fallback) => storage.get(key) ?? fallback;
globalThis.GM_setValue = (key, value) => storage.set(key, value);
globalThis.GM_xmlhttpRequest = options => { requests.push(options); };
globalThis.fetch = () => { throw new Error('Companion must not use the host page fetch'); };
const runtime = await import('./runtime.js');

test('JSON requests and preferences use the privileged manager APIs', async () => {
  const promise = runtime.requestJson('/api/enrich', { method: 'POST', body: JSON.stringify({ note_id: 10 }) });
  const request = requests.pop();
  assert.equal(request.url, 'http://nagare.test:9470/prefix/api/enrich');
  assert.equal(request.headers['Content-Type'], 'application/json');
  assert.equal(runtime.requestState().mutations, 1);
  request.onload({ status: 200, responseText: '{"success":true}' });
  assert.deepEqual(await promise, { success: true });
  assert.equal(runtime.requestState().mutations, 0);
  runtime.writePreference('opt_autoApprove', true);
  assert.equal(runtime.readPreference('opt_autoApprove', false), true);
});

test('proxy login pages and connection failures produce actionable errors', async () => {
  let promise = runtime.requestJson('/api/companion');
  requests.pop().onload({ status: 200, responseText: '<html>Sign in</html>' });
  await assert.rejects(promise, /proxy login/);
  promise = runtime.requestJson('/api/enrich', { method: 'POST' });
  requests.pop().onerror();
  await assert.rejects(promise, /allow it in your userscript manager/);
  assert.equal(runtime.requestState().mutations, 0);
});

test('slow media previews and diagnostics never block receipt of new cards', async () => {
  const before = runtime.requestState();
  const preview = runtime.requestJson('/api/preview-audio', { method: 'POST' });
  const report = runtime.requestJson('/api/enrich/client-event', { method: 'POST' });
  assert.deepEqual(runtime.requestState(), before);
  requests.pop().onload({ status: 200, responseText: '{"ok":true}' });
  requests.pop().onload({ status: 200, responseText: '{"audio_base64":""}' });
  await Promise.all([preview, report]);
  assert.deepEqual(runtime.requestState(), before);
});
