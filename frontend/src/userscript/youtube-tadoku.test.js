import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { runInNewContext } from 'node:vm';

const source = readFileSync(new URL('../../public/youtube-tadoku.user.js', import.meta.url), 'utf8');
const API = 'https://tadoku.app/api/internal/immersion';

function fixture({ duration = 1415, signedIn = true, incorrectLog = false, deferPost = false } = {}) {
  const elements = [];
  const listeners = new Map();
  const requests = [];
  const frames = [];
  let observer;
  let post;
  class Element {
    constructor(tag) {
      this.tagName = tag;
      this.style = {};
      this.children = [];
      this.listeners = new Map();
      this.textContent = '';
      elements.push(this);
    }
    append(...children) {
      for (const child of children) {
        child.remove();
        child.parent = this;
        this.children.push(child);
      }
    }
    prepend(child) { this.append(child); }
    after(child) { this.parent.append(child); }
    contains(child) { return child === this || this.children.some(item => item.contains(child)); }
    remove() {
      if (this.parent) this.parent.children = this.parent.children.filter(child => child !== this);
      this.parent = null;
    }
    setAttribute() {}
    addEventListener(type, callback) { this.listeners.set(type, callback); }
    querySelector(selector) {
      return ({ '#movie_player': player, 'video.html5-main-video': video, 'ytd-watch-metadata': metadata, '#top-row': row })[selector];
    }
    matches() { return this.blocked || false; }
  }
  const watch = new Element('ytd-watch-flexy');
  const player = new Element('div');
  const video = new Element('video');
  video.duration = duration;
  video.currentTime = 120;
  video.readyState = 1;
  const metadata = new Element('ytd-watch-metadata');
  const row = new Element('div');
  metadata.append(row);
  const document = {
    createElement: tag => new Element(tag),
    querySelector: () => watch,
    documentElement: {},
    addEventListener: (event, callback) => listeners.set(event, callback),
  };
  const location = { pathname: '/watch', href: 'https://www.youtube.com/watch?v=first&t=90&list=playlist' };
  const options = {
    activities: [{ id: 1, name: 'Reading' }, { id: 2, name: 'Listening' }],
    languages: [{ code: 'ja' }],
    units: [
      { id: 'reading', log_activity_id: 1, name: 'Minute', unit_key: 'reading_minute' },
      { id: 'dense', log_activity_id: 2, name: 'Dense minute', unit_key: 'listening_dense_minutes' },
      { id: 'ko', log_activity_id: 2, name: 'Minute', unit_key: 'listening_minute', language_code: 'ko' },
      { id: 'generic', log_activity_id: 2, name: 'Minute', unit_key: 'listening_minute' },
      { id: 'ja', log_activity_id: 2, name: 'Minute', unit_key: 'listening_minute', language_code: 'ja' },
    ],
  };
  const registration = (id, code, activity) => ({
    id, languages: [{ code }], contest: { allowed_activities: [{ id: activity }] },
  });
  const respond = (request, body, status = 200) => request.onload({ responseText: JSON.stringify(body), status });
  runInNewContext(source, {
    document, location, URL,
    window: { addEventListener: (event, callback) => listeners.set(event, callback) },
    requestAnimationFrame: callback => frames.push(callback),
    MutationObserver: class {
      constructor(callback) { observer = callback; }
      observe() {}
    },
    GM_xmlhttpRequest(request) {
      requests.push(request);
      if (request.url === 'https://tadoku.app/api/internal/identity/session') {
        respond(request, { account: signedIn ? { id: 'user' } : null, account_app: 'v2' });
      } else if (request.url.endsWith('/configuration-options')) {
        respond(request, options);
      } else if (request.url.endsWith('/ongoing-registrations')) {
        respond(request, { registrations: [
          registration('eligible', 'ja', 2), registration('korean', 'ko', 2),
          registration('reading-only', 'ja', 1), { id: 'no-contest', languages: [{ code: 'ja' }] },
        ] });
      } else if (request.method === 'POST') {
        post = request;
        if (!deferPost) respond(request, { ...JSON.parse(request.data), id: 'log', ...(incorrectLog ? { amount: 1, unit_id: 'dense', unit_key: 'listening_dense_minutes' } : {}) });
      } else if (request.method === 'PUT') {
        respond(request, { ...JSON.parse(request.data), id: 'log' });
      } else throw new Error(`Unexpected request: ${request.url}`);
    },
  });
  const container = elements.find(element => element.id === 'youtube-tadoku');
  const [button, status, signIn] = container.children;
  return {
    button, status, signIn, container, metadata, player, video, location, requests, options,
    click: () => button.listeners.get('click')(),
    event: type => listeners.get(type)(),
    observe: target => observer([{ target }]),
    flush() { while (frames.length) frames.shift()(); },
    get scheduledFrames() { return frames.length; },
    completePost() { respond(post, { ...JSON.parse(post.data), id: 'log' }); },
    failPost() { post.ontimeout(); },
  };
}

