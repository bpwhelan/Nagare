import assert from 'node:assert/strict';
import { test } from 'node:test';
import { getVideoBounds } from './video.js';

function video(left, top, width, height, options = {}) {
  return {
    tagName: 'VIDEO', paused: true, ended: false, style: {}, ...options,
    getBoundingClientRect: () => ({ left, top, right: left + width, bottom: top + height }),
  };
}
const viewport = { left: 0, top: 0, width: 1440, height: 900 };
const win = { innerWidth: viewport.width, innerHeight: viewport.height, getComputedStyle: video => video.style };
const page = (...videos) => ({ querySelectorAll: () => videos });

test('review centers on an off-center paused video, ignoring hidden and off-screen players', () => {
  const doc = page(
    video(0, 0, 1440, 900, { paused: false, style: { visibility: 'hidden' } }),
    video(0, 1000, 1440, 900, { paused: false }),
    video(100, 140, 800, 450),
    video(0, 0, 160, 90),
  );
  assert.deepEqual(getVideoBounds(doc, win), { left: 100, top: 140, width: 800, height: 450 });
});

test('active playback takes priority over larger paused videos and follows player replacement', () => {
  const doc = page(video(0, 0, 1200, 700), video(80, 200, 640, 360, { paused: false }));
  assert.deepEqual(getVideoBounds(doc, win), { left: 80, top: 200, width: 640, height: 360 });
  doc.querySelectorAll = () => [video(120, 100, 960, 540)];
  assert.deepEqual(getVideoBounds(doc, win), { left: 120, top: 100, width: 960, height: 540 });
});

test('fullscreen scopes video selection to the current player container', () => {
  const doc = page(video(0, 0, 1440, 900, { paused: false }));
  const current = video(20, 10, 1400, 800);
  doc.fullscreenElement = { querySelectorAll: () => [current] };
  assert.deepEqual(getVideoBounds(doc, win), { left: 20, top: 10, width: 1400, height: 800 });
  doc.fullscreenElement = current;
  assert.deepEqual(getVideoBounds(doc, win), { left: 20, top: 10, width: 1400, height: 800 });
});

test('partly scrolled videos keep review on screen; audio-only pages use the viewport', () => {
  assert.deepEqual(getVideoBounds(page(video(-100, -200, 800, 450)), win), { left: 0, top: 0, width: 700, height: 250 });
  assert.deepEqual(getVideoBounds(page(), win), viewport);
  assert.deepEqual(getVideoBounds(page(video(0, 0, 0, 0)), win), viewport);
});
