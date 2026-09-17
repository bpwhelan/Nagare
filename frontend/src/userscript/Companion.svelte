<script>
  import { onMount, onDestroy, tick } from 'svelte';
  import SubtitleTimeline from '../lib/SubtitleTimeline.svelte';
  import SessionSelector from '../lib/SessionSelector.svelte';
  import MediaRemote from '../lib/MediaRemote.svelte';
  import EnrichDialog from '../lib/EnrichDialog.svelte';
  import AutoEnhance from '../lib/AutoEnhance.svelte';
  import AudioTrackModal from '../lib/AudioTrackModal.svelte';
  import ToastContainer from '../lib/ToastContainer.svelte';
  import { connected, nowPlayingTitle, positionMs, durationMs, pendingCards, autoApprove, pauseOnEnhance,
    alwaysReuseMiningAssets, showNativeSubtitles, showDownloadButton, ankiStatus, ankiNotice,
    enhancementFlash, enhancementQueue, requestTimelineRecenter, showToast } from '../lib/stores.js';
  import { formatTimeFull } from '../lib/utils.js';
  import { startYomitanObserver, stopYomitanObserver } from '../lib/yomitan.js';
  import { configured, getSettings, saveSettings, requestAt } from './runtime.js';
  import { validateSettings, isTyping, matchesHotkey, matchesSite } from './settings.js';
  import { connectCompanion } from './connection.js';
  import { videoAnchor } from './video.js';

  export let settingsOnly = false;
  let settings = getSettings();
  let visible = settingsOnly || !configured || settings.mode === 'sidebar';
  let showSettings = settingsOnly || !configured;
  let form = { ...settings, sites: settings.sites.join('\n') };
  let formError = '';
  let testMessage = '';
  let testing = false;
  let connectionError = '';
  let disconnect = () => {};
  let pageVisible = document.visibilityState !== 'hidden';
  let panel;
  let previousFocus;

  $: needsAttention = $pendingCards.filter(c => !$autoApprove || c.source === 'retry').length;
  $: reviewEnabled = !settingsOnly && pageVisible && !showSettings && $connected
    && (settings.openOnCard || visible);

  function connect() {
    disconnect();
    if (!settingsOnly) disconnect = connectCompanion(error => { connectionError = error || ''; });
  }

  async function reveal() {
    visible = true;
    await tick();
    requestTimelineRecenter();
  }

  export function openSettings() {
    form = { ...settings, sites: settings.sites.join('\n') };
    formError = testMessage = '';
    showSettings = true;
    reveal();
  }

  export function toggle() {
    if (visible) {
      visible = false;
      if (previousFocus?.isConnected) previousFocus.focus({ preventScroll: true });
    } else {
      previousFocus = document.activeElement;
      reveal().then(() => panel?.focus({ preventScroll: true }));
    }
  }

  function keydown(event) {
    if (isTyping(event) || !matchesHotkey(event, settings.hotkey)) return;
    event.preventDefault();
    event.stopPropagation();
    toggle();
  }

  function save() {
    try {
      const value = validateSettings(form);
      saveSettings(value);
      settings = getSettings();
      settingsOnly = !settings.sites.some(pattern => matchesSite(location.origin, pattern));
      showSettings = settingsOnly;
      visible = true;
      formError = '';
      connect();
      testMessage = settingsOnly ? 'Settings saved. Add this site to enable the Companion here.' : '';
      showToast('success', 'Companion settings saved');
    } catch (error) { formError = error.message; }
  }

  async function testConnection() {
    testing = true;
    formError = testMessage = '';
    try {
      const value = validateSettings(form);
      const result = await requestAt(value.serverUrl, '/api/companion');
      if (result.protocol !== 1) throw new Error('Update Nagare to a version that supports the Companion.');
      testMessage = `Connected. ${result.snapshot.state?.sessions?.length || 0} playback session(s) available.`;
    } catch (error) { formError = error.message; }
    finally { testing = false; }
  }

  function enableThisSite() {
    const sites = form.sites.split('\n').map(s => s.trim()).filter(Boolean);
    if (!sites.includes(location.origin)) sites.push(location.origin);
    form.sites = sites.join('\n');
  }

  function reviewPending() {
    autoApprove.set(false);
    showSettings = false;
  }

  const visibility = () => { pageVisible = document.visibilityState !== 'hidden'; };
  onMount(() => {
    startYomitanObserver();
    window.addEventListener('keydown', keydown, true);
    document.addEventListener('visibilitychange', visibility);
    connect();
  });
  onDestroy(() => {
    disconnect();
    stopYomitanObserver();
    window.removeEventListener('keydown', keydown, true);
    document.removeEventListener('visibilitychange', visibility);
  });
</script>

