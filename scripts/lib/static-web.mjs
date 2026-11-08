import { spawnSync } from 'node:child_process';
import { readFile } from 'node:fs/promises';
import { createServer, request as httpRequest } from 'node:http';
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
 * Forwards one request to the API at `apiUrl`, like the Vite dev server's
 * proxy, so a page served next to it stays same-origin with the API.
 */
export function forwardToApi(apiUrl, request, response) {
  const target = new URL(request.url ?? '/', apiUrl);
  const upstream = httpRequest(
    target,
    {
      method: request.method,
      headers: { ...request.headers, host: target.host },
    },
    (reply) => {
      response.writeHead(reply.statusCode ?? 502, reply.headers);
      reply.pipe(response);
    },
  );
  upstream.on('error', () => {
    if (!response.headersSent) response.writeHead(502);
    response.end();
  });
  // A client that leaves ends the upstream request, as a real proxy does;
  // otherwise the API keeps every abandoned change stream and its slot.
  response.on('close', () => {
    if (!response.writableFinished) upstream.destroy();
  });
  request.pipe(upstream);
}

/**
 * Serves the files of `directory` like any static host would. Without
 * `apiUrl` there is no proxy and no API, so the page must reach its server
 * across origins; with it, `/v1/` is forwarded there (see `forwardToApi`).
 * Port 0 picks a free port, which `origin` reports. The directory may be
 * filled after the call.
 */
export async function serveStatic(directory, port, { apiUrl } = {}) {
  const root = resolve(directory);
  const server = createServer(async (request, response) => {
    if (apiUrl !== undefined && (request.url ?? '').startsWith('/v1/')) {
      forwardToApi(apiUrl, request, response);
      return;
    }
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
    origin: `http://127.0.0.1:${server.address().port}`,
    close: () => new Promise((done) => server.close(done)),
  };
}
