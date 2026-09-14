<script>
  import { onMount } from 'svelte';
  import { historyItems, minedHistoryItems, activeHistoryItemId, currentView, navigate, applySubtitlePayload, autoApprove } from './stores.js';
  import { getHistory, getMinedHistory, getReviewSessions, activateHistoryItem, getHistorySubtitles } from './api.js';
  import { plainText } from './review.js';
  import { formatTime } from './utils.js';

  let loading = false;
  let activeTab = 'sessions';
  let sessions = [], historyFilter = null, error = '';
  $: visibleSessions = sessions.filter(s => !historyFilter || s.history_id === historyFilter);

  onMount(loadHistory);

  async function loadHistory() {
    loading = true;
    try {
      const [items, mined, review] = await Promise.all([getHistory(), getMinedHistory(), getReviewSessions()]);
      if (!review.ok) throw new Error(review.error);
      sessions = review.sessions;
      error = '';
      historyItems.set(items);
      minedHistoryItems.set(mined);
    } catch (e) {
      error = e.message;
      console.error('Failed to load history:', e);
    } finally {
      loading = false;
    }
  }

  async function handleActivate(item) {
    try {
    const result = await activateHistoryItem(item.history_id);
    if (result.ok) {
      // The WS only pushes subtitles on now_playing item changes; after activating
      // a history item we must pull the subtitle lines ourselves and update the store.
      const subData = await getHistorySubtitles(item.history_id);
      applySubtitlePayload(subData);
      activeHistoryItemId.set(item.history_id);
      currentView.set('timeline');
    } else { error = result.error || 'Could not open subtitles'; }
    } catch(e) { error = e.message; }
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

  <div class="review-intro"><div><h3>Mine now. Review when you're ready.</h3><p>Open a session to review its cards, add subtitle context, and adjust audio.</p></div><label><input type="checkbox" bind:checked={$autoApprove} /> Automatically enhance new cards<small>While this browser stays open</small></label></div>
  {#if error}<p class="history-error" role="alert">{error}</p>{/if}

  <div class="history-tabs" role="tablist">
    <button class="tab" class:active={activeTab === 'sessions'} role="tab" aria-selected={activeTab === 'sessions'} on:click={() => { activeTab = 'sessions'; historyFilter = null; }}>Card sessions <span class="tab-count">{sessions.length}</span></button>
    <button
      class="tab"
      class:active={activeTab === 'mined'}
      role="tab"
      aria-selected={activeTab === 'mined'}
      on:click={() => (activeTab = 'mined')}
    >
      Mined Notes
      {#if $minedHistoryItems.length > 0}
        <span class="tab-count">{$minedHistoryItems.length}</span>
      {/if}
    </button>
    <button
      class="tab"
      class:active={activeTab === 'watch'}
      role="tab"
      aria-selected={activeTab === 'watch'}
      on:click={() => (activeTab = 'watch')}
    >
      Watch History
      {#if $historyItems.length > 0}
        <span class="tab-count">{$historyItems.length}</span>
      {/if}
    </button>
  </div>

  {#if activeTab === 'sessions'}
    {#if historyFilter}<button class="clear-filter" on:click={() => historyFilter = null}>← All sessions</button>{/if}
    <div class="history-list">
      {#each visibleSessions as session (session.id)}
        <button class="history-item review-session" on:click={() => navigate(`/review/${encodeURIComponent(session.id)}`)}>
          <div class="item-header"><div class="item-title">{session.title}</div><span class="review-arrow">→</span></div>
          <div class="item-meta"><span>{new Date(session.created_at).toLocaleString()}</span><span>{session.card_count} cards · {session.enhanced_count} enhanced</span></div>
          <div class="session-progress"><progress max={session.card_count || 1} value={session.reviewed_count}></progress><span>{session.reviewed_count} / {session.card_count} reviewed</span></div>
        </button>
      {:else}<div class="empty compact"><p>{loading ? 'Loading sessions…' : 'No card sessions yet'}</p><p class="hint">Cards detected from your subtitles will appear here, including cards you skip or enhance automatically.</p></div>{/each}
    </div>
  {:else if activeTab === 'mined'}
    <section class="history-section">
      <p class="hint">Tap a mined note to reopen the enhancement dialog.</p>
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
              <div class="item-preview">{plainText(item.sentence)}</div>
              <div class="item-meta">
                <span class="meta-time">{timeAgo(item.updated_at)}</span>
              </div>
            </button>
          {/each}
        </div>
      {/if}
    </section>
  {:else}
    <section class="history-section">
      <p class="hint">Load subtitles from something you watched recently.</p>
      {#if $historyItems.length === 0}
        <div class="empty compact">
          <p>No watch history yet</p>
          <p class="hint">Watch something with target language audio and it will appear here</p>
        </div>
      {:else}
        <div class="history-list">
          {#each $historyItems as item}
            <div class="history-item">
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
              <div class="watch-actions"><button on:click={() => handleActivate(item)}>Open subtitles</button><button on:click={() => { historyFilter = item.history_id; activeTab = 'sessions'; }}>Review cards ({sessions.filter(s => s.history_id === item.history_id).reduce((n,s) => n + s.card_count, 0)})</button></div>
            </div>
          {/each}
        </div>
      {/if}
    </section>
  {/if}
</div>

<style>
  .review-intro { border:1px solid var(--border); border-radius:10px; padding:20px; margin:16px 0 24px; background:var(--bg-secondary); display:grid; gap:16px; }
  .review-intro h3 { font-size:17px; margin-bottom:6px; } .review-intro p { font-size:13px; color:var(--text-secondary); line-height:1.7; }
  .review-intro label { font-size:13px; } .review-intro input { accent-color:#80d9c0; margin-right:6px; } .review-intro small { display:block; font-size:11px; color:var(--text-secondary); margin:5px 0 0 23px; }
  .review-session { padding:20px; gap:10px; } .review-arrow { font-size:20px; color:#80d9c0; } .session-progress { display:flex; align-items:center; gap:15px; margin-top:6px; } .session-progress progress { height:5px; flex:1; accent-color:#80d9c0; } .session-progress span { font-size:11px; color:var(--text-secondary); } .watch-actions { display:flex; gap:8px; margin-top:8px; } .watch-actions button { font-size:12px; } .clear-filter { margin-bottom:14px; } .history-error { color:#ffabab; margin:16px 0; }
  .history-page {
    padding: 1rem;
    max-width: 800px;
    margin: 0 auto;
    overflow-y: auto;
    height: 100%;
  }

  .history-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 1rem;
  }

  .history-tabs {
    display: flex;
    gap: 0.25rem;
    margin-bottom: 1rem;
    border-bottom: 1px solid var(--border);
  }

  .tab {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    padding: 0.5rem 0.9rem;
    background: transparent;
    border: none;
    border-radius: 0;
    border-bottom: 2px solid transparent;
    color: var(--text-secondary);
    font-size: 0.9rem;
    cursor: pointer;
  }

  .tab:hover {
    color: var(--text-primary);
  }

  .tab.active {
    color: var(--accent);
    border-bottom-color: var(--accent);
  }

  .tab-count {
    font-size: 0.7rem;
    font-weight: 600;
    padding: 0.05rem 0.4rem;
    border-radius: 10px;
    background: var(--bg-card);
    color: var(--text-dim);
  }

  .tab.active .tab-count {
    background: var(--accent);
    color: #fff;
  }

  .history-section {
    margin-bottom: 1.25rem;
  }

  .history-section .hint {
    margin: 0 0 0.75rem;
    font-size: 0.85rem;
    color: var(--text-dim);
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
