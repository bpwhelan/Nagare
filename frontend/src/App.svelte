<script>
  import { onMount, onDestroy } from 'svelte';
  import { connectWebSocket, disconnect } from './lib/websocket.js';
  import { getConfig, getDialogByCardId, getDialogByNoteId, getHistorySubtitles, getPendingEnrichments } from './lib/api.js';
  import {
    activeHistoryItemId,
    ankiStatus,
    applyMiningConfig,
    connected,
    currentView,
    dialogCard,
    durationMs,
    enhancementStatus,
    nowPlayingTitle,
    pendingCards,
    positionMs,
    route,
    showToast,
    subtitles,
    syncRouteFromLocation,
    yomitanPause,
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

  let lastRouteKey = null;
  let routeRequestId = 0;

  onMount(async () => {
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

  // Start/stop Yomitan observer reactively
  $: if ($yomitanPause) {
    startYomitanObserver();
  } else {
    stopYomitanObserver();
  }

  $: progressPct = $durationMs > 0 ? ($positionMs / $durationMs) * 100 : 0;
  $: showAnkiWarning = $ankiStatus.state === 'disconnected';
  $: showEnhancementBanner = Boolean($enhancementStatus);

  async function hydrateDialogRoute(routeState) {
    const requestId = ++routeRequestId;
    try {
      const result = routeState.name === 'mine_note'
        ? await getDialogByNoteId(routeState.noteId)
        : await getDialogByCardId(routeState.cardId);

      if (requestId !== routeRequestId) return;

      if (!result?.ok || !result.dialog) {
        dialogCard.set(null);
        showToast('error', result?.error || 'Could not load that note');
        history.replaceState({}, '', '/');
        syncRouteFromLocation();
        return;
      }

      const dialog = result.dialog;
      if (dialog.history_id) {
        const subData = await getHistorySubtitles(dialog.history_id);
        if (requestId !== routeRequestId) return;
        subtitles.set(subData.lines || []);
        activeHistoryItemId.set(dialog.history_id);
      } else {
        activeHistoryItemId.set(null);
      }

      dialogCard.set(dialog);
      currentView.set('timeline');
    } catch (e) {
      if (requestId !== routeRequestId) return;
      dialogCard.set(null);
      showToast('error', e.message || 'Could not load that note');
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

<div class="app">
  <!-- Top bar -->
  <header class="topbar">
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
      <nav>
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
      <strong>Enhancing card</strong>
      <span>{$enhancementStatus}</span>
    </div>
  {/if}

  <!-- Now playing bar -->
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

  <!-- Enrichment dialog (modal overlay) -->
  <EnrichDialog />

  <!-- Toast notifications -->
  <ToastContainer />
</div>

<style>
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

  nav {
    display: flex;
    gap: 0.25rem;
  }

  nav button {
    font-size: 0.85rem;
    padding: 0.3rem 0.75rem;
    background: transparent;
    border: none;
    color: var(--text-secondary);
  }

  nav button:hover {
    color: var(--text-primary);
    background: var(--bg-hover);
  }

  nav button.active {
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

  .processing-banner span:last-child {
    min-width: 0;
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
    to {
      transform: rotate(360deg);
    }
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
  }

  /* ── Mobile ── */
  @media (max-width: 768px) {
    .topbar {
      flex-wrap: wrap;
      padding: 0.4rem 0.5rem 0;
      gap: 0;
    }

    /* Row 1: logo + dot + session selector */
    .topbar-left {
      gap: 0.4rem;
      min-width: 0;
      flex: 1 1 100%;
      overflow: hidden;
      padding-bottom: 0.3rem;
    }

    .logo {
      font-size: 0.9rem;
      flex-shrink: 0;
    }

    /* Row 2: badge + nav tabs — full width, tabs fill evenly */
    .topbar-right {
      flex: 1 1 100%;
      gap: 0;
      border-top: 1px solid var(--border);
      position: relative;
    }

    .badge {
      position: absolute;
      top: 0.3rem;
      right: 0;
      z-index: 2;
      pointer-events: none;
    }

    nav {
      flex: 1;
      gap: 0;
      position: relative;
    }

    nav button {
      flex: 1;
      padding: 0.55rem 0.25rem;
      font-size: 0.85rem;
      min-height: 40px;
      border-radius: 0;
      text-align: center;
      border-bottom: 2px solid transparent;
    }

    nav button.active {
      border-bottom: 2px solid var(--accent);
    }

    .np-info {
      flex-wrap: wrap;
      padding: 0.4rem 0.5rem;
      gap: 0.3rem;
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

    .np-title {
      flex-basis: 100%;
      order: 2;
    }

    .np-time {
      font-size: 0.75rem;
      order: 3;
      margin-left: auto;
    }
  }
</style>
