<script>
  import { onDestroy, onMount } from 'svelte';
  import {
    getConfig,
    updateConfig,
    getServerUsers,
    testTadokuConnection,
    refreshTadokuLogin,
    clearTadokuLogin,
    getTadokuCandidates,
    syncTadokuCandidates,
    declineTadokuCandidates,
  } from './api.js';
  import { applyMiningConfig, autoApprove, pauseOnEnhance, showNativeSubtitles, showDownloadButton, showErrorToast, showToast } from './stores.js';

  const AUTO_APPROVE_STORAGE_KEY = 'opt_autoApprove';
  const AUTO_SAVE_DELAY_MS = 700;

  let config = null;
  let loading = true;
  let saveError = '';
  let saveTimer = null;
  let saveLoopPromise = null;
  let queuedConfig = null;
  let lastSavedConfig = null;
  let testingTadoku = false;
  let refreshingTadoku = false;
  let clearingTadoku = false;
  let loadingTadokuCandidates = false;
  let syncingTadoku = false;
  let decliningTadoku = false;
  let tadokuCandidates = [];
  let selectedTadokuIds = [];
  let activeTab = 'server';

  const TABS = [
    { id: 'server', label: 'Server' },
    { id: 'anki', label: 'Anki & Media' },
    { id: 'tadoku', label: 'Tadoku' },
    { id: 'frontend', label: 'Frontend' },
  ];

  onMount(async () => {
    try {
      config = await getConfig();
      migrateAutoApprove(config?.mining?.auto_approve);
      normalizeConfig();
      lastSavedConfig = serializeConfig(config);
    } catch (e) {
      console.error('Failed to load config:', e);
    } finally {
      loading = false;
    }
  });

  onDestroy(() => {
    if (saveTimer) clearTimeout(saveTimer);
    saveTimer = null;
    if (queuedConfig) startSaveLoop();
  });

  $: if (!loading && config) {
    queueConfigSave(serializeConfig(config));
  }

  // Per-server fetched user lists: { emby: { loading, error, users: [] }, ... }
  let serverUsers = {};

  function createServerConfig(kind) {
    return kind === 'plex'
      ? { enabled: false, url: '', token: '', users: [] }
      : { enabled: false, url: '', api_key: '', users: [] };
  }

  function normalizeConfig() {
    if (!config) return;
    config.emby = config.emby || createServerConfig('emby');
    config.jellyfin = config.jellyfin || createServerConfig('jellyfin');
    config.plex = config.plex || createServerConfig('plex');
    for (const kind of ['emby', 'jellyfin', 'plex']) {
      if (!Array.isArray(config[kind].users)) config[kind].users = [];
    }
    if (!config.anki) config.anki = {};
    if (!config.anki.fields) config.anki.fields = {};
    if (!config.path_mappings) config.path_mappings = [];
    if (!config.anki.add_tags) config.anki.add_tags = [];
    if (config.anki.series_tag_enabled == null) config.anki.series_tag_enabled = false;
    if (config.anki.series_tag_parent == null) config.anki.series_tag_parent = '';
    if (!config.anki.ignore_tags) config.anki.ignore_tags = [];
    if (!config.anki.require_tags) config.anki.require_tags = [];
    if (!config.anki.note_types) config.anki.note_types = [];
    if (!config.mining) config.mining = {};
    if (!config.tadoku) config.tadoku = {};
    if (config.tadoku.enabled == null) config.tadoku.enabled = false;
    if (config.tadoku.username == null) config.tadoku.username = '';
    if (config.tadoku.password == null) config.tadoku.password = '';
    if (config.tadoku.password_configured == null) config.tadoku.password_configured = false;
    delete config.tadoku.session_cookie;
    if (config.tadoku.language_code == null) config.tadoku.language_code = config.target_language || 'jpn';
    if (config.tadoku.export_hour_eastern == null) config.tadoku.export_hour_eastern = 20;
    const obsoleteTadokuUrls = [
      '',
      'https://tadoku.app/api/immersion',
      'https://tadoku.app/api/immersion/',
      'https://tadoku.app/api/internal',
      'https://tadoku.app/api/internal/',
    ];
    if (obsoleteTadokuUrls.includes(config.tadoku.api_url || '')) {
      config.tadoku.api_url = 'https://tadoku.app/api/internal/immersion';
    }
    if (!config.tadoku.session_url) config.tadoku.session_url = 'https://account.tadoku.app/kratos/sessions/whoami';
    if (config.mining.audio_start_offset_ms == null) config.mining.audio_start_offset_ms = 100;
    if (config.mining.audio_end_offset_ms == null) config.mining.audio_end_offset_ms = 500;
    if (config.mining.audio_codec == null) config.mining.audio_codec = 'mp3';
    if (config.mining.generate_avif == null) config.mining.generate_avif = true;
    if (config.mining.animated_screenshot_encoder == null) config.mining.animated_screenshot_encoder = 'libsvtav1';
    if (config.mining.avif_max_width == null) config.mining.avif_max_width = 480;
    if (config.mining.avif_max_fps == null) config.mining.avif_max_fps = 10;
    if (config.mining.static_screenshot_format == null) config.mining.static_screenshot_format = 'webp';
    delete config.mining.auto_approve;
    applyMiningConfig(config.mining);
  }

  function migrateAutoApprove(serverValue) {
    if (serverValue == null) return;
    if (localStorage.getItem(AUTO_APPROVE_STORAGE_KEY) != null) return;
    autoApprove.set(Boolean(serverValue));
  }

  function updateServerField(kind, field, value) {
    config[kind] = config[kind] || createServerConfig(kind);
    config[kind][field] = value;
    config = { ...config };
  }

  function toggleServer(kind, enabled) {
    config[kind] = config[kind] || createServerConfig(kind);
    config[kind].enabled = enabled;
    config = { ...config };
  }

  async function loadServerUsers() {
    const kinds = ['emby', 'jellyfin', 'plex'].filter((k) => config[k]?.enabled);
    if (kinds.length === 0) {
      showErrorToast('Enable a server connection first');
      return;
    }
    await startSaveLoop();
    if (saveError) return;
    for (const k of kinds) serverUsers[k] = { loading: true, users: [], error: null };
    serverUsers = { ...serverUsers };

    try {
      const list = await getServerUsers();
      const next = {};
      for (const entry of list) {
        next[entry.server_kind] = {
          loading: false,
          users: entry.users || [],
          error: entry.error || null,
        };
      }
      serverUsers = next;
    } catch (e) {
      showErrorToast('Failed to load users: ' + e.message);
      for (const k of kinds) {
        serverUsers[k] = { loading: false, users: [], error: 'Request failed' };
      }
      serverUsers = { ...serverUsers };
    }
  }

  function toggleUser(kind, userId, checked) {
    config[kind] = config[kind] || createServerConfig(kind);
    const users = config[kind].users || [];
    if (checked) {
      if (!users.includes(userId)) config[kind].users = [...users, userId];
    } else {
      config[kind].users = users.filter((u) => u !== userId);
    }
    config = { ...config };
  }

  function addPathMapping() {
    config.path_mappings = [...config.path_mappings, { from: '', to: '' }];
  }

  function removePathMapping(index) {
    config.path_mappings = config.path_mappings.filter((_, i) => i !== index);
  }

  // Tag input helpers
  function addTag(field) {
    const input = document.getElementById(`tag-input-${field}`);
    const val = input?.value?.trim();
    if (val && !config.anki[field].includes(val)) {
      config.anki[field] = [...config.anki[field], val];
    }
    if (input) input.value = '';
  }

  function removeTag(field, index) {
    config.anki[field] = config.anki[field].filter((_, i) => i !== index);
  }

  function addNoteType() {
    const input = document.getElementById('note-type-input');
    const val = input?.value?.trim();
    if (val && !config.anki.note_types.includes(val)) {
      config.anki.note_types = [...config.anki.note_types, val];
    }
    if (input) input.value = '';
  }

  function removeNoteType(index) {
    config.anki.note_types = config.anki.note_types.filter((_, i) => i !== index);
  }

  function createConfigPayload(source) {
    const payload = {
      ...source,
      mining: { ...source.mining },
      tadoku: { ...source.tadoku },
    };
    delete payload.mining.auto_approve;
    delete payload.tadoku.password_configured;
    delete payload.tadoku.session_cookie;
    return payload;
  }

  function serializeConfig(source) {
    return JSON.stringify(createConfigPayload(source));
  }

  function queueConfigSave(serialized) {
    if (lastSavedConfig == null) {
      lastSavedConfig = serialized;
      return;
    }

    if (serialized === lastSavedConfig) {
      queuedConfig = null;
      if (saveTimer) clearTimeout(saveTimer);
      saveTimer = null;
      return;
    }

    queuedConfig = serialized;
    saveError = '';
    if (saveTimer) clearTimeout(saveTimer);
    saveTimer = setTimeout(() => {
      saveTimer = null;
      startSaveLoop();
    }, AUTO_SAVE_DELAY_MS);
  }

  async function persistConfig(serialized) {
    const payload = JSON.parse(serialized);
    try {
      const result = await updateConfig(payload);
      if (!result.ok) throw new Error(result.error || 'Failed to save');
      applyMiningConfig(payload.mining);
      if (payload.tadoku?.username && payload.tadoku?.password) {
        config.tadoku.password_configured = true;
      }
      return true;
    } catch (e) {
      saveError = e.message || 'Failed to save';
      showErrorToast('Failed to save settings: ' + saveError);
      return false;
    }
  }

  async function startSaveLoop() {
    if (saveTimer) clearTimeout(saveTimer);
    saveTimer = null;
    if (saveLoopPromise) return saveLoopPromise;

    saveLoopPromise = (async () => {
      while (queuedConfig && queuedConfig !== lastSavedConfig) {
        const serialized = queuedConfig;
        queuedConfig = null;

        const saved = await persistConfig(serialized);
        if (!saved) {
          queuedConfig = null;
          break;
        }
        lastSavedConfig = serialized;

        const currentConfig = serializeConfig(config);
        if (currentConfig !== lastSavedConfig) {
          queuedConfig = currentConfig;
        }
      }
    })();

    try {
      await saveLoopPromise;
    } finally {
      saveLoopPromise = null;
    }
  }

  async function testTadoku() {
    testingTadoku = true;
    try {
      await startSaveLoop();
      if (saveError) return;
      const result = await testTadokuConnection();
      if (result.ok) {
        const name = result.connection?.display_name || result.connection?.user_id || 'your account';
        showToast('success', `Connected to Tadoku as ${name}`);
      } else {
        showErrorToast(result.error || 'Tadoku connection failed');
      }
    } catch (e) {
      showErrorToast('Tadoku connection failed: ' + e.message);
    } finally {
      testingTadoku = false;
    }
  }

  async function refreshTadoku() {
    refreshingTadoku = true;
    try {
      await startSaveLoop();
      if (saveError) return;
      const result = await refreshTadokuLogin();
      if (!result.authenticated) throw new Error(result.error || 'Could not refresh Tadoku login');
      config.tadoku.password_configured = true;
      showToast('success', 'Tadoku login refreshed');
    } catch (e) {
      showErrorToast(e.message || 'Could not refresh Tadoku login');
    } finally {
      refreshingTadoku = false;
    }
  }

  async function clearTadoku() {
    clearingTadoku = true;
    try {
      if (saveTimer) clearTimeout(saveTimer);
      saveTimer = null;
      queuedConfig = null;
      if (saveLoopPromise) await saveLoopPromise;
      const result = await clearTadokuLogin();
      if (!result.ok) throw new Error(result.error || 'Could not clear Tadoku login');
      config.tadoku.username = '';
      config.tadoku.password = '';
      config.tadoku.password_configured = false;
      showToast('success', 'Saved Tadoku login cleared');
    } catch (e) {
      showErrorToast(e.message || 'Could not clear Tadoku login');
    } finally {
      clearingTadoku = false;
    }
  }

  async function openTab(tab) {
    activeTab = tab;
    if (tab === 'tadoku') await loadTadokuCandidates();
  }

  async function loadTadokuCandidates() {
    loadingTadokuCandidates = true;
    try {
      const result = await getTadokuCandidates();
      if (!result.ok) throw new Error(result.error || 'Could not load episodes');
      tadokuCandidates = result.candidates || [];
      const available = new Set(tadokuCandidates.map((candidate) => candidate.history_id));
      selectedTadokuIds = selectedTadokuIds.filter((id) => available.has(id));
    } catch (e) {
      tadokuCandidates = [];
      selectedTadokuIds = [];
      showErrorToast('Failed to load Tadoku episodes: ' + e.message);
    } finally {
      loadingTadokuCandidates = false;
    }
  }

  function toggleTadokuCandidate(historyId, checked) {
    if (checked) {
      if (!selectedTadokuIds.includes(historyId)) {
        selectedTadokuIds = [...selectedTadokuIds, historyId];
      }
    } else {
      selectedTadokuIds = selectedTadokuIds.filter((id) => id !== historyId);
    }
  }

  async function syncTadokuEpisodes(historyIds) {
    if (historyIds.length === 0 || syncingTadoku || decliningTadoku) return;
    syncingTadoku = true;
    try {
      await startSaveLoop();
      if (saveError) return;
      const result = await syncTadokuCandidates(historyIds);
      if (!result.ok) throw new Error(result.error || 'Tadoku sync failed');
      const logCount = result.synced_logs || 0;
      showToast(
        'success',
        `Synced ${logCount} Tadoku ${logCount === 1 ? 'log' : 'logs'}`,
      );
      selectedTadokuIds = [];
    } catch (e) {
      showErrorToast('Tadoku sync failed: ' + e.message);
    } finally {
      syncingTadoku = false;
      await loadTadokuCandidates();
    }
  }

  async function declineTadokuEpisodes(historyIds) {
    if (historyIds.length === 0 || syncingTadoku || decliningTadoku) return;
    decliningTadoku = true;
    try {
      const result = await declineTadokuCandidates(historyIds);
      if (!result.ok) throw new Error(result.error || 'Could not decline episodes');
      const count = result.declined_episodes || 0;
      showToast(
        'success',
        `Declined ${count} Tadoku ${count === 1 ? 'episode' : 'episodes'}`,
      );
      selectedTadokuIds = [];
    } catch (e) {
      showErrorToast('Failed to decline Tadoku episodes: ' + e.message);
    } finally {
      decliningTadoku = false;
      await loadTadokuCandidates();
    }
  }

  function formatTadokuDuration(durationSeconds) {
    const minutes = Math.max(1, Math.round((durationSeconds || 0) / 60));
    return `${minutes} ${minutes === 1 ? 'minute' : 'minutes'}`;
  }

  function formatTadokuDate(value) {
    const date = new Date(value);
    return Number.isNaN(date.getTime()) ? value : date.toLocaleString();
  }

  function handleTagKeydown(e, field) {
    if (e.key === 'Enter') {
      e.preventDefault();
      addTag(field);
    }
  }

  function handleNoteTypeKeydown(e) {
    if (e.key === 'Enter') {
      e.preventDefault();
      addNoteType();
    }
  }
