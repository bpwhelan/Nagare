<script>
  import { onMount, onDestroy } from 'svelte';
  import { connectWebSocket, disconnect } from './lib/websocket.js';
  import { getConfig, getDialogByCardId, getDialogByNoteId, getHistorySubtitles, getPendingEnrichments } from './lib/api.js';
  import {
    activeHistoryItemId,
    ankiStatus,
    applySubtitlePayload,
    applyMiningConfig,
    connected,
    currentView,
    dialogCard,
    durationMs,
    enhancementQueue,
    isPlaying,
    nowPlayingTitle,
    pendingCards,
    positionMs,
    route,
    showErrorToast,
    syncRouteFromLocation,
  } from './lib/stores.js';
  import { formatTimeFull } from './lib/utils.js';
  import { startYomitanObserver, stopYomitanObserver } from './lib/yomitan.js';
  import SessionSelector from './lib/SessionSelector.svelte';
  import SubtitleTimeline from './lib/SubtitleTimeline.svelte';
  import EnrichDialog from './lib/EnrichDialog.svelte';
  import ConfigPage from './lib/ConfigPage.svelte';
  import HistoryPage from './lib/HistoryPage.svelte';
  import ToastContainer from './lib/ToastContainer.svelte';
  import MediaRemote from './lib/MediaRemote.svelte';
  import AudioTrackModal from './lib/AudioTrackModal.svelte';

  let lastRouteKey = null;
  let routeRequestId = 0;

  // Mobile: show full chrome when paused, auto-hide when playing resumes
  let mobileChrome = true;
  let mobileChromeTimer = null;

  $: {
    if ($isPlaying) {
      // When playback starts, hide chrome after a short delay
      clearTimeout(mobileChromeTimer);
      mobileChromeTimer = setTimeout(() => { mobileChrome = false; }, 1500);
    } else {
      // When paused, show chrome immediately
      clearTimeout(mobileChromeTimer);
      mobileChrome = true;
    }
  }

  onMount(async () => {
    startYomitanObserver();
    connectWebSocket();
    syncRouteFromLocation();

    try {
      const [config, pending] = await Promise.all([
        getConfig(),
        getPendingEnrichments(),
      ]);
      applyMiningConfig(config.mining || {});
      pendingCards.set(pending || []);
    } catch (e) {
      console.error('Failed to load initial app state:', e);
    }
  });

  onDestroy(() => {
    disconnect();
    stopYomitanObserver();
  });

  $: progressPct = $durationMs > 0 ? ($positionMs / $durationMs) * 100 : 0;
  $: showAnkiWarning = $ankiStatus.state === 'disconnected';
  $: showEnhancementBanner = $enhancementQueue.length > 0;
  $: enhancementBannerTitle = $enhancementQueue.length === 1 ? 'Enhancing card' : 'Enhancing cards';

  async function hydrateDialogRoute(routeState) {
    const requestId = ++routeRequestId;
    try {
      const result = routeState.name === 'mine_note'
        ? await getDialogByNoteId(routeState.noteId)
        : await getDialogByCardId(routeState.cardId);

      if (requestId !== routeRequestId) return;

      if (!result?.ok || !result.dialog) {
        dialogCard.set(null);
        showErrorToast(result?.error || 'Could not load that note');
        history.replaceState({}, '', '/');
        syncRouteFromLocation();
        return;
      }

      const dialog = result.dialog;
      if (dialog.history_id) {
        const subData = await getHistorySubtitles(dialog.history_id);
        if (requestId !== routeRequestId) return;
        applySubtitlePayload(subData);
        activeHistoryItemId.set(dialog.history_id);
      } else {
        activeHistoryItemId.set(null);
      }

      dialogCard.set(dialog);
      currentView.set('timeline');
    } catch (e) {
      if (requestId !== routeRequestId) return;
      dialogCard.set(null);
      showErrorToast(e.message || 'Could not load that note');
      history.replaceState({}, '', '/');
      syncRouteFromLocation();
    }
  }

  $: routeKey = $route.name === 'mine_note'
    ? `note:${$route.noteId}`
    : $route.name === 'mine_card'
      ? `card:${$route.cardId}`
      : 'home';

  $: if (routeKey !== lastRouteKey) {
    lastRouteKey = routeKey;
    if (routeKey === 'home') {
      dialogCard.set(null);
    } else {
      hydrateDialogRoute($route);
    }
  }
