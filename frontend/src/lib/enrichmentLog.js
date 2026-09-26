import { requestJson, clientKind } from '#runtime';

const received = new Map();

// Durations use the browser's monotonic clock, never subtraction of clocks on
// different machines. Only IDs, stage names, and timings leave the browser.
export function logEnrichment(noteId, stage, elapsedMs = null) {
  const now = performance.now();
  if (stage === 'received' && !received.has(noteId)) {
    received.set(noteId, now);
    if (received.size > 256) received.delete(received.keys().next().value);
  }
  const elapsed = elapsedMs ?? (received.has(noteId) ? now - received.get(noteId) : null);
  const event = { note_id: noteId, stage, client: clientKind, elapsed_ms: elapsed == null ? null : Math.max(0, Math.round(elapsed)) };
  console.info('[Nagare enhancement]', event);
  // Diagnostics must not block showing/confirming a card or show error toasts.
  requestJson('/api/enrich/client-event', { method: 'POST', body: JSON.stringify(event) }).catch(() => {});
}
