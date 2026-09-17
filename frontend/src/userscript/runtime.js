import { DEFAULTS, validateSettings } from './settings.js';

const SETTINGS_KEY = 'nagare_userscript_settings';
const overrides = typeof NAGARE_DEFAULTS === 'undefined' ? {} : NAGARE_DEFAULTS;
export const defaults = { ...DEFAULTS, ...overrides };
export const configured = Boolean(GM_getValue(SETTINGS_KEY, null));
let settings;
try { settings = validateSettings({ ...defaults, ...GM_getValue(SETTINGS_KEY, {}) }); }
catch { settings = validateSettings(defaults); }

export const getSettings = () => ({ ...settings, sites: [...settings.sites] });
export function saveSettings(value) {
  settings = validateSettings(value);
  GM_setValue(SETTINGS_KEY, settings);
}

// The poller discards snapshots overlapping a command, so an older response
// cannot reopen a card that was just confirmed/skipped or undo a track change.
let revision = 0;
let mutations = 0;
export const requestState = () => ({ revision, mutations });

export function requestJson(path, options = {}) {
  return requestAt(settings.serverUrl, path, options);
}

export function requestAt(serverUrl, path, options = {}) {
  const mutation = options.method && options.method !== 'GET';
  if (mutation) { revision++; mutations++; }
  return new Promise((resolve, reject) => {
    const failure = () => reject(new Error('Cannot reach Nagare. Check the server address and allow it in your userscript manager.'));
    GM_xmlhttpRequest({
      method: options.method || 'GET',
      url: `${serverUrl}${path}`,
      headers: { Accept: 'application/json', ...(options.body ? { 'Content-Type': 'application/json' } : {}) },
      data: options.body,
      timeout: 30000,
      onload: response => {
        if (response.status < 200 || response.status >= 300) {
          reject(new Error(response.status === 404
            ? 'Update Nagare to a version that supports the userscript, and check the server address.'
            : `Nagare request failed (${response.status}). Check the server address and proxy login.`));
          return;
        }
        try { resolve(JSON.parse(response.responseText)); }
        catch { reject(new Error('Nagare returned a page instead of JSON. Check the server address and proxy login.')); }
      },
      onerror: failure,
      ontimeout: () => reject(new Error('Nagare did not respond within 30 seconds.')),
      onabort: failure,
    });
  }).finally(() => {
    if (mutation) { revision++; mutations--; }
  });
}

export function readPreference(key, fallback) {
  return GM_getValue(`nagare_${key}`, fallback);
}
export function writePreference(key, value) {
  GM_setValue(`nagare_${key}`, value);
}
