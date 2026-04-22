<script>
  import { audioTracks, selectedAudioTrackIndex, showAudioTrackModal, showToast, applyAudioTracksPayload } from './stores.js';
  import { selectAudioTrack, previewAudioTrack } from './api.js';

  let previewingIndex = null;
  let previewAudioUrl = null;
  let previewAudioEl = null;
  let previewLoading = false;

  function cleanup() {
    if (previewAudioEl) {
      previewAudioEl.pause();
      previewAudioEl = null;
    }
    if (previewAudioUrl) {
      URL.revokeObjectURL(previewAudioUrl);
      previewAudioUrl = null;
    }
    previewingIndex = null;
    previewLoading = false;
  }

  function close() {
    cleanup();
    showAudioTrackModal.set(false);
  }

  async function handlePreview(track) {
    cleanup();
    previewingIndex = track.index;
    previewLoading = true;
    try {
      const result = await previewAudioTrack(track.index);
      if (result.error) {
        showToast('error', result.error);
        previewLoading = false;
        return;
      }
      if (!result.audio_base64) {
        showToast('error', 'No audio returned');
        previewLoading = false;
        return;
      }
      const bytes = atob(result.audio_base64);
      const arr = new Uint8Array(bytes.length);
      for (let i = 0; i < bytes.length; i++) arr[i] = bytes.charCodeAt(i);
      const blob = new Blob([arr], { type: 'audio/ogg; codecs=opus' });
      previewAudioUrl = URL.createObjectURL(blob);
      previewAudioEl = new Audio(previewAudioUrl);
      previewAudioEl.addEventListener('ended', () => { previewingIndex = null; });
      await previewAudioEl.play();
    } catch (e) {
      showToast('error', e.message || 'Preview failed');
    } finally {
      previewLoading = false;
    }
  }

  async function handleSelect(track) {
    cleanup();
    try {
      const result = await selectAudioTrack(track.index);
      if (result?.ok && result.audio_tracks) {
        applyAudioTracksPayload(result.audio_tracks);
      } else if (result?.error) {
        showToast('error', result.error);
        return;
      }
      close();
    } catch (e) {
      showToast('error', e.message || 'Selection failed');
    }
  }
</script>

{#if $showAudioTrackModal}
  <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
  <div class="overlay" on:click|self={close}>
    <div class="modal">
      <div class="modal-header">
        <h2>Select Audio Track</h2>
        <button class="close-btn" on:click={close}>&times;</button>
      </div>
      <p class="modal-hint">
        Multiple audio tracks were found and none match your target language. Please select the correct one.
        Use the preview button to hear a snippet from each track.
      </p>
      <div class="track-list">
        {#each $audioTracks as track}
          {@const isSelected = $selectedAudioTrackIndex === track.index}
          {@const isPreviewing = previewingIndex === track.index}
          <div class="track-card" class:selected={isSelected}>
            <div class="track-info">
              <div class="track-title">
                {track.display_title || `Track ${track.index}`}
              </div>
              <div class="track-meta">
                {#if track.language}
                  <span class="meta-tag">{track.language}</span>
                {/if}
                {#if track.codec}
                  <span class="meta-tag">{track.codec.toUpperCase()}</span>
                {/if}
                {#if track.channels}
                  <span class="meta-tag">{track.channels}</span>
                {/if}
                {#if track.is_default}
                  <span class="meta-tag default">Default</span>
                {/if}
                {#if track.title && track.title !== track.display_title}
                  <span class="meta-tag title-tag">{track.title}</span>
                {/if}
                <span class="meta-tag dim">Stream #{track.index}</span>
              </div>
            </div>
            <div class="track-actions">
              <button
                class="small-btn preview-btn"
                on:click={() => isPreviewing ? cleanup() : handlePreview(track)}
                disabled={previewLoading && previewingIndex !== track.index}
              >
                {isPreviewing ? '■ Stop' : (previewLoading && previewingIndex === track.index ? '...' : '▶ Preview')}
              </button>
              <button
                class="small-btn select-btn"
                class:primary={!isSelected}
                on:click={() => handleSelect(track)}
                disabled={isSelected}
              >
                {isSelected ? 'Selected' : 'Use This Track'}
              </button>
            </div>
          </div>
        {/each}
      </div>
    </div>
  </div>
{/if}

<style>
  .overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.7);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1100;
    padding: 1rem;
  }

  .modal {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: 12px;
    width: 100%;
    max-width: 600px;
    max-height: 80vh;
    overflow-y: auto;
    padding: 1.5rem;
  }

  .modal-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 0.5rem;
  }

  .modal-header h2 {
    margin: 0;
    font-size: 1.2rem;
    color: var(--text-primary);
  }

  .close-btn {
    background: none;
    border: none;
    font-size: 1.5rem;
    color: var(--text-dim);
    cursor: pointer;
    padding: 0.25rem;
    line-height: 1;
  }

  .close-btn:hover {
    color: var(--text-primary);
  }

  .modal-hint {
    color: var(--text-secondary);
    font-size: 0.9rem;
    margin: 0 0 1rem 0;
    line-height: 1.4;
  }

  .track-list {
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }

  .track-card {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 1rem;
    padding: 0.85rem 1rem;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--bg-card);
    transition: border-color 0.15s;
  }

  .track-card.selected {
    border-color: var(--accent);
  }

  .track-info {
    flex: 1;
    min-width: 0;
  }

  .track-title {
    font-size: 0.95rem;
    font-weight: 500;
    color: var(--text-primary);
    margin-bottom: 0.35rem;
  }

  .track-meta {
    display: flex;
    flex-wrap: wrap;
    gap: 0.35rem;
  }

  .meta-tag {
    font-size: 0.75rem;
    padding: 0.15rem 0.45rem;
    border-radius: 4px;
    background: var(--bg-hover, rgba(255, 255, 255, 0.06));
    color: var(--text-secondary);
  }

  .meta-tag.default {
    background: rgba(74, 158, 214, 0.2);
    color: var(--accent);
  }

  .meta-tag.title-tag {
    font-style: italic;
  }

  .meta-tag.dim {
    color: var(--text-dim);
  }

  .track-actions {
    display: flex;
    gap: 0.5rem;
    flex-shrink: 0;
  }

  .small-btn {
    padding: 0.4rem 0.75rem;
    border-radius: 6px;
    border: 1px solid var(--border);
    background: var(--bg-card);
    color: var(--text-primary);
    cursor: pointer;
    font-size: 0.82rem;
    white-space: nowrap;
  }

  .small-btn:hover:not(:disabled) {
    background: var(--bg-hover);
  }

  .small-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .small-btn.primary {
    background: var(--accent);
    color: white;
    border-color: var(--accent);
  }

  .small-btn.primary:hover:not(:disabled) {
    filter: brightness(1.1);
  }

  @media (max-width: 768px) {
    .track-card {
      flex-direction: column;
      align-items: stretch;
    }

    .track-actions {
      justify-content: flex-end;
    }
  }
</style>
