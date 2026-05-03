<script>
  import { onDestroy } from 'svelte';
  import { subtitles, subtitleCandidates, selectedSubtitleCandidateId, subtitleSelectionMode, subtitleOffsetMs, activeHistoryItemId, activeLineIndex, positionMs, pauseOnHover, pauseOnSeek, disableSubtitleSeeking, isPlaying, sessionState, showErrorToast, setOptimisticPosition, setOptimisticPlayState, applySubtitlePayload, yomitanPopupVisible } from './stores.js';
  import { fireSeek, firePlayPause, playPause, selectSubtitleTrack, setSubtitleOffset } from './api.js';
  import { formatTime } from './utils.js';
  import AudioTrackSelector from './AudioTrackSelector.svelte';

  const newLineCharacter = '\n';

  let container;
  let autoScroll = true;
  let userScrolling = false;
  let scrollTimeout;
  let controlsExpanded = false;
  let dismissingPopupPointerId = null;
  let consumeNextLineClick = false;

  // Track whether we paused due to hover so we can resume on leave
  let pausedByHover = false;

  $: remoteControlAvailable = $sessionState.now_playing?.supports_remote_control ?? false;
  $: selectedSubtitleCandidate = $subtitleCandidates.find(track => track.id === $selectedSubtitleCandidateId) || null;
  $: subtitlePickerValue = $subtitleSelectionMode === 'manual' && $selectedSubtitleCandidateId != null
    ? $selectedSubtitleCandidateId
    : 'auto';
  $: autoOptionLabel = $subtitleSelectionMode === 'auto' && selectedSubtitleCandidate
    ? `Auto-select (${selectedSubtitleCandidate.label})`
    : 'Auto-select';
  $: showSubtitleSelector = !$activeHistoryItemId && $subtitleCandidates.length > 0;

  $: if ($activeLineIndex != null && autoScroll && container) {
    scrollToLine($activeLineIndex);
  }

  function scrollToLine(index) {
    if (!container || userScrolling) return;
    const el = container.querySelector(`[data-index="${index}"]`);
    if (el) {
      const containerRect = container.getBoundingClientRect();
      const elRect = el.getBoundingClientRect();
      const offset = elRect.top - containerRect.top - (containerRect.height / 2) + (elRect.height / 2);
      container.scrollTo({ top: container.scrollTop + offset, behavior: 'smooth' });
    }
  }

  function handleScroll() {
    userScrolling = true;
    clearTimeout(scrollTimeout);
    scrollTimeout = setTimeout(() => {
      userScrolling = false;
    }, 3000);
  }

  async function handleSubtitleTrackChange(event) {
    const select = event.currentTarget;
    const previousValue = subtitlePickerValue;
    const rawValue = select.value;
    const candidateId = rawValue === 'auto' ? null : rawValue;

    try {
      const result = await selectSubtitleTrack(candidateId);
      if (result?.ok === false) {
        throw new Error(result.error || 'Could not switch subtitle track');
      }
      applySubtitlePayload(result?.subtitles);
    } catch (error) {
      select.value = previousValue;
      showErrorToast(error?.message || 'Could not switch subtitle track');
    }
  }

  let offsetBusy = false;

  $: offsetSupported = !$activeHistoryItemId && $subtitles.length > 0;
  $: offsetLabel = formatOffset($subtitleOffsetMs);

  function formatOffset(ms) {
    if (!ms) return '0 ms';
    const sign = ms > 0 ? '+' : '−';
    const absMs = Math.abs(ms);
    if (absMs >= 1000) {
      const secs = absMs / 1000;
      const text = secs >= 10 ? secs.toFixed(1) : secs.toFixed(2);
      return `${sign}${text} s`;
    }
    return `${sign}${absMs} ms`;
  }

  function handleNudgeClick(event) {
    const base = 100;
    const amount = event.shiftKey ? base * 5 : base;
    const direction = event.currentTarget.dataset.dir === 'back' ? -1 : 1;
    nudgeOffset(direction * amount);
  }

  async function nudgeOffset(deltaMs) {
    if (offsetBusy || !offsetSupported) return;
    offsetBusy = true;
    try {
      const result = await setSubtitleOffset({ deltaMs });
      if (result?.ok === false) {
        throw new Error(result.error || 'Failed to update offset');
      }
      applySubtitlePayload(result?.subtitles);
    } catch (error) {
      showErrorToast(error?.message || 'Failed to update subtitle offset');
    } finally {
      offsetBusy = false;
    }
  }

  async function resetOffset() {
    if (offsetBusy || !offsetSupported || !$subtitleOffsetMs) return;
    offsetBusy = true;
    try {
      const result = await setSubtitleOffset({ offsetMs: 0 });
      if (result?.ok === false) {
        throw new Error(result.error || 'Failed to reset offset');
      }
      applySubtitlePayload(result?.subtitles);
    } catch (error) {
      showErrorToast(error?.message || 'Failed to reset subtitle offset');
    } finally {
      offsetBusy = false;
    }
  }

  /**
   * Snap a subtitle line's start to the current playback position.
   * 'current' = the active line should start NOW (most common).
   * 'next'    = the next line should start NOW (when subs are one line behind).
   * @param {'current'|'next'} which
   */
  function snapLineToPosition(which) {
    const idx = $activeLineIndex;
    if (idx == null || !$subtitles.length) return;
    const targetIdx = which === 'current' ? idx : idx + 1;
    if (targetIdx < 0 || targetIdx >= $subtitles.length) return;
    const delta = $positionMs - $subtitles[targetIdx].start_ms;
    nudgeOffset(delta);
  }

  function handleLinePointerDown(event) {
    if ($yomitanPopupVisible) {
      dismissingPopupPointerId = event.pointerId;
    } else {
      dismissingPopupPointerId = null;
    }
  }

  function handleLinePointerUp(event) {
    if (dismissingPopupPointerId === event.pointerId) {
      consumeNextLineClick = true;
      dismissingPopupPointerId = null;
    }
  }

  function clearLinePointer(event) {
    if (dismissingPopupPointerId === event.pointerId) {
      dismissingPopupPointerId = null;
    }
  }

  function shouldIgnoreLineInteraction() {
    if (consumeNextLineClick) {
      consumeNextLineClick = false;
      return true;
    }

    return $yomitanPopupVisible;
  }

  function handleLineClick(line) {
    if (shouldIgnoreLineInteraction()) return;

    const shouldSeek = !$disableSubtitleSeeking;
    const shouldPause = $pauseOnSeek && $isPlaying;

    if (!shouldSeek && !shouldPause) {
      return;
    }

    if (!remoteControlAvailable) {
      return;
    }

    if (shouldSeek) {
      setOptimisticPosition(line.start_ms);
      fireSeek(line.start_ms);
    }

    if (shouldPause) {
      setOptimisticPlayState(true);
      firePlayPause(true);
    }
  }

  function handleLineMouseEnter(index) {
    if ($yomitanPopupVisible || consumeNextLineClick) return;

    if ($pauseOnHover && remoteControlAvailable && index === $activeLineIndex && $isPlaying) {
      setOptimisticPlayState(true);
      firePlayPause(true);
      pausedByHover = true;
    }
  }

  function handleLineMouseLeave(index) {
    if (pausedByHover) {
      setOptimisticPlayState(false);
      firePlayPause(false);
      pausedByHover = false;
    }
  }

  // If the option is toggled off while we're holding a pause, release it
  $: if (!$pauseOnHover && pausedByHover && remoteControlAvailable) {
    setOptimisticPlayState(false);
    playPause(false).then(result => {
      if (result?.ok === false) {
        showErrorToast(result.error || 'Resume failed');
      }
    });
    pausedByHover = false;
  }

  onDestroy(() => {
    if (pausedByHover && remoteControlAvailable) {
      playPause(false);
      pausedByHover = false;
    }
  });

  function getLineClass(index, activeIndex, posMs) {
    if (index === activeIndex) return 'line active';
    const line = $subtitles[index];
    if (!line) return 'line';
    if (line.end_ms < posMs) return 'line past';
    return 'line future';
  }
