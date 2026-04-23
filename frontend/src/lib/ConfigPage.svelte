<script>
  import { onMount } from 'svelte';
  import { getConfig, updateConfig } from './api.js';
  import { applyMiningConfig, autoApprove, pauseOnEnhance, showToast } from './stores.js';

  const AUTO_APPROVE_STORAGE_KEY = 'opt_autoApprove';

  let config = null;
  let loading = true;
  let saving = false;

  onMount(async () => {
    try {
      config = await getConfig();
      migrateAutoApprove(config?.mining?.auto_approve);
      normalizeConfig();
    } catch (e) {
      console.error('Failed to load config:', e);
    } finally {
      loading = false;
    }
  });

  function createServerConfig(kind) {
    return kind === 'plex'
      ? { enabled: false, url: '', token: '' }
      : { enabled: false, url: '', api_key: '' };
  }

  function normalizeConfig() {
    if (!config) return;
    config.emby = config.emby || createServerConfig('emby');
    config.jellyfin = config.jellyfin || createServerConfig('jellyfin');
    config.plex = config.plex || createServerConfig('plex');
    if (!config.anki) config.anki = {};
    if (!config.anki.fields) config.anki.fields = {};
    if (!config.path_mappings) config.path_mappings = [];
    if (!config.anki.add_tags) config.anki.add_tags = [];
    if (!config.anki.ignore_tags) config.anki.ignore_tags = [];
    if (!config.anki.require_tags) config.anki.require_tags = [];
    if (!config.anki.note_types) config.anki.note_types = [];
    if (!config.mining) config.mining = {};
    if (config.mining.audio_start_offset_ms == null) config.mining.audio_start_offset_ms = 100;
    if (config.mining.audio_end_offset_ms == null) config.mining.audio_end_offset_ms = 500;
    if (config.mining.generate_avif == null) config.mining.generate_avif = true;
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

  async function save() {
    saving = true;
    try {
      const payload = {
        ...config,
        mining: { ...config.mining },
      };
      delete payload.mining.auto_approve;

      const result = await updateConfig(payload);
      if (result.ok) {
        applyMiningConfig(payload.mining);
        config = payload;
        showToast('success', 'Server configuration saved');
      } else {
        showToast('error', result.error || 'Failed to save');
      }
    } catch (e) {
      showToast('error', 'Failed to save: ' + e.message);
    } finally {
      saving = false;
    }
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
    <section class="settings-part">
      <h2>Server Settings</h2>
      <p class="hint">Saved to the Nagare server and shared across clients.</p>
    </section>

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
        <div class="checkbox-row">
          <input id="default-avif" type="checkbox" bind:checked={config.mining.generate_avif} />
          <label for="default-avif">Generate animated screenshot by default</label>
        </div>
      </div>
    </section>

    <section class="settings-part">
      <h2>Client Settings</h2>
      <p class="hint">Saved only in this browser.</p>
    </section>

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

    <!-- ── Save Button ──────────────────────────────── -->
    <div class="save-bar">
      <button class="btn-save" on:click={save} disabled={saving}>
        {saving ? 'Saving...' : 'Save Server Settings'}
      </button>
    </div>
  {:else}
    <p class="error">Failed to load configuration</p>
  {/if}
</div>

<style>
  .config-page {
    padding: 1.5rem;
    max-width: 640px;
    overflow-y: auto;
    padding-bottom: 5rem;
  }

  .section {
    margin-bottom: 1.5rem;
    padding: 1rem;
    background: var(--bg-card);
    border-radius: 8px;
    border: 1px solid var(--border);
  }

  .settings-part {
    margin-bottom: 1rem;
  }

  .settings-part h2 {
    margin: 0 0 0.25rem;
    font-size: 1.2rem;
    color: var(--text-primary);
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

  .field > label {
    display: block;
    font-size: 0.85rem;
    color: var(--text-secondary);
    margin-bottom: 0.25rem;
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

  .save-bar {
    position: sticky;
    bottom: 0;
    padding: 0.75rem 0;
    background: var(--bg-primary);
    border-top: 1px solid var(--border);
    margin-top: 1rem;
  }

  .btn-save {
    width: 100%;
    padding: 0.6rem 1rem;
    font-size: 0.9rem;
    font-weight: 600;
    background: var(--accent);
    color: white;
    border: none;
    border-radius: 6px;
    cursor: pointer;
  }

  .btn-save:hover {
    opacity: 0.9;
  }

  .btn-save:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .error {
    color: var(--accent);
  }

  /* ── Mobile ── */
  @media (max-width: 768px) {
    .config-page {
      padding: 0.75rem;
      padding-bottom: 4rem;
    }

    .section {
      padding: 0.75rem;
      margin-bottom: 1rem;
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

    .btn-save {
      min-height: 44px;
      font-size: 1rem;
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
