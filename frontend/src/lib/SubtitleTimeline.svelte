<script>
  import { onDestroy } from 'svelte';
  import { subtitles, subtitleCandidates, selectedSubtitleCandidateId, subtitleSelectionMode, activeHistoryItemId, activeLineIndex, positionMs, pauseOnHover, pauseOnSeek, isPlaying, sessionState, showToast, setOptimisticPosition, setOptimisticPlayState, applySubtitlePayload } from './stores.js';
  import { fireSeek, firePlayPause, selectSubtitleTrack } from './api.js';
  import { formatTime } from './utils.js';

  const newLineCharacter = '\n';

  let container;
  let autoScroll = true;
  let userScrolling = false;
  let scrollTimeout;

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
      el.scrollIntoView({ behavior: 'smooth', block: 'center' });
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
      showToast('error', error?.message || 'Could not switch subtitle track');
    }
  }

  function handleLineClick(line) {
    if (!remoteControlAvailable) {
      showToast('error', 'Playback controls are unavailable for this player');
      return;
    }

    setOptimisticPosition(line.start_ms);
    fireSeek(line.start_ms);

    if ($pauseOnSeek && $isPlaying) {
      setOptimisticPlayState(true);
      firePlayPause(true);
    }
  }

  function handleLineMouseEnter(index) {
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
        showToast('error', result.error || 'Resume failed');
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

<div class="timeline-container" bind:this={container} on:scroll={handleScroll}>
  {#if showSubtitleSelector || $subtitles.length > 0}
    <div class="controls">
      <div class="controls-left">
        <label class="auto-scroll-toggle">
          <input type="checkbox" bind:checked={autoScroll} />
          Auto-scroll
        </label>
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
      </div>
      <span class="line-count">{$subtitles.length} lines</span>
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
          tabindex="0"
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
    justify-content: space-between;
    align-items: center;
    gap: 0.75rem;
    position: sticky;
    top: 0;
    z-index: 5;
    padding: 0.55rem 0.75rem;
    border-bottom: 1px solid var(--border);
    margin-bottom: 0;
    background: var(--bg-secondary);
    box-shadow: 0 1px 0 rgba(0, 0, 0, 0.18);
  }

  .controls-left {
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
      flex-direction: column;
    }

    .controls-left {
      width: 100%;
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
