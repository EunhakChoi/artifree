import assert from 'node:assert/strict';
import { execFile, spawnSync } from 'node:child_process';
import { once } from 'node:events';
import { readFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const root = fileURLToPath(new URL('../../', import.meta.url));
const manifest = JSON.parse(
  await readFile(new URL('../../package.json', import.meta.url), 'utf8'),
);
function cli(args) {
  const result = spawnSync(
    process.execPath,
    ['apps/cli/dist/main.js', ...args],
    { cwd: root, encoding: 'utf8', timeout: 10_000 },
  );
  if (result.error) throw result.error;
  return result;
}

test('CLI version emits only the requested version', () => {
  const result = cli(['--version']);
  assert.equal(result.status, 0);
  assert.equal(result.stdout, `${manifest.version}\n`);
  assert.equal(result.stderr, '');
});

test('CLI JSON matches the live Rust descriptor', () => {
  const rust = spawnSync(
    'cargo',
    ['run', '--quiet', '--locked', '-p', 'artifree-server', '--', '--describe'],
    { cwd: root, encoding: 'utf8', timeout: 120_000 },
  );
  if (rust.error) throw rust.error;
  assert.equal(rust.status, 0, rust.stderr);
  const result = cli(['describe', '--json']);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stderr, '');
  const descriptor = JSON.parse(result.stdout);
  assert.deepEqual(descriptor, JSON.parse(rust.stdout));
  assert.equal(descriptor.http_api, true);
  assert.equal(descriptor.mcp_api, true);
  assert.equal(descriptor.server_framework, 'axum');
  assert.equal(descriptor.async_runtime, 'tokio');
});

