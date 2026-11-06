import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { test } from 'node:test';
import { request } from '@playwright/test';
import { replaceData } from '../smoke-live.mjs';

const CASES = [
  {
    name: 'list',
    path: '/v1/artifacts',
    body: {
      artifacts: [
        { id: 'sample', current_revision: 3 },
        { id: 'other', current_revision: 4 },
      ],
    },
    rewritten: {
      artifacts: [
        { id: 'sample', current_revision: 1 },
        { id: 'other', current_revision: 4 },
      ],
    },
  },
  {
    name: 'artifact',
    path: '/v1/artifacts/sample',
    body: { id: 'sample', title: 'Example', current_revision: 3 },
    rewritten: { id: 'sample', title: 'Example', current_revision: 1 },
  },
  {
    name: 'revisions',
    path: '/v1/artifacts/sample/revisions',
    body: { revisions: [{ revision: 3 }, { revision: 2 }, { revision: 1 }] },
    rewritten: { revisions: [{ revision: 1 }] },
  },
];

/** Runs the actual replacement handler against an HTTP upstream. */
async function replacedAnswer(path, status, headers, body) {
  const server = createServer((_incoming, outgoing) => {
    outgoing.writeHead(status, headers);
    outgoing.end(body);
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  let context;
  try {
    context = await request.newContext();
    const url = `http://127.0.0.1:${server.address().port}${path}`;
    let accepts;
    let answer;
    replaceData(
      {
        routes: {
          always(matches, handle) {
            accepts = matches;
            answer = handle;
          },
        },
      },
      'sample',
      { upTo: 1 },
    );
    const incoming = { url: () => url, method: () => 'GET' };
    assert.equal(accepts(incoming), true);
    // Only Route is adapted: fetching and reading APIResponse use the real
    // Playwright HTTP client. Full browser smoke exercises Route fulfillment.
    return await answer({
      request: () => incoming,
      fetch: () => context.get(url),
      async fulfill({ response, status, headers, body: overridden, json }) {
        return {
          status: status ?? response?.status() ?? 200,
          headers: headers ??
            response?.headers() ?? { 'content-type': 'application/json' },
          body:
            json !== undefined
              ? Buffer.from(JSON.stringify(json))
              : overridden !== undefined
                ? Buffer.from(overridden)
                : await response.body(),
        };
      },
    });
  } finally {
    await context?.dispose();
    await new Promise((resolve, reject) =>
      server.close((error) => (error ? reject(error) : resolve())),
    );
  }
}

for (const scenario of CASES) {
  test(`Replacement rewrites a successful ${scenario.name} answer`, async () => {
    const result = await replacedAnswer(
      scenario.path,
      200,
      { 'content-type': 'application/json' },
      JSON.stringify(scenario.body),
    );
    assert.equal(result.status, 200);
    assert.deepEqual(JSON.parse(result.body.toString()), scenario.rewritten);
  });

  for (const status of [429, 503]) {
    test(`Replacement preserves ${status} status, headers, and bytes for ${scenario.name}`, async () => {
      const body = Buffer.from(' \n{"error":{"message":"Try later."}}\n');
      const result = await replacedAnswer(
        scenario.path,
        status,
        {
          'content-type': 'application/problem+json',
          'retry-after': '7',
          'x-upstream-example': 'unchanged',
        },
        body,
      );
      assert.equal(result.status, status);
      assert.equal(result.headers['retry-after'], '7');
      assert.equal(result.headers['content-type'], 'application/problem+json');
      assert.equal(result.headers['x-upstream-example'], 'unchanged');
      assert.deepEqual(result.body, body);
    });
  }
}
