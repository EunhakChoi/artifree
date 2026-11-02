import { defineConfig, loadEnv } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';
import { fileURLToPath } from 'node:url';
import { parseServerUrl } from './src/artifacts/server-url';

// The page calls the API on its own origin; Vite forwards /v1 to the server so
// the browser needs no CORS access to the API. Previews load from the
// server's separate content origin directly.
const api = process.env.ARTIFREE_API_URL ?? 'http://127.0.0.1:7410';
const proxy = { '/v1': { target: api, changeOrigin: false } };

// A static build can carry a default API server (decision 0028). Refuse a bad
// address when the config loads (build, dev server, and preview alike) so a
// broken page is never published. The page checks it again.
function checkDefaultServer(mode: string): void {
  const env = loadEnv(
    mode,
    fileURLToPath(new URL('.', import.meta.url)),
    'VITE_',
  );
  const raw = env['VITE_ARTIFREE_SERVER'];
  if (raw === undefined || raw.trim() === '') return;
  const checked = parseServerUrl(raw);
  if (!checked.ok) {
    throw new Error(`VITE_ARTIFREE_SERVER is not usable. ${checked.reason}`);
  }
}

export default defineConfig(({ mode }) => {
  checkDefaultServer(mode);
  return {
    plugins: [react(), tailwindcss()],
    base: '/',
    server: { proxy },
    preview: { proxy },
    resolve: {
      alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) },
    },
  };
});