</script>

<svelte:window on:popstate={syncRouteFromLocation} />

<div class="app" class:mobile-playing={$isPlaying && !mobileChrome}>
  <!-- Top bar -->
  <header class="topbar">
    <!-- Desktop: full topbar always. Mobile-playing: compact. Mobile-paused: compact. -->
    <div class="topbar-left">
      <h1 class="logo">⛏ Nagare</h1>
      <div class="connection" class:online={$connected}>
        {$connected ? '●' : '○'}
      </div>
      <SessionSelector />
    </div>
    <div class="topbar-right">
      {#if $pendingCards.length > 0}
        <span class="badge">{$pendingCards.length}</span>
      {/if}
      <nav class="desktop-nav">
        <button
          class:active={$currentView === 'timeline'}
          on:click={() => currentView.set('timeline')}
        >
          Subtitles
        </button>
        <button
          class:active={$currentView === 'history'}
          on:click={() => currentView.set('history')}
        >
          History
        </button>
        <button
          class:active={$currentView === 'config'}
          on:click={() => currentView.set('config')}
        >
          ⚙
        </button>
      </nav>
    </div>
    <!-- Mobile compact: show title + time inline when playing -->
    <div class="topbar-np">
      <span class="topbar-np-title">{$nowPlayingTitle}</span>
      <span class="topbar-np-time">{formatTimeFull($positionMs)} / {formatTimeFull($durationMs)}</span>
    </div>
  </header>

  {#if showAnkiWarning}
    <div class="warning-banner">
      <strong>AnkiConnect unavailable.</strong>
      <span>{$ankiStatus.message || 'Open Anki to enable card enrichment.'}</span>
    </div>
  {/if}

  {#if showEnhancementBanner}
    <div class="status-banner processing-banner" role="status" aria-live="polite">
      <span class="spinner" aria-hidden="true"></span>
      <div class="processing-copy">
        <strong>{enhancementBannerTitle}</strong>
        <ul class="processing-list">
          {#each $enhancementQueue as item (item.note_id)}
            <li class:item-running={item.state === 'running'}>{item.message}</li>
          {/each}
        </ul>
      </div>
    </div>
  {/if}

  <!-- Now playing bar (desktop only) -->
  <div class="now-playing">
    <div class="np-info">
      <MediaRemote />
      <span class="np-title">{$nowPlayingTitle}</span>
      <span class="np-time">
        {formatTimeFull($positionMs)} / {formatTimeFull($durationMs)}
      </span>
    </div>
    <div class="np-progress">
      <div class="np-progress-fill" style="width: {progressPct}%"></div>
    </div>
  </div>

  <!-- Mobile paused overlay: nav + settings, overlays on top of content -->
  <div class="mobile-chrome-overlay" class:visible={mobileChrome}>
    <nav class="mobile-chrome-nav">
      <button
        class:active={$currentView === 'timeline'}
        on:click={() => currentView.set('timeline')}
      >
        Subtitles
        {#if $pendingCards.length > 0}
          <span class="badge">{$pendingCards.length}</span>
        {/if}
      </button>
      <button
        class:active={$currentView === 'history'}
        on:click={() => currentView.set('history')}
      >
        History
      </button>
      <button
        class:active={$currentView === 'config'}
        on:click={() => currentView.set('config')}
      >
        Settings
      </button>
    </nav>
    <div class="mobile-chrome-settings">
      <MediaRemote settingsOnly />
    </div>
  </div>

  <!-- Main content -->
  <main class="content">
    {#if $currentView === 'timeline'}
      <SubtitleTimeline />
    {:else if $currentView === 'history'}
      <HistoryPage />
    {:else if $currentView === 'config'}
      <ConfigPage />
    {/if}
  </main>

  <!-- Mobile bottom bar: media controls when playing, nav handles when paused (via overlay) -->
  <div class="mobile-bottom-bar">
    <div class="mobile-bottom-remote">
      <MediaRemote compact />
    </div>
    <div class="np-progress mobile-progress">
      <div class="np-progress-fill" style="width: {progressPct}%"></div>
    </div>
  </div>

  <!-- Enrichment dialog (modal overlay) -->
  <EnrichDialog />

  <!-- Audio track selection modal -->
  <AudioTrackModal />

  <!-- Toast notifications -->
  <ToastContainer />
</div>

<style>
  /* ══════════════════════════════════════
     Desktop layout (unchanged)
     ══════════════════════════════════════ */
  .app {
    display: flex;
    flex-direction: column;
    height: 100vh;
  }

  .topbar {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0.5rem 1rem;
    background: var(--bg-secondary);
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
  }

  .topbar-left {
    display: flex;
    align-items: center;
    gap: 0.75rem;
  }

  .topbar-right {
    display: flex;
    align-items: center;
    gap: 0.75rem;
  }

  /* Hidden on desktop — only shows on mobile during playback */
  .topbar-np {
    display: none;
  }

  .logo {
    font-size: 1.1rem;
    font-weight: 700;
    color: var(--accent);
    white-space: nowrap;
  }

  .connection {
    font-size: 0.7rem;
    color: var(--accent);
  }

  .connection.online {
    color: var(--success);
  }

  .desktop-nav {
    display: flex;
    gap: 0.25rem;
  }

  .desktop-nav button {
    font-size: 0.85rem;
    padding: 0.3rem 0.75rem;
    background: transparent;
    border: none;
    color: var(--text-secondary);
  }

  .desktop-nav button:hover {
    color: var(--text-primary);
    background: var(--bg-hover);
  }

  .desktop-nav button.active {
    color: var(--accent);
    border-bottom: 2px solid var(--accent);
  }

  .badge {
    background: var(--accent);
    color: white;
    font-size: 0.7rem;
    font-weight: 700;
    padding: 0.15rem 0.4rem;
    border-radius: 10px;
    min-width: 1.2rem;
    text-align: center;
  }

  .now-playing {
    background: var(--bg-card);
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
  }

  .warning-banner {
    display: flex;
    gap: 0.5rem;
    align-items: center;
    padding: 0.6rem 1rem;
    background: color-mix(in srgb, var(--warning) 16%, var(--bg-secondary));
    border-bottom: 1px solid color-mix(in srgb, var(--warning) 45%, var(--border));
    color: var(--text-primary);
    font-size: 0.85rem;
  }

  .warning-banner strong {
    color: var(--warning);
    white-space: nowrap;
  }

  .warning-banner span {
    min-width: 0;
  }

  .status-banner {
    display: flex;
    gap: 0.55rem;
    align-items: center;
    padding: 0.55rem 1rem;
    border-bottom: 1px solid var(--border);
    color: var(--text-primary);
    font-size: 0.84rem;
  }

  .processing-banner {
    background:
      linear-gradient(90deg, rgba(74, 158, 214, 0.12), rgba(74, 158, 214, 0.03)),
      var(--bg-secondary);
    border-bottom-color: color-mix(in srgb, #4a9ed6 45%, var(--border));
  }

  .processing-banner strong {
    color: #7dc7f3;
    white-space: nowrap;
  }

  .processing-copy {
    min-width: 0;
  }

  .processing-list {
    margin: 0.15rem 0 0;
    padding-left: 1rem;
  }

  .processing-list li {
    color: var(--text-secondary);
  }

  .processing-list li.item-running {
    color: var(--text-primary);
  }

  .spinner {
    width: 0.85rem;
    height: 0.85rem;
    border-radius: 999px;
    border: 2px solid rgba(125, 199, 243, 0.25);
    border-top-color: #7dc7f3;
    flex-shrink: 0;
    animation: spin 0.8s linear infinite;
  }

  @keyframes spin {
    to { transform: rotate(360deg); }
  }

  .np-info {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.4rem 1rem;
    font-size: 0.85rem;
  }

  .np-title {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 500;
  }

  .np-time {
    font-variant-numeric: tabular-nums;
    color: var(--text-secondary);
    font-size: 0.8rem;
    flex-shrink: 0;
  }

  .np-progress {
    height: 3px;
    background: var(--bg-primary);
  }

  .np-progress-fill {
    height: 100%;
    background: var(--accent);
    transition: width 0.5s linear;
  }

  .content {
    flex: 1;
    overflow: hidden;
    display: flex;
    flex-direction: column;
    min-height: 0;
  }

  /* Hidden on desktop */
  .mobile-chrome-overlay {
    display: none;
  }
  .mobile-bottom-bar {
    display: none;
  }

  /* ══════════════════════════════════════
     Mobile layout
     ══════════════════════════════════════ */
  @media (max-width: 768px) {
    .app {
      padding-bottom: 0;
    }

    /* ── Topbar: always visible, compact ── */
    .topbar {
      padding: 0.35rem 0.5rem;
      gap: 0.4rem;
      flex-wrap: wrap;
    }

    .topbar-left {
      gap: 0.4rem;
      min-width: 0;
      overflow: hidden;
      flex: 1;
    }

    .logo {
      font-size: 0.9rem;
      flex-shrink: 0;
    }

    /* Hide desktop nav + now-playing bar on mobile */
    .topbar-right {
      display: none;
    }
    .now-playing {
      display: none;
    }

    /* Compact now-playing info in topbar */
    .topbar-np {
      display: flex;
      align-items: center;
      gap: 0.4rem;
      flex-basis: 100%;
      min-width: 0;
      padding-top: 0.2rem;
      border-top: 1px solid var(--border);
    }

    .topbar-np-title {
      flex: 1;
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
      font-size: 0.78rem;
      font-weight: 500;
      color: var(--text-primary);
    }

    .topbar-np-time {
      font-variant-numeric: tabular-nums;
      font-size: 0.7rem;
      color: var(--text-secondary);
      flex-shrink: 0;
    }

    /* ── Mobile chrome overlay: slides down when paused ── */
    .mobile-chrome-overlay {
      display: block;
      position: absolute;
      top: 0;
      left: 0;
      right: 0;
      z-index: 50;
      background: var(--bg-secondary);
      border-bottom: 1px solid var(--border);
      transform: translateY(-100%);
      transition: transform 0.25s ease-out, opacity 0.25s ease-out;
      opacity: 0;
      pointer-events: none;
    }

    .mobile-chrome-overlay.visible {
      transform: translateY(0);
      opacity: 1;
      pointer-events: auto;
      position: relative;
    }

    .mobile-chrome-nav {
      display: flex;
      border-bottom: 1px solid var(--border);
    }

    .mobile-chrome-nav button {
      flex: 1;
      padding: 0.55rem 0.25rem;
      background: transparent;
      border: none;
      border-radius: 0;
      border-bottom: 2px solid transparent;
      color: var(--text-secondary);
      font-size: 0.82rem;
      min-height: 38px;
      text-align: center;
      -webkit-tap-highlight-color: transparent;
    }

    .mobile-chrome-nav button:hover {
      background: var(--bg-hover);
    }

    .mobile-chrome-nav button.active {
      color: var(--accent);
      border-bottom-color: var(--accent);
    }

    .mobile-chrome-settings {
      padding: 0.3rem 0.5rem;
      border-top: 1px solid var(--border);
    }

    /* ── Bottom bar: always fixed at bottom on mobile ── */
    .mobile-bottom-bar {
      display: flex;
      flex-direction: column;
      position: fixed;
      bottom: 0;
      left: 0;
      right: 0;
      z-index: 100;
      background: var(--bg-secondary);
      border-top: 1px solid var(--border);
      padding-bottom: env(safe-area-inset-bottom, 0);
    }

    .mobile-bottom-remote {
      padding: 0.25rem 0;
    }

    .mobile-progress {
      height: 3px;
    }

    /* Content needs bottom padding to not be hidden behind bottom bar */
    .content {
      padding-bottom: 56px;
    }

    /* Playing mode: hide session selector, hide topbar-np border separation */
    .mobile-playing .topbar-np {
      border-top: none;
      padding-top: 0;
    }

    .warning-banner {
      flex-direction: column;
      align-items: flex-start;
      padding: 0.55rem 0.5rem;
    }

    .status-banner {
      padding: 0.55rem 0.5rem;
      align-items: flex-start;
      flex-wrap: wrap;
    }

    .processing-banner strong {
      flex-basis: calc(100% - 1.4rem);
    }

    .processing-list {
      margin-top: 0.2rem;
    }
  }
</style>
