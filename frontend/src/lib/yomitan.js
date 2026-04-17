import { get } from 'svelte/store';
import { yomitanPause, isPlaying, setOptimisticPlayState } from './stores.js';
import { firePlayPause } from './api.js';

let bodyObserver = null;
let iframeObserver = null;
let pausedByYomitan = false;
let trackedIframe = null;

function isVisible(iframe) {
  return iframe.style.visibility === 'visible';
}

function onVisibilityChange(iframe) {
  const enabled = get(yomitanPause);
  if (!enabled) return;

  if (isVisible(iframe)) {
    // Yomitan popup appeared — pause if playing
    if (get(isPlaying) && !pausedByYomitan) {
      pausedByYomitan = true;
      setOptimisticPlayState(true);
      firePlayPause(true);
    }
  } else {
    // Yomitan popup hidden — resume if we paused it
    if (pausedByYomitan) {
      pausedByYomitan = false;
      setOptimisticPlayState(false);
      firePlayPause(false);
    }
  }
}

function watchIframe(iframe) {
  if (trackedIframe === iframe) return;
  // Stop watching old iframe
  if (iframeObserver) {
    iframeObserver.disconnect();
  }
  trackedIframe = iframe;
  iframeObserver = new MutationObserver(() => {
    onVisibilityChange(iframe);
  });
  iframeObserver.observe(iframe, { attributes: true, attributeFilter: ['style'] });
}

function scanForYomitan() {
  const iframe = document.querySelector('iframe.yomitan-popup');
  if (iframe) {
    watchIframe(iframe);
  }
}

export function startYomitanObserver() {
  // Initial scan
  scanForYomitan();

  // Watch for the iframe being added to the DOM (Yomitan creates it lazily)
  bodyObserver = new MutationObserver((mutations) => {
    for (const m of mutations) {
      for (const node of m.addedNodes) {
        if (node.nodeType === Node.ELEMENT_NODE) {
          if (node.matches?.('iframe.yomitan-popup')) {
            watchIframe(node);
          } else {
            const inner = node.querySelector?.('iframe.yomitan-popup');
            if (inner) watchIframe(inner);
          }
        }
      }
    }
    // Also re-scan in case it was added somewhere unexpected
    if (!trackedIframe || !document.contains(trackedIframe)) {
      scanForYomitan();
    }
  });
  bodyObserver.observe(document.body, { childList: true, subtree: true });
}

export function stopYomitanObserver() {
  if (bodyObserver) {
    bodyObserver.disconnect();
    bodyObserver = null;
  }
  if (iframeObserver) {
    iframeObserver.disconnect();
    iframeObserver = null;
  }
  if (pausedByYomitan) {
    pausedByYomitan = false;
    firePlayPause(false);
  }
  trackedIframe = null;
}
