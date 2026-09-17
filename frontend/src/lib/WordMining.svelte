<script>
  import { onMount, onDestroy } from 'svelte';
  import { readPreference, writePreference } from '#runtime';
  import { navigate, currentView } from './stores.js';
  import { getConfig } from './api.js';
  import { formatTime } from './utils.js';
  import { miningApi, miningPath, miningRequestId, defaultMiningFields, rankedCandidates, makeMiningDraft, validMiningDraft, draftPayload, sentenceParts, unfamiliarCount } from './wordMining.js';

  export let historyId;
  const base = miningPath(historyId), storageKey = `nagare-miner:${historyId}`;
  let workspace = null, item = null, known = {}, pastJobs = [], job = null;
  let loading = true, analyzing = false, error = '', notice = '', warning = '', loaded = false;
  let activeTerm = '', drafts = {}, draft = null, selected = [], options = { search:'', status:'new', sort:'recommended', common:false, repeated:false, iPlusOne:false };
  let splitMode = 'B', pageSize = 100, padding = {}, settingsOpen = false, confirmOpen = false;
  let decks = [], models = [], modelFields = [], ankiError = '', ankiLoading = false, updating = false, knownBusy = false;
  let tracks = [], tracksLoading = false, mediaError = '', previewBusy = '', audioUrl = '', imageUrl = '', previewVersion = 0;
  let settings = { deck:'', model:'Nagare Vocabulary', fields:{...defaultMiningFields}, tags:[], audio_ordinal:null, screenshot:true, animated:false };
  let tagsText = '', sending = false, pendingRequest = null, pollTimer, disposed = false, modelRequest = 0;
  let recentOpen = false, resultsOpen = false, knownText = '', knownImportOpen = false;

  $: candidates = workspace?.candidates || [];
  $: lines = workspace?.track.lines || [];
  $: active = candidates.find(c => c.term === activeTerm);
  $: filtered = rankedCandidates(workspace, known, options);
  $: recommended = rankedCandidates(workspace, known, {}).filter(c => c.recommended);
  $: newCount = candidates.filter(c => !known[c.term]).length;
  $: selectedCandidates = candidates.filter(c => selected.includes(c.term));
  $: validDraft = validMiningDraft(draft, item?.duration_ms);
  $: running = job?.status === 'running';
  $: completed = job?.cards.filter(c => ['created','skipped','failed'].includes(c.status)).length || 0;
  $: createdCount = job?.cards.filter(c => c.status === 'created').length || 0;
  $: skippedCount = job?.cards.filter(c => c.status === 'skipped').length || 0;
  $: failedCount = job?.cards.filter(c => c.status === 'failed').length || 0;
  $: draft && activeTerm && rememberDraft(activeTerm, draft);
  $: savedState = JSON.stringify({ revision:workspace?.revision, drafts, selected, activeTerm, audio_ordinal:settings.audio_ordinal, pendingRequest });
  $: if (loaded) writePreference(storageKey, JSON.parse(savedState));
  $: savedSettings = JSON.stringify(settings);
  $: if (loaded) writePreference('nagare-miner-settings', JSON.parse(savedSettings));

  function rememberDraft(term, value) { drafts[term] = value; drafts = drafts; }
  function clearPreview() { previewVersion++; audioUrl=''; imageUrl=''; previewBusy=''; }
  function choose(term) {
    clearPreview(); activeTerm=term;
    const candidate=candidates.find(c=>c.term===term);
    draft=candidate ? (drafts[term] || makeMiningDraft(candidate,workspace,known,padding)) : null;
  }
  function applyWorkspace(value, restore = false) {
    workspace=value; item=value.history; splitMode=value.split_mode;
    const saved=restore ? readPreference(storageKey,{}) : {};
    const matching=saved.revision===workspace.revision;
    drafts=matching ? saved.drafts || {} : {};
    selected=matching ? (saved.selected || []).filter(t=>candidates.some(c=>c.term===t) || value.candidates.some(c=>c.term===t)) : [];
    pendingRequest=matching ? saved.pendingRequest || null : null;
    if (matching && saved.audio_ordinal != null) settings={...settings,audio_ordinal:saved.audio_ordinal};
    const term=matching && value.candidates.some(c=>c.term===saved.activeTerm) ? saved.activeTerm : (rankedCandidates(value,known,options)[0] || value.candidates[0])?.term;
    // Use the new workspace directly; reactive declarations update after this handler.
    activeTerm=term || ''; clearPreview();
    const candidate=value.candidates.find(c=>c.term===term);
    draft=candidate ? drafts[term] || makeMiningDraft(candidate,value,known,padding) : null;
  }

  onMount(async () => {
    const saved=readPreference('nagare-miner-settings',{});
    settings={...settings,...saved,fields:{...defaultMiningFields,...saved.fields},audio_ordinal:null};
    tagsText=(settings.tags || []).join(' ');
    try {
      const result=await miningApi(base);
      if(disposed)return;
      item=result.history; known=result.known; pastJobs=result.jobs;
      try { padding=(await getConfig()).mining || {}; } catch { /* defaults are usable */ }
      if(result.workspace) applyWorkspace(result.workspace,true);
      if(pastJobs[0]) await showJob(pastJobs[0].id);
    } catch(e) { error=e.message; }
    finally { loading=false; loaded=true; }
    if(!disposed) { loadAnki(); loadTracks(); }
  });
  onDestroy(()=>{disposed=true;clearTimeout(pollTimer);clearPreview();modelRequest++;});

  async function loadAnki(remap = false) {
    const request=++modelRequest; ankiLoading=true; ankiError='';
    try {
      const result=await miningApi(`/api/word-mining/anki?model=${encodeURIComponent(settings.model)}`);
      if(disposed || request!==modelRequest)return;
      decks=result.decks; models=[...new Set(['Nagare Vocabulary',...result.models])]; modelFields=result.fields;
      if(!decks.includes(settings.deck)) settings={...settings,deck:decks.length===1?decks[0]:''};
      if(remap) {
        const aliases={word:['word','expression','vocabulary','vocab'],reading:['reading','furigana'],meaning:['meaning','definition','glossary'],sentence:['sentence','examplesentence'],audio:['sentenceaudio','audio'],picture:['picture','image','screenshot'],source:['source','sourcename']};
        settings={...settings,fields:Object.fromEntries(Object.entries(aliases).map(([key,names])=>[key,modelFields.find(f=>names.includes(f.toLowerCase().replace(/[ _-]/g,''))) || '']))};
      }
    } catch(e) { if(request===modelRequest) ankiError=e.message; }
    finally { if(request===modelRequest) ankiLoading=false; }
  }
  async function loadTracks() {
    tracksLoading=true; mediaError='';
    try { const result=await miningApi(`${base}/media`); if(disposed)return; tracks=result.tracks;
      if(settings.audio_ordinal==null || !tracks.some(t=>t.ordinal===settings.audio_ordinal)) settings={...settings,audio_ordinal:result.selected};
    } catch(e) { mediaError=e.message; } finally { tracksLoading=false; }
  }
  async function analyze(file = null) {
    analyzing=true; error=''; warning=''; notice='';
    try {
      if(file && file.size>2000000)throw new Error('Choose a subtitle file under 2 MB.');
      const body={split_mode:splitMode};
      if(file) { body.subtitle_text=await file.text(); body.subtitle_name=file.name; }
      const result=await miningApi(`${base}/analyze`,body);
      if(disposed)return;
      known=result.known; applyWorkspace(result.workspace); warning=result.warning || '';
      notice=`Found ${result.workspace.candidates.length.toLocaleString()} words across ${result.workspace.track.lines.length.toLocaleString()} subtitle lines.`;
    } catch(e) { error=e.message; } finally { analyzing=false; }
  }
  async function upload(event) { const file=event.target.files?.[0]; event.target.value=''; if(file) await analyze(file); }
  async function addTerm() {
    try { const result=await miningApi(`${base}/term`,{term:options.search.trim(),revision:workspace.revision});
      const term=options.search.trim(); workspace=result.workspace; activeTerm=term;
      const candidate=workspace.candidates.find(c=>c.term===term); draft=drafts[term] || makeMiningDraft(candidate,workspace,known,padding);
    } catch(e) { error=e.message; }
  }
  function toggle(term) { selected=selected.includes(term)?selected.filter(t=>t!==term):[...selected,term].slice(0,500); }
  function selectVisible() { selected=[...new Set([...selected,...filtered.map(c=>c.term)])].slice(0,500); }
  function selectRecommended() { selected=recommended.slice(0,20).map(c=>c.term); if(selected[0])choose(selected[0]); }
  async function mark(terms,status) {
    knownBusy=true;error='';
    try { const result=await miningApi('/api/word-mining/known',{terms,status},'PUT'); known=result.known; if(status!=='new')selected=selected.filter(t=>!terms.includes(t)); }
    catch(e) {error=e.message;} finally {knownBusy=false;}
  }
  async function syncKnown() {
    knownBusy=true; ankiError='';
    try { const result=await miningApi('/api/word-mining/sync-known',{model:settings.model,field:settings.fields.word}); known=result.known;notice=`Matched ${result.count.toLocaleString()} words in Anki. “In Anki” does not imply the word is mastered.`; }
    catch(e) {ankiError=e.message;} finally {knownBusy=false;}
  }
  async function updateDictionary() {
    updating=true;error='';
    try { const result=await miningApi('/api/word-mining/dictionary',{});notice=`JMdict updated to ${result.date}. Analyze again to refresh meanings.`; }
    catch(e) {error=e.message;} finally {updating=false;}
  }
  function setOccurrence(index) {
    const line=lines[index]; clearPreview();
    draft={...draft,first:index,last:index,sentence:line.text,start:Math.max(0,line.start_ms-(padding.audio_start_offset_ms??100))/1000,
      end:Math.min(item.duration_ms||Infinity,line.end_ms+(padding.audio_end_offset_ms??200))/1000};
  }
  function extend(direction) {
    const first=direction<0?Math.max(0,draft.first-1):draft.first, last=direction>0?Math.min(lines.length-1,draft.last+1):draft.last;
    clearPreview(); draft={...draft,first,last,sentence:lines.slice(first,last+1).map(l=>l.text).join('\n'),
      start:Math.max(0,lines[first].start_ms-(padding.audio_start_offset_ms??100))/1000,
      end:Math.min(item.duration_ms||Infinity,lines[last].end_ms+(padding.audio_end_offset_ms??200))/1000};
  }
  async function preview(kind) {
    const version=++previewVersion; previewBusy=kind;error='';
    try { const result=await miningApi(`${base}/preview`,{kind,start_ms:Math.round(draft.start*1000),end_ms:Math.round(draft.end*1000),audio_ordinal:settings.audio_ordinal});
      if(version!==previewVersion || disposed)return;
      const url=`data:${result.mime};base64,${result.data}`; if(kind==='audio')audioUrl=url;else imageUrl=url;
    } catch(e) {if(version===previewVersion)error=e.message;} finally {if(version===previewVersion)previewBusy='';}
  }
  function prepareBatch(only = null) {
    error='';
    if(only)selected=[only];
    for(const candidate of candidates.filter(c=>selected.includes(c.term))) if(!drafts[candidate.term])drafts[candidate.term]=makeMiningDraft(candidate,workspace,known,padding);
    drafts={...drafts};
    const invalid=selected.find(term=>!validMiningDraft(drafts[term],item.duration_ms));
    if(invalid) {choose(invalid);error=`Check the sentence and clip range for ${invalid}. Clips must be within this file and at most 90 seconds.`;return;}
    settings={...settings,tags:tagsText.split(/\s+/).filter(Boolean)};confirmOpen=true;settingsOpen=true;
  }
  async function sendBatch() {
    sending=true;error='';
    try {
      const body={revision:workspace.revision,settings,cards:selected.map(term=>draftPayload(drafts[term]))};
      const fingerprint=JSON.stringify(body);
      if(!pendingRequest || pendingRequest.fingerprint!==fingerprint) pendingRequest={id:miningRequestId(),fingerprint};
      // Save the request before sending, so a lost HTTP response can be retried safely.
      writePreference(storageKey,{revision:workspace.revision,drafts,selected,activeTerm,audio_ordinal:settings.audio_ordinal,pendingRequest});
      const result=await miningApi(`${base}/jobs`,{...body,request_id:pendingRequest.id});
      job=result.job;confirmOpen=false;pendingRequest=null;resultsOpen=true;schedulePoll();
    } catch(e) {error=e.message;} finally {sending=false;}
  }
  function schedulePoll(delay=1800) {clearTimeout(pollTimer);if(job?.status==='running'&&!disposed)pollTimer=setTimeout(()=>showJob(job.id),delay);}
  async function showJob(id) {
    try {const result=await miningApi(`/api/word-mining/jobs/${encodeURIComponent(id)}`);if(disposed)return;job=result.job;
      if(job.status!=='running') {
        const finished=job.cards.filter(c=>['created','skipped'].includes(c.status));
        known={...known,...Object.fromEntries(finished.map(c=>[c.term,c.status==='created'?'mined':'in_anki']))};
        selected=selected.filter(t=>!finished.some(c=>c.term===t));
      }
      schedulePoll();
    } catch(e) {error=`Could not refresh batch progress: ${e.message}`;schedulePoll(5000);}
  }
  async function jobAction(action) {
    try {const result=await miningApi(`/api/word-mining/jobs/${encodeURIComponent(job.id)}/${action}`,{});
      if(result.job)job=result.job;notice=action==='pause'?'Pausing after the current card finishes.':'Resuming unfinished cards.';schedulePoll(500);
    } catch(e) {error=e.message;}
  }
  function back() {navigate('/');currentView.set('history');}
  function focusDialog(node) {
    const previous = document.activeElement;
    node.focus();
    const onKey = event => {
      if(event.key==='Escape' && !sending) {event.preventDefault();confirmOpen=false;}
      if(event.key!=='Tab')return;
      const items=[...node.querySelectorAll('button:not(:disabled),select:not(:disabled),input:not(:disabled),[tabindex="0"]')];
      const first=items[0],last=items.at(-1);
      if(event.shiftKey && (document.activeElement===first || document.activeElement===node)) {event.preventDefault();last?.focus();}
      else if(!event.shiftKey && (document.activeElement===last || document.activeElement===node)) {event.preventDefault();first?.focus();}
    };
    node.addEventListener('keydown',onKey);
    return {destroy(){node.removeEventListener('keydown',onKey);previous?.focus();}};
  }
