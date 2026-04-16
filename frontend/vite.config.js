import { defineConfig } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'

// https://vite.dev/config/
export default defineConfig({
  plugins: [svelte()],
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
