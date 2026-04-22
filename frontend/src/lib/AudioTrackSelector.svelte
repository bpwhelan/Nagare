<script>
  import { audioTracks, selectedAudioTrackIndex, audioTrackResolution, showAudioTrackModal, showToast, applyAudioTracksPayload } from './stores.js';
  import { selectAudioTrack } from './api.js';

  $: visible = $audioTracks.length > 1;
  $: selectedTrack = $audioTracks.find(t => t.index === $selectedAudioTrackIndex) || null;

  $: label = selectedTrack
    ? selectedTrack.display_title || selectedTrack.language || `Track ${selectedTrack.index}`
    : 'No audio track';

  async function handleChange(event) {
    const value = event.currentTarget.value;
    if (value === '__modal__') {
      showAudioTrackModal.set(true);
      return;
    }
    const streamIndex = Number(value);
    try {
      const result = await selectAudioTrack(streamIndex);
      if (result?.ok && result.audio_tracks) {
        applyAudioTracksPayload(result.audio_tracks);
      } else if (result?.error) {
        showToast('error', result.error);
      }
    } catch (e) {
      showToast('error', e.message || 'Failed to select audio track');
    }
  }
</script>

{#if visible}
  <div class="audio-track-picker">
    <span class="audio-track-label">Audio</span>
    <select value={$selectedAudioTrackIndex ?? ''} on:change={handleChange}>
      {#each $audioTracks as track}
        <option value={track.index}>
          {track.display_title || track.language || `Track ${track.index}`}
        </option>
      {/each}
      <option value="__modal__">Preview tracks...</option>
    </select>
  </div>
{/if}

<style>
  .audio-track-picker {
    display: flex;
    align-items: center;
    gap: 0.45rem;
    min-width: 0;
  }

  .audio-track-label {
    font-size: 0.8rem;
    color: var(--text-dim);
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }

  .audio-track-picker select {
    min-width: min(20rem, 40vw);
    max-width: 100%;
    padding: 0.35rem 0.5rem;
    border-radius: 6px;
    border: 1px solid var(--border);
    background: var(--bg-card);
    color: var(--text-primary);
  }

  @media (max-width: 768px) {
    .audio-track-picker {
      width: 100%;
      flex-direction: column;
      align-items: flex-start;
    }

    .audio-track-picker select {
      min-width: 0;
      width: 100%;
    }
  }
</style>
