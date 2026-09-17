import assert from 'node:assert/strict';
import { test } from 'node:test';
import { DEFAULTS, normalizeServerUrl, matchesSite, matchesHotkey, isTyping, validateSettings } from './settings.js';

test('Jellyfin defaults match enabled origins without enabling unrelated pages', () => {
  const pattern = DEFAULTS.sites[0];
  assert.equal(matchesSite('https://jellyfin.example.com', pattern), true);
  assert.equal(matchesSite('https://jellyfin.home:8920', pattern), true);
  assert.equal(matchesSite('https://example.com', pattern), false);
  assert.equal(matchesSite('http://jellyfin.example.com', pattern), false);
  assert.equal(matchesSite('https://notjellyfin.example.com', pattern), false);
  assert.equal(matchesSite('https://jellyfinXexample.com', 'https://jellyfin.example.com'), false);
  assert.equal(matchesSite('http://[::1]:8096', 'http://[::1]:8096'), true);
});

test('server addresses preserve reverse-proxy prefixes and reject invalid transports', () => {
  assert.equal(normalizeServerUrl(' https://nagare.example.com/nagare/ '), 'https://nagare.example.com/nagare');
  assert.equal(normalizeServerUrl('http://localhost:9470/'), 'http://localhost:9470');
  for (const url of ['javascript:alert(1)', 'file:///tmp/', 'https://user:pass@example.com', 'https://example.com/?key=x']) {
    assert.throws(() => normalizeServerUrl(url));
  }
});

test('hotkeys use exact modifiers and never fire during typing, repeats, or IME composition', () => {
  const event = { key: 'n', altKey: true, ctrlKey: false, shiftKey: false, metaKey: false };
  assert.equal(matchesHotkey(event, 'Alt+N'), true);
  assert.equal(matchesHotkey({ ...event, shiftKey: true }, 'Alt+N'), false);
  assert.equal(matchesHotkey({ ...event, repeat: true }, 'Alt+N'), false);
  assert.equal(matchesHotkey({ ...event, isComposing: true }, 'Alt+N'), false);
  assert.equal(isTyping({ composedPath: () => [{ tagName: 'INPUT' }, { tagName: 'DIV' }] }), true);
  assert.equal(isTyping({ composedPath: () => [{ isContentEditable: true }] }), true);
  assert.equal(isTyping({ target: { tagName: 'BUTTON' } }), false);
  assert.equal(isTyping({ target: { tagName: 'INPUT', type: 'checkbox' } }), false);
});

test('settings validate editable constants and persisted form values', () => {
  const settings = validateSettings({ ...DEFAULTS, sites: '\nhttps://jellyfin.*\nhttp://localhost:8096/\n', width: '500' });
  assert.equal(settings.width, 500);
  assert.deepEqual(settings.sites, ['https://jellyfin.*', 'http://localhost:8096']);
  for (const value of [{ width: 100 }, { pollIntervalMs: 0 }, { sites: 'https://example.com/path' }, { hotkey: 'N' }, { hotkey: 'Alt+Alt+N' }]) {
    assert.throws(() => validateSettings({ ...DEFAULTS, ...value }));
  }
});