test('YouTube script logs the full duration with clean URL, youtube tag, and eligible contests', async () => {
  const f = fixture();
  assert.equal(f.requests.length, 0, 'loading a video never contacts Tadoku');
  assert.ok(f.metadata.contains(f.container));
  assert.equal(f.button.textContent, 'Log to Tadoku');
  await f.click();
  const post = f.requests.find(request => request.method === 'POST');
  assert.equal(post.url, `${API}/logs`);
  assert.deepEqual(JSON.parse(post.data), {
    registration_ids: ['eligible'], language_code: 'ja', activity_id: 2,
    amount: 23.6, unit_id: 'ja', unit_key: 'listening_minute',
    tags: ['youtube'], description: 'https://www.youtube.com/watch?v=first',
  });
  assert.ok(f.requests.every(request => request.anonymous === false));
  assert.equal(f.status.textContent, '23.6 minutes logged.');
  assert.equal(f.button.disabled, true);
  await f.click();
  assert.equal(f.requests.filter(request => request.method === 'POST').length, 1);
});

test('expired Tadoku login offers sign-in and never submits a log', async () => {
  const f = fixture({ signedIn: false });
  await f.click();
  assert.equal(f.requests.length, 1);
  assert.match(f.status.textContent, /Sign in to Tadoku/);
  assert.equal(f.signIn.hidden, false);
  assert.equal(f.button.disabled, false);
});

test('missing duration, live streams, and ads cannot submit a log', async () => {
  for (const duration of [NaN, Infinity, 0, -1]) {
    const f = fixture({ duration });
    assert.equal(f.button.disabled, true);
    await f.click();
    assert.equal(f.requests.length, 0);
  }
  const f = fixture();
  f.player.blocked = true;
  f.event('durationchange');
  f.flush();
  assert.equal(f.button.disabled, true);
  await f.click();
  assert.equal(f.requests.length, 0);
  f.player.blocked = false;
  f.event('loadedmetadata');
  f.flush();
  assert.equal(f.button.disabled, false);
});

test('in-page navigation removes the button off watch pages and restores per-video results', async () => {
  const f = fixture();
  await f.click();
  f.event('yt-navigate-start');
  assert.equal(f.container.parent, null);
  f.location.pathname = '/';
  f.event('yt-navigate-finish');
  f.flush();
  assert.equal(f.container.parent, null);
  f.location.pathname = '/watch';
  f.location.href = 'https://www.youtube.com/watch?v=second';
  f.video.duration = 60;
  f.event('yt-navigate-finish');
  f.flush();
  assert.equal(f.button.disabled, false);
  await f.click();
  assert.equal(f.status.textContent, '1 minutes logged.');
  f.location.href = 'https://www.youtube.com/watch?v=first';
  f.event('yt-navigate-finish');
  f.flush();
  assert.equal(f.button.disabled, true);
  assert.equal(f.status.textContent, '23.6 minutes logged.');
});

test('pending clicks stay tied to the original video when autoplay navigates', async () => {
  const f = fixture({ deferPost: true });
  const first = f.click();
  await new Promise(resolve => setImmediate(resolve));
  await f.click();
  assert.equal(f.requests.filter(request => request.method === 'POST').length, 1);
  f.location.href = 'https://www.youtube.com/watch?v=second';
  f.video.duration = 60;
  f.event('yt-navigate-finish');
  f.flush();
  f.completePost();
  await first;
  assert.equal(f.status.textContent, '');
  assert.equal(f.button.disabled, false);
  const post = f.requests.find(request => request.method === 'POST');
  assert.equal(JSON.parse(post.data).description, 'https://www.youtube.com/watch?v=first');
  assert.equal(JSON.parse(post.data).amount, 23.6);
});

test('an uncertain submission is never retried automatically', async () => {
  const f = fixture({ deferPost: true });
  const click = f.click();
  await new Promise(resolve => setImmediate(resolve));
  f.failPost();
  await click;
  assert.match(f.status.textContent, /Check your Tadoku logs before retrying/);
  assert.equal(f.requests.filter(request => request.method === 'POST').length, 1);
});

test('incorrect saved minutes are corrected by updating the created log', async () => {
  const f = fixture({ incorrectLog: true });
  await f.click();
  const update = f.requests.find(request => request.method === 'PUT');
  assert.equal(update.url, `${API}/logs/log`);
  assert.equal(JSON.parse(update.data).amount, 23.6);
  assert.equal(JSON.parse(update.data).unit_key, 'listening_minute');
  assert.equal(f.status.textContent, '23.6 minutes logged.');
});

test('legacy unit names and generic units work without selecting another language or dense minutes', async () => {
  const f = fixture();
  f.options.units = f.options.units.filter(unit => unit.id !== 'ja');
  delete f.options.units.find(unit => unit.id === 'generic').unit_key;
  await f.click();
  const payload = JSON.parse(f.requests.find(request => request.method === 'POST').data);
  assert.equal(payload.unit_id, 'generic');
  assert.equal(payload.unit_key, undefined);
  assert.equal(f.status.textContent, '23.6 minutes logged.');
});

test('own status mutations do not keep refreshing every animation frame', () => {
  const f = fixture();
  f.observe(f.button);
  assert.equal(f.scheduledFrames, 0);
  f.observe(f.player);
  f.observe(f.metadata);
  assert.equal(f.scheduledFrames, 1);
  f.flush();
  assert.equal(f.scheduledFrames, 0);
});
