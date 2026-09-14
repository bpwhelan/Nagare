<script>
  import { onMount, onDestroy, tick } from 'svelte';
  import { getReviewSession, markCardReviewed, enrichCard, getConfig, previewAudio, previewScreenshot } from './api.js';
  import { autoApprove, currentView, navigate, reviewRevision, enhancementQueue, pendingCards } from './stores.js';
  import { formatTime, audioMimeType, imageMimeType } from './utils.js';
  import { plainText, contextSentence, validRange, makeReviewDraft } from './review.js';

  export let sessionId;
  let review = null, config = {}, loading = true, error = '', notice = '';
  let selectedId = null, filter = 'all', search = '', contextSearch = '';
  let drafts = {}, draft = null, saving = false, marking = false;
  let submitted = {};
  let audioUrl = '', screenshotUrl = '', audio = null, previewing = false, screenshotLoading = false;
  let requestId = 0, previewId = 0, screenshotId = 0, refreshTimer, disposed = false;
  let lastRevision = $reviewRevision, lastQueue = '';

  $: cards = review?.cards ?? [];
  $: lines = review?.track?.lines ?? [];
  $: selected = cards.find(c => c.dialog.event.note_id === selectedId);
  $: selectedIndex = cards.findIndex(c => c.dialog.event.note_id === selectedId);
  $: reviewedCount = cards.filter(c => c.reviewed_at).length;
  $: needsAttention = cards.filter(c => ['failed', 'pending', 'skipped'].includes(c.status)).length;
  $: filtered = cards.filter(c => (filter !== 'unreviewed' || !c.reviewed_at)
    && (filter !== 'attention' || ['failed', 'pending', 'skipped'].includes(c.status))
    && (!search || `${plainText(c.dialog.event.sentence)} ${Object.values(c.dialog.event.fields).map(f => plainText(f.value)).join(' ')}`.toLowerCase().includes(search.toLowerCase())));
  $: inFlight = selected && ($enhancementQueue.some(j => j.note_id === selectedId)
    || ['queued', 'running'].includes(selected.status));
  $: rangeValid = draft && validRange(draft.start, draft.end);
  $: context = lines.map((line, index) => ({ ...line, index })).filter(line => contextSearch
    ? line.text.toLowerCase().includes(contextSearch.toLowerCase())
    : line.index >= Math.max(0, (draft?.first ?? 0) - 3) && line.index <= (draft?.last ?? 0) + 3);
  $: word = selected ? Object.entries(selected.dialog.event.fields)
    .find(([key]) => /^(expression|word|vocabulary|vocab|target.?word)$/i.test(key))?.[1]?.value : '';

  onMount(async () => {
    await load();
    try {
      config = await getConfig();
      if (selected && !draft?.dirty) selectCard(selectedId);
    } catch { /* Saved ranges remain usable without config. */ }
  });
  onDestroy(() => {
    disposed = true; requestId++; clearTimeout(refreshTimer); clearPreviews();
  });

  $: if ($reviewRevision !== lastRevision) {
    lastRevision = $reviewRevision;
    scheduleRefresh();
  }
  $: queueKey = $enhancementQueue.map(j => `${j.note_id}:${j.state}`).join(',');
  $: if (queueKey !== lastQueue) {
    lastQueue = queueKey;
    scheduleRefresh();
  }
  function scheduleRefresh() {
    clearTimeout(refreshTimer);
    refreshTimer = setTimeout(() => load(false), 300);
  }

  async function load(initial = true) {
    const id = ++requestId;
    if (initial) loading = true;
    try {
      const result = await getReviewSession(sessionId);
      if (disposed || id !== requestId) return;
      if (!result.ok) throw new Error(result.error);
      const previousUpdatedAt = review?.cards.find(c => c.dialog.event.note_id === selectedId)?.dialog.updated_at;
      review = result.review;
      for (const card of review.cards) {
        const noteId = card.dialog.event.note_id;
        if (card.status === 'enhanced' && submitted[noteId]) {
          delete drafts[noteId]; delete submitted[noteId];
          if (selectedId === noteId) draft = null;
        }
      }
      error = '';
      if (!selectedId) selectedId = review.cards.find(c => !c.reviewed_at)?.dialog.event.note_id ?? review.cards[0]?.dialog.event.note_id;
      const updatedAt = review.cards.find(c => c.dialog.event.note_id === selectedId)?.dialog.updated_at;
      // Queue/status updates for other cards must not stop a playing preview
      // or reset the context search while the user is reviewing this card.
      if (!draft || (!draft.dirty && previousUpdatedAt !== updatedAt)) selectCard(selectedId);
    } catch (e) { if (id === requestId && !disposed) error = e.message; }
    finally { if (id === requestId && !disposed) loading = false; }
  }

  function selectCard(id) {
    if (draft && selectedId) drafts[selectedId] = draft;
    clearPreviews();
    selectedId = id;
    const card = review?.cards.find(c => c.dialog.event.note_id === id);
    if (!card) { draft = null; return; }
    if (drafts[id]?.dirty) draft = drafts[id];
    else {
      draft = makeReviewDraft(card, review.track.lines, config.mining);
      const field = config.anki?.fields?.sentence_translation;
      draft.translation = field ? card.dialog.event.fields[field]?.value ?? '' : '';
    }
    contextSearch = ''; notice = '';
  }
  function step(delta) {
    const index = cards.findIndex(c => c.dialog.event.note_id === selectedId);
    const next = cards[index + delta];
    if (next) selectCard(next.dialog.event.note_id);
  }
  function changed() { draft.dirty = true; draft = draft; clearPreviews(); }
  function include(first, last) {
    if (first == null || last == null || !lines[first] || !lines[last]) return;
    draft.first = first; draft.last = last;
    draft.sentence = contextSentence(lines, first, last, selected.dialog.event.sentence);
    draft.start = Math.max(0, lines[first].start_ms - (config.mining?.audio_start_offset_ms ?? 100)) / 1000;
    draft.end = (lines[last].end_ms + (config.mining?.audio_end_offset_ms ?? 500)) / 1000;
    changed();
  }
  function selectLine(index) {
    if (draft.first == null || draft.last == null) include(index, index);
    else if (index < draft.first) include(index, draft.last);
    else if (index > draft.last) include(draft.first, index);
    else if (index === draft.first && index < draft.last) include(index + 1, draft.last);
    else if (index === draft.last && index > draft.first) include(draft.first, index - 1);
  }
  function discard() {
    delete drafts[selectedId]; draft = null; selectCard(selectedId);
  }
  function back() { navigate('/'); currentView.set('history'); }
  function clearPreviews() {
    previewId++; screenshotId++;
    audio?.pause(); audio = null;
    if (audioUrl) URL.revokeObjectURL(audioUrl);
    if (screenshotUrl) URL.revokeObjectURL(screenshotUrl);
    audioUrl = ''; screenshotUrl = ''; previewing = false; screenshotLoading = false;
  }
  function mediaUrl(base64, type) {
    return URL.createObjectURL(new Blob([Uint8Array.from(atob(base64), c => c.charCodeAt(0))], { type }));
  }
  async function playAudio() {
    if (!rangeValid || previewing) return;
    const id = ++previewId;
    previewing = true; error = '';
    try {
      if (!audioUrl) {
        const result = await previewAudio(Math.round(draft.start * 1000), Math.round(draft.end * 1000), review.session.history_id, selectedId);
        if (id !== previewId || disposed) return;
        if (result.error || !result.audio_base64) throw new Error(result.error || 'No audio returned');
        audioUrl = mediaUrl(result.audio_base64, audioMimeType(result.format, result.mime_type));
      }
      // The native controls remain available if the browser blocks autoplay.
      await tick();
      if (audio) { audio.currentTime = 0; await audio.play(); }
    } catch (e) { if (id === previewId) error = e.message; }
    finally { if (id === previewId) previewing = false; }
  }
  async function showScreenshot() {
    const id = ++screenshotId;
    screenshotLoading = true;
    try {
      const result = await previewScreenshot(Math.round((Number(draft.start) + Number(draft.end)) * 500), review.session.history_id);
      if (id !== screenshotId || disposed) return;
      if (result.error || !result.image_base64) throw new Error(result.error || 'No image returned');
      if (screenshotUrl) URL.revokeObjectURL(screenshotUrl);
      screenshotUrl = mediaUrl(result.image_base64, imageMimeType(result.format, result.mime_type));
    } catch (e) { if (id === screenshotId) error = e.message; }
    finally { if (id === screenshotId) screenshotLoading = false; }
  }
  async function save() {
    if (!rangeValid || saving || inFlight) return;
    const id = selectedId, payload = { ...draft };
    saving = true; error = ''; notice = '';
    try {
      const result = await enrichCard({
        noteId: id, sentence: payload.sentence,
        translation: config.anki?.fields?.sentence_translation ? payload.translation : null,
        startMs: Math.round(payload.start * 1000), endMs: Math.round(payload.end * 1000),
        generateAvif: payload.generateAvif, itemId: review.session.history_id,
        matchedLineIndex: selected.dialog.matched_line_index,
        includedLineFirst: payload.first, includedLineLast: payload.last,
      });
      if (!result.success) throw new Error(result.error || 'Could not queue enhancement');
      submitted[id] = payload;
      pendingCards.update(cards => cards.filter(c => c.event.note_id !== id));
      review = { ...review, cards: review.cards.map(c => c.dialog.event.note_id === id ? { ...c, status: 'queued', reviewed_at: null } : c) };
      // Keep the submitted draft visible until the confirmed note snapshot arrives.
      if (selectedId === id) notice = 'Updating in Anki. Review it once enhancement finishes.';
    } catch (e) { error = e.message; }
    finally { saving = false; }
  }
  async function mark(reviewed = true) {
    if (!selected || marking || inFlight || (reviewed && draft.dirty)) return;
    const id = selectedId;
    marking = true; error = '';
    try {
      const result = await markCardReviewed(id, reviewed);
      if (!result.ok) throw new Error(result.error);
      review = { ...review, cards: review.cards.map(c => c.dialog.event.note_id === id ? { ...c, reviewed_at: reviewed ? new Date().toISOString() : null } : c) };
      if (reviewed) {
        const ordered = [...cards.slice(selectedIndex + 1), ...cards.slice(0, selectedIndex)];
        const next = ordered.find(c => !c.reviewed_at);
        if (next) selectCard(next.dialog.event.note_id);
        else notice = 'Every card in this session has been reviewed.';
      }
    } catch (e) { error = e.message; }
    finally { marking = false; }
  }
  function keyboard(event) {
    if (event.target.closest('input,textarea,select,button,a,audio') || event.ctrlKey || event.metaKey || event.altKey) return;
    if (event.key === 'j') { event.preventDefault(); step(1); }
    if (event.key === 'k') { event.preventDefault(); step(-1); }
    if (event.key === 'r') { event.preventDefault(); mark(); }
    if (event.code === 'Space') { event.preventDefault(); playAudio(); }
  }
  const statusLabel = { pending: 'Needs enhancement', skipped: 'Skipped', failed: 'Needs attention', queued: 'Queued', running: 'Enhancing', enhanced: 'Enhanced', existing: 'Has media' };
