const BASE = '';

async function api(path, options = {}) {
  const resp = await fetch(`${BASE}${path}`, {
    headers: { 'Content-Type': 'application/json' },
    ...options,
  });
  return resp.json();
}

export async function getState() {
  return api('/api/state');
}

export async function getSessions() {
  return api('/api/sessions');
}

export async function selectSession(sessionId) {
  return api('/api/sessions/select', {
    method: 'POST',
    body: JSON.stringify({ session_id: sessionId }),
  });
}

export async function getSubtitles() {
  return api('/api/subtitles');
}

export async function selectSubtitleTrack(candidateId = null) {
  const body = { candidate_id: candidateId };
  return api('/api/subtitles/select', {
    method: 'POST',
    body: JSON.stringify(body),
  });
}

export async function enrichCard({
  noteId,
  sentence,
  startMs,
  endMs,
  generateAvif = true,
  itemId = null,
  matchedLineIndex = null,
  includedLineFirst = null,
  includedLineLast = null,
}) {
  const body = {
    note_id: noteId,
    sentence,
    start_ms: startMs,
    end_ms: endMs,
    generate_avif: generateAvif,
  };
  if (matchedLineIndex != null) body.matched_line_index = matchedLineIndex;
  if (includedLineFirst != null) body.included_line_first = includedLineFirst;
  if (includedLineLast != null) body.included_line_last = includedLineLast;
  if (itemId) body.item_id = itemId;
  return api('/api/enrich', {
    method: 'POST',
    body: JSON.stringify(body),
  });
}

export async function skipEnrichment(noteId) {
  return api('/api/enrich/skip', {
    method: 'POST',
    body: JSON.stringify({ note_id: noteId }),
  });
}

export async function getPendingEnrichments() {
  return api('/api/enrich/pending');
}

export async function getMinedHistory() {
  return api('/api/mined');
}

export async function getDialogByNoteId(noteId) {
  return api(`/api/dialog/note/${noteId}`);
}

export async function getDialogByCardId(cardId) {
  return api(`/api/dialog/card/${cardId}`);
}

export async function getConfig() {
  return api('/api/config');
}

export async function seekTo(positionMs) {
  return api('/api/seek', {
    method: 'POST',
    body: JSON.stringify({ position_ms: positionMs }),
  });
}

/**
 * Toggle play/pause. Pass `paused` to force a specific state, or omit to toggle.
 * @param {boolean} [paused]
 */
export async function playPause(paused) {
  const body = {};
  if (paused !== undefined) body.paused = paused;
  return api('/api/play-pause', {
    method: 'POST',
    body: JSON.stringify(body),
  });
}

export async function previewAudio(startMs, endMs, itemId = null) {
  const body = { start_ms: startMs, end_ms: endMs };
  if (itemId) body.item_id = itemId;
  return api('/api/preview-audio', {
    method: 'POST',
    body: JSON.stringify(body),
  });
}

export async function previewScreenshot(timeMs, itemId = null) {
  const body = { time_ms: timeMs };
  if (itemId) body.item_id = itemId;
  return api('/api/preview-screenshot', {
    method: 'POST',
    body: JSON.stringify(body),
  });
}

export async function getHistory() {
  return api('/api/history');
}

export async function getHistorySubtitles(itemId) {
  return api(`/api/history/${itemId}/subtitles`);
}

export async function activateHistoryItem(itemId) {
  return api(`/api/history/${itemId}/activate`, { method: 'POST' });
}

export async function getSubtitleMatches(sentence, itemId = null) {
  const body = { sentence };
  if (itemId) body.item_id = itemId;
  return api('/api/subtitle/matches', {
    method: 'POST',
    body: JSON.stringify(body),
  });
}

export async function updateConfig(config) {
  return api('/api/config', {
    method: 'PUT',
    body: JSON.stringify(config),
  });
}
