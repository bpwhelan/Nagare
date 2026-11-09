// ==UserScript==
// @name         YouTube - Log to Tadoku
// @namespace    https://github.com/bpwhelan/Nagare
// @version      1.0.2
// @description  Log a YouTube video's full duration to Tadoku as Japanese listening.
// @match        https://www.youtube.com/*
// @match        https://youtube.com/*
// @noframes
// @run-at       document-idle
// @grant        GM_xmlhttpRequest
// @connect      tadoku.app
// ==/UserScript==

(() => {
  'use strict';

  // Sign in at https://tadoku.app in this browser first. No Nagare server needed.
  const LANGUAGE = 'jpn';
  const API = 'https://tadoku.app/api/internal/immersion';
  const pending = new Set();
  const logged = new Map();
  const messages = new Map();
  let navigating = false;
  let scheduled = false;

  const container = document.createElement('div');
  container.id = 'youtube-tadoku';
  Object.assign(container.style, {
    display: 'flex', alignItems: 'center', flexWrap: 'wrap', gap: '12px',
    margin: '12px 0', font: '14px Roboto, Arial, sans-serif',
    color: 'var(--yt-spec-text-primary, #0f0f0f)',
  });
  const button = document.createElement('button');
  button.type = 'button';
  Object.assign(button.style, {
    border: 'none', borderRadius: '18px', padding: '0 16px', height: '36px',
    font: '500 14px Roboto, Arial, sans-serif', cursor: 'pointer',
    background: 'var(--yt-spec-badge-chip-background, #eee)', color: 'inherit',
  });
  const status = document.createElement('span');
  status.setAttribute('role', 'status');
  const signIn = document.createElement('a');
  signIn.textContent = 'Sign in to Tadoku';
  signIn.href = 'https://tadoku.app';
  signIn.target = '_blank';
  signIn.rel = 'noopener noreferrer';
  signIn.style.color = 'var(--yt-spec-call-to-action, #065fd4)';
  signIn.hidden = true;
  container.append(button, status, signIn);

  function videoId() {
    return location.pathname === '/watch' ? new URL(location.href).searchParams.get('v') : null;
  }

  function currentVideo() {
    const watch = document.querySelector('ytd-watch-flexy:not([hidden])');
    const player = watch?.querySelector('#movie_player');
    const video = player?.querySelector('video.html5-main-video');
    // Ads and live DVR windows are not the video's full duration.
    if (navigating || player?.matches('.ad-showing, .ad-interrupting, .ytp-live')) return null;
    return video?.readyState >= 1 && Number.isFinite(video.duration) && video.duration > 0 ? video : null;
  }

  function refresh() {
    const id = videoId();
    const watch = document.querySelector('ytd-watch-flexy:not([hidden])');
    const metadata = watch?.querySelector('ytd-watch-metadata');
    if (!id || !metadata || navigating) {
      container.remove();
      return;
    }
    if (!metadata.contains(container)) {
      const row = metadata.querySelector('#top-row');
      if (row) row.after(container);
      else metadata.prepend(container);
    }
    const video = currentVideo();
    const busy = pending.has(id);
    const done = logged.has(id);
    button.textContent = busy ? 'Logging to Tadoku…' : done ? 'Logged to Tadoku ✓' : 'Log to Tadoku';
    button.disabled = busy || done || !video;
    button.style.opacity = button.disabled ? '0.6' : '1';
    button.title = video ? `Log the full video: ${Math.ceil(video.duration / 6) / 10} minutes` : 'Waiting for a video duration (ads and live streams cannot be logged).';
    const message = messages.get(id);
    const text = done ? `${logged.get(id)} minutes logged.` : message?.text || '';
    if (status.textContent !== text) status.textContent = text;
    signIn.hidden = !message?.needsLogin;
  }

  function scheduleRefresh() {
    if (scheduled) return;
    scheduled = true;
    requestAnimationFrame(() => {
      scheduled = false;
      refresh();
    });
  }

  function request(url, method = 'GET', body) {
    return new Promise((resolve, reject) => {
      GM_xmlhttpRequest({
        url, method, timeout: 30000, anonymous: false,
        cookiePartition: { topLevelSite: 'https://tadoku.app' },
        headers: { Accept: 'application/json', ...(body ? { 'Content-Type': 'application/json' } : {}) },
        data: body ? JSON.stringify(body) : undefined,
        onload(response) {
          if (response.status === 401 || response.status === 403) {
            const error = new Error('Sign in to Tadoku in this browser, then click Log to Tadoku again.');
            error.needsLogin = true;
            reject(error);
            return;
          }
          if (response.status < 200 || response.status >= 300) {
            reject(new Error(`Tadoku returned HTTP ${response.status}. Check your Tadoku logs before retrying.`));
            return;
          }
          try { resolve(JSON.parse(response.responseText)); }
          catch { reject(new Error('Unexpected Tadoku response. Check your Tadoku logs before retrying.')); }
        },
        onerror: () => reject(new Error('Cannot reach Tadoku. Allow tadoku.app in your userscript manager; check your logs before retrying.')),
        ontimeout: () => reject(new Error('Tadoku timed out. Check your Tadoku logs before retrying.')),
        onabort: () => reject(new Error('Request cancelled. Check your Tadoku logs before retrying.')),
      });
    });
  }

  function logMatches(log, payload) {
    return log.duration_seconds === payload.duration_seconds
      && (log.amount == null || log.amount === 0) && !log.unit_key
      && log.description === payload.description
      && log.tags?.some(tag => tag.toLowerCase() === 'youtube');
  }

  async function createLog(id, seconds) {
    const session = await request('https://tadoku.app/api/internal/identity/session');
    if (!session.account?.id) {
      const error = new Error('Sign in to Tadoku in this browser, then click Log to Tadoku again.');
      error.needsLogin = true;
      throw error;
    }
    const [options, { registrations = [] }] = await Promise.all([
      request(`${API}/logs/configuration-options`),
      request(`${API}/contests/ongoing-registrations`),
    ]);
    if (!options.languages?.some(language => language.code.toLowerCase() === LANGUAGE)) {
      throw new Error('Tadoku did not offer Japanese as a logging language.');
    }
    const activity = options.activities.find(activity => activity.name.toLowerCase() === 'listening');
    if (!activity) throw new Error('Tadoku did not offer Listening.');
    const payload = {
      registration_ids: registrations.filter(registration =>
        registration.languages?.some(language => language.code.toLowerCase() === LANGUAGE)
        && registration.contest?.allowed_activities?.some(allowed => allowed.id === activity.id)
      ).map(registration => registration.id),
      language_code: LANGUAGE,
      activity_id: activity.id,
      duration_seconds: Math.ceil(seconds),
      tags: ['youtube'],
      description: `https://www.youtube.com/watch?v=${encodeURIComponent(id)}`,
    };
    let log = await request(`${API}/logs`, 'POST', payload);
    if (!log.id) throw new Error('Tadoku did not return a log ID. Check your logs before retrying.');
    if (!logMatches(log, payload)) {
      log = await request(`${API}/logs/${encodeURIComponent(log.id)}`, 'PUT', {
        duration_seconds: payload.duration_seconds,
        tags: [...new Set([...(log.tags || []), 'youtube'])],
        description: payload.description,
      });
      if (!logMatches(log, payload)) {
        throw new Error('Tadoku saved unexpected log details. Check your logs before retrying.');
      }
    }
    return Math.ceil(payload.duration_seconds / 6) / 10;
  }

  button.addEventListener('click', async () => {
    const id = videoId();
    const video = currentVideo();
    if (!id || !video || pending.has(id) || logged.has(id)) return;
    // Capture the duration and URL before any requests; autoplay may change videos.
    const seconds = video.duration;
    pending.add(id);
    messages.delete(id);
    refresh();
    try { logged.set(id, await createLog(id, seconds)); }
    catch (error) { messages.set(id, { text: error.message, needsLogin: error.needsLogin }); }
    finally {
      pending.delete(id);
      refresh();
    }
  });

  document.addEventListener('yt-navigate-start', () => { navigating = true; refresh(); });
  document.addEventListener('yt-navigate-finish', () => { navigating = false; scheduleRefresh(); });
  document.addEventListener('loadedmetadata', scheduleRefresh, true);
  document.addEventListener('durationchange', scheduleRefresh, true);
  document.addEventListener('emptied', scheduleRefresh, true);
  window.addEventListener('popstate', scheduleRefresh);
  // YouTube inserts/replaces metadata and toggles ad classes without a full reload.
  new MutationObserver(records => {
    if (records.some(record => !container.contains(record.target))) scheduleRefresh();
  }).observe(document.documentElement, {
    childList: true, subtree: true, attributes: true, attributeFilter: ['class', 'hidden'],
  });
  refresh();
})();
