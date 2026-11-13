import { svelte } from '@sveltejs/vite-plugin-svelte'
import { defineConfig } from 'vite'

// Tauri loads the dev server on a fixed port; 5173 belongs to XkyHold web.
export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: { port: 5174, strictPort: true },
  build: { target: 'safari16', outDir: 'dist' },
})
