// Test harness only. No requests leave this page.
(() => {
  const log = text => { document.getElementById('log').textContent += `${text}\n`; };
  window.addEventListener('error', event => log(`ERROR: ${event.message}`));
  window.addEventListener('unhandledrejection', event => log(`ERROR: ${event.reason}`));
  const values = JSON.parse(sessionStorage.getItem('companion_fixture') || '{}');
  const defaults = { serverUrl: 'http://nagare.test:9470', sites: [location.origin] };
  window.GM_getValue = (key, fallback) => values[key] ?? (key === 'nagare_userscript_settings' ? defaults : fallback);
  window.GM_setValue = (key, value) => {
    values[key] = value;
    sessionStorage.setItem('companion_fixture', JSON.stringify(values));
  };
  window.GM_registerMenuCommand = (_, handler) => { document.getElementById('settings').onclick = handler; };
  let pending = [], queue = [], events = [], id = 100, offline = false, failNext = false, revision = 1;
  const lines = ['窓の外には、静かな町が広がっていた。', 'もう少し、ここで待っていよう。', '今日は何かいいことが起こりそうだ。', '風が少し冷たくなってきたね。', '温かいお茶でも飲みに行こうか。', 'それはいい考えだと思う。', '二人はゆっくりと歩き始めた。', '次の角を曲がったところに、お店がある。', '小さな明かりが、道を照らしていた。']
    .map((text, index) => ({ index, text, start_ms: index * 4000, end_ms: index * 4000 + 3500 }));
  const subtitles = () => ({ lines, count: lines.length, native_lines: [{ index: 0, text: 'The wind has become a little cold.', start_ms: 12000, end_ms: 15500 }], candidates: [{ id: 'ja', label: 'Japanese · SRT', source: 'server', stream_index: 2 }], selected_candidate_id: 'ja', selection_mode: 'auto', subtitle_offset_ms: 0, loading: false });
  const audio = { tracks: [{ index: 1, display_title: 'Japanese', language: 'jpn' }, { index: 3, display_title: 'English', language: 'eng' }], selected_index: 1, resolution: 'auto_language' };
  const state = { active_session_id: 'jellyfin|browser', sessions: [{ id: 'jellyfin|browser', title: 'A quiet afternoon · Episode 1', server_kind: 'jellyfin', device_name: 'Chrome', client: 'Jellyfin Web', user_name: 'Test viewer', is_target_language: true }], now_playing: { history_id: 'jellyfin|episode-one', item_id: 'episode-one', title: 'A quiet afternoon · Episode 1', position_ms: 13000, duration_ms: 900000, is_paused: true, supports_remote_control: true } };
  document.getElementById('fullscreen').onclick = () => document.fullscreenElement
    ? document.exitFullscreen() : document.getElementById('player').requestFullscreen();
  document.getElementById('resize').onclick = () => document.getElementById('player').classList.toggle('compact');
  document.getElementById('replace').onclick = () => {
    const old = document.getElementById('video');
    const replacement = old.cloneNode();
    replacement.style.width = '85%';
    old.replaceWith(replacement);
  };
  document.addEventListener('fullscreenchange', () => {
    document.getElementById('fullscreen').textContent = document.fullscreenElement ? 'Exit fullscreen' : 'Enter fullscreen';
  });
  document.getElementById('offline').onclick = () => { offline = !offline; log(`Offline: ${offline}`); };
  document.getElementById('fail').onclick = () => { failNext = true; log('Next enhancement will fail'); };
  document.getElementById('card').onclick = () => {
    const card = { source: 'pending', event: { note_id: ++id, sentence: '<b>冷たく</b>', fields: { Sentence: { value: '冷たく' } }, model_name: 'Test', tags: [] }, history_id: 'jellyfin|episode-one', matched_line_index: 3, matched_text: lines[3].text, start_ms: 11900, end_ms: 16000, included_line_first: 3, included_line_last: 3, generate_avif: true };
    pending.push(card);
    events.push({ type: 'new_card', new_card: card });
    log(`Received note ${id}`);
  };
  window.GM_xmlhttpRequest = options => {
    const url = new URL(options.url);
    const body = options.data ? JSON.parse(options.data) : {};
    setTimeout(() => {
      if (offline) { options.onerror(); return; }
      let result = { ok: true };
      if (url.pathname === '/api/companion') {
        result = { protocol: 1, epoch: 'fixture', cursor: events.length, subtitle_revision: String(revision),
          events: url.searchParams.has('after') ? events.slice(Number(url.searchParams.get('after'))) : [],
          mining: { audio_start_offset_ms: 100, audio_end_offset_ms: 500, generate_avif: true },
          snapshot: { type: 'full_update', state, subtitles: url.searchParams.get('subtitle_revision') === String(revision) ? null : subtitles(), pending_cards: pending, audio_tracks: audio, enhancement_queue: queue, anki_status: { state: 'connected' } } };
      } else {
        log(`${options.method} ${url.pathname} ${JSON.stringify(body)}`);
        if (url.pathname === '/api/enrich') {
          const card = pending.find(c => c.event.note_id === body.note_id);
          pending = pending.filter(c => c.event.note_id !== body.note_id);
          queue = [{ note_id: body.note_id, state: 'running', message: `Enhancing note #${body.note_id}` }];
          result = { success: true };
          const fail = failNext; failNext = false;
          setTimeout(() => {
            queue = [];
            if (fail && card) pending.push({ ...card, source: 'retry' });
            events.push({ type: 'enhancement_result', enhancement_result: { note_id: body.note_id, success: !fail, message: fail ? 'Simulated Anki failure. Review and retry this card.' : 'Enhanced' } });
          }, 500);
        }
        if (url.pathname === '/api/enrich/skip') pending = pending.filter(c => c.event.note_id !== body.note_id);
        if (url.pathname === '/api/seek') state.now_playing.position_ms = body.position_ms;
        if (url.pathname === '/api/play-pause') state.now_playing.is_paused = body.paused ?? !state.now_playing.is_paused;
        if (url.pathname === '/api/subtitles/select' || url.pathname === '/api/subtitles/offset') { revision++; result.subtitles = subtitles(); }
        if (url.pathname === '/api/audio-tracks/select') { audio.selected_index = body.stream_index; result.audio_tracks = audio; }
        if (url.pathname === '/api/preview-screenshot') result = { image_base64: 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aB9kAAAAASUVORK5CYII=', format: 'png' };
        if (url.pathname === '/api/preview-audio' || url.pathname === '/api/audio-tracks/preview') {
          const buffer = new ArrayBuffer(44 + 8000), view = new DataView(buffer);
          const text = (at, value) => [...value].forEach((char, i) => view.setUint8(at + i, char.charCodeAt(0)));
          text(0, 'RIFF'); view.setUint32(4, 8036, true); text(8, 'WAVEfmt '); view.setUint32(16, 16, true);
          view.setUint16(20, 1, true); view.setUint16(22, 1, true); view.setUint32(24, 8000, true); view.setUint32(28, 16000, true);
          view.setUint16(32, 2, true); view.setUint16(34, 16, true); text(36, 'data'); view.setUint32(40, 8000, true);
          result = { audio_base64: btoa(String.fromCharCode(...new Uint8Array(buffer))), mime_type: 'audio/wav' };
        }
      }
      options.onload({ status: 200, responseText: JSON.stringify(result) });
    }, 30);
    return { abort() {} };
  };
  // Vite ignores changes in dist; use a fresh transform key after every rebuild.
  const bundle = document.createElement('script');
  bundle.src = `/dist/userscript/nagare.user.js?smoke=${Date.now()}`;
  document.head.append(bundle);
})();
