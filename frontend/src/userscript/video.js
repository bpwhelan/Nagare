// Prefer the playing video, then the largest visible video (including paused
// playback). Jellyfin can retain hidden players while navigating between items.
export function getVideoBounds(doc = document, win = window) {
  const viewport = { left: 0, top: 0, width: win.innerWidth, height: win.innerHeight };
  const scope = doc.fullscreenElement || doc;
  const videos = scope.tagName === 'VIDEO' ? [scope] : scope.querySelectorAll('video');
  let best = null;
  for (const video of videos) {
    const style = win.getComputedStyle(video);
    if (style.display === 'none' || style.visibility === 'hidden' || style.opacity === '0') continue;
    const rect = video.getBoundingClientRect();
    const left = Math.max(0, rect.left), top = Math.max(0, rect.top);
    const width = Math.min(viewport.width, rect.right) - left;
    const height = Math.min(viewport.height, rect.bottom) - top;
    if (width <= 0 || height <= 0) continue;
    const playing = !video.paused && !video.ended;
    const area = width * height;
    if (!best || (playing && !best.playing) || (playing === best.playing && area > best.area)) {
      best = { playing, area, bounds: { left, top, width, height } };
    }
  }
  // Audio playback and pages without an on-screen video use the viewport.
  return best?.bounds || viewport;
}

// A Svelte action: follow geometry only while a review is open. The lightweight
// timer also catches SPA player replacements and CSS layout changes without
// observing every mutation in the host page or depending on Jellyfin internals.
export function videoAnchor(node, enabled) {
  const doc = node.ownerDocument, win = doc.defaultView;
  let stop = () => {};
  function update(active) {
    stop();
    stop = () => {};
    if (!active) return;
    let previous = '';
    const position = () => {
      if (doc.visibilityState === 'hidden') return;
      const bounds = getVideoBounds(doc, win);
      const key = JSON.stringify(bounds);
      if (key === previous) return;
      previous = key;
      for (const [property, value] of Object.entries(bounds)) node.style[property] = `${value}px`;
    };
    position();
    const timer = win.setInterval(position, 250);
    const events = ['scroll', 'fullscreenchange', 'play', 'loadedmetadata'];
    win.addEventListener('resize', position);
    for (const event of events) doc.addEventListener(event, position, true);
    stop = () => {
      win.clearInterval(timer);
      win.removeEventListener('resize', position);
      for (const event of events) doc.removeEventListener(event, position, true);
    };
  }
  update(enabled);
  return { update, destroy: () => stop() };
}
