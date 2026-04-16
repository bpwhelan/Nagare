<script>
  import { sessionState } from './stores.js';
  import { selectSession } from './api.js';

  let showDropdown = false;

  $: sessions = $sessionState.sessions || [];
  $: activeId = $sessionState.active_session_id;

  async function handleSelect(sessionId) {
    await selectSession(sessionId);
    showDropdown = false;
  }

  async function handleAutoSelect() {
    await selectSession(null);
    showDropdown = false;
  }
</script>

<div class="session-selector">
  {#if sessions.length === 0}
    <span class="no-sessions">No active sessions</span>
  {:else}
    <button class="selector-btn" on:click={() => showDropdown = !showDropdown}>
      {#if activeId}
        {@const active = sessions.find(s => s.id === activeId)}
        {#if active}
          <span class="device">{active.device_name}</span>
          <span class="server">{active.server_kind}</span>
          {#if active.user_name}
            <span class="user">({active.user_name})</span>
          {/if}
        {:else}
          <span class="device">Session disconnected</span>
        {/if}
      {:else}
        <span class="device">No session selected</span>
      {/if}
      <span class="arrow">{showDropdown ? '▲' : '▼'}</span>
    </button>

    {#if showDropdown}
      <div class="dropdown">
        <button class="dropdown-item auto" on:click={handleAutoSelect}>
          ⟳ Auto-select
        </button>
        {#each sessions as session}
          <button
            class="dropdown-item"
            class:active={session.id === activeId}
            class:target={session.is_target_language}
            on:click={() => handleSelect(session.id)}
          >
            <div class="item-main">
              <span class="item-device">{session.device_name}</span>
              <span class="item-server">{session.server_kind}</span>
              <span class="item-client">{session.client}</span>
            </div>
            <div class="item-detail">
              {#if session.title}
                <span class="item-title">{session.title}</span>
              {/if}
              {#if session.user_name}
                <span class="item-user">{session.user_name}</span>
              {/if}
            </div>
            {#if session.is_target_language}
              <span class="target-badge">🎯</span>
            {/if}
          </button>
        {/each}
      </div>
    {/if}
  {/if}
</div>

<style>
  .session-selector {
    position: relative;
  }

  .no-sessions {
    color: var(--text-dim);
    font-size: 0.85rem;
  }

  .selector-btn {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.4rem 0.75rem;
    font-size: 0.85rem;
  }

  .device {
    font-weight: 500;
  }

  .user {
    color: var(--text-secondary);
    font-size: 0.8rem;
  }

  .server,
  .item-server {
    text-transform: uppercase;
    color: var(--text-dim);
    font-size: 0.68rem;
    letter-spacing: 0.08em;
  }

  .arrow {
    font-size: 0.6rem;
    color: var(--text-dim);
  }

  .dropdown {
    position: absolute;
    top: 100%;
    left: 0;
    margin-top: 4px;
    background: var(--bg-card);
    border: 1px solid var(--border);
    border-radius: 8px;
    min-width: 280px;
    z-index: 100;
    overflow: hidden;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.4);
  }

  .dropdown-item {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    width: 100%;
    padding: 0.6rem 0.75rem;
    border: none;
    border-radius: 0;
    text-align: left;
    font-size: 0.85rem;
    position: relative;
  }

  .dropdown-item:hover {
    background: var(--bg-hover);
  }

  .dropdown-item.active {
    border-left: 3px solid var(--accent);
  }

  .dropdown-item.auto {
    border-bottom: 1px solid var(--border);
    color: var(--text-secondary);
  }

  .item-main {
    display: flex;
    gap: 0.5rem;
    align-items: center;
  }

  .item-device {
    font-weight: 500;
  }

  .item-client {
    color: var(--text-dim);
    font-size: 0.75rem;
  }

  .item-detail {
    display: flex;
    gap: 0.5rem;
    font-size: 0.8rem;
  }

  .item-title {
    color: var(--text-secondary);
  }

  .item-user {
    color: var(--text-dim);
  }

  .target-badge {
    position: absolute;
    right: 0.75rem;
    top: 50%;
    transform: translateY(-50%);
  }

  /* ── Mobile ── */
  @media (max-width: 768px) {
    .selector-btn {
      padding: 0.4rem 0.5rem;
      font-size: 0.8rem;
      max-width: 180px;
      overflow: hidden;
      text-overflow: ellipsis;
    }

    .device {
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }

    .dropdown {
      position: fixed;
      top: auto;
      bottom: 0;
      left: 0;
      right: 0;
      margin-top: 0;
      min-width: unset;
      border-radius: 12px 12px 0 0;
      max-height: 60vh;
      overflow-y: auto;
    }

    .dropdown-item {
      padding: 0.75rem;
      min-height: 44px;
    }

    .user {
      display: none;
    }
  }
</style>
