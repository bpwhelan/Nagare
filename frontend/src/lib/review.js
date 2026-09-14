// Keep Anki markup (including highlighted words) when adding subtitle context.
export function plainText(value = '') {
  return value.replace(/<br\s*\/?>/gi, '\n').replace(/<[^>]*>/g, '')
    .replace(/&nbsp;/g, ' ').replace(/&lt;/g, '<').replace(/&gt;/g, '>')
    .replace(/&quot;/g, '"').replace(/&#39;/g, "'").replace(/&amp;/g, '&').trim();
}

function escapeHtml(text) {
  return text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

export function contextSentence(lines, first, last, original) {
  const text = lines.slice(first, last + 1).map(l => l.text).join('');
  const originalText = plainText(original);
  const index = originalText ? text.indexOf(originalText) : -1;
  if (index >= 0) {
    return escapeHtml(text.slice(0, index)) + original + escapeHtml(text.slice(index + originalText.length));
  }
  // Subtitle punctuation can differ from the sentence that Yomitan supplied.
  const normalized = value => {
    let text = '', positions = [];
    for (let i = 0; i < value.length;) {
      const char = String.fromCodePoint(value.codePointAt(i));
      if (!/[\s。、！？「」『』（）【】・…―.,!?"'()]/u.test(char)) {
        text += char;
        for (let j = 0; j < char.length; j++) positions.push([i, i + char.length]);
      }
      i += char.length;
    }
    return { text, positions };
  };
  const full = normalized(text), source = normalized(originalText);
  const start = source.text ? full.text.indexOf(source.text) : -1;
  if (start >= 0) {
    const from = full.positions[start][0], to = full.positions[start + source.text.length - 1][1];
    return escapeHtml(text.slice(0, from)) + original + escapeHtml(text.slice(to));
  }
  return escapeHtml(text);
}

export function validRange(startSeconds, endSeconds) {
  return startSeconds !== '' && endSeconds !== '' && Number.isFinite(Number(startSeconds))
    && Number.isFinite(Number(endSeconds)) && Number(startSeconds) >= 0 && Number(endSeconds) > Number(startSeconds);
}

export function makeReviewDraft(card, lines, config = {}) {
  const d = card.dialog;
  const first = d.included_line_first ?? d.matched_line_index ?? null;
  const last = d.included_line_last ?? first;
  const start = d.start_ms ?? Math.max(0, (lines[first]?.start_ms ?? 0) - (config.audio_start_offset_ms ?? 100));
  const end = d.end_ms ?? (lines[last]?.end_ms ?? 0) + (config.audio_end_offset_ms ?? 500);
  return { sentence: d.event.sentence, translation: '', first, last,
    start: start / 1000, end: end / 1000, generateAvif: d.generate_avif ?? true, dirty: false };
}
