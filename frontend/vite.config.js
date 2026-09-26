import { defineConfig } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'
import { fileURLToPath } from 'node:url'

// https://vite.dev/config/
export default defineConfig({
  plugins: [svelte()],
  build: {
    rollupOptions: {
      input: {
        main: fileURLToPath(new URL('./index.html', import.meta.url)),
        database: fileURLToPath(new URL('./database.html', import.meta.url)),
      },
    },
  },
  server: {
    proxy: {
      '/api': 'http://localhost:9470',
      '/ws': {
        target: 'ws://localhost:9470',
        ws: true,
      },
    },
  },
})
