<script>
  import { onDestroy } from 'svelte';
  import {
    activeHistoryItemId,
    audioEndOffset,
    audioStartOffset,
    autoApprove,
    currentView,
    defaultGenerateAvif,
    dialogCard,
    pauseOnEnhance,
    pendingCards,
    positionMs,
    replaceRoute,
    showErrorToast,
    showToast,
    subtitles,
    isPlaying,
  } from './stores.js';
  import { enrichCard, skipEnrichment, previewAudio, previewScreenshot, getSubtitleMatches, firePlayPause, queueEnrichCard } from './api.js';
  import { formatTime } from './utils.js';

  $: card = $dialogCard || $pendingCards[0] || null;
  $: isRouteCard = $dialogCard != null;
  $: isHistoryCard = card?.source === 'mining_history';
  $: shouldAutoApprovePending = Boolean(card && !isRouteCard && card.source === 'pending' && $autoApprove);
  $: mediaItemId = card?.history_id || $activeHistoryItemId || null;
  $: matchedIndex = card?.matched_line_index;
  $: matchedLine = matchedIndex != null ? $subtitles[matchedIndex] : null;

  // Fallback: nearest subtitle to current playback position (live mode only)
  $: nearestFallback = (!matchedLine && !$activeHistoryItemId && $subtitles.length > 0)
    ? $subtitles.reduce((best, line) => {
        const mid = (line.start_ms + line.end_ms) / 2;
        const bestMid = (best.start_ms + best.end_ms) / 2;
        return Math.abs(mid - $positionMs) < Math.abs(bestMid - $positionMs) ? line : best;
      }, $subtitles[0])
    : null;

  // History match state
  let historyMatches = null;
  let fetchingMatches = false;
  let selectedHistoryMatch = null;
  let lastCardKey = null;
  let submitting = false;
  let lastAutoApprovedCardKey = null;

  // Track which subtitle lines are included in the current selection
  // (the original matched line index + any appended prev/next lines)
  let includedLineFirst = null;  // first subtitle index in selection
  let includedLineLast = null;   // last subtitle index in selection

  // Which line index is the "anchor" (the originally matched line)
  $: storedAnchorLineIndex = matchedIndex ?? card?.included_line_first ?? null;
  $: anchorLineIndex = $activeHistoryItemId
    ? (selectedHistoryMatch?.line_index ?? storedAnchorLineIndex)
    : (matchedIndex ?? (nearestFallback ? $subtitles.indexOf(nearestFallback) : null));

  $: activeLine = $activeHistoryItemId
    ? ($subtitles[selectedHistoryMatch?.line_index ?? storedAnchorLineIndex ?? -1] || null)
    : (matchedLine || nearestFallback);

  // Context lines showing included range highlighted
  $: contextLines = getContextLines(anchorLineIndex, includedLineFirst, includedLineLast, $subtitles);

  let editedSentence = '';
  let startMs = 0;
  let endMs = 0;
  let generateAvif = true;

  // Audio preview state
  let audioPreviewUrl = null;
  let audioElement = null;
  let audioLoading = false;
  let audioIsPlaying = false;
  let audioRangeStart = null; // startMs the cached audio was built for
  let audioRangeEnd = null;   // endMs the cached audio was built for

  // Screenshot preview state
  let screenshotUrl = null;
  let screenshotLoading = false;

  // Track if we paused playback when the dialog opened
  let pausedByDialog = false;

  // Slider window: show ±15s around the anchor line for the range slider
  const SLIDER_PADDING_MS = 15000;
  $: sliderMin = activeLine ? Math.max(0, activeLine.start_ms - SLIDER_PADDING_MS) : 0;
  $: sliderMax = activeLine ? activeLine.end_ms + SLIDER_PADDING_MS : 10000;

  const NORMALIZE_IGNORED_CHARS = new Set([
    '。', '、', '！', '？', '「', '」', '『', '』', '（', '）', '【', '】', '・', '…', '―',
    '.', ',', '!', '?', '"', '\'', '(', ')', ' '
  ]);

  function decodeHtml(html) {
    if (!html) return '';
    const textarea = document.createElement('textarea');
    textarea.innerHTML = html;
    return textarea.value;
  }

  function stripHtml(html) {
    return decodeHtml((html || '').replace(/<br\s*\/?>/gi, '\n').replace(/<[^>]*>/g, '')).trim();
  }

  function buildNormalizedIndex(text) {
    const positions = [];
    let normalized = '';

    for (let i = 0; i < text.length;) {
      const codePoint = text.codePointAt(i);
      const char = String.fromCodePoint(codePoint);
      const end = i + char.length;
      if (!/\s/u.test(char) && !NORMALIZE_IGNORED_CHARS.has(char)) {
        normalized += char;
        positions.push({ start: i, end });
      }
      i = end;
    }

    return { normalized, positions };
  }

  function mergeSentenceMarkup(fullText, sentenceHtml) {
    const sourceHtml = sentenceHtml || '';
    const sourcePlain = stripHtml(sourceHtml);
    if (!sourceHtml || !sourcePlain || !fullText) return fullText;

    const exactStart = fullText.indexOf(sourcePlain);
    if (exactStart >= 0) {
      return fullText.slice(0, exactStart) + sourceHtml + fullText.slice(exactStart + sourcePlain.length);
    }

    const fullIndex = buildNormalizedIndex(fullText);
    const sourceIndex = buildNormalizedIndex(sourcePlain);
    if (!fullIndex.normalized || !sourceIndex.normalized) return fullText;

    const normalizedStart = fullIndex.normalized.indexOf(sourceIndex.normalized);
    if (normalizedStart < 0) return fullText;

    const start = fullIndex.positions[normalizedStart]?.start;
    const end = fullIndex.positions[normalizedStart + sourceIndex.normalized.length - 1]?.end;
    if (start == null || end == null || end <= start) return fullText;

    return fullText.slice(0, start) + sourceHtml + fullText.slice(end);
  }

  function buildSelectedSentence() {
    if (includedLineFirst == null || includedLineLast == null) {
      return card?.event.sentence || '';
    }

    const lines = $subtitles.slice(includedLineFirst, includedLineLast + 1);
    const fullText = lines.map(l => l.text).join('');
    return mergeSentenceMarkup(fullText, card?.event.sentence || '');
  }

  // Detect new card or route-backed edit state — reset local UI state
  $: cardKey = card
    ? `${card.source || 'pending'}:${card.event.note_id}:${card.updated_at || 'new'}`
    : null;

  $: if (card && cardKey !== lastCardKey) {
    lastCardKey = cardKey;
    editedSentence = card.event.sentence || '';
    historyMatches = null;
    selectedHistoryMatch = matchedIndex != null ? { line_index: matchedIndex } : null;
    includedLineFirst = card.included_line_first ?? null;
    includedLineLast = card.included_line_last ?? null;
    startMs = card.start_ms ?? 0;
    endMs = card.end_ms ?? 0;
    generateAvif = card.generate_avif ?? $defaultGenerateAvif;
    submitting = false;
    cleanupAudio();
    cleanupScreenshot();
    if ($activeHistoryItemId && matchedIndex == null && includedLineFirst == null) {
      fetchHistoryMatches(card.event.sentence);
    } else if ($pauseOnEnhance && $isPlaying && !shouldAutoApprovePending) {
      firePlayPause(true);
      pausedByDialog = true;
    }
  }

  /** Compute startMs/endMs from includedLine* bounds + configured offsets. */
  function setRangeFromInclusion() {
    if (includedLineFirst == null || includedLineLast == null) return;
    const first = $subtitles[includedLineFirst];
    const last = $subtitles[includedLineLast];
    if (!first || !last) return;
    startMs = Math.max(0, first.start_ms - $audioStartOffset);
    endMs = last.end_ms + $audioEndOffset;
  }

  // When the anchor line resolves, initialize the included range to just that line
  $: if (anchorLineIndex != null && includedLineFirst == null && $subtitles[anchorLineIndex]) {
    includedLineFirst = anchorLineIndex;
    includedLineLast = anchorLineIndex;
    setRangeFromInclusion();
    editedSentence = buildSelectedSentence();
  }

  $: autoApproveReady = shouldAutoApprovePending
    && includedLineFirst != null
    && includedLineLast != null
    && endMs > startMs;

  $: if (autoApproveReady && cardKey !== lastAutoApprovedCardKey && !submitting) {
    lastAutoApprovedCardKey = cardKey;
    handleConfirm();
  }

  function rebuildSentence() {
    editedSentence = buildSelectedSentence();
  }

  async function fetchHistoryMatches(sentence) {
    fetchingMatches = true;
    try {
      const results = await getSubtitleMatches(sentence, $activeHistoryItemId);
      historyMatches = results;
      if (results.length === 1) {
        applyHistoryMatch(results[0]);
      }
    } catch (e) {
      historyMatches = [];
      showErrorToast('Match search failed: ' + e.message);
    } finally {
      fetchingMatches = false;
    }
  }

  function applyHistoryMatch(match) {
    selectedHistoryMatch = match;
    includedLineFirst = match.line_index;
    includedLineLast = match.line_index;
    setRangeFromInclusion();
    rebuildSentence();
  }

  // Append the previous subtitle line (extend selection backwards)
  function appendPrev() {
    if (includedLineFirst == null || includedLineFirst <= 0) return;
    includedLineFirst--;
    setRangeFromInclusion();
    rebuildSentence();
  }

  // Append the next subtitle line (extend selection forwards)
  function appendNext() {
    if (includedLineLast == null || includedLineLast >= $subtitles.length - 1) return;
    includedLineLast++;
    setRangeFromInclusion();
    rebuildSentence();
  }

  // Reset selection to just the anchor line
  function resetSelection() {
    if (anchorLineIndex == null) return;
    includedLineFirst = anchorLineIndex;
    includedLineLast = anchorLineIndex;
    setRangeFromInclusion();
    rebuildSentence();
  }

  function getContextLines(anchor, first, last, subs) {
    if (anchor == null || !subs.length) return [];
    const viewStart = Math.max(0, (first ?? anchor) - 3);
    const viewEnd = Math.min(subs.length, (last ?? anchor) + 4);
    return subs.slice(viewStart, viewEnd).map((line, i) => {
      const idx = viewStart + i;
      return {
        ...line,
        isAnchor: idx === anchor,
        isIncluded: first != null && last != null && idx >= first && idx <= last,
      };
    });
  }

  function removeCardFromQueue(noteId) {
    pendingCards.update(cards => cards.filter(item => item.event.note_id !== noteId));
  }

  function closeRouteDialog(returnToHistory = false) {
    dialogCard.set(null);
    replaceRoute('/');
    if (returnToHistory) {
      currentView.set('history');
    }
  }

  function closeDialogAfterDispatch(noteId, returnToHistory = false) {
    cleanupAudio();
    cleanupScreenshot();
    removeCardFromQueue(noteId);

    // Only unpause playback when there are no more pending cards.
    // This prevents play/pause ping-pong that floods the media server
    // with requests and starves the enrichment API calls.
    const hasMoreCards = $pendingCards.length > 0;
    if (pausedByDialog && !hasMoreCards) {
      firePlayPause(false);
      pausedByDialog = false;
    }

    if (isRouteCard) {
      closeRouteDialog(returnToHistory);
    }
    submitting = false;
  }

  async function handleConfirm() {
    if (!card || submitting) return;
    const noteId = card.event.note_id;
    const payload = {
      noteId,
      sentence: editedSentence,
      startMs,
      endMs,
      generateAvif,
      itemId: mediaItemId,
      matchedLineIndex: matchedIndex,
      includedLineFirst,
      includedLineLast,
    };

    console.log('[EnrichDialog] Confirming note', noteId, payload);

    // Close dialog immediately — enhancement runs in the background
    closeDialogAfterDispatch(noteId, isHistoryCard);
    showToast('success', isHistoryCard ? 'Save queued in background' : 'Enhancement queued');

    // Queue the enrichment — calls are serialized to avoid connection pool exhaustion
    queueEnrichCard(payload);
  }

  async function handleSkip() {
    if (!card || submitting) return;
    submitting = true;
    if (pausedByDialog) {
      firePlayPause(false);
      pausedByDialog = false;
    }
    cleanupAudio();
    cleanupScreenshot();
    if (card.source === 'pending') {
      await skipEnrichment(card.event.note_id);
      removeCardFromQueue(card.event.note_id);
    }
    if (isRouteCard) {
      closeRouteDialog(isHistoryCard);
    }
    submitting = false;
  }

  // ── Audio preview ──

  function cleanupAudio() {
    if (audioElement) {
      audioElement.pause();
      audioElement = null;
    }
    if (audioPreviewUrl) {
      URL.revokeObjectURL(audioPreviewUrl);
      audioPreviewUrl = null;
    }
    audioLoading = false;
    audioIsPlaying = false;
    audioRangeStart = null;
    audioRangeEnd = null;
  }

  function buildAudioElement(url) {
    const audio = new Audio(url);
    audio.addEventListener('play', () => { audioIsPlaying = true; });
    audio.addEventListener('pause', () => { audioIsPlaying = false; });
    audio.addEventListener('ended', () => { audioIsPlaying = false; });
    return audio;
  }

  async function fetchAudioPreview() {
    const result = await previewAudio(startMs, endMs, mediaItemId);
    if (result.error) {
      throw new Error(result.error);
    }
    if (!result.audio_base64) {
      throw new Error('No audio returned');
    }

    const bytes = atob(result.audio_base64);
    const arr = new Uint8Array(bytes.length);
    for (let i = 0; i < bytes.length; i++) arr[i] = bytes.charCodeAt(i);
    const blob = new Blob([arr], { type: 'audio/ogg; codecs=opus' });
    const nextUrl = URL.createObjectURL(blob);

    if (audioElement) {
      audioElement.pause();
    }
    if (audioPreviewUrl) {
      URL.revokeObjectURL(audioPreviewUrl);
    }

    audioPreviewUrl = nextUrl;
    audioElement = buildAudioElement(nextUrl);
    audioRangeStart = startMs;
    audioRangeEnd = endMs;
  }

  async function handlePlayAudio() {
    try {
      const audioStale = !audioElement || audioRangeStart !== startMs || audioRangeEnd !== endMs;
      if (audioStale) {
        audioLoading = true;
        await fetchAudioPreview();
      }

      if (!audioElement) return;
      audioElement.currentTime = 0;
      await audioElement.play();
    } catch (e) {
      showErrorToast(e.message);
    } finally {
      audioLoading = false;
    }
  }

  const SUBTITLE_MARKER_PALETTE = ['#4a9ed6', '#e94560', '#4ab870'];

  function subtitleMarkerColor(index, isIncluded) {
    const color = SUBTITLE_MARKER_PALETTE[index % SUBTITLE_MARKER_PALETTE.length];
    return isIncluded ? color : `${color}80`;
  }

  // ── Screenshot preview ──

  function cleanupScreenshot() {
    if (screenshotUrl) {
      URL.revokeObjectURL(screenshotUrl);
      screenshotUrl = null;
    }
  }

  async function handlePreviewScreenshot() {
    cleanupScreenshot();
    screenshotLoading = true;
    try {
      const midMs = Math.round((startMs + endMs) / 2);
      const result = await previewScreenshot(midMs, mediaItemId);
      if (result.error) { showErrorToast(result.error); screenshotLoading = false; return; }
      if (result.image_base64) {
        const bytes = atob(result.image_base64);
        const arr = new Uint8Array(bytes.length);
        for (let i = 0; i < bytes.length; i++) arr[i] = bytes.charCodeAt(i);
        const blob = new Blob([arr], { type: 'image/avif' });
        screenshotUrl = URL.createObjectURL(blob);
      }
    } catch (e) {
      showErrorToast(e.message);
    } finally {
      screenshotLoading = false;
    }
  }

  // ── Range slider helpers ──

  let sliderTrack = null;
  let dragging = null; // 'start' | 'end' | null

  function sliderPct(ms) {
    const range = sliderMax - sliderMin;
    if (range <= 0) return 0;
    return Math.max(0, Math.min(100, ((ms - sliderMin) / range) * 100));
  }

  function msFromPct(pct) {
    return Math.round(sliderMin + (pct / 100) * (sliderMax - sliderMin));
  }

  function handleSliderPointerDown(e, thumb) {
    dragging = thumb;
    e.currentTarget.setPointerCapture(e.pointerId);
  }

  function handleSliderPointerMove(e) {
    if (!dragging || !sliderTrack) return;
    const rect = sliderTrack.getBoundingClientRect();
    const pct = Math.max(0, Math.min(100, ((e.clientX - rect.left) / rect.width) * 100));
    const ms = msFromPct(pct);
    if (dragging === 'start') {
      startMs = Math.min(ms, endMs - 100);
    } else {
      endMs = Math.max(ms, startMs + 100);
    }
    updateIncludedFromSlider();
  }

  /** Recompute includedLine* from current startMs/endMs and rebuild sentence if changed. */
  function updateIncludedFromSlider() {
    const subs = $subtitles;
    let first = null;
    for (let i = 0; i < subs.length; i++) {
      if (subs[i].end_ms > startMs) { first = i; break; }
    }
    let last = null;
    for (let i = subs.length - 1; i >= 0; i--) {
      if (subs[i].start_ms < endMs) { last = i; break; }
    }
    if (first != null && last != null && first <= last) {
      const changed = first !== includedLineFirst || last !== includedLineLast;
      includedLineFirst = first;
      includedLineLast = last;
      if (changed) rebuildSentence();
    }
  }

  function handleSliderPointerUp() {
    dragging = null;
  }

  onDestroy(() => {
    cleanupAudio();
    cleanupScreenshot();
  });
