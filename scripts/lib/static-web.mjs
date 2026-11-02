import { spawnSync } from 'node:child_process';
import { readFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { extname, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const webRoot = fileURLToPath(new URL('../../apps/web/', import.meta.url));
const vite = fileURLToPath(
  new URL('../../node_modules/vite/bin/vite.js', import.meta.url),
);
const mime = {
  '.html': 'text/html',
  '.js': 'text/javascript',
  '.css': 'text/css',
  '.json': 'application/json',
  '.svg': 'image/svg+xml',
};

/**
 * Runs the real Vite build of the web app into `outDir` with
 * `VITE_ARTIFREE_SERVER` set to `server` (also when empty, so that
 * apps/web/.env* files cannot change the result). Returns the finished
 * process; the caller checks `status`.
 */
export function buildStaticWeb(outDir, server) {
  return spawnSync(
    process.execPath,
    [vite, 'build', '--outDir', outDir, '--emptyOutDir'],
    {
      cwd: webRoot,
      env: { ...process.env, VITE_ARTIFREE_SERVER: server },
      encoding: 'utf8',
      timeout: 170_000,
    },
  );
}

/**
 * Serves the files of `directory` on `port` like any static host would: no
 * proxy, no API, so the page must reach its server across origins.
 */
export async function serveStatic(directory, port) {
  const root = resolve(directory);
  const server = createServer(async (request, response) => {
    try {
      const pathname = decodeURIComponent(
        new URL(request.url ?? '/', 'http://localhost').pathname,
      );
      const file = resolve(
        root,
        `.${pathname === '/' ? '/index.html' : pathname}`,
      );
      if (!file.startsWith(`${root}${sep}`)) {
        response.writeHead(403).end();
        return;
      }
      const content = await readFile(file);
      response.writeHead(200, {
        'content-type': `${mime[extname(file)] ?? 'application/octet-stream'}; charset=utf-8`,
      });
      response.end(content);
    } catch {
      response.writeHead(404).end();
    }
  });
  await new Promise((done) => server.listen(port, '127.0.0.1', done));
  return {
    origin: `http://127.0.0.1:${port}`,
    close: () => new Promise((done) => server.close(done)),
  };
}
