<script>
  import { getServerLibraries } from './api.js';
  import { mediaFilterMode, mediaFilterSelection, updateMediaFilter } from './mediaFilters.js';

  export let kind;
  export let filter;
  export let beforeLoad;

  let libraries = [];
  let loaded = false;
  let loading = false;
  let error = '';

  $: mode = mediaFilterMode(filter);
  $: selection = mediaFilterSelection(filter);
  $: missingIds = selection.libraries.filter((id) => !libraries.some((library) => library.id === id));

  async function loadLibraries() {
    loading = true;
    error = '';
    try {
      await beforeLoad();
      const result = (await getServerLibraries()).find((entry) => entry.server_kind === kind);
      if (!result) throw new Error('Enable this server and save its connection first.');
      if (result.error) throw new Error(result.error);
      libraries = result.libraries || [];
      loaded = true;
    } catch (e) {
      error = e.message || 'Could not load libraries.';
    } finally {
      loading = false;
    }
  }

  function setMode(nextMode) {
    filter = updateMediaFilter(filter, nextMode);
  }

  function toggleLibrary(id, checked) {
    const libraries = selection.libraries.filter((value) => value !== id);
    if (checked) libraries.push(id);
    filter = updateMediaFilter(filter, mode, { ...selection, libraries });
  }

  function setFolders(text) {
    filter = updateMediaFilter(filter, mode, { ...selection, folders: text ? text.split('\n') : [] });
  }
</script>

<div class="media-filters">
  <h4>Libraries & folders</h4>
  <fieldset class="mode-options">
    <legend>Filter mode</legend>
    <label>
      <input type="radio" name={`${kind}-filter-mode`} value="include" checked={mode === 'include'} on:change={() => setMode('include')} />
      Only include selected (whitelist)
    </label>
    <label>
      <input type="radio" name={`${kind}-filter-mode`} value="exclude" checked={mode === 'exclude'} on:change={() => setMode('exclude')} />
      Exclude selected (blacklist)
    </label>
  </fieldset>
  <p class="hint">{mode === 'include'
    ? 'Only checked libraries are tracked. All other libraries are ignored.'
    : 'Checked libraries are ignored. All other libraries are tracked.'}
    With no libraries checked, all libraries are allowed.</p>
  <div class="library-heading">
    <span class="field-label">{mode === 'include' ? 'Libraries to include' : 'Libraries to exclude'}</span>
    <button type="button" class="btn-small" on:click={loadLibraries} disabled={loading}>
      {loading ? 'Loading…' : loaded ? 'Reload libraries' : 'Load libraries'}
    </button>
  </div>
  {#if error}<p class="hint error-text" role="alert">{error}</p>{/if}
  {#if loaded && libraries.length === 0}<p class="hint">No libraries found on this server.</p>{/if}
  {#each libraries as library (library.id)}
    <label class="library-row">
      <input type="checkbox" checked={selection.libraries.includes(library.id)} on:change={(e) => toggleLibrary(library.id, e.target.checked)} />
      <span class="library-name">
        <span>{library.name}</span>
        {#if library.paths.length}<span class="paths">{library.paths.join(' · ')}</span>{/if}
      </span>
    </label>
  {/each}
  {#each missingIds as id (id)}
    <label class="library-row">
      <input type="checkbox" checked on:change={(e) => toggleLibrary(id, e.target.checked)} />
      <span class="library-name">Saved library <span class="paths">{id}</span></span>
    </label>
  {/each}
  <div class="field folder-field">
    <label for={`${kind}-filter-folders`}>{mode === 'include' ? 'Folders to include' : 'Folders to exclude'}</label>
    <textarea id={`${kind}-filter-folders`} rows="3" placeholder={mode === 'include' ? '/media/Japanese\n/media/Anime' : '/media/English\n/media/English dubs'} value={selection.folders.join('\n')} on:input={(e) => setFolders(e.target.value)}></textarea>
  </div>
  <p class="hint">One server folder path per line, including subfolders. Use paths before Nagare path mappings.
    {mode === 'include' ? 'Folder paths further limit the included libraries.' : 'Items in these folders are also ignored.'}
    Leave blank to allow all folders.</p>
  <p class="hint">Saved automatically for this server. Existing history is kept.</p>
</div>

<style>
  .media-filters { border-top: 1px solid var(--border); margin-top: 1rem; padding-top: 1rem; text-align: left; }
  h4 { margin: 0 0 0.5rem; font-size: 0.95rem; }
  .hint { color: var(--text-secondary); font-size: 0.8rem; margin: 0.4rem 0 0.7rem; line-height: 1.5; }
  .library-heading { display: flex; justify-content: space-between; align-items: center; gap: 0.75rem; }
  .field-label, label { font-size: 0.85rem; font-weight: 500; }
  .mode-options { border: 0; padding: 0; margin: 0.75rem 0; display: flex; flex-wrap: wrap; gap: 0.5rem 1.25rem; }
  .mode-options legend { font-size: 0.85rem; font-weight: 500; margin-bottom: 0.5rem; }
  .mode-options label { display: flex; align-items: center; gap: 0.4rem; cursor: pointer; }
  input[type='radio'], input[type='checkbox'] { accent-color: var(--accent); flex-shrink: 0; }
  .btn-small { padding: 0.35rem 0.65rem; border: 1px solid var(--border); border-radius: 6px; background: var(--bg-hover); color: var(--text-primary); cursor: pointer; font-size: 0.8rem; }
  .btn-small:disabled { opacity: 0.6; cursor: wait; }
  .library-row { display: flex; align-items: center; gap: 0.75rem; padding: 0.5rem 0; }
  .library-row { cursor: pointer; }
  .library-name { min-width: 0; overflow-wrap: anywhere; }
  .paths { display: block; font-weight: 400; font-size: 0.75rem; color: var(--text-secondary); margin-top: 0.2rem; }
  textarea { border: 1px solid var(--border); border-radius: 6px; background: var(--bg-primary); color: var(--text-primary); padding: 0.5rem; font: inherit; font-size: 0.85rem; }
  .folder-field { margin-top: 1rem; }
  .field { display: flex; flex-direction: column; gap: 0.4rem; }
  textarea { width: 100%; box-sizing: border-box; resize: vertical; }
  .error-text { color: var(--danger, #f87171); }
</style>
