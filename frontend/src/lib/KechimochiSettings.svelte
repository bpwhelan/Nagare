<script>
  import { onMount, onDestroy } from 'svelte';
  import { getKechimochiStatus, testKechimochiConnection, syncKechimochi } from './api.js';
  import { showErrorToast, showToast } from './stores.js';

  export let settings;
  export let ensureSaved;

  let status = null;
  let error = '';
  let testing = false;
  let starting = false;
  let connection = '';
  let timer;
  let disposed = false;

  $: report = status?.state?.report;
  $: dailyTime = `${String(settings.daily_hour).padStart(2, '0')}:${String(settings.daily_minute).padStart(2, '0')}`;

  function dateTime(value) {
    if (!value) return 'Not yet';
    return new Date(value).toLocaleString();
  }

  async function refresh() {
    try {
      const result = await getKechimochiStatus();
      if (!result.ok) throw new Error(result.error);
      if (disposed) return;
      status = result;
      error = '';
    } catch (e) {
      if (!disposed) error = e.message;
    }
  }

  async function poll() {
    await refresh();
    if (!disposed) timer = setTimeout(poll, status?.running ? 1500 : 5000);
  }

  onMount(() => { poll(); });
  onDestroy(() => { disposed = true; clearTimeout(timer); });

  async function test() {
    testing = true;
    connection = '';
    try {
      await ensureSaved();
      const result = await testKechimochiConnection();
      if (!result.ok) throw new Error(result.error);
      connection = `Connected · ${result.version} · ${result.media_count} media · ${result.log_count} activity logs`;
      showToast('success', 'Connected to Kechimochi');
    } catch (e) {
      error = e.message;
      showErrorToast(e.message);
    } finally { testing = false; }
  }

  async function sync() {
    starting = true;
    try {
      await ensureSaved();
      const result = await syncKechimochi();
      if (!result.ok) throw new Error(result.error);
      showToast('success', result.started ? 'Kechimochi sync started' : 'Kechimochi sync is already running');
      await refresh();
    } catch (e) { showErrorToast(e.message); }
    finally { starting = false; }
  }

  function setTime(value) {
    if (!/^\d{2}:\d{2}$/.test(value)) return;
    const [hour, minute] = value.split(':').map(Number);
    settings = { ...settings, daily_hour: hour, daily_minute: minute };
  }
</script>

