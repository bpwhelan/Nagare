import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { fileURLToPath } from 'node:url';
import { DEFAULTS } from './src/userscript/settings.js';

const header = `// ==UserScript==
// @name         Nagare Companion
// @namespace    https://github.com/bpwhelan/Nagare
// @version      0.1.3
// @description  Nagare subtitles, context, and Anki enhancement alongside your player.
// @homepageURL  https://github.com/bpwhelan/Nagare
// @match        https://*/*
// @match        http://*/*
// @noframes
// @run-at       document-idle
// @grant        GM_xmlhttpRequest
// @grant        GM_getValue
// @grant        GM_setValue
// @grant        GM_registerMenuCommand
// @connect      *
// ==/UserScript==

// Edit these defaults if you prefer. Saved Companion settings take precedence.
// Only matching sites start the companion; other sites get a manager menu command.
const NAGARE_DEFAULTS = ${JSON.stringify(DEFAULTS, null, 2)};
`;

export default defineConfig({
  plugins: [
    svelte(),
    {
      name: 'inline-companion-css',
      enforce: 'post',
      generateBundle(_, bundle) {
        const css = Object.values(bundle).filter(file => file.type === 'asset' && file.fileName.endsWith('.css'));
        const styles = css.map(file => file.source).join('\n');
        for (const file of css) delete bundle[file.fileName];
        for (const file of Object.values(bundle)) {
          if (file.type === 'chunk') file.code = `${header}\n(function () {\nconst NAGARE_STYLES = ${JSON.stringify(styles)};\n${file.code}\n})();\n`;
        }
      },
    },
  ],
  resolve: { alias: { '#runtime': fileURLToPath(new URL('./src/userscript/runtime.js', import.meta.url)) } },
  build: {
    outDir: 'dist/userscript',
    emptyOutDir: true,
    copyPublicDir: false,
    target: 'es2022',
    lib: { entry: 'src/userscript/main.js', name: 'NagareCompanion', formats: ['iife'], fileName: () => 'nagare.user.js' },
  },
});