</script>

{#if card && !shouldAutoApprovePending}
  <div class="overlay">
    <div class="dialog">
      <div class="dialog-header">
        <h2>{isHistoryCard ? 'Edit Mined Note' : 'New Card Detected'}</h2>
        <span class="note-id">Note #{card.event.note_id}</span>
      </div>

      <!-- History match picker (only in history mode) -->
      {#if $activeHistoryItemId}
        {#if fetchingMatches}
          <div class="match-status">Searching for subtitle match...</div>
        {:else if historyMatches === null}
          <!-- waiting -->
        {:else if historyMatches.length === 0}
          <div class="match-status no-match">No subtitle match found — set range manually below.</div>
        {:else if historyMatches.length === 1}
          <div class="match-status ok">Auto-matched to subtitle.</div>
        {:else}
          <div class="match-picker-section">
            <h3>Multiple matches — pick one</h3>
            <div class="match-list">
              {#each historyMatches as match}
                <button
                  class="match-option"
                  class:selected={selectedHistoryMatch?.line_index === match.line_index}
                  on:click={() => applyHistoryMatch(match)}
                >
                  <span class="match-time">{formatTime(match.start_ms)}</span>
                  <span class="match-text">{@html match.text.replace(/\n/g, '<br>')}</span>
                </button>
              {/each}
            </div>
          </div>
        {/if}
      {/if}

      <!-- Context lines with included-range highlighting -->
      <div class="context-section">
        <h3>Subtitle Context</h3>
        <div class="context-lines">
          {#each contextLines as line}
            <div class="context-line" class:included={line.isIncluded} class:anchor={line.isAnchor}>
              <span class="ctx-time">{formatTime(line.start_ms)}</span>
              <span class="ctx-text">{@html line.text.replace(/\n/g, '<br>')}</span>
            </div>
          {/each}
        </div>
        <div class="context-actions">
          <button class="small-btn" on:click={appendPrev} disabled={includedLineFirst == null || includedLineFirst <= 0} title="Include previous subtitle line">
            ← Prev line
          </button>
          <button class="small-btn" on:click={resetSelection} title="Reset to original matched line">
            Reset
          </button>
          <button class="small-btn" on:click={appendNext} disabled={includedLineLast == null || includedLineLast >= $subtitles.length - 1} title="Include next subtitle line">
            Next line →
          </button>
        </div>
      </div>

      <!-- Sentence editor -->
      <div class="field-section">
        <label>
          <span class="label-text">Sentence</span>
          <textarea bind:value={editedSentence} rows="2"></textarea>
        </label>
      </div>

      <!-- Range slider -->
      <div class="range-section">
        <div class="range-header">
          <h3>Audio / Video Range</h3>
          <span class="range-info">{formatTime(startMs)} → {formatTime(endMs)}  ({((endMs - startMs) / 1000).toFixed(1)}s)</span>
        </div>
        <div class="offset-row">
          <span class="offset-hint">Start −{$audioStartOffset}ms / End +{$audioEndOffset}ms</span>
        </div>

        <!-- Dual-thumb slider -->
        <div class="slider-container" bind:this={sliderTrack}
          role="group" aria-label="Audio range slider"
          on:pointermove={handleSliderPointerMove}
          on:pointerup={handleSliderPointerUp}
        >
          <!-- Subtitle line markers on the track -->
          {#each $subtitles.filter(l => l.start_ms >= sliderMin && l.end_ms <= sliderMax) as line}
            {@const isIncluded = includedLineFirst != null && line.index >= includedLineFirst && line.index <= includedLineLast}
            <div
              class="slider-sub-marker"
              style="left: {sliderPct(line.start_ms)}%; width: {Math.max(2, sliderPct(line.end_ms) - sliderPct(line.start_ms))}%; background: {subtitleMarkerColor(line.index, isIncluded)}; opacity: {isIncluded ? 1 : 0.55}"
            ></div>
          {/each}

          <!-- Selected range highlight -->
          <div class="slider-range" style="left: {sliderPct(startMs)}%; width: {Math.max(0, sliderPct(endMs) - sliderPct(startMs))}%"></div>

          <!-- Start thumb -->
          <div
            class="slider-thumb start"
            role="slider" aria-label="Range start" aria-valuenow={startMs} tabindex="0"
            style="left: {sliderPct(startMs)}%"
            on:pointerdown={(e) => handleSliderPointerDown(e, 'start')}
          ></div>

          <!-- End thumb -->
          <div
            class="slider-thumb end"
            role="slider" aria-label="Range end" aria-valuenow={endMs} tabindex="0"
            style="left: {sliderPct(endMs)}%"
            on:pointerdown={(e) => handleSliderPointerDown(e, 'end')}
          ></div>

          <!-- Time labels -->
          <span class="slider-label left">{formatTime(sliderMin)}</span>
          <span class="slider-label right">{formatTime(sliderMax)}</span>
        </div>
      </div>

      <!-- Audio preview -->
      <div class="preview-section">
        <div class="preview-row">
          <button class="small-btn" on:click={handlePlayAudio} disabled={audioLoading}>
            {audioLoading ? 'Loading...' : '▶ Play Audio'}
          </button>
          <button class="small-btn" on:click={handlePreviewScreenshot} disabled={screenshotLoading}>
            {screenshotLoading ? '...' : '🖼 Preview Screenshot'}
          </button>
        </div>

        {#if screenshotUrl}
          <div class="screenshot-wrap">
            <img src={screenshotUrl} alt="Preview" class="screenshot-img" />
          </div>
        {/if}
      </div>

      <!-- Options -->
      <div class="options-section">
        <label class="checkbox-label">
          <input type="checkbox" bind:checked={generateAvif} />
          Generate animated screenshot (AVIF)
        </label>
      </div>

      <!-- Fields preview -->
      <div class="fields-section">
        <h3>Card Fields</h3>
        <div class="field-list">
          {#each Object.entries(card.event.fields) as [name, field]}
            <div class="field-item">
              <span class="field-name">{name}</span>
              <span class="field-value">{field.value ? field.value.substring(0, 80) : '(empty)'}{field.value?.length > 80 ? '...' : ''}</span>
            </div>
          {/each}
        </div>
      </div>

      <!-- Actions -->
      <div class="actions">
        <button on:click={handleSkip} disabled={submitting}>
          {card.source === 'pending' ? 'Skip' : 'Close'}
        </button>
        <button class="primary" on:click={handleConfirm} disabled={submitting}>
          {submitting ? 'Saving...' : (isHistoryCard ? 'Save Changes' : 'Confirm & Enrich')}
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  .overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.7);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
    padding: 1rem;
  }

  .dialog {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: 12px;
    width: 100%;
    max-width: 700px;
    max-height: 90vh;
    overflow-y: auto;
    padding: 1.5rem;
    display: flex;
    flex-direction: column;
    gap: 0.8rem;
  }

  .dialog-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }

  .dialog-header h2 {
    font-size: 1.2rem;
    color: var(--accent);
  }

  .note-id {
    font-size: 0.8rem;
    color: var(--text-dim);
  }

  h3 {
    font-size: 0.9rem;
    color: var(--text-secondary);
    margin-bottom: 0.3rem;
  }

  /* ── Context lines ── */

  .context-lines {
    background: var(--bg-primary);
    border-radius: 8px;
    padding: 0.5rem;
    max-height: 180px;
    overflow-y: auto;
  }

  .context-line {
    display: flex;
    gap: 0.5rem;
    padding: 0.25rem 0.5rem;
    border-radius: 4px;
    font-size: 0.9rem;
    border-left: 3px solid transparent;
  }

  .context-line.included {
    background: rgba(233, 69, 96, 0.08);
    border-left-color: var(--accent-dim);
  }

  .context-line.anchor {
    background: rgba(233, 69, 96, 0.15);
    border-left-color: var(--accent);
  }

  .ctx-time {
    flex-shrink: 0;
    font-size: 0.75rem;
    color: var(--text-dim);
    min-width: 3rem;
    padding-top: 0.1rem;
  }

  .ctx-text {
    word-break: break-word;
  }

  .context-actions {
    display: flex;
    justify-content: center;
    gap: 0.5rem;
    margin-top: 0.4rem;
  }

  .small-btn {
    font-size: 0.8rem;
    padding: 0.3rem 0.7rem;
  }

  .small-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  /* ── Sentence ── */

  .field-section label {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
  }

  .label-text {
    font-size: 0.85rem;
    color: var(--text-secondary);
  }

  textarea {
    resize: vertical;
    font-size: 1rem;
    line-height: 1.5;
  }

  /* ── Offsets ── */

  .offset-row {
    margin-bottom: 0.2rem;
  }

  .offset-hint {
    font-size: 0.75rem;
    color: var(--text-dim);
  }

  /* ── Range slider ── */

  .range-section {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
  }

  .range-header {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
  }

  .range-info {
    font-size: 0.8rem;
    color: var(--text-dim);
    font-variant-numeric: tabular-nums;
  }

  .slider-container {
    position: relative;
    height: 36px;
    background: var(--bg-primary);
    border-radius: 6px;
    margin: 0.2rem 0;
    cursor: pointer;
    touch-action: none;
    user-select: none;
  }

  .slider-sub-marker {
    position: absolute;
    top: 10px;
    height: 16px;
    border-radius: 3px;
    pointer-events: none;
  }

  .slider-range {
    position: absolute;
    top: 0;
    bottom: 0;
    background: rgba(233, 69, 96, 0.15);
    border-radius: 6px;
    pointer-events: none;
  }

  .slider-thumb {
    position: absolute;
    top: 2px;
    width: 3px;
    height: 32px;
    background: var(--accent);
    border-radius: 2px;
    transform: translateX(-50%);
    cursor: ew-resize;
    z-index: 2;
    box-shadow: 0 0 0 2px rgba(233, 69, 96, 0.35);
  }

  .slider-thumb:hover, .slider-thumb:active {
    width: 4px;
    box-shadow: 0 0 0 3px rgba(233, 69, 96, 0.55);
  }

  .slider-label {
    position: absolute;
    bottom: -14px;
    font-size: 0.65rem;
    color: var(--text-dim);
    font-variant-numeric: tabular-nums;
  }

  .slider-label.left { left: 0; }
  .slider-label.right { right: 0; }

  /* ── Preview section ── */

  .preview-section {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }

  .preview-row {
    display: flex;
    gap: 0.5rem;
  }

  .screenshot-wrap {
    display: flex;
    justify-content: center;
    background: var(--bg-primary);
    border-radius: 6px;
    padding: 4px;
  }

  .screenshot-img {
    max-width: 100%;
    max-height: 200px;
    border-radius: 4px;
    object-fit: contain;
  }

  /* ── Options ── */

  .options-section {
    padding: 0.3rem 0;
  }

  .checkbox-label {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-size: 0.9rem;
    cursor: pointer;
  }

  /* ── Fields ── */

  .fields-section {
    max-height: 120px;
    overflow-y: auto;
  }

  .field-list {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
  }

  .field-item {
    display: flex;
    gap: 0.5rem;
    font-size: 0.8rem;
    padding: 0.2rem 0;
  }

  .field-name {
    flex-shrink: 0;
    color: var(--text-secondary);
    font-weight: 500;
    min-width: 100px;
  }

  .field-value {
    color: var(--text-dim);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* ── Actions ── */

  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.75rem;
    padding-top: 0.5rem;
    border-top: 1px solid var(--border);
  }

  /* ── Match picker (history mode) ── */

  .match-status {
    font-size: 0.85rem;
    padding: 0.5rem 0.75rem;
    border-radius: 6px;
    background: var(--bg-primary);
    color: var(--text-secondary);
  }

  .match-status.ok {
    color: #4caf50;
    background: rgba(76, 175, 80, 0.1);
  }

  .match-status.no-match {
    color: #ff9800;
    background: rgba(255, 152, 0, 0.1);
  }

  .match-picker-section {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }

  .match-list {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    max-height: 160px;
    overflow-y: auto;
  }

  .match-option {
    display: flex;
    gap: 0.75rem;
    align-items: baseline;
    text-align: left;
    padding: 0.4rem 0.75rem;
    border-radius: 6px;
    background: var(--bg-primary);
    border: 1px solid var(--border);
    cursor: pointer;
    transition: border-color 0.15s, background 0.15s;
    font-size: 0.9rem;
  }

  .match-option:hover {
    background: var(--bg-card);
    border-color: var(--accent);
  }

  .match-option.selected {
    background: var(--bg-card);
    border-color: var(--accent);
    border-left-width: 3px;
  }

  .match-time {
    flex-shrink: 0;
    font-size: 0.75rem;
    color: var(--text-dim);
    font-variant-numeric: tabular-nums;
    min-width: 3.5rem;
  }

  .match-text {
    word-break: break-word;
  }

  /* ── Mobile ── */
  @media (max-width: 768px) {
    .overlay {
      padding: 0;
      align-items: stretch;
    }

    .dialog {
      max-width: 100%;
      max-height: 100vh;
      border-radius: 0;
      padding: 1rem;
      gap: 0.6rem;
    }

    .dialog-header h2 {
      font-size: 1rem;
    }

    .context-lines {
      max-height: 132px;
    }

    .context-line {
      font-size: 0.98rem;
      padding: 0.45rem 0.4rem;
      line-height: 1.45;
    }

    .context-actions {
      gap: 0.3rem;
    }

    .context-actions .small-btn {
      min-height: 36px;
      padding: 0.4rem 0.6rem;
    }

    textarea {
      font-size: 1rem;
    }

    .slider-container {
      height: 44px;
      margin: 0.3rem 0;
    }

    .slider-thumb {
      width: 6px;
      height: 40px;
      top: 2px;
      box-shadow: 0 0 0 4px rgba(233, 69, 96, 0.3);
    }

    .slider-thumb:hover, .slider-thumb:active {
      width: 8px;
      box-shadow: 0 0 0 5px rgba(233, 69, 96, 0.5);
    }

    .slider-sub-marker {
      top: 12px;
      height: 20px;
    }

    .preview-row {
      flex-direction: column;
    }

    .preview-row .small-btn {
      min-height: 40px;
      font-size: 0.85rem;
    }

    .actions {
      flex-direction: column;
      gap: 0.5rem;
    }

    .actions button {
      width: 100%;
      min-height: 44px;
      font-size: 0.95rem;
    }

    .field-item {
      flex-direction: column;
      gap: 0.1rem;
    }

    .fields-section {
      max-height: 88px;
    }

    .field-name {
      min-width: unset;
    }

    .range-header {
      flex-direction: column;
      gap: 0.15rem;
    }

    .match-option {
      padding: 0.5rem 0.75rem;
      min-height: 44px;
    }
  }
</style>
