<script>
  import { onMount, onDestroy } from 'svelte';
  import { subtitles, activeLineIndex, positionMs, pauseOnHover, pauseOnSeek, isPlaying, sessionState, showToast, syncPositionFromSessionState, setOptimisticPosition, setOptimisticPlayState } from './stores.js';
  import { seekTo, playPause } from './api.js';
  import { formatTime } from './utils.js';

  const newLineCharacter = '\n';

  let container;
  let autoScroll = true;
  let userScrolling = false;
  let scrollTimeout;

  // Track whether we paused due to hover so we can resume on leave
  let pausedByHover = false;

  $: remoteControlAvailable = $sessionState.now_playing?.supports_remote_control ?? false;

  $: if ($activeLineIndex != null && autoScroll && container) {
    scrollToLine($activeLineIndex);
  }

  function restorePlaybackState(snapshot) {
    sessionState.set(snapshot);
    syncPositionFromSessionState(snapshot);
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

  async function handleLineClick(line) {
    if (!remoteControlAvailable) {
      showToast('error', 'Playback controls are unavailable for this player');
      return;
    }

    const previousState = structuredClone($sessionState);
    setOptimisticPosition(line.start_ms);
    const seekResult = await seekTo(line.start_ms);
    if (seekResult?.ok === false) {
      restorePlaybackState(previousState);
      showToast('error', seekResult.error || 'Seek failed');
      return;
    }

    if ($pauseOnSeek && $isPlaying) {
      const pauseSnapshot = structuredClone($sessionState);
      setOptimisticPlayState(true);
      const pauseResult = await playPause(true);
      if (pauseResult?.ok === false) {
        restorePlaybackState(pauseSnapshot);
        showToast('error', pauseResult.error || 'Pause failed');
      }
    }
  }

  async function handleLineMouseEnter(index) {
    if ($pauseOnHover && remoteControlAvailable && index === $activeLineIndex && $isPlaying) {
      const previousState = structuredClone($sessionState);
      setOptimisticPlayState(true);
      const result = await playPause(true);
      if (result?.ok === false) {
        restorePlaybackState(previousState);
        showToast('error', result.error || 'Pause failed');
        return;
      }
      pausedByHover = true;
    }
  }

  async function handleLineMouseLeave(index) {
    if (pausedByHover) {
      const previousState = structuredClone($sessionState);
      setOptimisticPlayState(false);
      const result = await playPause(false);
      if (result?.ok === false) {
        restorePlaybackState(previousState);
        showToast('error', result.error || 'Resume failed');
        return;
      }
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
  {#if $subtitles.length === 0}
    <div class="empty">
      <p>No subtitles loaded</p>
      <p class="hint">Start playing content with target language audio</p>
    </div>
  {:else}
    <div class="controls">
      <label class="auto-scroll-toggle">
        <input type="checkbox" bind:checked={autoScroll} />
        Auto-scroll
      </label>
      <span class="line-count">{$subtitles.length} lines</span>
    </div>
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
    padding: 0.5rem;
  }

  .empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    height: 100%;
    color: var(--text-dim);
    gap: 0.5rem;
  }

  .empty .hint {
    font-size: 0.85rem;
  }

  .controls {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0.5rem;
    border-bottom: 1px solid var(--border);
    margin-bottom: 0.5rem;
  }

  .auto-scroll-toggle {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    font-size: 0.85rem;
    color: var(--text-secondary);
    cursor: pointer;
  }

  .line-count {
    font-size: 0.8rem;
    color: var(--text-dim);
  }

  .lines {
    display: flex;
    flex-direction: column;
    gap: 2px;
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

  .line.past .text {
    color: var(--text-secondary);
  }

  .line.future .text {
    color: var(--text-primary);
  }

  /* ── Mobile ── */
  @media (max-width: 768px) {
    .timeline-container {
      padding: 0.2rem;
    }

    .controls {
      padding: 0.35rem 0.45rem;
      position: sticky;
      top: 0;
      z-index: 2;
      background: color-mix(in srgb, var(--bg-secondary) 88%, transparent);
      backdrop-filter: blur(6px);
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
