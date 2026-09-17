# Nagare Companion userscript

The Companion brings Nagare's **Subtitles** workflow into your player tab. It
shares the website's Svelte timeline, enhancement dialog, playback controls,
audio-track picker, and auto-enhancement logic. Changes to these components go
into both frontends when Nagare is built.

## Install and configure

1. Install a userscript manager that provides `GM_xmlhttpRequest`, `GM_getValue`,
   `GM_setValue`, and `GM_registerMenuCommand`, such as Tampermonkey.
2. Update Nagare, then open **Settings → Frontend → Interface → Optional browser
   companion** and click **Install Nagare Companion userscript**. You can also
   open `/userscript/nagare.user.js` on your Nagare server to install it.
3. Open or reload your Jellyfin tab. By default the Companion starts on origins
   matching **`https://jellyfin.*`**. On another origin, use the userscript
   manager's **Nagare: open companion / settings** command, then **Add this site**.
4. Enter your **Nagare server** address, use **Test connection**, and **Save settings**.
   Use the same address as the Nagare website, including any reverse-proxy path.
   The default is `http://localhost:9470`; replace it if Nagare runs elsewhere.
   Allow access to that server when the manager requests it.
5. Play something in Jellyfin and select the appropriate session in the sidebar.

Settings live in the userscript manager, independently of Jellyfin's storage and
Nagare's website preferences. The gear opens settings at any time. `NAGARE_DEFAULTS`
at the top of the installed script is also editable; saved settings take precedence.

### Display and hotkeys

- **Show sidebar** opens on each page load. **Hide until hotkey** starts collapsed.
- **Alt+N** toggles the sidebar by default. Choose another modified letter, digit,
  or function key in settings, such as `Ctrl+Shift+N`. Typing in inputs, editors,
  and IME composition does not trigger it.
- Choose left/right placement and a width from 320–1000 pixels. The sidebar
  overlays the player; it does not change the video's dimensions.
- The optional **流** launcher toggles the panel and shows pending review counts.
- Card confirmation appears centered over the current tab's visible video,
  independently of the sidebar. It follows player resizing, replacement, and
  fullscreen. Pages without a visible video use the center of the viewport.
- **Show card review over video when sidebar is hidden** is enabled by default.
  Receiving a card keeps the sidebar's current visibility. Disable this setting
  to open review only when you choose to show the sidebar with the launcher or hotkey.
- Hiding the sidebar keeps an open review available when the setting above is
  enabled. Opening settings or switching to a background tab suspends review,
  retains the draft, and releases a confirmation pause. Reloading discards unsaved edits.

## Supported workflow

- Live subtitles with automatic scrolling, jump to current line, translations,
  subtitle download, track selection, offset nudges, and timing alignment.
- Session selection and Nagare's remote play/pause, previous/next subtitle, and
  seek controls. These control **Nagare's selected session**. They require the
  player to expose remote control through its media server.
- Anki card receipt, pending count, confirmation, skip, previous/next context,
  sentence and translation editing, clip-range adjustment, audio preview,
  screenshot preview, animated AVIF choice, and consecutive asset reuse.
- Auto-confirm plus processing, completion, and error feedback. It runs while
  this browser tab is visible, even if the sidebar is hidden. Keep one mining
  tab active at a time, including the Nagare website. Failed cards remain pending;
  **Review pending** turns off auto-confirm and opens manual review.
- Hover-pause, click-pause, no line-seek, and Yomitan-pause preferences are in
  Companion settings. For Yomitan popup detection, turn off its Secure Popup
  option, as with Nagare's normal frontend. Dictionary scanning and popup
  placement should be included in your Jellyfin smoke test.

History, session review, and server-wide configuration remain accessible through
the **↗ Open Nagare** link. The Companion is a frontend for playback that Nagare
already tracks; it does not extract subtitles from arbitrary streaming sites.

## Browser considerations

The script uses the manager's cross-origin HTTP requests for the API, with a
default update interval of 750 ms (3 seconds in a background tab). It doesn't
require an HTTPS player page to embed an HTTP Nagare iframe or open an insecure
WebSocket. It retries disconnected requests and retains server result events
between polls. Subtitle files are only transferred when their revision changes.

The metadata matches HTTP/HTTPS pages so sites can be changed in settings. Only
enabled origins mount the panel or contact Nagare; other sites receive the manager
menu command. `@connect *` supports configurable server addresses. You can narrow
the metadata to your own Jellyfin and Nagare hosts if preferred. See the
[Tampermonkey API documentation](https://www.tampermonkey.net/documentation.php)
for manager permissions and supported APIs.

Jellyfin's container fullscreen is supported by moving the Companion into the
fullscreen element. Native video-only fullscreen, cross-origin iframe players,
and picture-in-picture cannot contain the Companion overlay. Use Jellyfin's normal
container fullscreen or windowed player. Very restrictive host-page media/CSS
policies may also affect previews or dictionary extensions; test these on your
actual Jellyfin origin before deciding whether an extension is necessary.

Reinstall from your Nagare server after updates that change the frontend. The
script has no external runtime/CDN dependencies or remote code loader.

## Development and validation

```powershell
cd frontend
npm test
npm run build
```

`npm run build` builds the website and a self-contained
`dist/userscript/nagare.user.js`. Docker and the Rust embedded frontend include
it automatically. `npm run build:userscript` rebuilds only the userscript.

Source is in `frontend/src/userscript/`. `#runtime` resolves to the ordinary
browser adapter for the website and the manager adapter for the userscript.
`/api/companion` serves a compact state snapshot and a bounded, cursor-based
event log; it shares the same backend state and commands as the website.

For browser testing without real Anki mutations, build the userscript, run
`npm run dev`, and open `/tests/companion-smoke.html`. This fixture runs the
production bundle with mocked manager APIs and displays submitted commands.
It can simulate new cards, failed enhancements, disconnections, fullscreen,
video resizing, and player replacement.
It is a development page and is not included in the production build.

### Jellyfin smoke checklist

1. Set the server, test the connection, select the right session, and seek a line.
2. Toggle with the hotkey, type in a field, change width/side, and reload to check persistence.
3. Enter and exit Jellyfin fullscreen and check Yomitan selection/popup behavior.
4. Hide the sidebar, create a card, and check that review is centered over the
   video without opening the sidebar. Resize or enter fullscreen while reviewing;
   add surrounding context, preview audio/image, and confirm.
5. Enable auto-confirm, hide the panel, and create another card.
6. Check that a failed enhancement is visible and can be retried manually.
