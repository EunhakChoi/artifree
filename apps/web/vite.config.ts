import { defineConfig } from 'vite';
import type { Plugin } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';
import { fileURLToPath } from 'node:url';
import { parseServerUrl } from './src/artifacts/server-url';

// The page calls the API on its own origin; Vite forwards /v1 to the server so
// the browser needs no CORS access to the API. Previews load from the
// server's separate content origin directly.
const api = process.env.ARTIFREE_API_URL ?? 'http://127.0.0.1:7410';
const proxy = { '/v1': { target: api, changeOrigin: false } };

// VITE_ARTIFREE_SERVER sets the server a static build talks to when neither a
// ?server= link nor the tab names one (decision 0028). A value the page would
// refuse stops the build instead of shipping a default that is never used.
function checkDefaultServer(): Plugin {
  return {
    name: 'artifree-default-server',
    configResolved(config) {
      const value: unknown = config.env.VITE_ARTIFREE_SERVER;
      if (typeof value !== 'string' || value.trim() === '') return;
      const result = parseServerUrl(value);
      if (!result.ok)
        throw new Error(
          `VITE_ARTIFREE_SERVER must be an absolute http or https address without credentials, query, or fragment: ${result.reason}.`,
        );
    },
  };
}

export default defineConfig({
  plugins: [checkDefaultServer(), react(), tailwindcss()],
  base: '/',
  server: { proxy },
  preview: { proxy },
  resolve: {
    alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) },
  },
});