test('CLI list without paging options prints every page', async () => {
  // A stand-in server with two pages: a real one pages only after 500.
  const artifact = (title, index) => ({
    id: `art_${String(index).repeat(32)}`,
    kind: 'app',
    title,
    pinned: false,
    current_revision: 1,
    metadata_revision: 1,
    deleted_at_ms: null,
    created_at_ms: index,
    updated_at_ms: index,
  });
  const pages = new Map([
    [null, { artifacts: [artifact('One', 1)], next_page_token: 'second' }],
    ['second', { artifacts: [artifact('Two', 2)], next_page_token: null }],
  ]);
  const queries = [];
  const server = createServer((request, response) => {
    const { searchParams } = new URL(request.url ?? '/', 'http://stub');
    queries.push(Object.fromEntries(searchParams));
    response.setHeader('content-type', 'application/json');
    response.end(JSON.stringify(pages.get(searchParams.get('page_token'))));
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  try {
    const address = server.address();
    assert.ok(address !== null && typeof address === 'object');
    const env = {
      ...process.env,
      ARTIFREE_URL: `http://127.0.0.1:${address.port}`,
      ARTIFREE_TOKEN: 'f'.repeat(64),
    };
    delete env.ARTIFREE_TOKEN_FILE;
    const { stdout } = await promisify(execFile)(
      process.execPath,
      ['apps/cli/dist/main.js', 'artifact', 'list', '--search', 'o'],
      { cwd: root, env, timeout: 10_000 },
    );
    assert.deepEqual(
      JSON.parse(stdout).artifacts.map(({ title }) => title),
      ['One', 'Two'],
    );
    assert.deepEqual(queries, [{ q: 'o' }, { q: 'o', page_token: 'second' }]);
  } finally {
    server.close();
  }
});

test('CLI purge sends nothing without --yes, and cleanup deletes only with --apply', async () => {
  // A stand-in server records every request; the real server is in
  // server.test.mjs.
  const requests = [];
  const digest = 'cd'.repeat(32);
  const server = createServer((request, response) => {
    let body = '';
    request.on('data', (chunk) => {
      body += chunk;
    });
    request.on('end', () => {
      requests.push([
        request.method,
        request.url,
        request.headers['idempotency-key'],
        body === '' ? undefined : JSON.parse(body),
      ]);
      response.setHeader('content-type', 'application/json');
      if (request.url.endsWith('/purge'))
        response.end(
          JSON.stringify({
            artifact_id: `art_${'1'.repeat(32)}`,
            metadata_revision: 3,
            purged_at_ms: 5,
            replayed: false,
          }),
        );
      else if (request.method === 'GET')
        response.end(JSON.stringify({ blobs: 2, bytes: 40, digest }));
      else
        response.end(JSON.stringify({ blobs: 2, bytes: 40, replayed: false }));
    });
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  try {
    const address = server.address();
    assert.ok(address !== null && typeof address === 'object');
    const env = {
      ...process.env,
      ARTIFREE_URL: `http://127.0.0.1:${address.port}`,
      ARTIFREE_TOKEN: 'f'.repeat(64),
    };
    delete env.ARTIFREE_TOKEN_FILE;
    // Asynchronous: the stand-in server lives in this process, so a
    // blocking spawn would stop it from answering.
    const run = async (args) => {
      try {
        const { stdout, stderr } = await promisify(execFile)(
          process.execPath,
          ['apps/cli/dist/main.js', ...args],
          { cwd: root, env, timeout: 10_000 },
        );
        return { status: 0, stdout, stderr };
      } catch (error) {
        return {
          status: error.code,
          stdout: error.stdout,
          stderr: error.stderr,
        };
      }
    };
    const id = `art_${'1'.repeat(32)}`;
    const unconfirmed = await run([
      'artifact',
      'purge',
      id,
      '--base-metadata',
      '2',
    ]);
    assert.equal(unconfirmed.status, 2);
    assert.equal(unconfirmed.stdout, '');
    assert.match(unconfirmed.stderr, /--yes/);
    assert.deepEqual(requests, [], 'an unconfirmed purge sent a request');

    const purged = await run([
      'artifact',
      'purge',
      id,
      '--base-metadata',
      '2',
      '--yes',
    ]);
    assert.equal(purged.status, 0, purged.stderr);
    assert.equal(JSON.parse(purged.stdout).metadata_revision, 3);
    assert.deepEqual(
      requests.map(([method, url]) => [method, url]),
      [['POST', `/v1/artifacts/${id}/purge`]],
    );

    requests.length = 0;
    const dry = await run(['content', 'cleanup']);
    assert.deepEqual(JSON.parse(dry.stdout), {
      applied: false,
      blobs: 2,
      bytes: 40,
      digest,
    });
    assert.deepEqual(requests, [
      ['GET', '/v1/content/unreferenced', undefined, undefined],
    ]);

    // The apply reviews the set, then deletes exactly that set under a key;
    // stdout is the machine-readable report, the reviewed set is on stderr.
    requests.length = 0;
    const applied = await run(['content', 'cleanup', '--apply']);
    assert.equal(applied.status, 0, applied.stderr);
    assert.deepEqual(JSON.parse(applied.stdout), {
      applied: true,
      blobs: 2,
      bytes: 40,
      replayed: false,
    });
    assert.match(applied.stderr, new RegExp(`Reviewed set ${digest}`));
    assert.equal(requests.length, 2);
    assert.deepEqual(requests[0], [
      'GET',
      '/v1/content/unreferenced',
      undefined,
      undefined,
    ]);
    const [method, url, key, body] = requests[1];
    assert.deepEqual(
      [method, url, body],
      ['DELETE', '/v1/content/unreferenced', { base_digest: digest }],
    );
    assert.match(key, /^[A-Za-z0-9_.:-]{8,128}$/);

    // A set reviewed earlier and a key chosen by the caller are sent as given,
    // without another dry run: this is how a lost response is retried.
    requests.length = 0;
    const retried = await run([
      'content',
      'cleanup',
      '--apply',
      '--base-digest',
      digest,
      '--idempotency-key',
      'clean-retry-1',
    ]);
    assert.equal(retried.status, 0, retried.stderr);
    assert.deepEqual(requests, [
      [
        'DELETE',
        '/v1/content/unreferenced',
        'clean-retry-1',
        { base_digest: digest },
      ],
    ]);

    // The retry options mean nothing to a dry run, so nothing is sent.
    requests.length = 0;
    const misplaced = await run([
      'content',
      'cleanup',
      '--base-digest',
      digest,
    ]);
    assert.equal(misplaced.status, 2);
    assert.equal(misplaced.stdout, '');
    assert.deepEqual(requests, []);
  } finally {
    server.close();
  }
});

test('CLI help says the inventory does not probe a server', () => {
  const result = cli(['--help']);
  assert.equal(result.status, 0);
  assert.match(result.stdout, /Usage:/);
  assert.match(result.stdout, /does not probe a server/);
  assert.equal(result.stderr, '');
});

for (const args of [
  ['serve'],
  ['describe', 'extra'],
  ['--json'],
  ['--unknown'],
  ['--help', '--version'],
  ['--help', '--json'],
  ['describe', '--version'],
  ['artifact', 'unknown'],
  ['artifact', 'list', 'extra'],
  ['artifact', '--help', 'list'],
]) {
  test(`CLI rejects ${args.join(' ')} without success output`, () => {
    const result = cli(args);
    assert.equal(result.status, 2);
    assert.equal(result.stdout, '');
    assert.ok(
      result.stderr.trim().length > 0,
      'Invalid input needs an actionable diagnostic.',
    );
  });
}

test('CLI job commands send the right requests and refuse bad usage before sending anything', async () => {
  const requests = [];
  const jobId = `job_${'2'.repeat(32)}`;
  const artifactId = `art_${'1'.repeat(32)}`;
  const job = {
    id: jobId,
    artifact_id: artifactId,
    revision: 3,
    kind: 'verify_revision',
    state: 'queued',
    attempt: 0,
    max_attempts: 3,
    cancel_requested: false,
    created_at_ms: 4,
  };
  const server = createServer((request, response) => {
    let body = '';
    request.on('data', (chunk) => {
      body += chunk;
    });
    request.on('end', () => {
      requests.push([
        request.method,
        request.url,
        request.headers['idempotency-key'],
        body === '' ? undefined : JSON.parse(body),
      ]);
      response.setHeader('content-type', 'application/json');
      if (request.url.endsWith('/cancel'))
        response.end(JSON.stringify({ job, replayed: false }));
      else if (request.method === 'POST')
        response.end(JSON.stringify({ job, replayed: false }));
      else if (request.url.startsWith('/v1/jobs/'))
        response.end(JSON.stringify(job));
      else response.end(JSON.stringify({ jobs: [job] }));
    });
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  try {
    const address = server.address();
    assert.ok(address !== null && typeof address === 'object');
    const env = {
      ...process.env,
      ARTIFREE_URL: `http://127.0.0.1:${address.port}`,
      ARTIFREE_TOKEN: 'f'.repeat(64),
    };
    delete env.ARTIFREE_TOKEN_FILE;
    const run = async (args) => {
      try {
        const { stdout, stderr } = await promisify(execFile)(
          process.execPath,
          ['apps/cli/dist/main.js', 'job', ...args],
          { cwd: root, env, timeout: 10_000 },
        );
        return { status: 0, stdout, stderr };
      } catch (error) {
        return {
          status: error.code,
          stdout: error.stdout,
          stderr: error.stderr,
        };
      }
    };
    for (const bad of [
      ['enqueue', artifactId],
      ['enqueue', artifactId, '--revision', '0'],
      ['enqueue', artifactId, '--revision', '1.5'],
      ['enqueue', '--revision', '1'],
      ['show'],
      ['show', jobId, 'extra'],
      ['list', 'extra'],
      ['list', '--limit', '0'],
      ['cancel'],
      ['frobnicate'],
    ]) {
      const refused = await run(bad);
      assert.equal(refused.status, 2, bad.join(' '));
      assert.equal(refused.stdout, '', bad.join(' '));
    }
    assert.deepEqual(requests, [], 'a bad command line sent a request');

    const enqueued = await run([
      'enqueue',
      artifactId,
      '--revision',
      '3',
      '--idempotency-key',
      'cli-job-key-1',
    ]);
    assert.equal(enqueued.status, 0, enqueued.stderr);
    assert.deepEqual(JSON.parse(enqueued.stdout), { job, replayed: false });
    await run(['show', jobId]);
    await run([
      'list',
      '--artifact',
      artifactId,
      '--limit',
      '5',
      '--page-token',
      'j1.a',
    ]);
    await run(['list']);
    await run(['cancel', jobId, '--idempotency-key', 'cli-job-key-2']);
    assert.deepEqual(requests, [
      [
        'POST',
        `/v1/artifacts/${artifactId}/jobs`,
        'cli-job-key-1',
        { kind: 'verify_revision', revision: 3 },
      ],
      ['GET', `/v1/jobs/${jobId}`, undefined, undefined],
      [
        'GET',
        `/v1/artifacts/${artifactId}/jobs?limit=5&page_token=j1.a`,
        undefined,
        undefined,
      ],
      ['GET', '/v1/jobs', undefined, undefined],
      ['POST', `/v1/jobs/${jobId}/cancel`, 'cli-job-key-2', {}],
    ]);
  } finally {
    server.close();
  }
});

test('CLI data commands refuse a bad command line before sending, and send JSON text as written', async () => {
  const requests = [];
  const artifactId = `art_${'1'.repeat(32)}`;
  const server = createServer((request, response) => {
    let body = '';
    request.on('data', (chunk) => {
      body += chunk;
    });
    request.on('end', () => {
      requests.push([
        request.method,
        request.url,
        request.headers['idempotency-key'],
        body === '' ? undefined : body,
      ]);
      response.setHeader('content-type', 'application/json');
      if (request.headers['idempotency-key'] === 'cli-data-conflict') {
        response.statusCode = 409;
        response.end(
          JSON.stringify({
            error: {
              code: 'data_conflict',
              message: 'stale',
              current_data_version: 4,
            },
          }),
        );
      } else if (request.method === 'GET')
        response.end(
          JSON.stringify({
            artifact_id: artifactId,
            path: '',
            data_version: 0,
            exists: true,
            value: {},
          }),
        );
      else
        response.end(
          JSON.stringify({
            artifact_id: artifactId,
            path: 'a',
            data_version: 1,
            changed: true,
            replayed: false,
          }),
        );
    });
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  try {
    const address = server.address();
    assert.ok(address !== null && typeof address === 'object');
    const env = {
      ...process.env,
      ARTIFREE_URL: `http://127.0.0.1:${address.port}`,
      ARTIFREE_TOKEN: 'f'.repeat(64),
    };
    delete env.ARTIFREE_TOKEN_FILE;
    const run = async (args) => {
      try {
        const { stdout, stderr } = await promisify(execFile)(
          process.execPath,
          ['apps/cli/dist/main.js', 'data', ...args],
          { cwd: root, env, timeout: 10_000 },
        );
        return { status: 0, stdout, stderr };
      } catch (error) {
        return {
          status: error.code,
          stdout: error.stdout,
          stderr: error.stderr,
        };
      }
    };
    const base = ['--path', 'a', '--base-data-version', '0'];
    for (const bad of [
      ['get'],
      ['get', artifactId, 'extra'],
      ['set', artifactId, ...base],
      ['set', artifactId, ...base, '--value', '1', '--value-file', 'x'],
      ['set', artifactId, '--base-data-version', '0', '--value', '1'],
      ['set', artifactId, '--path', 'a', '--value', '1'],
      [
        'set',
        artifactId,
        ...base.slice(0, 2),
        '--base-data-version',
        '-1',
        '--value',
        '1',
      ],
      [
        'set',
        artifactId,
        ...base.slice(0, 2),
        '--base-data-version',
        '1.5',
        '--value',
        '1',
      ],
      ['update', artifactId, ...base],
      ['delete', artifactId, '--path', 'a'],
      ['delete', artifactId, ...base, '--value', '1'],
      ['frobnicate'],
    ]) {
      const refused = await run(bad);
      assert.equal(refused.status, 2, bad.join(' '));
      assert.equal(refused.stdout, '', bad.join(' '));
    }
    assert.deepEqual(requests, [], 'a bad command line sent a request');

    // The text reaches the server exactly: a repeated name and a large
    // integer are what JSON.parse would have changed.
    const text = '{"a":1,"a":2,"big":9007199254740993}';
    const set = await run([
      'set',
      artifactId,
      ...base,
      '--value',
      text,
      '--idempotency-key',
      'cli-data-key-1',
    ]);
    assert.equal(set.status, 0, set.stderr);
    assert.equal(JSON.parse(set.stdout).data_version, 1);
    await run([
      'update',
      artifactId,
      '--path',
      '',
      '--base-data-version',
      '1',
      '--patch',
      '{"x":null}',
      '--idempotency-key',
      'cli-data-key-2',
    ]);
    await run([
      'delete',
      artifactId,
      '--path',
      'a/b',
      '--base-data-version',
      '2',
      '--idempotency-key',
      'cli-data-key-3',
    ]);
    await run(['get', artifactId]);
    await run(['get', artifactId, '--path', 'a/b']);
    const url = `/v1/artifacts/${artifactId}/data`;
    assert.deepEqual(requests, [
      [
        'PUT',
        url,
        'cli-data-key-1',
        `{"base_data_version":0,"path":"a","value":${text}}`,
      ],
      [
        'PATCH',
        url,
        'cli-data-key-2',
        '{"base_data_version":1,"path":"","patch":{"x":null}}',
      ],
      ['DELETE', url, 'cli-data-key-3', '{"path":"a/b","base_data_version":2}'],
      ['GET', url, undefined, undefined],
      ['GET', `${url}?path=a%2Fb`, undefined, undefined],
    ]);

    // The version to write against is printed when the write was stale.
    const stale = await run([
      'set',
      artifactId,
      ...base,
      '--value',
      '1',
      '--idempotency-key',
      'cli-data-conflict',
    ]);
    assert.equal(stale.status, 1);
    assert.equal(stale.stdout, '');
    assert.match(stale.stderr, /^error\[data_conflict\]: stale\n/);
    assert.match(stale.stderr, /^current_data_version=4$/m);
  } finally {
    server.close();
  }
});