</script>

<div class="config-page">
  {#if loading}
    <p>Loading configuration...</p>
  {:else if config}
    <div class="tab-bar">
      {#each TABS as tab}
        <button
          class="tab"
          class:active={activeTab === tab.id}
          on:click={() => openTab(tab.id)}
        >{tab.label}</button>
      {/each}
    </div>

    {#if activeTab === 'server'}
    <p class="hint tab-hint">Saved to the Nagare server and shared across clients.</p>

    {#snippet userSelector(kind)}
      {@const sel = config[kind]?.users || []}
      {@const su = serverUsers[kind]}
      <div class="field">
        <span class="field-label">Users to monitor</span>
        <p class="hint">Leave all unchecked to monitor every user on this server.</p>
        {#if su?.loading}
          <p class="hint">Loading users…</p>
        {:else if su?.error}
          <p class="hint error-text">{su.error}</p>
        {:else if su?.users?.length}
          <div class="user-list">
            {#each su.users as u}
              <label class="user-item">
                <input
                  type="checkbox"
                  checked={sel.includes(u.id)}
                  on:change={(e) => toggleUser(kind, u.id, e.target.checked)}
                />
                <span>{u.name}</span>
              </label>
            {/each}
          </div>
        {:else if su}
          <p class="hint">No users found on this server.</p>
        {:else if sel.length}
          <p class="hint">{sel.length} user{sel.length === 1 ? '' : 's'} selected. Load users to edit.</p>
        {/if}
        <button type="button" class="btn-small" on:click={loadServerUsers}>
          {su ? 'Reload users' : 'Load users'}
        </button>
      </div>
    {/snippet}

    <!-- ── Media Servers ────────────────────────────── -->
    <section class="section">
      <h2>Media Servers</h2>
      <p class="hint">Configure each service separately. You can enable one, two, or all three at the same time.</p>

      <div class="server-card" class:enabled={config.emby.enabled}>
        <div class="server-head">
          <div>
            <h3>Emby</h3>
            <p class="hint">API key based connection</p>
          </div>
          <label class="toggle">
            <input
              type="checkbox"
              checked={config.emby.enabled}
              on:change={(e) => toggleServer('emby', e.target.checked)}
            />
            <span>Enabled</span>
          </label>
        </div>
        <div class="field">
          <label for="emby-url">Server URL</label>
          <input
            id="emby-url"
            type="url"
            placeholder="http://192.168.1.44:8096"
            value={config.emby.url}
            on:input={(e) => updateServerField('emby', 'url', e.target.value)}
          />
        </div>
        <div class="field">
          <label for="emby-key">API Key</label>
          <input
            id="emby-key"
            type="password"
            placeholder="Your Emby API key"
            value={config.emby.api_key}
            on:input={(e) => updateServerField('emby', 'api_key', e.target.value)}
          />
        </div>
        {#if config.emby.enabled}{@render userSelector('emby')}{/if}
      </div>

      <div class="server-card" class:enabled={config.jellyfin.enabled}>
        <div class="server-head">
          <div>
            <h3>Jellyfin</h3>
            <p class="hint">API key based connection</p>
          </div>
          <label class="toggle">
            <input
              type="checkbox"
              checked={config.jellyfin.enabled}
              on:change={(e) => toggleServer('jellyfin', e.target.checked)}
            />
            <span>Enabled</span>
          </label>
        </div>
        <div class="field">
          <label for="jellyfin-url">Server URL</label>
          <input
            id="jellyfin-url"
            type="url"
            placeholder="http://192.168.1.44:8096"
            value={config.jellyfin.url}
            on:input={(e) => updateServerField('jellyfin', 'url', e.target.value)}
          />
        </div>
        <div class="field">
          <label for="jellyfin-key">API Key</label>
          <input
            id="jellyfin-key"
            type="password"
            placeholder="Your Jellyfin API key"
            value={config.jellyfin.api_key}
            on:input={(e) => updateServerField('jellyfin', 'api_key', e.target.value)}
          />
        </div>
        {#if config.jellyfin.enabled}{@render userSelector('jellyfin')}{/if}
      </div>

      <div class="server-card" class:enabled={config.plex.enabled}>
        <div class="server-head">
          <div>
            <h3>Plex</h3>
            <p class="hint">Token based connection</p>
          </div>
          <label class="toggle">
            <input
              type="checkbox"
              checked={config.plex.enabled}
              on:change={(e) => toggleServer('plex', e.target.checked)}
            />
            <span>Enabled</span>
          </label>
        </div>
        <div class="field">
          <label for="plex-url">Server URL</label>
          <input
            id="plex-url"
            type="url"
            placeholder="http://192.168.1.44:32400"
            value={config.plex.url}
            on:input={(e) => updateServerField('plex', 'url', e.target.value)}
          />
        </div>
        <div class="field">
          <label for="plex-token">Plex Token</label>
          <input
            id="plex-token"
            type="password"
            placeholder="Your Plex token"
            value={config.plex.token}
            on:input={(e) => updateServerField('plex', 'token', e.target.value)}
          />
        </div>
        {#if config.plex.enabled}{@render userSelector('plex')}{/if}
      </div>
    </section>

    <!-- ── General ──────────────────────────────────── -->
    <section class="section">
      <h2>General</h2>
      <div class="field">
        <label for="target-lang">Target Language <span class="hint">(ISO 639-2/B)</span></label>
        <input id="target-lang" type="text" placeholder="jpn"
          bind:value={config.target_language} />
      </div>
      <div class="field">
        <label for="native-lang">Native Language <span class="hint">(ISO 639-2/B — secondary subtitle shown alongside)</span></label>
        <input id="native-lang" type="text" placeholder="eng"
          bind:value={config.native_language} />
      </div>
      <div class="field">
        <label for="media-mode">Media Access Mode</label>
        <select id="media-mode" bind:value={config.media_access_mode}>
          <option value="auto">Auto (disk → API fallback)</option>
          <option value="disk">Disk only</option>
          <option value="api">API only</option>
        </select>
      </div>
    </section>

    <!-- ── Path Mappings ────────────────────────────── -->
    <section class="section">
      <h2>Path Mappings</h2>
      <p class="hint">Translate server file paths to local paths accessible by this app.</p>
      {#each config.path_mappings as mapping, i}
        <div class="mapping-row">
          <input type="text" placeholder="Server path prefix" bind:value={mapping.from} />
          <span class="arrow">→</span>
          <input type="text" placeholder="Local path" bind:value={mapping.to} />
          <button class="btn-icon" on:click={() => removePathMapping(i)} title="Remove">✕</button>
        </div>
      {/each}
      <button class="btn-small" on:click={addPathMapping}>+ Add Mapping</button>
    </section>
    {/if}

    {#if activeTab === 'tadoku'}
    <p class="hint tab-hint">Review completed episodes yourself or let Nagare sync them on a daily schedule.</p>

    <section class="section">
      <h2>Sync Mode</h2>
      <div class="tadoku-mode-grid">
        <button
          type="button"
          class="tadoku-mode"
          class:selected={!config.tadoku.enabled}
          aria-pressed={!config.tadoku.enabled}
          on:click={() => (config.tadoku.enabled = false)}
        >
          <strong>Manual review</strong>
          <span>Choose each completed episode before it is sent.</span>
        </button>
        <button
          type="button"
          class="tadoku-mode"
          class:selected={config.tadoku.enabled}
          aria-pressed={config.tadoku.enabled}
          on:click={() => (config.tadoku.enabled = true)}
        >
          <strong>Automatic sync</strong>
          <span>Send all ready episodes once per day.</span>
        </button>
      </div>
      <p class="hint">Episodes are ready at 80% watched and can only be synced once. Durations round up to the next whole minute.</p>
    </section>

    {#if !config.tadoku.enabled}
    <section class="section">
      <div class="tadoku-review-heading">
        <div>
          <h2>Episodes Ready to Sync</h2>
          <p class="hint">Checked episodes are grouped into one Tadoku listening log per show.</p>
        </div>
        <button type="button" class="btn-small" disabled={loadingTadokuCandidates || syncingTadoku || decliningTadoku} on:click={loadTadokuCandidates}>
          {loadingTadokuCandidates ? 'Loading…' : 'Refresh'}
        </button>
      </div>

      {#if loadingTadokuCandidates}
        <p class="hint">Loading completed episodes…</p>
      {:else if tadokuCandidates.length === 0}
        <div class="tadoku-empty">No completed episodes are waiting to sync.</div>
      {:else}
        <div class="tadoku-candidate-list">
          {#each tadokuCandidates as candidate}
            <label class="tadoku-candidate">
              <input
                type="checkbox"
                checked={selectedTadokuIds.includes(candidate.history_id)}
                disabled={syncingTadoku || decliningTadoku}
                on:change={(event) => toggleTadokuCandidate(candidate.history_id, event.currentTarget.checked)}
              />
              <span class="tadoku-candidate-copy">
                <strong>
                  {candidate.title}
                  {#if candidate.pending_retry}<em>Retry</em>{/if}
                </strong>
                <span>{candidate.series_name} · {formatTadokuDuration(candidate.duration_seconds)}</span>
                <small>Completed {formatTadokuDate(candidate.watched_at)}</small>
                {#if candidate.last_error}<small class="error-text">Last attempt: {candidate.last_error}</small>{/if}
              </span>
            </label>
          {/each}
        </div>
        <div class="tadoku-review-actions">
          <button
            type="button"
            class="btn-small tadoku-decline"
            disabled={syncingTadoku || decliningTadoku || selectedTadokuIds.length === 0}
            on:click={() => declineTadokuEpisodes(selectedTadokuIds)}
          >
            {decliningTadoku ? 'Declining…' : `Decline selected (${selectedTadokuIds.length})`}
          </button>
          <button
            type="button"
            class="btn-small"
            disabled={syncingTadoku || decliningTadoku || selectedTadokuIds.length === 0}
            on:click={() => syncTadokuEpisodes(selectedTadokuIds)}
          >
            {syncingTadoku ? 'Syncing…' : `Sync selected (${selectedTadokuIds.length})`}
          </button>
          <button
            type="button"
            class="btn-small tadoku-sync-all"
            disabled={syncingTadoku || decliningTadoku}
            on:click={() => syncTadokuEpisodes(tadokuCandidates.map((candidate) => candidate.history_id))}
          >Sync all ({tadokuCandidates.length})</button>
        </div>
        <p class="hint tadoku-decline-hint">Declined episodes stay in Nagare history and will never be sent to Tadoku.</p>
      {/if}
    </section>
    {/if}

    <section class="section">
      <h2>Tadoku Connection</h2>
      <div class="field">
        <label for="tadoku-username">Username or email</label>
        <input id="tadoku-username" type="text" autocomplete="username"
          bind:value={config.tadoku.username} />
      </div>
      <div class="field">
        <label for="tadoku-password">Password</label>
        <input id="tadoku-password" type="password" autocomplete="current-password"
          placeholder={config.tadoku.password_configured ? 'Saved — leave blank to keep it' : ''}
          bind:value={config.tadoku.password} />
        <p class="hint">Nagare signs in automatically and keeps the Tadoku browser session fresh. Leave this blank to preserve an already saved password.</p>
      </div>
      <div class="field">
        <label for="tadoku-language">Language <span class="hint">(ISO 639-3)</span></label>
        <input id="tadoku-language" type="text" placeholder="jpn"
          bind:value={config.tadoku.language_code} />
      </div>
      {#if config.tadoku.enabled}
      <div class="field">
        <label for="tadoku-hour">Daily Export Hour <span class="hint">(Eastern time, 0–23)</span></label>
        <input id="tadoku-hour" type="number" min="0" max="23" step="1"
          bind:value={config.tadoku.export_hour_eastern} />
        <p class="hint">The default is 20 (8 PM). Daylight-saving time is handled automatically.</p>
      </div>
      {/if}
      <div class="field">
        <label for="tadoku-api-url">API URL</label>
        <input id="tadoku-api-url" type="url"
          bind:value={config.tadoku.api_url} />
      </div>
      <div class="tadoku-connection-actions">
        <button type="button" class="btn-small" disabled={testingTadoku || refreshingTadoku || clearingTadoku} on:click={testTadoku}>
          {testingTadoku ? 'Testing…' : 'Test connection'}
        </button>
        <button type="button" class="btn-small" disabled={testingTadoku || refreshingTadoku || clearingTadoku || !config.tadoku.username || (!config.tadoku.password && !config.tadoku.password_configured)} on:click={refreshTadoku}>
          {refreshingTadoku ? 'Refreshing…' : 'Refresh Tadoku login'}
        </button>
        <button type="button" class="btn-small tadoku-clear" disabled={testingTadoku || refreshingTadoku || clearingTadoku || (!config.tadoku.username && !config.tadoku.password_configured)} on:click={clearTadoku}>
          {clearingTadoku ? 'Clearing…' : 'Clear saved login'}
        </button>
      </div>
    </section>
    {/if}

    {#if activeTab === 'anki'}
    <p class="hint tab-hint">Saved to the Nagare server and shared across clients.</p>

    <!-- ── AnkiConnect ──────────────────────────────── -->
    <section class="section">
      <h2>AnkiConnect</h2>
      <div class="field">
        <label for="anki-url">AnkiConnect URL</label>
        <input id="anki-url" type="url" placeholder="http://localhost:8765"
          bind:value={config.anki.url} />
      </div>
      <div class="field">
        <label for="polling-rate">Polling Rate <span class="hint">(ms)</span></label>
        <input id="polling-rate" type="number" min="200" step="100"
          bind:value={config.anki.polling_rate_ms} />
      </div>
    </section>

    <!-- ── Anki Field Mapping ───────────────────────── -->
    <section class="section">
      <h2>Anki Field Mapping</h2>
      <div class="field">
        <label for="f-sentence">Sentence Field</label>
        <input id="f-sentence" type="text" bind:value={config.anki.fields.sentence} />
      </div>
      <div class="field">
        <label for="f-audio">Sentence Audio Field</label>
        <input id="f-audio" type="text" bind:value={config.anki.fields.sentence_audio} />
      </div>
      <div class="field">
        <label for="f-picture">Picture Field</label>
        <input id="f-picture" type="text" bind:value={config.anki.fields.picture} />
      </div>
      <div class="field">
        <label for="f-source">Source Name Field <span class="hint">(optional)</span></label>
        <input id="f-source" type="text" placeholder="Leave empty to skip"
          value={config.anki.fields.source_name || ''}
          on:input={(e) => {
            config.anki.fields.source_name = e.target.value || null;
          }} />
      </div>
      <div class="field">
        <label for="f-translation">Sentence Translation Field <span class="hint">(optional — native-language translation)</span></label>
        <input id="f-translation" type="text" placeholder="Leave empty to skip"
          value={config.anki.fields.sentence_translation || ''}
          on:input={(e) => {
            config.anki.fields.sentence_translation = e.target.value || null;
          }} />
      </div>
    </section>

    <!-- ── Anki Filtering ───────────────────────────── -->
    <section class="section">
      <h2>Anki Card Filtering</h2>

      <div class="field">
        <div class="checkbox-row">
          <input id="skip-audio" type="checkbox" bind:checked={config.anki.skip_if_audio_exists} />
          <label for="skip-audio">Skip if audio field already populated</label>
        </div>
      </div>
      <div class="field">
        <div class="checkbox-row">
          <input id="skip-picture" type="checkbox" bind:checked={config.anki.skip_if_picture_exists} />
          <label for="skip-picture">Skip if picture field already populated</label>
        </div>
      </div>

      <div class="field">
        <!-- svelte-ignore a11y_label_has_associated_control -->
        <label>Note Types <span class="hint">(empty = all)</span></label>
        <div class="tag-list">
          {#each config.anki.note_types as nt, i}
            <span class="tag">{nt} <button on:click={() => removeNoteType(i)}>✕</button></span>
          {/each}
        </div>
        <div class="tag-input-row">
          <input id="note-type-input" type="text" placeholder="Add note type..."
            on:keydown={handleNoteTypeKeydown} />
          <button class="btn-small" on:click={addNoteType}>Add</button>
        </div>
      </div>

      <div class="field">
        <!-- svelte-ignore a11y_label_has_associated_control -->
        <label>Add Tags <span class="hint">(added to every enriched card)</span></label>
        <div class="tag-list">
          {#each config.anki.add_tags as tag, i}
            <span class="tag">{tag} <button on:click={() => removeTag('add_tags', i)}>✕</button></span>
          {/each}
        </div>
        <div class="tag-input-row">
          <input id="tag-input-add_tags" type="text" placeholder="Add tag..."
            on:keydown={(e) => handleTagKeydown(e, 'add_tags')} />
          <button class="btn-small" on:click={() => addTag('add_tags')}>Add</button>
        </div>
      </div>

      <div class="field">
        <div class="checkbox-row">
          <input id="series-tag-enabled" type="checkbox" bind:checked={config.anki.series_tag_enabled} />
          <label for="series-tag-enabled">Add a per-series tag to each card</label>
        </div>
      </div>
      {#if config.anki.series_tag_enabled}
        <div class="field">
          <label for="series-tag-parent">Parent Tag <span class="hint">(optional — <code>parent::Series_Name</code>; empty uses <code>Series_Name</code> alone)</span></label>
          <input id="series-tag-parent" type="text" placeholder="e.g. anime"
            bind:value={config.anki.series_tag_parent} />
        </div>
      {/if}

      <div class="field">
        <!-- svelte-ignore a11y_label_has_associated_control -->
        <label>Ignore Tags <span class="hint">(skip cards with any of these)</span></label>
        <div class="tag-list">
          {#each config.anki.ignore_tags as tag, i}
            <span class="tag">{tag} <button on:click={() => removeTag('ignore_tags', i)}>✕</button></span>
          {/each}
        </div>
        <div class="tag-input-row">
          <input id="tag-input-ignore_tags" type="text" placeholder="Add tag..."
            on:keydown={(e) => handleTagKeydown(e, 'ignore_tags')} />
          <button class="btn-small" on:click={() => addTag('ignore_tags')}>Add</button>
        </div>
      </div>

      <div class="field">
        <!-- svelte-ignore a11y_label_has_associated_control -->
        <label>Require Tags <span class="hint">(only process cards with at least one)</span></label>
        <div class="tag-list">
          {#each config.anki.require_tags as tag, i}
            <span class="tag">{tag} <button on:click={() => removeTag('require_tags', i)}>✕</button></span>
          {/each}
        </div>
        <div class="tag-input-row">
          <input id="tag-input-require_tags" type="text" placeholder="Add tag..."
            on:keydown={(e) => handleTagKeydown(e, 'require_tags')} />
          <button class="btn-small" on:click={() => addTag('require_tags')}>Add</button>
        </div>
      </div>
    </section>

    <!-- ── Mining Options ────────────────────────────── -->
    <section class="section">
      <h2>Mining Options</h2>
      <div class="field">
        <label for="start-offset">Audio start offset <span class="hint">(ms before subtitle start)</span></label>
        <input id="start-offset" type="number" min="0" max="5000" step="50" bind:value={config.mining.audio_start_offset_ms} />
      </div>
      <div class="field">
        <label for="end-offset">Audio end offset <span class="hint">(ms after subtitle end)</span></label>
        <input id="end-offset" type="number" min="0" max="5000" step="50" bind:value={config.mining.audio_end_offset_ms} />
      </div>
      <div class="field">
        <label for="audio-codec">Audio Codec</label>
        <select id="audio-codec" bind:value={config.mining.audio_codec}>
          <option value="mp3">MP3 (most compatible)</option>
          <option value="aac">AAC / M4A</option>
          <option value="opus">Opus</option>
        </select>
      </div>
      <div class="field">
        <div class="checkbox-row">
          <input id="default-avif" type="checkbox" bind:checked={config.mining.generate_avif} />
          <label for="default-avif">Generate animated screenshot by default</label>
        </div>
      </div>
      <div class="field">
        <label for="animated-screenshot-encoder">Animated Screenshot Encoder</label>
        <select id="animated-screenshot-encoder" bind:value={config.mining.animated_screenshot_encoder}>
          <option value="libsvtav1">libsvtav1 (fast)</option>
          <option value="libaom-av1">libaom-av1 (slow, higher quality)</option>
        </select>
      </div>
      <div class="field">
        <label for="avif-max-width">Animated Screenshot Max Width <span class="hint">(px, never upscaled; longer clips scale down further)</span></label>
        <input id="avif-max-width" type="number" min="480" max="1280" step="16" bind:value={config.mining.avif_max_width} />
      </div>
      <div class="field">
        <label for="avif-max-fps">Animated Screenshot Max FPS <span class="hint">(longer clips scale down further)</span></label>
        <input id="avif-max-fps" type="number" min="1" max="30" step="1" bind:value={config.mining.avif_max_fps} />
      </div>
      <div class="field">
        <label for="static-screenshot-format">Static Screenshot Format</label>
        <select id="static-screenshot-format" bind:value={config.mining.static_screenshot_format}>
          <option value="webp">WebP</option>
          <option value="jpg">JPG</option>
          <option value="png">PNG</option>
        </select>
      </div>
    </section>
    {/if}

    {#if activeTab === 'frontend'}
    <p class="hint tab-hint">Saved only in this browser, applies to this device.</p>

    <section class="section">
      <h2>Mining Options</h2>
      <div class="field">
        <p class="hint">This setting is stored locally and applies only on this device.</p>
        <div class="checkbox-row">
          <input id="auto-approve" type="checkbox" bind:checked={$autoApprove} />
          <label for="auto-approve">Auto-approve new cards with the default matched subtitle range</label>
        </div>
      </div>
      <div class="field">
        <div class="checkbox-row">
          <input id="pause-on-enhance" type="checkbox" bind:checked={$pauseOnEnhance} />
          <label for="pause-on-enhance">Pause playback when the Anki enhance screen opens</label>
        </div>
      </div>
    </section>

    <section class="section">
      <h2>Interface</h2>
      <p class="hint">Show or hide optional UI elements. Stored locally and applies only on this device.</p>
      <div class="field">
        <div class="checkbox-row">
          <input id="show-native-subtitles" type="checkbox" bind:checked={$showNativeSubtitles} />
          <label for="show-native-subtitles">Show native-language secondary subtitle</label>
        </div>
      </div>
      <div class="field">
        <div class="checkbox-row">
          <input id="show-download-button" type="checkbox" bind:checked={$showDownloadButton} />
          <label for="show-download-button">Show subtitle download button</label>
        </div>
      </div>
    </section>
    {/if}

  {:else}
    <p class="error">Failed to load configuration</p>
  {/if}
</div>

<style>
  .config-page {
    padding: 1.5rem;
    width: 100%;
    box-sizing: border-box;
    overflow-y: auto;
  }

  .section {
    margin-bottom: 1.5rem;
    padding: 1rem;
    background: var(--bg-card);
    border-radius: 8px;
    border: 1px solid var(--border);
  }

  .tab-bar {
    display: flex;
    gap: 0.25rem;
    margin-bottom: 1rem;
    border-bottom: 1px solid var(--border);
  }

  .tab {
    background: transparent;
    border: none;
    border-bottom: 2px solid transparent;
    color: var(--text-secondary);
    cursor: pointer;
    padding: 0.5rem 0.9rem;
    font-size: 0.9rem;
    font-weight: 600;
    margin-bottom: -1px;
  }

  .tab:hover {
    color: var(--text-primary);
  }

  .tab.active {
    color: var(--accent);
    border-bottom-color: var(--accent);
  }

  .tab-hint {
    margin-bottom: 1rem;
  }

  .section h2 {
    font-size: 1rem;
    font-weight: 600;
    margin: 0 0 0.75rem;
    color: var(--accent);
  }

  .section h3 {
    margin: 0;
    font-size: 0.95rem;
    color: var(--text-primary);
  }

  .field {
    margin-bottom: 0.75rem;
  }

  .field:last-child {
    margin-bottom: 0;
  }

  .field > label,
  .field > .field-label {
    display: block;
    font-size: 0.85rem;
    color: var(--text-secondary);
    margin-bottom: 0.25rem;
  }

  .user-list {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    margin-bottom: 0.5rem;
    max-height: 220px;
    overflow-y: auto;
    padding: 0.5rem 0.6rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg-primary);
  }

  .user-item {
    display: flex;
    align-items: center;
    gap: 0.45rem;
    font-size: 0.85rem;
    color: var(--text-primary);
    cursor: pointer;
  }

  .user-item input {
    cursor: pointer;
  }

  .error-text {
    color: var(--danger, #e5534b);
    opacity: 1;
  }

  .hint {
    font-size: 0.75rem;
    color: var(--text-secondary);
    opacity: 0.7;
  }

  input[type="text"],
  input[type="url"],
  input[type="password"],
  input[type="number"],
  select {
    width: 100%;
    padding: 0.4rem 0.6rem;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg-primary);
    color: var(--text-primary);
    font-size: 0.85rem;
    box-sizing: border-box;
  }

  input:focus, select:focus {
    outline: none;
    border-color: var(--accent);
  }

  .server-card {
    margin-top: 0.9rem;
    padding: 0.9rem;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--bg-primary);
  }

  .server-card.enabled {
    border-color: color-mix(in srgb, var(--accent) 45%, var(--border));
    background: color-mix(in srgb, var(--accent) 6%, var(--bg-primary));
  }

  .server-head {
    justify-content: space-between;
    display: flex;
    align-items: center;
    gap: 1rem;
    margin-bottom: 0.75rem;
  }

  .toggle {
    display: flex;
    align-items: center;
    gap: 0.45rem;
    font-size: 0.85rem;
    color: var(--text-primary);
    cursor: pointer;
    white-space: nowrap;
  }

  .checkbox-row {
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }

  .checkbox-row label {
    font-size: 0.85rem;
    color: var(--text-primary);
    cursor: pointer;
    margin-bottom: 0 !important;
  }

  .tadoku-mode-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0.6rem;
    margin-bottom: 0.65rem;
  }

  .tadoku-connection-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
  }

  .tadoku-clear {
    color: var(--danger, #e5534b);
    border-color: color-mix(in srgb, var(--danger, #e5534b) 55%, var(--border));
  }

  .tadoku-mode {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    padding: 0.75rem;
    text-align: left;
    color: var(--text-primary);
    background: var(--bg-primary);
    border: 1px solid var(--border);
    border-radius: 7px;
    cursor: pointer;
  }

  .tadoku-mode:hover,
  .tadoku-mode.selected {
    border-color: var(--accent);
  }

  .tadoku-mode.selected {
    background: color-mix(in srgb, var(--accent) 8%, var(--bg-primary));
  }

  .tadoku-mode span {
    color: var(--text-secondary);
    font-size: 0.75rem;
    line-height: 1.35;
  }

  .tadoku-review-heading,
  .tadoku-review-actions {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.75rem;
  }

  .tadoku-review-heading {
    align-items: flex-start;
    margin-bottom: 0.75rem;
  }

  .tadoku-review-heading h2,
  .tadoku-review-heading p {
    margin-bottom: 0;
  }

  .tadoku-candidate-list {
    display: flex;
    flex-direction: column;
    max-height: 360px;
    overflow-y: auto;
    border: 1px solid var(--border);
    border-radius: 7px;
  }

  .tadoku-candidate {
    display: flex;
    align-items: flex-start;
    gap: 0.65rem;
    padding: 0.7rem;
    cursor: pointer;
    background: var(--bg-primary);
    border-bottom: 1px solid var(--border);
  }

  .tadoku-candidate:last-child {
    border-bottom: none;
  }

  .tadoku-candidate:hover {
    background: var(--bg-hover);
  }

  .tadoku-candidate input {
    margin-top: 0.2rem;
  }

  .tadoku-candidate-copy {
    display: flex;
    min-width: 0;
    flex-direction: column;
    gap: 0.1rem;
    font-size: 0.82rem;
  }

  .tadoku-candidate-copy > span,
  .tadoku-candidate-copy small {
    color: var(--text-secondary);
  }

  .tadoku-candidate-copy em {
    display: inline-block;
    margin-left: 0.35rem;
    padding: 0.05rem 0.3rem;
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    border-radius: 999px;
    font-size: 0.65rem;
    font-style: normal;
    font-weight: 600;
    vertical-align: middle;
  }

  .tadoku-candidate-copy small {
    font-size: 0.7rem;
    opacity: 0.75;
  }

  .tadoku-review-actions {
    justify-content: flex-end;
    flex-wrap: wrap;
    margin-top: 0.75rem;
  }

  .tadoku-decline {
    margin-right: auto;
    color: var(--danger, #e5534b);
    border-color: color-mix(in srgb, var(--danger, #e5534b) 55%, var(--border));
  }

  .tadoku-decline-hint {
    margin: 0.5rem 0 0;
  }

  .tadoku-sync-all {
    color: white;
    background: var(--accent);
    border-color: var(--accent);
  }

  .tadoku-empty {
    padding: 1.25rem 0.75rem;
    color: var(--text-secondary);
    background: var(--bg-primary);
    border: 1px dashed var(--border);
    border-radius: 7px;
    text-align: center;
    font-size: 0.82rem;
  }

  .mapping-row {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    margin-bottom: 0.4rem;
  }

  .mapping-row input {
    flex: 1;
  }

  .arrow {
    color: var(--text-secondary);
    font-size: 0.9rem;
    flex-shrink: 0;
  }

  .btn-icon {
    background: transparent;
    border: none;
    color: var(--text-secondary);
    cursor: pointer;
    padding: 0.2rem 0.4rem;
    font-size: 0.8rem;
    border-radius: 4px;
    flex-shrink: 0;
  }

  .btn-icon:hover {
    color: var(--accent);
    background: var(--bg-hover);
  }

  .btn-small {
    font-size: 0.8rem;
    padding: 0.3rem 0.6rem;
    background: var(--bg-hover);
    border: 1px solid var(--border);
    border-radius: 4px;
    color: var(--text-primary);
    cursor: pointer;
  }

  .btn-small:hover {
    border-color: var(--accent);
  }

  .tag-list {
    display: flex;
    flex-wrap: wrap;
    gap: 0.3rem;
    margin-bottom: 0.4rem;
  }

  .tag {
    display: inline-flex;
    align-items: center;
    gap: 0.2rem;
    background: var(--bg-hover);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 0.15rem 0.4rem;
    font-size: 0.8rem;
  }

  .tag button {
    background: none;
    border: none;
    color: var(--text-secondary);
    cursor: pointer;
    padding: 0;
    font-size: 0.7rem;
    line-height: 1;
  }

  .tag button:hover {
    color: var(--accent);
  }

  .tag-input-row {
    display: flex;
    gap: 0.4rem;
  }

  .tag-input-row input {
    flex: 1;
  }

  .error {
    color: var(--accent);
  }

  /* ── Mobile ── */
  @media (max-width: 768px) {
    .config-page {
      padding: 0.75rem;
    }

    .section {
      padding: 0.75rem;
      margin-bottom: 1rem;
    }

    .tadoku-mode-grid {
      grid-template-columns: 1fr;
    }

    .tab {
      flex: 1;
      padding: 0.6rem 0.4rem;
      min-height: 44px;
      font-size: 0.85rem;
    }

    .mapping-row {
      flex-wrap: wrap;
      gap: 0.3rem;
    }

    .server-head {
      flex-wrap: wrap;
      align-items: flex-start;
    }

    .mapping-row input {
      flex: 1 1 100%;
    }

    .arrow {
      display: none;
    }

    .tag-input-row {
      flex-wrap: wrap;
    }

    .tag-input-row input {
      flex: 1 1 0;
      min-width: 0;
    }

    input[type="text"],
    input[type="url"],
    input[type="password"],
    input[type="number"],
    select {
      font-size: 16px;
      padding: 0.5rem 0.6rem;
    }
  }
</style>
