import { get } from 'svelte/store';
import { yomitanPause, yomitanPopupVisible, isPlaying, setOptimisticPlayState } from './stores.js';
import { firePlayPause } from './api.js';

let bodyObserver = null;
let iframeObserver = null;
let pausedByYomitan = false;
let trackedIframe = null;

function isVisible(iframe) {
  if (!iframe || !document.contains(iframe)) return false;

  const style = window.getComputedStyle(iframe);
  return style.visibility !== 'hidden'
    && style.display !== 'none'
    && style.opacity !== '0';
}

function syncPlaybackState(visible = get(yomitanPopupVisible)) {
  const shouldPauseForYomitan = visible && get(yomitanPause);

  if (shouldPauseForYomitan) {
    if (get(isPlaying) && !pausedByYomitan) {
      pausedByYomitan = true;
      setOptimisticPlayState(true);
      firePlayPause(true);
    }
    return;
  }

  if (pausedByYomitan) {
    pausedByYomitan = false;
    setOptimisticPlayState(false);
    firePlayPause(false);
  }
}

function setPopupVisibility(visible) {
  yomitanPopupVisible.set(visible);
  syncPlaybackState(visible);
}

function stopWatchingIframe() {
  if (iframeObserver) {
    iframeObserver.disconnect();
    iframeObserver = null;
  }
  trackedIframe = null;
}

function syncTrackedIframeVisibility() {
  setPopupVisibility(isVisible(trackedIframe));
}

function watchIframe(iframe) {
  if (trackedIframe === iframe) {
    syncTrackedIframeVisibility();
    return;
  }

  stopWatchingIframe();
  trackedIframe = iframe;
  iframeObserver = new MutationObserver(syncTrackedIframeVisibility);
  iframeObserver.observe(iframe, { attributes: true, attributeFilter: ['style', 'class', 'hidden'] });
  syncTrackedIframeVisibility();
}

function scanForYomitan() {
  const iframe = document.querySelector('iframe.yomitan-popup');
  if (iframe) {
    watchIframe(iframe);
    return;
  }

  stopWatchingIframe();
  setPopupVisibility(false);
}

export function startYomitanObserver() {
  if (bodyObserver) return;

  // Initial scan
  scanForYomitan();

  // Watch for the iframe being added to the DOM (Yomitan creates it lazily)
  bodyObserver = new MutationObserver((mutations) => {
    if (trackedIframe && !document.contains(trackedIframe)) {
      stopWatchingIframe();
      setPopupVisibility(false);
    }

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

    if (!trackedIframe) {
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
  stopWatchingIframe();
  setPopupVisibility(false);
}

yomitanPause.subscribe(() => {
  syncPlaybackState();
});

isPlaying.subscribe(() => {
  syncPlaybackState();
});
