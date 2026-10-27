import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';
import { fileURLToPath } from 'node:url';

// The page calls the API on its own origin; Vite forwards /v1 to the server so
// the browser needs no CORS access to the API. Previews load from the
// server's separate content origin directly.
const api = process.env.ARTIFREE_API_URL ?? 'http://127.0.0.1:7410';
const proxy = { '/v1': { target: api, changeOrigin: false } };

export default defineConfig({
  plugins: [react(), tailwindcss()],
  base: './',
  server: { proxy },
  preview: { proxy },
  resolve: {
    alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) },
  },
});
