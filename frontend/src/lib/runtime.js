// The website uses browser APIs. The userscript build substitutes an adapter
// with privileged cross-origin requests and userscript-manager storage.
export async function requestJson(path, options = {}) {
  const response = await fetch(path, {
    headers: { 'Content-Type': 'application/json' },
    ...options,
  });
  if (!response.ok) throw new Error(`Request failed (${response.status})`);
  return response.json();
}

export function readPreference(key, fallback) {
  try {
    const stored = localStorage.getItem(key);
    return stored === null ? fallback : JSON.parse(stored);
  } catch {
    return fallback;
  }
}

export function writePreference(key, value) {
  try { localStorage.setItem(key, JSON.stringify(value)); } catch { /* Storage may be disabled. */ }
}
