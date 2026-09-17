// Defaults also appear, unminified, at the top of the installable userscript.
export const DEFAULTS = {
  serverUrl: 'http://localhost:9470',
  sites: ['https://jellyfin.*'],
  mode: 'sidebar',
  side: 'right',
  width: 440,
  hotkey: 'Alt+N',
  showLauncher: true,
  openOnCard: true,
  pollIntervalMs: 750,
};

export function normalizeServerUrl(value) {
  const url = new URL(String(value).trim());
  if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password) {
    throw new Error('Use an HTTP or HTTPS Nagare address without embedded credentials.');
  }
  if (url.search || url.hash) throw new Error('Use the Nagare base address without a query or fragment.');
  return url.href.replace(/\/+$/, '');
}

// Patterns match origins, including an optional port. A wildcard never turns
// into a regular expression supplied by the page or the user.
export function matchesSite(origin, pattern) {
  const escaped = pattern.trim().replace(/\/+$/, '').replace(/[.+?^${}()|[\]\\]/g, '\\$&');
  return new RegExp(`^${escaped.replace(/\*/g, '.*')}$`, 'i').test(origin);
}

export function parseHotkey(value) {
  const parts = String(value).split('+').map(p => p.trim().toLowerCase());
  const key = parts.pop();
  const allowed = ['alt', 'ctrl', 'shift', 'meta'];
  if (!key || !parts.length || parts.some(p => !allowed.includes(p)) || new Set(parts).size !== parts.length
    || allowed.includes(key) || !(/^[a-z0-9]$/.test(key) || /^f(?:[1-9]|1[0-2])$/.test(key))) {
    throw new Error('Choose a modified letter, digit, or function key, such as Alt+N or Ctrl+Shift+N.');
  }
  return { key, alt: parts.includes('alt'), ctrl: parts.includes('ctrl'), shift: parts.includes('shift'), meta: parts.includes('meta') };
}

export function matchesHotkey(event, value) {
  const hotkey = parseHotkey(value);
  return !event.repeat && !event.isComposing && event.key.toLowerCase() === hotkey.key
    && event.altKey === hotkey.alt && event.ctrlKey === hotkey.ctrl
    && event.shiftKey === hotkey.shift && event.metaKey === hotkey.meta;
}

export function isTyping(event) {
  return (event.composedPath?.() || [event.target]).some(el =>
    el?.isContentEditable || /^(TEXTAREA|SELECT)$/.test(el?.tagName)
    || (el?.tagName === 'INPUT' && !/^(checkbox|radio|range|button|submit|reset)$/i.test(el.type || 'text'))
    || el?.getAttribute?.('role') === 'textbox');
}

export function validateSettings(value) {
  const settings = { ...DEFAULTS, ...value };
  settings.serverUrl = normalizeServerUrl(settings.serverUrl);
  parseHotkey(settings.hotkey);
  if (!['sidebar', 'hotkey'].includes(settings.mode)) throw new Error('Choose a display mode.');
  if (!['left', 'right'].includes(settings.side)) throw new Error('Choose a sidebar side.');
  settings.width = Number(settings.width);
  if (!Number.isFinite(settings.width) || settings.width < 320 || settings.width > 1000) {
    throw new Error('Sidebar width must be between 320 and 1000 pixels.');
  }
  settings.pollIntervalMs = Number(settings.pollIntervalMs);
  if (!Number.isFinite(settings.pollIntervalMs) || settings.pollIntervalMs < 500 || settings.pollIntervalMs > 10000) {
    throw new Error('Refresh interval must be between 500 and 10000 milliseconds.');
  }
  settings.sites = (Array.isArray(settings.sites) ? settings.sites : String(settings.sites).split('\n'))
    .map(s => s.trim().replace(/\/+$/, '')).filter(Boolean);
  if (settings.sites.some(s => !/^https?:\/\/[^/\s?#]+$/.test(s))) {
    throw new Error('Enter one site origin per line, such as https://jellyfin.* or http://localhost:8096.');
  }
  return settings;
}
