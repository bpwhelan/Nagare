import { mount } from 'svelte';
import Companion from './Companion.svelte';
import { getSettings } from './runtime.js';
import { matchesSite } from './settings.js';
import './companion.css';

let app;
let host;
function launch(settingsOnly = false) {
  if (app) { app.openSettings(); return; }
  host = document.createElement('div');
  host.id = 'nagare-companion';
  // No styles or markup enter the player document beyond this single host.
  host.style.cssText = 'all:initial!important;position:fixed!important;inset:0!important;z-index:2147483000!important;pointer-events:none!important;font:14px/1.5 "Segoe UI",system-ui,sans-serif!important;color:#f0edf8!important;text-align:left!important;color-scheme:dark!important;';
  const root = host.attachShadow({ mode: 'open' });
  const style = document.createElement('style');
  style.textContent = NAGARE_STYLES;
  root.append(style);
  document.documentElement.append(host);
  app = mount(Companion, { target: root, props: { settingsOnly } });
  for (const event of ['keydown', 'keyup', 'click', 'dblclick']) {
    root.addEventListener(event, e => e.stopPropagation());
  }
  // Jellyfin enters fullscreen on its player container. Moving the host into
  // that container keeps the panel in the fullscreen top layer.
  function placeHost() {
    const fullscreen = document.fullscreenElement;
    const parent = fullscreen && !/^(VIDEO|AUDIO|IFRAME)$/.test(fullscreen.tagName)
      ? fullscreen : document.documentElement;
    if (host.parentNode !== parent) parent.append(host);
  }
  document.addEventListener('fullscreenchange', placeHost);
  placeHost();
}

GM_registerMenuCommand('Nagare: open companion / settings', () => launch(true));
const settings = getSettings();
if (location.origin !== new URL(settings.serverUrl).origin && settings.sites.some(pattern => matchesSite(location.origin, pattern))) {
  launch();
}
