<script>
  import { autoApprove, alwaysReuseMiningAssets, pendingCards, nativeSubtitles, sessionState, showErrorToast } from './stores.js';
  import { enrichCard } from './api.js';
  import { gatherTranslation } from './utils.js';
  import { contextSentence } from './review.js';

  let busy = false;
  let lastMine = null;
  const attempted = new Set();
  $: next = $autoApprove && !busy
    ? $pendingCards.find(c => c.source === 'pending' && !attempted.has(c.event.note_id)
      && c.end_ms > c.start_ms)
    : null;
  $: if (next) enhance(next);

  async function enhance(card) {
    busy = true;
    const noteId = card.event.note_id;
    attempted.add(noteId);
    const rangeKey = `${card.history_id}:${card.start_ms}:${card.end_ms}:${card.generate_avif}`;
    try {
      const result = await enrichCard({
        noteId, sentence: card.matched_text
          ? contextSentence([{ text: card.matched_text }], 0, 0, card.event.sentence) : card.event.sentence,
        translation: card.history_id === $sessionState.now_playing?.history_id
          ? gatherTranslation($nativeSubtitles, card.start_ms, card.end_ms) || null : null,
        startMs: card.start_ms, endMs: card.end_ms, itemId: card.history_id,
        generateAvif: card.generate_avif, matchedLineIndex: card.matched_line_index,
        includedLineFirst: card.included_line_first, includedLineLast: card.included_line_last,
        reuseAssetsFromNoteId: $alwaysReuseMiningAssets && lastMine?.rangeKey === rangeKey ? lastMine.noteId : null,
      });
      if (!result.success) throw new Error(result.error || 'Could not queue enhancement');
      lastMine = { noteId, rangeKey };
      pendingCards.update(cards => cards.filter(c => c.event.note_id !== noteId));
    } catch (error) {
      pendingCards.update(cards => cards.map(c => c.event.note_id === noteId ? { ...c, source: 'retry' } : c));
      showErrorToast(error.message);
    } finally {
      busy = false;
    }
  }
</script>