</script>

<div class="timeline-container" class:navigation-disabled={$yomitanPopupVisible} bind:this={container} on:scroll={handleScroll}>
  {#if showSubtitleSelector || $subtitles.length > 0}
    <div class="controls" class:controls-collapsed={!controlsExpanded}>
      <div class="controls-summary">
        <button class="controls-toggle" on:click={() => controlsExpanded = !controlsExpanded} title={controlsExpanded ? 'Collapse track options' : 'Expand track options'}>
          {controlsExpanded ? '▾' : '▸'}
        </button>
        <label class="auto-scroll-toggle">
          <input type="checkbox" bind:checked={autoScroll} />
          Auto-scroll
        </label>
        <span class="line-count">{$subtitles.length} lines</span>
      </div>
      <div class="controls-detail">
        {#if showSubtitleSelector}
          <label class="track-picker">
            <span class="track-picker-label">Track</span>
            <select value={subtitlePickerValue} on:change={handleSubtitleTrackChange}>
              <option value="auto">{autoOptionLabel}</option>
              {#each $subtitleCandidates as track}
                <option value={track.id}>{track.label}</option>
              {/each}
            </select>
          </label>
        {/if}
        {#if offsetSupported}
          <div class="offset-adjuster" role="group" aria-label="Subtitle timing offset">
            <span class="offset-label">Offset</span>
            <div class="offset-buttons">
              <button type="button" class="offset-btn jump" on:click={() => snapLineToPosition('current')} disabled={offsetBusy || $activeLineIndex == null} title="Snap current subtitle to playback position">⏮</button>
              <button type="button" class="offset-btn" data-dir="back" on:click={handleNudgeClick} disabled={offsetBusy} title="−100 ms (Shift: −500 ms)">◂</button>
              <button type="button" class="offset-value" on:click={resetOffset} disabled={offsetBusy || !$subtitleOffsetMs} title={$subtitleOffsetMs ? 'Reset to 0' : 'No offset applied'}>
                {offsetLabel}
              </button>
              <button type="button" class="offset-btn" data-dir="fwd" on:click={handleNudgeClick} disabled={offsetBusy} title="+100 ms (Shift: +500 ms)">▸</button>
              <button type="button" class="offset-btn jump" on:click={() => snapLineToPosition('next')} disabled={offsetBusy || $activeLineIndex == null || $activeLineIndex >= $subtitles.length - 1} title="Snap next subtitle to playback position">⏭</button>
            </div>
          </div>
        {/if}
        {#if !$activeHistoryItemId}
          <AudioTrackSelector />
        {/if}
      </div>
    </div>
  {/if}

  {#if $subtitles.length === 0}
    <div class="empty">
      <p>No subtitles loaded</p>
      {#if showSubtitleSelector}
        <p class="hint">Try another subtitle track from the selector above.</p>
      {:else}
        <p class="hint">Start playing something and Nagare will try the target-language subtitle track first.</p>
      {/if}
    </div>
  {:else}
    <div class="lines">
      {#each $subtitles as line, i}
        <div
          class={getLineClass(i, $activeLineIndex, $positionMs)}
          data-index={i}
          data-ts={formatTime(line.start_ms)}
          role="button"
          aria-disabled={$yomitanPopupVisible}
          tabindex="0"
          on:pointerdown={handleLinePointerDown}
          on:pointerup={handleLinePointerUp}
          on:pointercancel={clearLinePointer}
          on:click={() => handleLineClick(line)}
          on:keyup={(e) => e.key === 'Enter' && handleLineClick(line)}
          on:mouseenter={() => handleLineMouseEnter(i)}
          on:mouseleave={() => handleLineMouseLeave(i)}
        >
          <p class="text">
            {@html line.text.replace(/\n/g, '<br>')}
          </p>
        </div>
        {@html newLineCharacter}
      {/each}
    </div>
  {/if}
</div>

<style>
  .timeline-container {
    flex: 1;
    overflow-y: auto;
    padding: 0;
  }

  .empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    height: 100%;
    color: var(--text-dim);
    gap: 0.5rem;
    padding: 1rem;
  }

  .empty .hint {
    font-size: 0.85rem;
  }

  .controls {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    position: sticky;
    top: 0;
    z-index: 5;
    padding: 0.55rem 0.75rem;
    border-bottom: 1px solid var(--border);
    margin-bottom: 0;
    background: var(--bg-secondary);
    box-shadow: 0 1px 0 rgba(0, 0, 0, 0.18);
  }

  .controls-summary {
    display: flex;
    align-items: center;
    gap: 0.75rem;
  }

  .controls-toggle {
    display: none;
  }

  .controls-detail {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    min-width: 0;
    flex-wrap: wrap;
  }

  .auto-scroll-toggle {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    font-size: 0.85rem;
    color: var(--text-secondary);
    cursor: pointer;
  }

  .track-picker {
    display: flex;
    align-items: center;
    gap: 0.45rem;
    min-width: 0;
  }

  .track-picker-label {
    font-size: 0.8rem;
    color: var(--text-dim);
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }

  .track-picker select {
    min-width: min(28rem, 58vw);
    max-width: 100%;
    padding: 0.35rem 0.5rem;
    border-radius: 6px;
    border: 1px solid var(--border);
    background: var(--bg-card);
    color: var(--text-primary);
  }

  .line-count {
    font-size: 0.8rem;
    color: var(--text-dim);
    white-space: nowrap;
  }

  .offset-adjuster {
    display: flex;
    align-items: center;
    gap: 0.45rem;
    min-width: 0;
  }

  .offset-label {
    font-size: 0.8rem;
    color: var(--text-dim);
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }

  .offset-buttons {
    display: inline-flex;
    align-items: stretch;
    border-radius: 6px;
    overflow: hidden;
    border: 1px solid var(--border);
    background: var(--bg-card);
  }

  .offset-buttons button {
    background: transparent;
    border: none;
    border-right: 1px solid var(--border);
    color: var(--text-secondary);
    font: inherit;
    font-size: 0.8rem;
    padding: 0.3rem 0.55rem;
    cursor: pointer;
    font-variant-numeric: tabular-nums;
    line-height: 1;
  }

  .offset-buttons button:last-child {
    border-right: none;
  }

  .offset-buttons button:hover:not(:disabled) {
    background: var(--bg-hover);
    color: var(--text-primary);
  }

  .offset-buttons button:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .offset-value {
    min-width: 4.2rem;
    text-align: center;
    color: var(--text-primary) !important;
    font-weight: 500;
  }

  .offset-btn.jump {
    font-size: 0.7rem;
    color: var(--text-dim);
  }

  .offset-btn.jump:hover:not(:disabled) {
    color: var(--accent);
  }
  .lines {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 0.5rem;
  }

  .line {
    display: flex;
    align-items: flex-start;
    gap: 0.75rem;
    padding: 0.6rem 0.75rem;
    border-radius: 6px;
    width: 100%;
    transition: all 0.15s;
    background: transparent;
    cursor: pointer;
  }

  .line::before {
    content: attr(data-ts);
    flex-shrink: 0;
    font-size: 0.75rem;
    font-variant-numeric: tabular-nums;
    color: var(--text-dim);
    min-width: 3.5rem;
    padding-top: 0.15rem;
    user-select: none;
    -webkit-user-select: none;
  }

  .line:hover {
    background: var(--bg-hover);
  }

  .timeline-container.navigation-disabled .line {
    cursor: default;
  }

  .line .text {
    font-size: 1.1rem;
    line-height: 1.5;
    word-break: break-word;
    margin: 0;
  }

  .line.active {
    background: var(--bg-card);
    border-left: 3px solid var(--accent);
  }

  .line.active .text {
    color: var(--text-primary);
    font-weight: 500;
  }

  .line.active::before {
    color: var(--accent);
  }

  @media (max-width: 768px) {
    .controls {
      align-items: flex-start;
      gap: 0;
    }

    .controls-toggle {
      display: flex;
      align-items: center;
      justify-content: center;
      width: 1.6rem;
      height: 1.6rem;
      flex-shrink: 0;
      border-radius: 4px;
      font-size: 0.8rem;
      background: transparent;
      border: 1px solid var(--border);
      color: var(--text-secondary);
      padding: 0;
      cursor: pointer;
    }

    .controls-collapsed .controls-detail {
      display: none;
    }

    .controls-detail {
      width: 100%;
      padding-top: 0.4rem;
      flex-direction: column;
      align-items: flex-start;
    }

    .track-picker {
      width: 100%;
      align-items: flex-start;
      flex-direction: column;
    }

    .track-picker select {
      min-width: 0;
      width: 100%;
    }

    .offset-adjuster {
      width: 100%;
      align-items: stretch;
      flex-direction: column;
      gap: 0.3rem;
    }

    .offset-buttons {
      width: 100%;
    }

    .offset-buttons button {
      flex: 1 1 0;
      padding: 0.55rem 0.4rem;
      font-size: 0.9rem;
      min-height: 40px;
    }

    .offset-value {
      flex: 1.4 1 0;
      min-width: 0;
    }
  }

  .line.past .text {
    color: var(--text-secondary);
  }

  .line.future .text {
    color: var(--text-primary);
  }

  /* ── Mobile ── */
  @media (max-width: 768px) {
    .timeline-container {
      padding: 0;
    }

    .controls {
      padding: 0.45rem 0.55rem;
      z-index: 5;
      background: var(--bg-secondary);
    }

    .lines {
      padding: 0.2rem;
    }

    .line {
      padding: 0.95rem 0.55rem;
      gap: 0.45rem;
      min-height: 56px;
    }

    .line::before {
      font-size: 0.65rem;
      min-width: 2.6rem;
    }

    .line .text {
      font-size: 1.18rem;
      line-height: 1.6;
    }

    .line.active .text {
      font-size: 1.28rem;
    }

    .line-count {
      display: none;
    }
  }
</style>