<section class="panel">
  <div class="heading">
    <div>
      <h2>Kechimochi sync</h2>
      <p>Keep your media and recorded playback progress up to date automatically.</p>
    </div>
    <span class="badge" class:working={status?.running} class:healthy={status?.enabled && !status?.state?.last_error}>
      {status?.running ? 'Syncing…' : !settings.enabled ? 'Paused' : status?.state?.last_error ? 'Retry scheduled' : 'Automatic sync on'}
    </span>
  </div>

  <label class="toggle">
    <input type="checkbox" bind:checked={settings.enabled} />
    <span><strong>Enable automatic sync</strong><small>Backfill all history when enabled, then keep it in sync on your schedule.</small></span>
  </label>

  <div class="field">
    <label for="kechimochi-url">Kechimochi API URL</label>
    <input id="kechimochi-url" type="url" placeholder="http://127.0.0.1:3031" bind:value={settings.api_url} />
    <small>Use this computer’s LAN address if Nagare runs on another computer or in Docker. Enable LAN access in Kechimochi’s HTTP API settings.</small>
  </div>

  <div class="fields">
    <div class="field">
      <label for="kechimochi-mode">Schedule</label>
      <select id="kechimochi-mode" bind:value={settings.sync_mode}>
        <option value="automatic">Every few minutes</option>
        <option value="daily">Once a day</option>
      </select>
    </div>
    {#if settings.sync_mode === 'automatic'}
      <div class="field">
        <label for="kechimochi-interval">Interval (minutes)</label>
        <input id="kechimochi-interval" type="number" min="1" max="1440" step="1" bind:value={settings.interval_minutes} />
      </div>
    {:else}
      <div class="field">
        <label for="kechimochi-time">Daily sync time</label>
        <input id="kechimochi-time" type="time" value={dailyTime} on:input={(event) => setTime(event.target.value)} />
      </div>
    {/if}
  </div>
  <div class="field">
    <label for="kechimochi-timezone">Time zone for activity dates and daily scheduling</label>
    <input id="kechimochi-timezone" type="text" list="kechimochi-timezones" bind:value={settings.timezone} />
    <datalist id="kechimochi-timezones">
      <option value="America/New_York"></option><option value="America/Chicago"></option>
      <option value="America/Denver"></option><option value="America/Los_Angeles"></option>
      <option value="Europe/London"></option><option value="Europe/Berlin"></option>
      <option value="Asia/Tokyo"></option><option value="UTC"></option>
    </datalist>
    <small>Use an IANA time zone, such as America/New_York. Daylight saving time is handled automatically.</small>
  </div>
  <div class="actions">
    <button type="button" disabled={testing} on:click={test}>{testing ? 'Testing…' : 'Test connection'}</button>
    <button type="button" class="primary" disabled={starting || status?.running} on:click={sync}>{starting || status?.running ? 'Syncing…' : 'Sync now'}</button>
  </div>
  {#if connection}<p class="connection">{connection}</p>{/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
</section>

<section class="panel" aria-live="polite">
  <h2>Sync status</h2>
  <dl class="status">
    <div><dt>Last successful sync</dt><dd>{dateTime(status?.state?.last_success_at)}</dd></div>
    <div><dt>Next check</dt><dd>{status?.running ? 'In progress' : !settings.enabled ? 'Automatic sync paused' : status?.next_run_at ? dateTime(status.next_run_at) : 'Waiting for schedule'}</dd></div>
  </dl>
  {#if report && status?.state?.last_finished_at}
    <div class="totals">
      <div><strong>{report.history_items}</strong><span>history items</span></div>
      <div><strong>{report.logs_created}</strong><span>logs added</span></div>
      <div><strong>{report.logs_updated}</strong><span>logs updated</span></div>
      <div><strong>{report.unchanged_logs}</strong><span>already in sync</span></div>
    </div>
    <p class="detail">Last check: {dateTime(status.state.last_finished_at)}. Media: {report.media_created} added, {report.media_updated} updated, {report.media_deleted} removed. Activity logs removed: {report.logs_deleted}.</p>
    {#if report.media_only_items}<p class="detail">{report.media_only_items} items have no recorded playback yet. Their history is synced with the media; activity logs start when playback progress is recorded.</p>{/if}
    {#if report.retained_media}<p class="detail">Kept {report.retained_media} former Nagare media entries with additional activity or milestones.</p>{/if}
  {/if}
  {#if status?.state?.last_error}
    <p class="error">{status.state.last_error}</p>
    <p class="detail">{settings.enabled ? 'Nagare retries automatically with increasing delays while Kechimochi is unavailable. Successful writes are retained.' : 'Enable automatic sync or use Sync now to retry.'}</p>
  {/if}
</section>

<section class="panel explanation">
  <h2>What stays in sync</h2>
  <p>All Nagare history is included: episodes, movies, audiobooks, partial playback, older items, and every audio language. Episodes are grouped by show and media server. Items with recorded playback have an activity log that updates with their progress and latest playback date. Zero-progress items remain in the media library.</p>
  <p>Watching and listening time comes from Nagare’s saved playback position, capped at the item’s runtime and rounded to the nearest minute, with a one-minute minimum for positive progress. Exact positions, media details, subtitle counts, and mined-note counts are kept with the media. Nagare does not have a historical ledger of time spent or rewatches.</p>
  <p>Nagare manages its own media variants and logs. Your other Kechimochi entries, covers, descriptions, custom metadata, and milestones are preserved. Append personal log notes after the Nagare section. Removing history from Nagare removes its synced logs; deleting a synced entry in Kechimochi recreates it on the next check.</p>
</section>

<style>
  .panel { padding: 1.2rem; margin-bottom: 1.25rem; border: 1px solid var(--border); border-radius: 10px; background: var(--bg-card); text-align: left; }
  .heading { display: flex; align-items: start; justify-content: space-between; gap: 1rem; }
  h2 { font-size: 1.05rem; margin: 0 0 .65rem; }
  p { line-height: 1.55; color: var(--text-secondary); margin: .4rem 0 1rem; font-size: .88rem; }
  .badge { flex-shrink: 0; border: 1px solid var(--border); border-radius: 20px; padding: .35rem .65rem; font-size: .75rem; }
  .badge.healthy { color: var(--success, #63c99a); }
  .badge.working { color: var(--accent); }
  .toggle { display: flex; align-items: center; gap: .8rem; margin: .9rem 0 1.3rem; }
  .toggle input { width: auto; }
  .toggle span { display: grid; gap: .25rem; }
  .toggle strong { font-size: .9rem; }
  small { color: var(--text-secondary); font-size: .78rem; line-height: 1.5; }
  .field { display: grid; gap: .45rem; margin-bottom: 1rem; }
  .field label { font-size: .85rem; }
  .fields { display: grid; grid-template-columns: 1fr 1fr; gap: 1rem; }
  input, select { width: 100%; box-sizing: border-box; padding: .65rem .75rem; color: var(--text-primary); background: var(--bg-input, var(--bg-card)); border: 1px solid var(--border); border-radius: 6px; font: inherit; font-size: .88rem; }
  .actions { display: flex; gap: .65rem; flex-wrap: wrap; }
  button { padding: .6rem .9rem; border-radius: 6px; border: 1px solid var(--border); color: var(--text-primary); background: var(--bg-card); cursor: pointer; }
  button.primary { background: var(--accent); color: white; border-color: var(--accent); }
  button:disabled { opacity: .55; cursor: default; }
  .connection { margin-top: .8rem; color: var(--success, #63c99a); }
  .error { color: var(--error, #f08080); overflow-wrap: anywhere; }
  .status { display: grid; grid-template-columns: 1fr 1fr; gap: 1rem; margin-bottom: 1.2rem; }
  dt { color: var(--text-secondary); font-size: .78rem; margin-bottom: .35rem; }
  dd { margin: 0; font-size: .9rem; }
  .totals { display: grid; grid-template-columns: repeat(4, 1fr); border: 1px solid var(--border); border-radius: 8px; }
  .totals div { padding: 1rem; display: grid; gap: .25rem; }
  .totals strong { font-size: 1.5rem; }
  .totals span, .detail { color: var(--text-secondary); font-size: .78rem; }
  .detail { margin: .8rem 0 0; }
  .explanation p:last-child { margin-bottom: 0; }
  @media (max-width: 640px) {
    .heading { flex-direction: column; }
    .fields, .status { grid-template-columns: 1fr; }
    .totals { grid-template-columns: repeat(2, 1fr); }
    .badge { margin-bottom: .5rem; }
  }
</style>
