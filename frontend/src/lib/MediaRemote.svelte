<script>
  import { subtitles, activeLineIndex, positionMs, isPlaying, pauseOnHover, pauseOnSeek, yomitanPause, durationMs, sessionState, showToast, syncPositionFromSessionState, setOptimisticPosition, setOptimisticPlayState } from './stores.js';
  import { seekTo, playPause } from './api.js';

  $: remoteControlAvailable = $sessionState.now_playing?.supports_remote_control ?? false;

  function restorePlaybackState(snapshot) {
    sessionState.set(snapshot);
    syncPositionFromSessionState(snapshot);
  }

  async function runSeek(target) {
    if (!remoteControlAvailable) {
      showToast('error', 'Playback controls are unavailable for this player');
      return;
    }

    const previousState = structuredClone($sessionState);
    setOptimisticPosition(target);

    const result = await seekTo(target);
    if (result?.ok === false) {
      restorePlaybackState(previousState);
      showToast('error', result.error || 'Seek failed');
    }
  }

  async function prevSubtitle() {
    const idx = $activeLineIndex;
    const subs = $subtitles;
    if (idx == null || subs.length === 0) return;

    const current = subs[idx];
    if (!current) return;

    // If more than 20% through current line, go to its start; else go to previous
    const duration = current.end_ms - current.start_ms;
    const elapsed = $positionMs - current.start_ms;
    let target;
    if (duration > 0 && elapsed / duration > 0.2) {
      target = current.start_ms;
    } else if (idx > 0) {
      target = subs[idx - 1].start_ms;
    } else {
      target = current.start_ms;
    }
    await runSeek(target);
  }

  async function nextSubtitle() {
    const idx = $activeLineIndex;
    const subs = $subtitles;
    if (idx == null || subs.length === 0) return;
    if (idx < subs.length - 1) {
      const target = subs[idx + 1].start_ms;
      await runSeek(target);
    }
  }

  async function back5() {
    const target = Math.max(0, $positionMs - 5000);
    await runSeek(target);
  }

  async function forward10() {
    const target = Math.min($durationMs || Infinity, $positionMs + 10000);
    await runSeek(target);
  }

  async function togglePlayPause() {
    if (!remoteControlAvailable) {
      showToast('error', 'Playback controls are unavailable for this player');
      return;
    }

    const shouldPause = $isPlaying;
    const previousState = structuredClone($sessionState);
    setOptimisticPlayState(shouldPause);

    const result = await playPause(shouldPause);
    if (result?.ok === false) {
      restorePlaybackState(previousState);
      showToast('error', result.error || 'Play/pause failed');
    }
  }
</script>

<div class="remote">
  <button class="remote-btn" on:click={prevSubtitle} title="Previous subtitle" disabled={!remoteControlAvailable}>⏮</button>
  <button class="remote-btn" on:click={back5} title="Back 5s" disabled={!remoteControlAvailable}>−5</button>
  <button class="remote-btn play" on:click={togglePlayPause} title={$isPlaying ? 'Pause' : 'Play'} disabled={!remoteControlAvailable}>
    {$isPlaying ? '⏸' : '▶'}
  </button>
  <button class="remote-btn" on:click={forward10} title="Forward 10s" disabled={!remoteControlAvailable}>+10</button>
  <button class="remote-btn" on:click={nextSubtitle} title="Next subtitle" disabled={!remoteControlAvailable}>⏭</button>

  <label class="hover-pause" title="Pause video while hovering the active subtitle line">
    <input type="checkbox" bind:checked={$pauseOnHover} />
    <span>Hover-pause</span>
  </label>
  <label class="hover-pause" title="Pause video when clicking a subtitle line">
    <input type="checkbox" bind:checked={$pauseOnSeek} />
    <span>Click-pause</span>
  </label>
  <label class="hover-pause" title="Pause when Yomitan popup appears, resume when it closes">
    <input type="checkbox" bind:checked={$yomitanPause} />
    <span>Yomitan-pause</span>
  </label>

  {#if $sessionState.now_playing && !remoteControlAvailable}
    <span class="warning">This player is not exposing server-side remote control.</span>
  {/if}
</div>

<style>
  .remote {
    display: flex;
    align-items: center;
    gap: 0.25rem;
    padding: 0 0.5rem;
  }

  .remote-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 2rem;
    height: 2rem;
    border-radius: 6px;
    font-size: 0.9rem;
    font-weight: 600;
    background: var(--bg-primary);
    border: 1px solid var(--border);
    color: var(--text-primary);
    cursor: pointer;
    transition: background 0.12s, border-color 0.12s;
    padding: 0;
    line-height: 1;
  }

  .remote-btn:hover {
    background: var(--bg-hover);
    border-color: var(--accent);
  }

  .remote-btn:active {
    transform: scale(0.88);
    background: var(--accent);
    color: var(--bg-primary);
  }

  .remote-btn:disabled {
    cursor: not-allowed;
    opacity: 0.45;
    transform: none;
    border-color: var(--border);
    background: var(--bg-primary);
    color: var(--text-dim);
  }

  .remote-btn.play {
    width: 2.4rem;
    font-size: 1rem;
    color: var(--accent);
  }

  .hover-pause {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    font-size: 0.75rem;
    color: var(--text-dim);
    cursor: pointer;
    margin-left: 0.5rem;
    white-space: nowrap;
    user-select: none;
  }

  .warning {
    font-size: 0.75rem;
    color: #d86c4a;
    margin-left: 0.5rem;
    white-space: nowrap;
  }

  .hover-pause input {
    margin: 0;
  }

  /* ── Mobile ── */
  @media (max-width: 768px) {
    .remote {
      flex-wrap: wrap;
      gap: 0.3rem;
      padding: 0.3rem 0.5rem;
      justify-content: center;
    }

    .remote-btn {
      width: 2.5rem;
      height: 2.5rem;
      font-size: 1rem;
    }

    .remote-btn.play {
      width: 3rem;
      height: 2.5rem;
      font-size: 1.1rem;
    }

    .hover-pause {
      margin-left: 0;
      font-size: 0.8rem;
      min-height: 32px;
      padding: 0.15rem 0.3rem;
    }

    .warning {
      width: 100%;
      margin-left: 0;
      text-align: center;
      white-space: normal;
    }
  }
</style>