</script>

<svelte:window on:keydown={keyboard} />

<div class="review-page">
  <header class="review-header">
    <div class="heading"><button class="back" on:click={back}>← History</button><div><p class="eyebrow">SESSION REVIEW</p><h2>{review?.session.title || 'Loading session…'}</h2>
      {#if review}<p class="subtitle">{new Date(review.session.created_at).toLocaleString()} · {lines.length} subtitle lines · {cards.length} cards</p>{/if}</div></div>
    <label class="auto"><input type="checkbox" bind:checked={$autoApprove} /><span>Automatically enhance new cards<small>While this browser stays open</small></span></label>
  </header>
  {#if error}<div class="message error" role="alert">{error}<button on:click={() => load(false)}>Refresh</button></div>{/if}
  {#if notice}<div class="message" role="status">{notice}</div>{/if}
  {#if loading && !review}<div class="empty">Loading your cards…</div>
  {:else if review}
    <div class="progress-row"><span><strong>{reviewedCount}</strong> of {cards.length} reviewed</span><progress max={cards.length || 1} value={reviewedCount}></progress><span>{needsAttention} need enhancement</span></div>
    {#if review.session.imported}<p class="import-hint">Earlier cards were grouped by title. Their available subtitle history is shown here.</p>{/if}
    <div class="workspace">
      <aside class="card-list" aria-label="Session cards">
        <div class="list-tools"><input aria-label="Search session cards" type="search" placeholder="Search cards…" bind:value={search} />
          <select aria-label="Filter cards" bind:value={filter}><option value="all">All cards ({cards.length})</option><option value="unreviewed">Unreviewed ({cards.length - reviewedCount})</option><option value="attention">Need enhancement ({needsAttention})</option></select></div>
        <div class="card-scroll">
          {#each filtered as card, index (card.dialog.event.note_id)}
            <button class="card-row" class:selected={selectedId === card.dialog.event.note_id} aria-current={selectedId === card.dialog.event.note_id ? 'true' : undefined} on:click={() => selectCard(card.dialog.event.note_id)}>
              <div class="row-meta"><span>{String(index + 1).padStart(2, '0')} · {formatTime(card.dialog.start_ms)}</span><span class:done={card.reviewed_at}>{card.reviewed_at ? '✓ Reviewed' : statusLabel[card.status]}</span></div>
              <p lang="ja">{plainText(card.dialog.event.sentence)}</p>
              {#if drafts[card.dialog.event.note_id]?.dirty}<small>Unsaved edits</small>{/if}
            </button>
          {:else}<p class="empty">No cards match this filter.</p>{/each}
        </div>
        <div class="list-footer">J / K to move · Space to listen · R to review</div>
      </aside>
      {#if selected && draft}
        <section class="editor" aria-label="Card editor">
          <div class="editor-top"><span class="eyebrow">CARD {selectedIndex + 1} / {cards.length}</span><span class="state" class:done={selected.reviewed_at}>{selected.reviewed_at ? '✓ Reviewed' : statusLabel[selected.status]}</span>
            <div class="arrows"><button aria-label="Previous card" on:click={() => step(-1)} disabled={selectedIndex <= 0}>←</button><button aria-label="Next card" on:click={() => step(1)} disabled={selectedIndex >= cards.length - 1}>→</button></div></div>
          <div class="editor-scroll">
            {#if selected.last_error}<p class="card-error">{selected.last_error}</p>{/if}
            <div class="note-preview"><p class="eyebrow">{selected.dialog.event.model_name} · #{selectedId}</p>{#if word}<h3 lang="ja">{plainText(word)}</h3>{/if}<p class="sentence-preview" lang="ja">{plainText(draft.sentence)}</p></div>
            <label class="field">Sentence<textarea rows="3" lang="ja" bind:value={draft.sentence} on:input={changed} disabled={inFlight || saving}></textarea><small>Existing Anki markup is preserved when you add context.</small></label>
            {#if config.anki?.fields?.sentence_translation}<label class="field">Translation<textarea rows="2" bind:value={draft.translation} on:input={changed} disabled={inFlight || saving}></textarea></label>{/if}
            <div class="audio-section"><div class="section-title"><h3>Audio clip</h3><span>{rangeValid ? (draft.end - draft.start).toFixed(2) : '0.00'} s</span></div>
              <div class="time-inputs"><label>Start (seconds)<input type="number" min="0" step="0.05" bind:value={draft.start} on:input={changed} disabled={inFlight || saving} /></label><span>→</span><label>End (seconds)<input type="number" min="0" step="0.05" bind:value={draft.end} on:input={changed} disabled={inFlight || saving} /></label></div>
              {#if !rangeValid}<p class="card-error">End must be after start.</p>{/if}
              <div class="preview-actions"><button on:click={playAudio} disabled={!rangeValid || previewing}>{previewing ? 'Preparing audio…' : '▶ Listen to clip'}</button><button on:click={showScreenshot} disabled={!rangeValid || screenshotLoading}>{screenshotLoading ? 'Loading frame…' : 'Preview frame'}</button></div>
              {#if audioUrl}<audio bind:this={audio} src={audioUrl} controls preload="auto"></audio>{/if}
              {#if screenshotUrl}<img class="frame" src={screenshotUrl} alt="Preview from the selected clip" />{/if}
              <label class="check"><input type="checkbox" bind:checked={draft.generateAvif} on:change={changed} disabled={inFlight || saving} /> Generate animated image for video</label>
            </div>
            <details class="note-fields"><summary>All saved Anki fields <span>{Object.keys(selected.dialog.event.fields).length}</span></summary><dl>{#each Object.entries(selected.dialog.event.fields).sort((a,b) => a[1].order - b[1].order) as [name, field]}<dt>{name}</dt><dd>{plainText(field.value) || 'Empty'}</dd>{/each}</dl><p class="tags">{selected.dialog.event.tags.join(' · ')}</p></details>
          </div>
          <footer class="editor-actions"><div>{#if draft.dirty}<span class="unsaved">Unsaved changes</span><button class="text-button" on:click={discard} disabled={saving || inFlight}>Discard</button>{:else}<span class="quiet">{selected.reviewed_at ? 'Review complete' : 'Ready to review'}</span>{/if}</div>
            <div class="action-buttons"><button on:click={save} disabled={!rangeValid || saving || inFlight}>{saving || inFlight ? 'Enhancing…' : 'Save to Anki'}</button>{#if selected.reviewed_at}<button on:click={() => mark(false)} disabled={marking || inFlight}>Mark unreviewed</button>{:else}<button class="primary" on:click={() => mark()} disabled={draft.dirty || marking || inFlight || saving}>Reviewed & next ✓</button>{/if}</div></footer>
        </section>
        <aside class="context-panel" aria-label="Subtitle context"><div class="context-header"><p class="eyebrow">SUBTITLE CONTEXT</p><h3>A little more of the story</h3><p>Add surrounding lines to expand the sentence and audio together.</p><input type="search" aria-label="Search subtitle context" placeholder="Find a line in this SRT…" bind:value={contextSearch} /></div>
          <div class="context-actions"><button disabled={draft.first == null || draft.first <= 0 || inFlight || saving} on:click={() => include(draft.first - 1, draft.last)}>+ Previous</button><button disabled={draft.last == null || draft.last >= lines.length - 1 || inFlight || saving} on:click={() => include(draft.first, draft.last + 1)}>+ Next</button><button disabled={inFlight || saving || selected.dialog.matched_line_index == null} on:click={() => include(selected.dialog.matched_line_index, selected.dialog.matched_line_index)}>Reset</button></div>
          <div class="context-scroll">{#each context as line (line.index)}<button class="context-line" class:included={draft.first != null && line.index >= draft.first && line.index <= draft.last} disabled={inFlight || saving} on:click={() => selectLine(line.index)}><span class="line-time">{formatTime(line.start_ms)}<span>{draft.first != null && line.index >= draft.first && line.index <= draft.last ? '✓ Included' : '+ Add context'}</span></span><p lang="ja">{line.text}</p></button>{:else}<p class="empty">{lines.length ? 'No matching subtitle lines.' : 'No subtitle snapshot is available. You can still edit the sentence and audio times.'}</p>{/each}</div>
        </aside>
      {:else}<div class="empty">No cards in this session yet.</div>{/if}
    </div>
  {/if}
</div>

<style>
  .review-page { text-align:left; }
  .review-page { --review-bg:#11151b; --review-panel:#191f28; --review-border:#303844; --review-text:#e6eaf0; --review-muted:#a3afbd; --review-accent:#80d9c0; color:var(--review-text); background:var(--review-bg); height:100%; min-height:0; display:flex; flex-direction:column; font-size:14px; }
  button,input,select,textarea { font:inherit; color:var(--review-text); background:#202833; border-color:var(--review-border); }
  button:hover { background:#2a3642; } button:disabled { opacity:.45; cursor:default; } button:focus-visible,input:focus-visible,select:focus-visible,textarea:focus-visible { outline:2px solid var(--review-accent); outline-offset:2px; }
  .review-header { display:flex; align-items:center; justify-content:space-between; gap:20px; padding:22px 28px 18px; border-bottom:1px solid var(--review-border); }
  .heading { display:flex; align-items:flex-start; gap:22px; min-width:0; } .back { white-space:nowrap; background:transparent; }
  .eyebrow { font-size:10px; font-weight:700; letter-spacing:1.6px; color:var(--review-muted); margin:0 0 7px; }
  h2 { font-size:21px; line-height:1.35; margin:0; color:var(--review-text); letter-spacing:0; } h3 { font-size:15px; font-weight:600; margin:0; }
  .subtitle { color:var(--review-muted); font-size:12px; margin-top:6px; } .auto { display:flex; align-items:center; gap:10px; flex-shrink:0; font-size:12px; } input[type=checkbox] { accent-color:var(--review-accent); width:16px; height:16px; }
  .auto small { display:block; color:var(--review-muted); margin-top:3px; font-size:11px; }
  .progress-row { display:flex; align-items:center; gap:20px; padding:12px 28px; font-size:12px; color:var(--review-muted); } .progress-row strong { color:var(--review-accent); } progress { flex:1; height:5px; border:0; border-radius:5px; accent-color:var(--review-accent); } progress::-webkit-progress-bar { background:#2d3742; border-radius:5px; } progress::-webkit-progress-value { background:var(--review-accent); border-radius:5px; }
  .workspace { flex:1; min-height:0; display:grid; grid-template-columns:minmax(210px, .85fr) minmax(340px, 1.7fr) minmax(255px, 1fr); border-top:1px solid var(--review-border); }
  .card-list,.editor,.context-panel { min-height:0; min-width:0; display:flex; flex-direction:column; } .card-list { border-right:1px solid var(--review-border); } .editor { background:var(--review-panel); } .context-panel { border-left:1px solid var(--review-border); }
  .list-tools { padding:16px; display:grid; gap:8px; } input[type=search],select { width:100%; font-size:12px; padding:9px 11px; } .card-scroll,.editor-scroll,.context-scroll { overflow:auto; min-height:0; flex:1; }
  .card-row { display:block; width:100%; text-align:left; border:0; border-left:3px solid transparent; border-bottom:1px solid #262e38; border-radius:0; padding:16px; background:transparent; } .card-row.selected { background:#213331; border-left-color:var(--review-accent); } .row-meta { display:flex; justify-content:space-between; gap:8px; font-size:10px; color:var(--review-muted); margin-bottom:8px; } .card-row p { font-size:14px; line-height:1.8; display:-webkit-box; -webkit-line-clamp:3; -webkit-box-orient:vertical; overflow:hidden; } .card-row small { color:#f3cd84; font-size:10px; }
  .done { color:var(--review-accent); } .list-footer { color:var(--review-muted); font-size:10px; padding:12px 16px; border-top:1px solid var(--review-border); }
  .editor-top { display:flex; align-items:center; gap:12px; padding:15px 24px; border-bottom:1px solid var(--review-border); } .editor-top .eyebrow { margin:0; } .state { font-size:10px; padding:3px 8px; border:1px solid var(--review-border); border-radius:20px; } .arrows { display:flex; margin-left:auto; gap:5px; } .arrows button { padding:4px 10px; }
  .editor-scroll { padding:24px; } .note-preview { padding:24px; background:#121921; border:1px solid var(--review-border); border-radius:10px; margin-bottom:22px; } .note-preview h3 { font-size:27px; color:var(--review-accent); margin:14px 0 10px; } .sentence-preview { font-size:20px; line-height:1.9; white-space:pre-wrap; overflow-wrap:anywhere; }
  .field { display:grid; gap:8px; margin:18px 0; font-size:12px; color:var(--review-muted); } textarea { width:100%; resize:vertical; line-height:1.7; font-size:14px; background:#131a22; } .field small { font-size:10px; } .audio-section { border-top:1px solid var(--review-border); margin-top:24px; padding-top:20px; } .section-title { display:flex; justify-content:space-between; align-items:center; } .section-title span { color:var(--review-accent); font-variant-numeric:tabular-nums; }
  .time-inputs { display:flex; align-items:center; gap:12px; margin:15px 0; } .time-inputs label { flex:1; font-size:11px; color:var(--review-muted); min-width:0; } .time-inputs input { width:100%; margin-top:7px; font-variant-numeric:tabular-nums; } .preview-actions { display:flex; flex-wrap:wrap; gap:8px; } .preview-actions button { font-size:12px; } audio { width:100%; margin-top:14px; height:36px; } .frame { width:100%; max-height:250px; object-fit:contain; margin-top:14px; border-radius:8px; } .check { display:flex; align-items:center; gap:8px; margin:18px 0 0; font-size:11px; color:var(--review-muted); }
  .note-fields { margin-top:24px; border-top:1px solid var(--review-border); padding-top:18px; font-size:12px; } summary { cursor:pointer; color:var(--review-muted); } summary span { float:right; } dl { margin-top:16px; } dt { color:var(--review-accent); font-size:10px; margin-top:14px; } dd { margin:5px 0 0; white-space:pre-wrap; overflow-wrap:anywhere; line-height:1.7; } .tags { color:var(--review-muted); font-size:10px; margin-top:16px; }
  .editor-actions { border-top:1px solid var(--review-border); padding:16px 20px; display:flex; justify-content:space-between; align-items:center; gap:12px; flex-wrap:wrap; } .action-buttons { display:flex; gap:8px; } .action-buttons button { font-size:12px; } .primary { background:var(--review-accent); border-color:var(--review-accent); color:#0d2822; } .primary:hover { background:#9be7d1; } .text-button { border:0; background:transparent; padding:5px 8px; font-size:11px; } .unsaved { color:#f3cd84; font-size:11px; } .quiet { color:var(--review-muted); font-size:11px; }
  .context-header { padding:22px 20px 12px; } .context-header p:not(.eyebrow) { color:var(--review-muted); font-size:12px; line-height:1.7; margin:10px 0 14px; } .context-actions { display:flex; gap:6px; padding:0 20px 15px; } .context-actions button { font-size:10px; padding:6px 8px; } .context-scroll { padding:0 12px 20px; } .context-line { width:100%; display:block; background:transparent; border-color:transparent; border-radius:7px; padding:14px 12px; margin:3px 0; text-align:left; } .context-line.included { background:#20332e; border-color:#36594d; } .line-time { display:flex; justify-content:space-between; color:var(--review-muted); font-size:10px; } .line-time span { color:var(--review-accent); } .context-line p { font-size:14px; line-height:1.9; margin-top:7px; }
  .message { padding:12px 28px; background:#213c34; color:#c4efdf; display:flex; align-items:center; justify-content:space-between; font-size:12px; } .message.error,.card-error { color:#ffb7b7; background:#3e2629; } .card-error { padding:12px; font-size:12px; margin:0 0 12px; border-radius:6px; } .empty { padding:32px; color:var(--review-muted); font-size:13px; text-align:center; } .import-hint { color:var(--review-muted); font-size:11px; padding:0 28px 12px; }
  @media(max-width:1150px) { .workspace { grid-template-columns:220px minmax(300px, 1fr); } .context-panel { grid-column:2; max-height:340px; border-top:1px solid var(--review-border); } .card-list { grid-row:1 / span 2; } .review-header { padding:16px; } .heading { gap:12px; } .editor-scroll { padding:18px; } }
  @media(max-width:760px) { .review-page { height:auto; min-height:100%; overflow:visible; } .review-header { align-items:flex-start; flex-direction:column; gap:14px; } .heading { flex-direction:column; } h2 { font-size:19px; } .progress-row { padding:12px 16px; gap:10px; } .progress-row span:last-child { display:none; } .workspace { display:flex; flex-direction:column; } .card-list { max-height:245px; border-bottom:1px solid var(--review-border); } .list-tools { display:flex; padding:12px; } .card-scroll { display:flex; overflow-x:auto; flex-shrink:0; } .card-row { min-width:220px; width:220px; } .list-footer { display:none; } .editor-scroll { overflow:visible; } .editor-actions { position:sticky; bottom:0; background:var(--review-panel); z-index:1; } .context-panel { max-height:none; } .context-scroll { max-height:400px; } .note-preview { padding:18px; } .sentence-preview { font-size:18px; } .auto { font-size:12px; } }
  @media(min-width:761px) and (max-width:1150px) {
    .review-page { overflow:auto; } .workspace { flex:none; }
    .editor { min-height:650px; } .context-panel { min-height:340px; }
  }
  @media(max-width:760px) {
    .review-page,.workspace,.editor,.card-list,.context-panel { display:block; }
    .card-list { max-height:none; }
    .card-scroll { height:135px; flex:none; }
    .card-row { flex:0 0 220px; }
    .editor-scroll { display:block; flex:none; height:auto; }
    .context-scroll { overflow:auto; }
  }
</style>
