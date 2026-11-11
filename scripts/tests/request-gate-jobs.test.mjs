import assert from 'node:assert/strict';
import { afterEach, beforeEach, describe, it, mock } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The Jobs tab's requests go through the request gate (decision 0007): its
// reads queue behind the page's other reads, and its Verify and Cancel writes
// are sent at once (decision 0055, rules 14 and 15). The gate finds each route
// by its own path, so every check runs under a plain address and under
// prefixes of the server address.

// The gate is web code without DOM or React imports; bundle it as it ships.
const bundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL('../../apps/web/src/artifacts/request-gate.ts', import.meta.url),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { RequestGate, requestKind } = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);

beforeEach(() => mock.timers.enable({ apis: ['setTimeout', 'Date'] }));
afterEach(() => mock.timers.reset());

/** Lets settled promises run their continuations. */
async function settle() {
  for (let round = 0; round < 10; round += 1) await Promise.resolve();
}

/** A page that stays visible, so the gate never waits for it. */
const visibility = {
  visible: () => true,
  subscribe: () => () => {},
};

/**
 * A server the test answers by hand. `urls` lists each request as it was
 * sent, as `METHOD url`; `answer(index, response)` settles one of them.
 */
function server() {
  const sent = [];
  return {
    urls: () => sent.map(({ method, url }) => `${method} ${url}`),
    send: (input, init) =>
      new Promise((resolve) => {
        sent.push({
          method: init?.method ?? 'GET',
          url: String(input),
          resolve,
        });
      }),
    answer(index, response = new Response('{}')) {
      sent[index].resolve(response);
    },
  };
}

/**
 * The addresses the requests are sent under: a plain one, a path prefix, and a
 * prefix that looks like an API version.
 */
const BASES = [
  'http://127.0.0.1:4100',
  'https://api.example/prefix',
  'https://api.example/v1/api',
];

/** The Jobs tab's reads (decision 0055): each is a read the gate queues. */
const TAB_READS = [
  {
    name: 'the first page of an artifact job list',
    method: 'GET',
    path: '/v1/artifacts/a1/jobs?limit=50',
  },
  {
    name: 'a later page of an artifact job list',
    method: 'GET',
    path: '/v1/artifacts/a1/jobs?limit=50&page_token=j1.x',
  },
  { name: 'one job', method: 'GET', path: '/v1/jobs/j1' },
  { name: 'the background status', method: 'GET', path: '/v1/background' },
];

/** The Jobs tab's writes (decision 0055): each is sent at once. */
const TAB_WRITES = [
  {
    name: 'a Verify revision request',
    method: 'POST',
    path: '/v1/artifacts/a1/jobs',
  },
  { name: 'a Cancel request', method: 'POST', path: '/v1/jobs/j1/cancel' },
];

describe('The Jobs tab reads are classified as reads', () => {
  for (const base of BASES) {
    for (const request of TAB_READS) {
      it(`${request.name} under ${base} is a read`, () => {
        const url = base + request.path;
        assert.equal(requestKind(request.method, url), 'read', url);
      });
    }
  }
});

describe('The Jobs tab Verify and Cancel are classified as writes', () => {
  for (const base of BASES) {
    for (const request of TAB_WRITES) {
      it(`${request.name} under ${base} is a write`, () => {
        const url = base + request.path;
        assert.equal(requestKind(request.method, url), 'write', url);
      });
    }
  }
});

describe('A Verify or Cancel is sent while a job read is held', () => {
  for (const base of BASES) {
    for (const request of TAB_WRITES) {
      it(`${request.name} under ${base} is sent before the read settles`, async () => {
        const api = server();
        const gate = new RequestGate(visibility, api.send);
        const readUrl = `${base}/v1/jobs/j1`;
        const writeUrl = base + request.path;
        const read = gate.fetch(readUrl);
        let readSettled = false;
        const markSettled = () => {
          readSettled = true;
        };
        read.then(markSettled, markSettled);
        await settle();
        assert.deepEqual(api.urls(), [`GET ${readUrl}`]);
        const write = gate.fetch(writeUrl, { method: request.method });
        await settle();
        assert.deepEqual(api.urls(), [
          `GET ${readUrl}`,
          `${request.method} ${writeUrl}`,
        ]);
        assert.equal(readSettled, false, 'the read is still held');
        api.answer(1);
        assert.equal((await write).status, 200);
        api.answer(0);
        assert.equal((await read).status, 200);
      });
    }
  }
});

describe('A Jobs tab read waits its turn behind a held read', () => {
  for (const base of BASES) {
    for (const request of TAB_READS) {
      it(`${request.name} under ${base} is sent once the held body is read`, async () => {
        const api = server();
        const gate = new RequestGate(visibility, api.send);
        const heldUrl = `${base}/v1/artifacts`;
        const readUrl = base + request.path;
        const held = gate.fetch(heldUrl);
        await settle();
        const read = gate.fetch(readUrl);
        await settle();
        assert.deepEqual(api.urls(), [`GET ${heldUrl}`]);
        api.answer(0, Response.json({ artifacts: [] }));
        const answer = await held;
        await settle();
        assert.deepEqual(
          api.urls(),
          [`GET ${heldUrl}`],
          'the answer body is still being read',
        );
        await answer.json();
        await settle();
        assert.deepEqual(api.urls(), [`GET ${heldUrl}`, `GET ${readUrl}`]);
        api.answer(1);
        assert.equal((await read).status, 200);
      });
    }
  }
});
