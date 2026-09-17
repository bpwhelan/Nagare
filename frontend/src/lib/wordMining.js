import { requestJson } from '#runtime';

export async function miningApi(path, body, method = 'POST') {
  const result = await requestJson(path, body === undefined ? {} : { method, body: JSON.stringify(body) });
  if (!result?.ok) throw new Error(result?.error || 'The mining request failed');
  return result;
}
export const miningPath = id => `/api/history/${encodeURIComponent(id)}/word-mining`;
export const defaultMiningFields = { word: 'Word', reading: 'Reading', meaning: 'Meaning', sentence: 'Sentence', audio: 'SentenceAudio', picture: 'Picture', source: 'Source' };

// LAN installations commonly use HTTP, where crypto.randomUUID is unavailable.
export function miningRequestId() {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  bytes[6] = (bytes[6] & 15) | 64;
  bytes[8] = (bytes[8] & 63) | 128;
  const hex = [...bytes].map(b => b.toString(16).padStart(2, '0')).join('');
  return `${hex.slice(0,8)}-${hex.slice(8,12)}-${hex.slice(12,16)}-${hex.slice(16,20)}-${hex.slice(20)}`;
}

export function unfamiliarCount(words = [], known = {}) {
  return new Set(words.filter(word => !['known', 'in_anki', 'mined'].includes(known[word]))).size;
}

export function bestOccurrence(candidate, workspace, known = {}) {
  return [...candidate.occurrences].sort((a, b) => {
    const score = index => {
      const line = workspace.track.lines[index];
      const duration = line.end_ms - line.start_ms;
      return unfamiliarCount(workspace.line_words[index], known) * 10
        + (duration < 800 || duration > 15000 ? 6 : 0)
        + Math.abs(line.text.length - 22) / 100;
    };
    return score(a) - score(b) || a - b;
  })[0];
}

export function makeMiningDraft(candidate, workspace, known = {}, padding = {}) {
  const index = bestOccurrence(candidate, workspace, known);
  const line = workspace.track.lines[index];
  return { term: candidate.term, reading: candidate.reading, meaning: candidate.definitions[0]?.meaning || '',
    sentence: line.text, first: index, last: index,
    start: Math.max(0, line.start_ms - (padding.audio_start_offset_ms ?? 100)) / 1000,
    end: Math.min(workspace.history.duration_ms || Infinity, line.end_ms + (padding.audio_end_offset_ms ?? 200)) / 1000 };
}

export function validMiningDraft(draft, duration) {
  return draft && draft.sentence.trim() && Number.isFinite(draft.start) && Number.isFinite(draft.end)
    && draft.start >= 0 && draft.end > draft.start && draft.end - draft.start <= 90
    && (!duration || Math.round(draft.end * 1000) <= duration);
}

export function draftPayload(draft) {
  const { start, end, ...rest } = draft;
  return { ...rest, start_ms: Math.round(start * 1000), end_ms: Math.round(end * 1000) };
}

export function rankedCandidates(workspace, known = {}, options = {}) {
  if (!workspace) return [];
  const search = (options.search || '').trim().toLocaleLowerCase();
  return workspace.candidates.map(candidate => {
    const occurrence = bestOccurrence(candidate, workspace, known);
    const unfamiliar = unfamiliarCount(workspace.line_words[occurrence], known);
    const recommended = !known[candidate.term] && candidate.definitions.length > 0
      && candidate.part_of_speech !== 'proper noun' && (candidate.common || candidate.occurrences.length > 1);
    const score = Math.min(candidate.occurrences.length, 12) * 3 + (candidate.common ? 12 : 0)
      + (candidate.definitions.length ? 5 : 0) - unfamiliar * 2 - (candidate.part_of_speech === 'proper noun' ? 20 : 0);
    return { ...candidate, occurrence, unfamiliar, recommended, score };
  }).filter(candidate => {
    const status = known[candidate.term];
    return (!options.status || options.status === 'all' || (options.status === 'new' ? !status : !!status))
      && (!options.common || candidate.common)
      && (!options.repeated || candidate.occurrences.length > 1)
      && (!options.iPlusOne || (candidate.unfamiliar === 1 && !status && candidate.part_of_speech !== 'custom phrase'))
      && (!search || `${candidate.term} ${candidate.reading} ${candidate.surfaces.join(' ')} ${candidate.definitions.map(d => d.meaning).join(' ')}`.toLocaleLowerCase().includes(search));
  }).sort((a, b) => (options.sort === 'appearance' ? a.occurrences[0] - b.occurrences[0]
    : options.sort === 'frequency' ? b.occurrences.length - a.occurrences.length : b.score - a.score)
    || a.term.localeCompare(b.term, 'ja'));
}

export function sentenceParts(text, candidate) {
  const forms = [...new Set([...(candidate?.surfaces || []), candidate?.term].filter(Boolean))].sort((a, b) => b.length - a.length);
  if (!forms.length) return [{ text, match: false }];
  const pattern = new RegExp(`(${forms.map(f => f.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('|')})`, 'g');
  return text.split(pattern).filter(Boolean).map(part => ({ text: part, match: forms.includes(part) }));
}