</script>

<div class="miner-page">
  <header class="miner-header">
    <button class="back" on:click={back}>← History</button>
    <div class="page-title"><p class="eyebrow">WORD MINING</p><h2>{item?.title || 'Loading file…'}</h2><p class="subtitle">Find your next words in something you’ve already watched or heard.</p></div>
    {#if workspace}<div class="file-stat"><strong>{newCount.toLocaleString()}</strong><span>new words to explore</span></div>{/if}
  </header>
  {#if error}<div class="message error" role="alert">{error}<button on:click={()=>error=''} aria-label="Dismiss error">×</button></div>{/if}
  {#if notice}<p class="message notice" role="status">{notice}</p>{/if}
  {#if warning}<p class="message warning">{warning}</p>{/if}

  {#if loading}<div class="empty"><h3>Opening your mining workspace…</h3></div>
  {:else}
    <section class="source-bar" aria-label="Subtitle source">
      <div><strong>{workspace?.subtitle_name || 'Start with this file’s subtitles'}</strong><span>{workspace ? `${lines.length.toLocaleString()} lines · Sudachi ${workspace.split_mode}` : 'Use the saved track or choose a matching subtitle file.'}</span></div>
      <label class="split-mode">Sudachi <select bind:value={splitMode} disabled={analyzing}><option value="A">A · Short words</option><option value="B">B · Words</option><option value="C">C · Compounds</option></select></label>
      <label class="upload-button" class:disabled={analyzing}>Choose subtitles<input type="file" accept=".srt,.vtt,.ass,.ssa" on:change={upload} disabled={analyzing} /></label>
      <button class:primary={!workspace} on:click={()=>analyze()} disabled={analyzing || running}>{analyzing?'Analyzing…':workspace?'Analyze again':'Analyze saved subtitles'}</button>
    </section>
    {#if analyzing}<p class="loading-hint" role="status">Reading Japanese words with Sudachi… The first analysis downloads Sudachi Core (about 72 MB) and JMdict; later analyses use the local copies.</p>{/if}

    {#if job}
      <section class="batch-status" aria-label="Mining batch progress">
        <div class="batch-heading"><div><p class="eyebrow">{running?'MINING IN PROGRESS':job.status==='complete'?'BATCH COMPLETE':job.status==='paused'?'BATCH PAUSED':'BATCH NEEDS ATTENTION'}</p><strong>{createdCount} created <span>· {skippedCount} already in Anki · {failedCount} failed</span></strong></div>
          <div class="row-actions">{#if running}<button on:click={()=>jobAction('pause')}>Pause after this card</button>{:else if job.status!=='complete'}<button on:click={()=>jobAction('resume')}>Resume unfinished</button>{/if}
          {#if createdCount}<button on:click={()=>navigate(`/review/${encodeURIComponent(job.id)}`)}>Review cards →</button>{/if}<button on:click={()=>resultsOpen=!resultsOpen}>{resultsOpen?'Hide':'Show'} results</button></div>
        </div>
        <progress max={job.cards.length} value={completed}></progress>
        {#if running}<p class="hint">{completed} / {job.cards.length} processed. This batch continues if you close the page.</p>{/if}
        {#if resultsOpen}<div class="results">{#each job.cards as card}<div><span lang="ja">{card.term}</span><span class:failed={card.status==='failed'}>{card.status==='creating'?'Creating…':card.status}{card.note_id?` · #${card.note_id}`:''}</span>{#if card.error}<small>{card.error}</small>{/if}</div>{/each}</div>{/if}
      </section>
    {/if}

    {#if workspace}
      <div class="workspace-toolbar"><div class="row-actions"><button class="recommend" on:click={selectRecommended} disabled={!recommended.length}>✦ Select {Math.min(recommended.length,20)} recommended</button><button on:click={()=>settingsOpen=!settingsOpen}>Anki & audio settings {settingsOpen?'−':'+'}</button></div>
        <span class="hint">Suggestions favor common, recurring words with simpler context.</span></div>
      {#if settingsOpen || confirmOpen}
        <section class="settings-panel" aria-label="Anki and media settings">
          <div class="section-heading"><h3>Destination & media</h3><button on:click={()=>loadAnki()} disabled={ankiLoading}>{ankiLoading?'Connecting…':'Refresh Anki'}</button></div>
          {#if ankiError}<p class="inline-error" role="alert">{ankiError}. Open Anki with AnkiConnect to create cards.</p>{/if}
          <div class="settings-grid">
            <label>Deck<select bind:value={settings.deck}><option value="">Choose a deck</option>{#each decks as deck}<option value={deck}>{deck}</option>{/each}</select></label>
            <label>Note type<select bind:value={settings.model} on:change={(event)=>{settings={...settings,model:event.target.value};loadAnki(true);}}>{#each models.length?models:['Nagare Vocabulary'] as model}<option value={model}>{model}</option>{/each}</select></label>
            <label>Audio track<select bind:value={settings.audio_ordinal} on:change={clearPreview}><option value={null}>{tracksLoading?'Reading tracks…':'Automatic · target language'}</option>{#each tracks as track}<option value={track.ordinal}>{track.title}{track.language?` · ${track.language}`:''}</option>{/each}</select></label>
            <label>Tags<input bind:value={tagsText} placeholder="mining japanese" on:change={()=>settings={...settings,tags:tagsText.split(/\s+/).filter(Boolean)}} /></label>
          </div>
          {#if mediaError}<p class="inline-error">{mediaError} <button on:click={loadTracks}>Retry media access</button></p>{/if}
          {#if !tracksLoading && tracks.length>1 && settings.audio_ordinal==null}<p class="hint">Choose the audio language to use for these cards.</p>{/if}
          <div class="row-actions"><label class="check"><input type="checkbox" bind:checked={settings.screenshot} /> Include screenshot</label><label class="check"><input type="checkbox" bind:checked={settings.animated} disabled={!settings.screenshot} /> Animated clip</label><span class="hint">Audio-only files automatically omit screenshots.</span></div>
          <details><summary>Field mapping</summary><p class="hint">Nagare Vocabulary is created automatically when you send your first batch. For another note type, map its fields below.</p><div class="mapping-grid">{#each Object.keys(defaultMiningFields) as key}<label>{key}<select bind:value={settings.fields[key]}><option value="">{['word','sentence'].includes(key)?'Choose a field (required)':'Omit'}</option>{#each modelFields as field}<option value={field}>{field}</option>{/each}</select></label>{/each}</div></details>
          <div class="known-tools"><button on:click={syncKnown} disabled={knownBusy || ankiLoading || !settings.fields.word}>{knownBusy?'Updating known words…':'Sync words from this Anki note type'}</button><button on:click={()=>knownImportOpen=!knownImportOpen}>Import known words</button><span class="hint">Used for filtering and i+1 suggestions. Anki words are treated as familiar.</span></div>
          {#if knownImportOpen}<label class="known-import">One known word per line<textarea rows="4" bind:value={knownText} placeholder="猫&#10;食べる"></textarea><button on:click={async()=>{await mark(knownText.split(/\r?\n/).map(t=>t.trim()).filter(Boolean),'known');knownText='';}} disabled={knownBusy || !knownText.trim()}>Save known words</button></label>{/if}
        </section>
      {/if}

      <div class="mining-layout">
        <section class="candidate-panel" aria-label="Candidate words">
          <div class="filters"><label class="search-label"><span>Search words, readings, or meanings</span><input type="search" bind:value={options.search} placeholder="Find a word that interests you…" on:input={()=>pageSize=100} /></label>
            <div class="filter-selects"><select aria-label="Word status" bind:value={options.status}><option value="new">New words</option><option value="all">All words</option><option value="known">Known, mined & ignored</option></select><select aria-label="Sort words" bind:value={options.sort}><option value="recommended">Recommended first</option><option value="frequency">Most repeated</option><option value="appearance">Subtitle order</option></select></div>
            <div class="filter-checks"><label><input type="checkbox" bind:checked={options.common} /> Common</label><label><input type="checkbox" bind:checked={options.repeated} /> Repeated</label><label title="A sentence with exactly one unfamiliar content word, based on your known words"><input type="checkbox" bind:checked={options.iPlusOne} /> i+1</label></div>
          </div>
          <div class="list-meta"><span>{filtered.length.toLocaleString()} words</span><button on:click={selectVisible} disabled={!filtered.length}>Select filtered</button><button on:click={()=>selected=[]} disabled={!selected.length}>Clear</button></div>
          <div class="candidate-list">
            {#each filtered.slice(0,pageSize) as candidate (candidate.term)}
              <div class="candidate-row" class:active={activeTerm===candidate.term} class:selected={selected.includes(candidate.term)}>
                <input type="checkbox" aria-label={`Select ${candidate.term}`} checked={selected.includes(candidate.term)} on:change={()=>toggle(candidate.term)} />
                <button class="candidate-main" on:click={()=>choose(candidate.term)}><div class="candidate-title"><strong lang="ja">{candidate.term}</strong><span lang="ja">{candidate.reading}</span>{#if candidate.common}<small>common</small>{/if}</div><p>{candidate.definitions[0]?.meaning.split('\n')[0] || candidate.part_of_speech}</p>
                  <div class="candidate-meta"><span>{candidate.occurrences.length} {candidate.occurrences.length===1?'line':'lines'}</span>{#if known[candidate.term]}<span>{known[candidate.term].replace('_',' ')}</span>{:else if candidate.unfamiliar===1}<span class="i-one">i+1 context</span>{/if}<span>{formatTime(lines[candidate.occurrence]?.start_ms)}</span></div>
                </button>
              </div>
            {:else}<div class="empty small"><h3>{candidates.length?'No words match these filters':'No Japanese words found'}</h3><p>Try another filter or subtitle track.</p>{#if options.search.trim()}<button on:click={addTerm}>Find “{options.search.trim()}” in subtitles</button>{/if}</div>{/each}
            {#if filtered.length>pageSize}<button class="show-more" on:click={()=>pageSize+=100}>Show 100 more</button>{/if}
          </div>
        </section>

        <section class="detail-panel" aria-label="Card preview">
          {#if active && draft}
            <div class="detail-heading"><div><p class="eyebrow">YOUR CARD</p><h3 lang="ja">{active.term}</h3><p class="hint">{active.part_of_speech} · {active.occurrences.length} subtitle occurrences</p></div><button class:selected={selected.includes(activeTerm)} on:click={()=>toggle(activeTerm)}>{selected.includes(activeTerm)?'✓ Selected':'+ Select word'}</button></div>
            <div class="word-controls"><label>Reading<input lang="ja" bind:value={draft.reading} /></label><div class="row-actions"><button on:click={()=>mark([activeTerm],'known')} disabled={knownBusy}>Know it</button><button on:click={()=>mark([activeTerm],'ignored')} disabled={knownBusy}>Ignore</button>{#if known[activeTerm]}<button on:click={()=>mark([activeTerm],'new')} disabled={knownBusy}>Mark new</button>{/if}</div></div>
            {#if active.definitions.length>1}<label class="definition-choice">Dictionary entry<select on:change={(e)=>{const d=active.definitions[Number(e.target.value)];draft={...draft,reading:d.reading,meaning:d.meaning};}}>{#each active.definitions as definition,index}<option value={index}>{definition.reading} · {definition.meaning.split('\n')[0].slice(0,70)}</option>{/each}</select></label>{/if}
            <label class="meaning-label">Meaning<textarea rows="3" bind:value={draft.meaning} placeholder="Add a definition or a personal explanation"></textarea></label>
            <div class="context-heading"><h4>Choose the moment</h4><span>{unfamiliarCount(workspace.line_words.slice(draft.first,draft.last+1).flat(),known)} unfamiliar content words</span></div>
            <div class="occurrences" aria-label="Subtitle occurrences">{#each active.occurrences as index}<button class:chosen={draft.first<=index&&draft.last>=index} on:click={()=>setOccurrence(index)}>{formatTime(lines[index].start_ms)}</button>{/each}</div>
            <div class="scene-sentence" lang="ja">{#each sentenceParts(draft.sentence,active) as part}{#if part.match}<mark>{part.text}</mark>{:else}{part.text}{/if}{/each}</div>
            <div class="context-buttons"><button on:click={()=>extend(-1)} disabled={draft.first===0}>+ Previous line</button><button on:click={()=>extend(1)} disabled={draft.last===lines.length-1}>+ Next line</button><button on:click={()=>setOccurrence(active.occurrences.find(i=>i>=draft.first&&i<=draft.last)??active.occurrences[0])}>Reset context</button></div>
            <label class="sentence-edit">Sentence<textarea lang="ja" rows="3" bind:value={draft.sentence}></textarea></label>
            <div class="clip-controls"><label>Start (seconds)<input type="number" min="0" step="0.01" bind:value={draft.start} on:change={clearPreview} /></label><label>End (seconds)<input type="number" min="0" step="0.01" bind:value={draft.end} on:change={clearPreview} /></label><span>{Number.isFinite(draft.end-draft.start)?(draft.end-draft.start).toFixed(2):'—'}s clip</span></div>
            {#if !validDraft}<p class="inline-error">Enter a sentence and a valid clip within this file (up to 90 seconds).</p>{/if}
            <div class="preview-controls"><button on:click={()=>preview('audio')} disabled={!validDraft||!!previewBusy}>{previewBusy==='audio'?'Loading audio…':'▶ Preview audio'}</button><button on:click={()=>preview('image')} disabled={!validDraft||!!previewBusy}>{previewBusy==='image'?'Loading frame…':'Preview frame'}</button><button class="primary" on:click={()=>prepareBatch(activeTerm)} disabled={!validDraft||running}>Mine this word</button></div>
            {#if audioUrl}<audio controls autoplay src={audioUrl}></audio>{/if}
            {#if imageUrl}<img class="frame-preview" src={imageUrl} alt="Frame from the selected subtitle scene" />{/if}
          {:else}<div class="empty"><h3>A word, in its own context.</h3><p>Choose a word on the left to preview its meaning and scene.</p></div>{/if}
        </section>
      </div>

      <footer class="selection-bar"><div><strong>{selected.length} selected</strong><span>One card per word · maximum 500 per batch</span></div><div class="row-actions"><button on:click={()=>mark(selected,'known')} disabled={!selected.length||knownBusy}>Mark selected known</button><button class="primary" on:click={()=>prepareBatch()} disabled={!selected.length||running||analyzing}>Review {selected.length} {selected.length===1?'card':'cards'} →</button></div></footer>
      <p class="dictionary-credit">Japanese analysis: <a href="https://github.com/WorksApplications/sudachi.rs" target="_blank" rel="noreferrer">Sudachi</a> · Definitions: <a href="https://www.edrdg.org/wiki/index.php/JMdict-EDICT_Dictionary_Project" target="_blank" rel="noreferrer">JMdict</a> {workspace.dictionary_date || ''} © EDRDG, <a href="https://www.edrdg.org/edrdg/licence.html" target="_blank" rel="noreferrer">CC BY-SA 4.0</a> · <button on:click={updateDictionary} disabled={updating}>{updating?'Updating…':'Update dictionary'}</button></p>
    {:else if !analyzing}<div class="empty welcome"><span class="welcome-icon">言</span><h3>Your next deck is already in your history.</h3><p>Analyze the subtitles to discover vocabulary, choose useful scenes,<br />and turn the words you care about into Anki cards.</p><p class="hint">Japanese · Sudachi word analysis · Audio and screenshots · Bulk or individual cards</p></div>{/if}
    {#if pastJobs.length}<details class="past-jobs" bind:open={recentOpen}><summary>Previous batches ({pastJobs.length})</summary>{#each pastJobs as previous}<button on:click={()=>showJob(previous.id)}>{new Date(previous.created_at).toLocaleString()} · {previous.created} / {previous.total} created · {previous.status.replace('_',' ')}</button>{/each}</details>{/if}
  {/if}
</div>

{#if confirmOpen}
  <div class="confirmation-backdrop" role="presentation">
    <div class="confirmation" role="dialog" aria-modal="true" aria-labelledby="batch-title" tabindex="-1" use:focusDialog>
      <p class="eyebrow">READY TO MINE</p><h2 id="batch-title">Create {selected.length} {selected.length===1?'card':'cards'} in Anki</h2>
      <p>Each card uses the sentence and clip you selected from <strong>{item.title}</strong>.</p>
      <label>Destination deck<select bind:value={settings.deck}><option value="">Choose a deck</option>{#each decks as deck}<option value={deck}>{deck}</option>{/each}</select></label>
      <p class="hint">{settings.model} · {settings.fields.audio?'Sentence audio':'No audio field'} · {settings.screenshot?(settings.animated?'Animated screenshots':'Still screenshots'):'No screenshots'}</p>
      <div class="selected-words">{#each selectedCandidates as candidate}<button on:click={()=>{confirmOpen=false;choose(candidate.term);}} lang="ja">{candidate.term}</button>{/each}</div>
      {#if selected.some(term=>!drafts[term]?.meaning.trim())}<p class="warning-text">Some selected words have no definition. You can edit them before creating the batch.</p>{/if}
      <p class="hint">Existing words in this note type are skipped. Progress is saved, and you can pause after the current card or resume unfinished cards.</p>
      {#if error}<p class="inline-error" role="alert">{error}</p>{/if}
      {#if ankiError}<p class="inline-error">{ankiError}</p><button on:click={()=>loadAnki()}>Reconnect Anki</button>{/if}
      <div class="confirmation-actions"><button on:click={()=>confirmOpen=false} disabled={sending}>Back to selection</button><button class="primary" on:click={sendBatch} disabled={sending||!settings.deck||ankiLoading||!!ankiError}>{sending?'Starting batch…':`Create ${selected.length} ${selected.length===1?'card':'cards'}`}</button></div>
    </div>
  </div>
{/if}

<style>
  .miner-page{height:100%;width:100%;overflow:auto;padding:28px 32px 0;max-width:1500px;margin:auto;text-align:left;color:var(--text-primary)}
  .miner-header{display:flex;align-items:center;gap:24px;margin-bottom:28px}.back{align-self:flex-start;white-space:nowrap;background:transparent;border:0;padding:8px 0;color:var(--text-secondary)}.page-title{flex:1;min-width:0}.eyebrow{font-size:10px;font-weight:700;letter-spacing:1.8px;color:#80d9c0;margin:0 0 8px}h2{font-size:25px;line-height:1.3;overflow-wrap:anywhere}.subtitle{font-size:13px;color:var(--text-secondary);margin-top:8px}.file-stat{text-align:right;display:grid;gap:4px}.file-stat strong{font-size:32px;font-weight:500}.file-stat span,.hint{font-size:12px;color:var(--text-secondary);line-height:1.6}
  .source-bar{display:flex;gap:12px;align-items:center;border:1px solid var(--border);border-radius:10px;padding:16px;background:var(--bg-secondary);flex-wrap:wrap}.source-bar>div{flex:1;min-width:180px;display:grid;gap:5px}.source-bar strong{font-size:13px}.source-bar span{font-size:11px;color:var(--text-secondary)}.source-bar button,.upload-button{font-size:12px;white-space:nowrap}.split-mode{font-size:12px;display:flex;align-items:center;gap:8px}.upload-button{position:relative;border:1px solid var(--border);border-radius:6px;padding:9px 12px;cursor:pointer;background:var(--bg-card)}.upload-button input{position:absolute;width:1px;height:1px;opacity:0}.upload-button:focus-within{outline:2px solid #80d9c0}.disabled{opacity:.5;pointer-events:none}.loading-hint{padding:16px;color:#80d9c0;font-size:13px;line-height:1.7}
  .message{padding:12px 16px;border-radius:7px;font-size:13px;line-height:1.6;margin-bottom:15px;display:flex;align-items:center;gap:20px}.message button{margin-left:auto;border:0;background:none;padding:0}.error,.inline-error{color:#ffabab}.error{background:#54262b44;border:1px solid #713e46}.notice{background:#214f3e33;border:1px solid #376657;color:#a4e4cf}.warning,.warning-text{color:#e8c783}.warning{background:#4a3d2544}.inline-error{font-size:12px;line-height:1.6;padding:8px 0;overflow-wrap:anywhere}.warning-text{font-size:12px;line-height:1.7}
  .workspace-toolbar{display:flex;align-items:center;justify-content:space-between;gap:16px;margin:20px 0}.row-actions{display:flex;align-items:center;gap:8px;flex-wrap:wrap}.row-actions button,.workspace-toolbar button{font-size:12px}.recommend{color:#9be6d0;border-color:#4c8273;background:#1b453b55}.settings-panel{border:1px solid var(--border);border-radius:10px;padding:20px;margin:0 0 20px;background:var(--bg-secondary)}.section-heading{display:flex;align-items:center;justify-content:space-between;margin-bottom:15px}.section-heading h3{font-size:15px}.section-heading button{font-size:12px}.settings-grid{display:grid;grid-template-columns:repeat(4,minmax(0,1fr));gap:14px;margin-bottom:18px}.settings-grid label,.mapping-grid label{font-size:11px;color:var(--text-secondary);display:grid;gap:6px}.settings-grid select,.settings-grid input,.mapping-grid select{width:100%;font-size:12px}.mapping-grid{display:grid;grid-template-columns:repeat(4,minmax(0,1fr));gap:12px;margin:12px 0}.mapping-grid label{text-transform:capitalize}.check{font-size:12px;display:flex;align-items:center;gap:6px}.settings-panel details{margin-top:18px;padding-top:14px;border-top:1px solid var(--border)}summary{cursor:pointer;font-size:12px;color:var(--text-secondary)}.settings-panel details p{margin:10px 0}.known-tools{display:flex;gap:10px;align-items:center;flex-wrap:wrap;margin-top:16px}.known-tools button{font-size:12px}.known-import{display:grid;gap:8px;font-size:12px;margin-top:14px}.known-import button{justify-self:start}
  .mining-layout{display:grid;grid-template-columns:minmax(280px,.9fr) minmax(380px,1.3fr);gap:20px;align-items:start}.candidate-panel,.detail-panel{border:1px solid var(--border);border-radius:10px;overflow:hidden;background:var(--bg-secondary)}.filters{padding:16px;border-bottom:1px solid var(--border)}.search-label{display:grid;gap:6px;font-size:11px;color:var(--text-secondary)}.search-label input{width:100%;font-size:13px;padding:10px}.filter-selects{display:flex;gap:8px;margin-top:10px}.filter-selects select{min-width:0;flex:1;font-size:11px;padding:8px}.filter-checks{display:flex;gap:18px;margin-top:13px;font-size:12px;color:var(--text-secondary)}.filter-checks label{display:flex;gap:6px;align-items:center}input[type=checkbox]{accent-color:#80d9c0}.list-meta{display:flex;align-items:center;gap:10px;padding:10px 16px;border-bottom:1px solid var(--border);font-size:11px;color:var(--text-secondary)}.list-meta span{flex:1}.list-meta button{font-size:11px;border:0;padding:2px 0;background:none}.candidate-list{max-height:650px;overflow:auto}.candidate-row{display:flex;align-items:flex-start;gap:12px;padding:14px 16px;border-bottom:1px solid var(--border);border-left:3px solid transparent}.candidate-row>input{margin-top:8px;flex-shrink:0}.candidate-row.active{background:#4a80661b;border-left-color:#80d9c0}.candidate-row.selected{background:#4a806612}.candidate-main{flex:1;min-width:0;text-align:left;border:0;background:none!important;padding:0;border-radius:0}.candidate-title{display:flex;align-items:baseline;gap:10px;flex-wrap:wrap}.candidate-title strong{font-size:21px;font-weight:500}.candidate-title>span{font-size:11px;color:var(--text-secondary)}.candidate-title small{color:#9ac5b7;font-size:9px;border:1px solid #425d53;border-radius:3px;padding:1px 4px;margin-left:auto}.candidate-main>p{font-size:12px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;margin:5px 0 8px;color:var(--text-secondary)}.candidate-meta{display:flex;gap:12px;font-size:10px;color:var(--text-secondary)}.candidate-meta span:last-child{margin-left:auto}.candidate-meta .i-one{color:#8cdbc4}.show-more{margin:12px;width:calc(100% - 24px);font-size:12px}
  .detail-panel{padding:24px;position:sticky;top:0}.detail-heading{display:flex;justify-content:space-between;align-items:center;gap:20px}.detail-heading h3{font-size:40px;font-weight:500;line-height:1.3;margin-bottom:6px}.detail-heading>button{font-size:12px;white-space:nowrap}.detail-heading>button.selected{color:#80d9c0;border-color:#487d6d}.word-controls{display:flex;align-items:flex-end;justify-content:space-between;gap:12px;margin:20px 0 14px}.word-controls>label{max-width:220px;min-width:0;display:grid;gap:6px;font-size:11px;color:var(--text-secondary)}.word-controls input{width:100%}.word-controls .row-actions button{font-size:10px;padding:7px}.meaning-label,.definition-choice,.sentence-edit{display:grid;gap:6px;font-size:11px;color:var(--text-secondary);margin:12px 0}.meaning-label textarea,.sentence-edit textarea{font-size:14px;line-height:1.8;resize:vertical}.definition-choice select{font-size:12px;min-width:0;width:100%}.context-heading{display:flex;justify-content:space-between;gap:12px;align-items:center;padding-top:20px;margin-top:20px;border-top:1px solid var(--border)}.context-heading h4{font-size:13px;font-weight:500}.context-heading span{font-size:10px;color:var(--text-secondary)}.occurrences{display:flex;gap:6px;overflow:auto;padding:12px 0;max-height:130px;flex-wrap:wrap}.occurrences button{font-size:10px;padding:4px 9px;background:transparent}.occurrences button.chosen{background:#37625455;color:#a4e4cf;border-color:#548170}.scene-sentence{font-size:23px;line-height:1.9;white-space:pre-wrap;overflow-wrap:anywhere;padding:16px;background:var(--bg-primary);border-radius:6px;min-height:96px}mark{background:#35614c55;color:#b0edda;border-bottom:1px solid #7abb9f}.context-buttons{display:flex;gap:6px;margin-top:10px;flex-wrap:wrap}.context-buttons button{font-size:10px;padding:6px 10px}.sentence-edit{margin-top:16px}.clip-controls{display:flex;align-items:flex-end;gap:12px;margin-top:18px}.clip-controls label{display:grid;gap:6px;font-size:10px;color:var(--text-secondary);flex:1;min-width:0}.clip-controls input{width:100%;font-size:13px}.clip-controls>span{font-size:11px;color:var(--text-secondary);padding:10px 0;white-space:nowrap}.preview-controls{display:flex;gap:8px;margin-top:16px;flex-wrap:wrap}.preview-controls button{font-size:11px;padding:9px 12px}.preview-controls .primary{margin-left:auto}.frame-preview{width:100%;border-radius:6px;margin-top:16px;max-height:360px;object-fit:contain}audio{width:100%;margin-top:16px;height:36px}
  .selection-bar{position:sticky;bottom:0;margin-top:20px;background:var(--bg-secondary);border:1px solid var(--border);border-radius:8px;padding:16px 20px;display:flex;align-items:center;justify-content:space-between;gap:16px;box-shadow:0 -8px 30px #0003;z-index:2}.selection-bar>div:first-child{display:grid;gap:5px}.selection-bar strong{font-size:14px}.selection-bar span{font-size:11px;color:var(--text-secondary)}.selection-bar button{font-size:12px}.dictionary-credit{font-size:10px;color:var(--text-secondary);line-height:1.8;padding:20px 0}.dictionary-credit a{color:inherit;text-decoration:underline}.dictionary-credit button{border:0;background:none;font-size:10px;padding:0;color:#80d9c0}.empty{display:grid;place-content:center;text-align:center;padding:70px 24px;gap:14px;min-height:250px}.empty h3{font-weight:500;font-size:20px}.empty p{font-size:13px;color:var(--text-secondary);line-height:1.8}.empty.small{padding:30px 20px}.empty.small h3{font-size:16px}.welcome{min-height:400px}.welcome-icon{font-size:45px;color:#80d9c0;margin-bottom:10px}
  .batch-status{border:1px solid #3f6357;background:#25473522;border-radius:10px;padding:18px 20px;margin:20px 0}.batch-heading{display:flex;justify-content:space-between;gap:20px;align-items:center}.batch-heading strong{font-size:14px}.batch-heading strong span{font-weight:400;color:var(--text-secondary)}.batch-status progress{width:100%;height:6px;accent-color:#80d9c0;margin:14px 0 5px}.results{max-height:240px;overflow:auto;margin-top:12px}.results>div{display:grid;grid-template-columns:minmax(80px,.6fr) 1fr;gap:6px;padding:10px;border-top:1px solid var(--border);font-size:12px}.results small{grid-column:1/-1;color:var(--text-secondary);overflow-wrap:anywhere;line-height:1.7}.results .failed{color:#ffabab}.past-jobs{margin:12px 0 26px}.past-jobs button{display:block;margin-top:8px;font-size:12px}
  .confirmation-backdrop{position:fixed;inset:0;z-index:1500;background:#000a;display:grid;place-items:center;padding:20px;backdrop-filter:blur(5px)}.confirmation{background:var(--bg-secondary);border:1px solid var(--border);border-radius:14px;max-width:600px;width:100%;padding:30px;max-height:90vh;overflow:auto;box-shadow:0 20px 80px #0008}.confirmation h2{font-size:23px;margin-bottom:16px}.confirmation>p{font-size:13px;line-height:1.8;margin-bottom:16px}.confirmation>label{display:grid;gap:7px;font-size:12px;margin-bottom:14px}.selected-words{display:flex;gap:7px;flex-wrap:wrap;max-height:160px;overflow:auto;margin:18px 0}.selected-words button{font-size:16px;padding:5px 10px}.confirmation-actions{display:flex;justify-content:flex-end;gap:10px;margin-top:24px}.confirmation-actions button{font-size:12px}button:disabled{opacity:.45;cursor:default}button:focus-visible,a:focus-visible{outline:2px solid #80d9c0;outline-offset:3px}
  @media(min-width:1700px){.candidate-list{max-height:800px}}
  @media(max-width:1000px){.miner-page{padding:22px 20px 0}.settings-grid,.mapping-grid{grid-template-columns:repeat(2,minmax(0,1fr))}.file-stat{display:none}.workspace-toolbar{align-items:flex-start;flex-direction:column}.mining-layout{grid-template-columns:minmax(260px,.9fr) minmax(330px,1.2fr);gap:12px}.detail-panel{padding:18px}.word-controls{align-items:stretch;flex-direction:column}.batch-heading{align-items:flex-start;flex-direction:column}}
  @media(max-width:720px){.miner-page{padding:18px 12px 0}.miner-header{align-items:flex-start;flex-direction:column;gap:12px;margin-bottom:18px}.page-title h2{font-size:22px}.source-bar{padding:12px;gap:10px}.source-bar>div{flex-basis:100%}.source-bar select{max-width:160px}.source-bar button,.upload-button{font-size:11px}.mining-layout{display:flex;flex-direction:column}.candidate-panel,.detail-panel{width:100%}.candidate-list{max-height:330px}.detail-panel{position:static}.word-controls{flex-direction:row}.settings-grid,.mapping-grid{grid-template-columns:1fr 1fr}.settings-grid select,.mapping-grid select,.settings-grid input{font-size:16px}.selection-bar{padding:12px;align-items:flex-start}.selection-bar span{display:none}.selection-bar .row-actions>button:first-child{display:none}.selection-bar .primary{font-size:11px}.selection-bar strong{font-size:12px}.context-heading{align-items:flex-start;flex-direction:column;gap:6px}.confirmation{padding:22px}.confirmation-actions{flex-wrap:wrap}.preview-controls button{font-size:11px}.dictionary-credit{padding-bottom:20px}.clip-controls input{font-size:16px}}
</style>