{#if settings.showLauncher || !configured}
  <button class="launcher" class:left={settings.side === 'left'} on:click={toggle}
    title={`Nagare · ${settings.hotkey}`} aria-label="Toggle Nagare sidebar" aria-expanded={visible}>
    <span>流</span>{#if needsAttention}<b>{needsAttention}</b>{/if}
  </button>
{/if}

<section class="panel" class:left={settings.side === 'left'} class:hidden={!visible}
  style:width={`${settings.width}px`} bind:this={panel} tabindex="-1" aria-label="Nagare Companion">
  <header class="header">
    <div class="brand"><span class="mark">流</span><strong>Nagare</strong><span class="status" class:online={$connected} title={$connected ? 'Connected' : 'Disconnected'}>●</span></div>
    <div class="header-actions">
      <a href={settings.serverUrl} target="_blank" rel="noopener noreferrer" title="Open Nagare" aria-label="Open Nagare">↗</a>
      <button on:click={openSettings} aria-label="Companion settings" title="Settings">⚙</button>
      <button on:click={toggle} aria-label="Hide Nagare sidebar" title={`Hide · ${settings.hotkey}`}>×</button>
    </div>
  </header>

  {#if showSettings}
    <form class="settings" on:submit|preventDefault={save}>
      <div class="section-title"><h2>Companion settings</h2><span>Saved in your userscript manager</span></div>
      <label>Nagare server<input type="url" bind:value={form.serverUrl} placeholder="http://localhost:9470" required /></label>
      <p class="hint">Use the address you normally open for Nagare. Allow the connection if your userscript manager asks.</p>
      <div class="row"><button type="button" on:click={testConnection} disabled={testing}>{testing ? 'Connecting…' : 'Test connection'}</button></div>
      {#if testMessage}<p class="success" role="status">{testMessage}</p>{/if}
      <label>Enable on these sites<textarea rows="3" bind:value={form.sites} spellcheck="false"></textarea></label>
      <p class="hint">One origin per line. * is a wildcard. Default: https://jellyfin.*</p>
      <button type="button" on:click={enableThisSite}>Add this site</button>
      <div class="two-columns">
        <label>On page load<select bind:value={form.mode}><option value="sidebar">Show sidebar</option><option value="hotkey">Hide until hotkey</option></select></label>
        <label>Side<select bind:value={form.side}><option value="right">Right</option><option value="left">Left</option></select></label>
      </div>
      <div class="two-columns">
        <label>Width (pixels)<input type="number" min="320" max="1000" step="10" bind:value={form.width} required /></label>
        <label>Toggle hotkey<input bind:value={form.hotkey} placeholder="Alt+N" required /></label>
      </div>
      <label>Refresh interval (ms)<input type="number" min="500" max="10000" step="250" bind:value={form.pollIntervalMs} required /></label>
      <label class="check"><input type="checkbox" bind:checked={form.showLauncher} />Show floating launcher</label>
      <label class="check"><input type="checkbox" bind:checked={form.openOnCard} />Show card review over video when sidebar is hidden</label>
      <p class="hint">Card review is centered over the current video. Disable this to review cards only while the sidebar is open.</p>
      <div class="section-title"><h2>Mining preferences</h2><span>Changes below apply immediately</span></div>
      <label class="check"><input type="checkbox" bind:checked={$autoApprove} />Automatically confirm enhancements</label>
      <p class="hint">Runs in this visible browser tab, even with the sidebar hidden. Failed cards require manual review. Use one mining tab at a time.</p>
      <label class="check"><input type="checkbox" bind:checked={$pauseOnEnhance} />Pause playback for confirmation</label>
      <label class="check"><input type="checkbox" bind:checked={$alwaysReuseMiningAssets} />Reuse assets for consecutive mines of the same line</label>
      <label class="check"><input type="checkbox" bind:checked={$showNativeSubtitles} />Show native-language subtitles</label>
      <label class="check"><input type="checkbox" bind:checked={$showDownloadButton} />Show subtitle download</label>
      <MediaRemote settingsOnly />
      {#if formError}<p class="error" role="alert">{formError}</p>{/if}
      <div class="settings-footer"><button class="primary" type="submit">Save settings</button>{#if !settingsOnly}<button type="button" on:click={() => showSettings = false}>Back to subtitles</button>{/if}</div>
    </form>
  {:else}
    <div class="session"><SessionSelector /></div>
    {#if connectionError}
      <div class="notice error" role="status">{connectionError}<button on:click={openSettings}>Connection settings</button></div>
    {:else if !$connected}
      <div class="notice" role="status">Connecting to Nagare…</div>
    {/if}
    {#if $ankiStatus.state === 'disconnected'}<div class="notice warning">{$ankiStatus.message || 'Open Anki with AnkiConnect to enhance cards.'}</div>{/if}
    <div class="now-playing"><strong>{$nowPlayingTitle || 'Waiting for playback'}</strong><span>{formatTimeFull($positionMs)} / {formatTimeFull($durationMs)}</span></div>
    <div class="playback"><MediaRemote compact /></div>
    <div class="mining-bar">
      <label class="check"><input type="checkbox" bind:checked={$autoApprove} />Auto-confirm</label>
      {#if needsAttention}<button on:click={reviewPending}>Review {needsAttention} pending</button>{/if}
      {#if $enhancementFlash}<span class="success" role="status">✓ Enhanced</span>{:else if $ankiNotice}<span class="success" role="status">✓ Card received</span>{/if}
    </div>
    {#if $enhancementQueue.length}<div class="notice processing" role="status">{#each $enhancementQueue as item (item.note_id)}<div>{item.message}</div>{/each}</div>{/if}
    <div class="timeline"><SubtitleTimeline /></div>
    <footer class="footer"><span>{settings.hotkey} to toggle</span><button on:click={requestTimelineRecenter}>Jump to current line</button></footer>
  {/if}
  <div class="dialogs">
    {#if visible && !showSettings}<AudioTrackModal />{/if}
  </div>
</section>
<div class="video-dialogs" use:videoAnchor={reviewEnabled && !$autoApprove && $pendingCards.length > 0}>
  <EnrichDialog enabled={reviewEnabled} />
</div>
<AutoEnhance enabled={!settingsOnly && pageVisible && $connected} />
<ToastContainer />
