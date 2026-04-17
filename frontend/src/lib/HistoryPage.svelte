<script>
  import { onMount } from 'svelte';
  import { historyItems, minedHistoryItems, activeHistoryItemId, currentView, navigate, applySubtitlePayload } from './stores.js';
  import { getHistory, getMinedHistory, activateHistoryItem, getHistorySubtitles } from './api.js';
  import { formatTime } from './utils.js';

  let loading = false;

  onMount(loadHistory);

  async function loadHistory() {
    loading = true;
    try {
      const [items, mined] = await Promise.all([getHistory(), getMinedHistory()]);
      historyItems.set(items);
      minedHistoryItems.set(mined);
    } catch (e) {
      console.error('Failed to load history:', e);
    } finally {
      loading = false;
    }
  }

  async function handleActivate(item) {
    const result = await activateHistoryItem(item.history_id);
    if (result.ok) {
      // The WS only pushes subtitles on now_playing item changes; after activating
      // a history item we must pull the subtitle lines ourselves and update the store.
      const subData = await getHistorySubtitles(item.history_id);
      applySubtitlePayload(subData);
      activeHistoryItemId.set(item.history_id);
      currentView.set('timeline');
    }
  }

  function handleOpenMined(item) {
    navigate(`/mine/note/${item.note_id}`);
  }

  function timeAgo(dateStr) {
    const now = new Date();
    const then = new Date(dateStr);
    const diffMs = now - then;
    const mins = Math.floor(diffMs / 60000);
    if (mins < 1) return 'just now';
    if (mins < 60) return `${mins}m ago`;
    const hrs = Math.floor(mins / 60);
    if (hrs < 24) return `${hrs}h ago`;
    const days = Math.floor(hrs / 24);
    return `${days}d ago`;
  }
</script>

<div class="history-page">
  <div class="history-header">
    <h2>History</h2>
    <button class="refresh-btn" on:click={loadHistory} disabled={loading}>
      {loading ? '...' : '↻'} Refresh
    </button>
  </div>

  <section class="history-section">
    <div class="section-header">
      <h3>Mined Notes</h3>
      <p class="hint">Tap a mined note to reopen the enhancement dialog.</p>
    </div>
    {#if $minedHistoryItems.length === 0}
      <div class="empty compact">
        <p>No mined notes yet</p>
      </div>
    {:else}
      <div class="history-list">
        {#each $minedHistoryItems as item}
          <button class="history-item mined-item" on:click={() => handleOpenMined(item)}>
            <div class="item-header">
              <div class="item-title">{item.title}</div>
              <span class="item-server">note #{item.note_id}</span>
            </div>
            <div class="item-preview">{@html item.sentence.replace(/\n/g, '<br>')}</div>
            <div class="item-meta">
              <span class="meta-time">{timeAgo(item.updated_at)}</span>
            </div>
          </button>
        {/each}
      </div>
    {/if}
  </section>

  <section class="history-section">
    <div class="section-header">
      <h3>Watch History</h3>
      <p class="hint">Load subtitles from something you watched recently.</p>
    </div>
    {#if $historyItems.length === 0}
      <div class="empty compact">
        <p>No watch history yet</p>
        <p class="hint">Watch something with target language audio and it will appear here</p>
      </div>
    {:else}
      <div class="history-list">
        {#each $historyItems as item}
          <button class="history-item" on:click={() => handleActivate(item)}>
            <div class="item-header">
              <div class="item-title">{item.title}</div>
              <span class="item-server">{item.server_kind}</span>
            </div>
            <div class="item-meta">
              <span class="meta-subs">📝 {item.subtitle_count} lines</span>
              {#if item.duration_ms}
                <span class="meta-duration">⏱ {formatTime(item.duration_ms)}</span>
              {/if}
              <span class="meta-time">{timeAgo(item.last_seen)}</span>
            </div>
          </button>
        {/each}
      </div>
    {/if}
  </section>
</div>

<style>
  .history-page {
    padding: 1rem;
    max-width: 800px;
    margin: 0 auto;
  }

  .history-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 1rem;
  }

  .history-section {
    margin-bottom: 1.25rem;
  }

  .section-header {
    margin-bottom: 0.75rem;
  }

  .section-header h3 {
    margin: 0 0 0.2rem;
    font-size: 1rem;
  }

  .history-header h2 {
    margin: 0;
    font-size: 1.2rem;
  }

  .refresh-btn {
    font-size: 0.85rem;
    padding: 0.3rem 0.6rem;
  }

  .empty {
    text-align: center;
    color: var(--text-dim);
    padding: 3rem 1rem;
  }

  .empty.compact {
    padding: 1.25rem 1rem;
    background: var(--bg-card);
    border: 1px solid var(--border);
    border-radius: 8px;
  }

  .empty .hint {
    font-size: 0.85rem;
    margin-top: 0.5rem;
  }

  .history-list {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }

  .history-item {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    width: 100%;
    padding: 0.8rem 1rem;
    border-radius: 8px;
    background: var(--bg-card);
    border: 1px solid var(--border);
    text-align: left;
    cursor: pointer;
    transition: border-color 0.15s;
  }

  .history-item:hover {
    border-color: var(--accent);
  }

  .mined-item .item-preview {
    font-size: 0.9rem;
    color: var(--text-secondary);
    display: -webkit-box;
    -webkit-line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }

  .item-header {
    display: flex;
    align-items: baseline;
    gap: 0.6rem;
  }

  .item-title {
    font-size: 1rem;
    font-weight: 500;
  }

  .item-server {
    text-transform: uppercase;
    font-size: 0.68rem;
    color: var(--text-dim);
    letter-spacing: 0.08em;
  }

  .item-meta {
    display: flex;
    gap: 1rem;
    font-size: 0.8rem;
    color: var(--text-dim);
  }

  .meta-time {
    margin-left: auto;
  }

  /* ── Mobile ── */
  @media (max-width: 768px) {
    .history-page {
      padding: 0.75rem;
    }

    .history-item {
      padding: 0.75rem;
    }

    .item-meta {
      flex-wrap: wrap;
      gap: 0.5rem;
    }

    .item-title {
      font-size: 0.95rem;
      word-break: break-word;
    }
  }
</style>
