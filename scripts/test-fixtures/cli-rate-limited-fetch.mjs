import assert from 'node:assert/strict';

// Loaded only in a test CLI child. Supply a known response without assuming
// how much of a live limiter's interval process startup has consumed.
const retryAfter = process.env.ARTIFREE_TEST_RETRY_AFTER;
assert.match(retryAfter ?? '', /^[1-9]\d*$/);
let requests = 0;

globalThis.fetch = async (url, options) => {
  requests += 1;
  assert.equal(requests, 1, 'the CLI must leave retrying to its caller');
  const request = new Request(url, options);
  assert.equal(new URL(request.url).pathname, '/v1/artifacts');
  assert.equal(request.method, 'POST');
  assert.equal(request.headers.get('idempotency-key'), 'cli-limited-1');
  assert.equal(
    request.headers.get('authorization'),
    `Bearer ${process.env.ARTIFREE_TOKEN}`,
  );
  assert.deepEqual(await request.json(), {
    title: 'T',
    files: [{ path: 'index.html', encoding: 'utf8', content: '<p>x</p>' }],
  });
  return Response.json(
    {
      error: {
        code: 'rate_limited',
        message: 'too many requests; retry after the given delay',
      },
    },
    { status: 429, headers: { 'retry-after': retryAfter } },
  );
};
