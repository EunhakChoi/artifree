import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

// Decision 0055, rule 11: `artifree job show --wait` waits the way followJob
// does and prints only the job once it has ended. Without --wait, `job show`
// reads the job once and prints it as read. These tests run the built CLI
// against a stub server that serves the change feed and the job route.

const root = fileURLToPath(new URL('../../', import.meta.url));
const execFileAsync = promisify(execFile);
const TIMEOUT_MS = 10_000;
const TOKEN = 'f'.repeat(64);
const FEED = 'f'.repeat(32);
const ARTIFACT_ID = `art_${'1'.repeat(32)}`;
const JOB_ID = `job_${'2'.repeat(32)}`;

// The client keeps the contract fields of a job in this order, so the text the
// CLI prints is exactly the JSON of these objects.
const runningJob = {
  id: JOB_ID,
  artifact_id: ARTIFACT_ID,
  revision: 3,
  kind: 'verify_revision',
  state: 'running',
  attempt: 1,
  max_attempts: 3,
  cancel_requested: false,
  created_at_ms: 4,
  started_at_ms: 5,
};

const succeededJob = {
  id: JOB_ID,
  artifact_id: ARTIFACT_ID,
  revision: 3,
  kind: 'verify_revision',
  state: 'succeeded',
  attempt: 1,
  max_attempts: 3,
  cancel_requested: false,
  created_at_ms: 4,
  result: {
    verdict: 'ok',
    files_checked: 2,
    bytes_checked: 9,
    mismatch_count: 0,
    mismatches: [],
    truncated: false,
  },
  started_at_ms: 5,
  finished_at_ms: 6,
};

// The change a job update commits (decision 0055, rule 8): the artifact's
// revision and metadata revision, and the job that changed.
const jobChanged = {
  cursor: `${FEED}.1`,
  artifact_id: ARTIFACT_ID,
  change: 'job_changed',
  revision: 3,
  metadata_revision: 1,
  committed_at_ms: 6,
  job_id: JOB_ID,
};

/** One server-sent event whose data is `data` as JSON. */
function sse(type, data) {
  return `event: ${type}\ndata: ${JSON.stringify(data)}\n\n`;
}

/**
 * Starts a stub API on a free loopback port. `/v1/changes` opens an event
 * stream that sends `ready` at once. `/v1/jobs/{id}` answers its reads in
 * order from `replies`, repeating the last reply. With `changeAfterRead`, the
 * open stream sends `job_changed` for the job once that many reads are
 * answered.
 */
async function startStub({ replies, changeAfterRead }) {
  const requests = [];
  let feed;
  let reads = 0;
  const server = createServer((request, response) => {
    requests.push(`${request.method} ${request.url}`);
    if (request.url === '/v1/changes') {
      feed = response;
      response.writeHead(200, {
        'content-type': 'text/event-stream',
        'cache-control': 'no-store',
      });
      response.write(sse('ready', { cursor: `${FEED}.0` }));
      return;
    }
    if (request.url === `/v1/jobs/${JOB_ID}`) {
      const reply = replies[Math.min(reads, replies.length - 1)];
      reads += 1;
      response.writeHead(reply.status, { 'content-type': 'application/json' });
      response.end(JSON.stringify(reply.body));
      if (reads === changeAfterRead) feed?.write(sse('change', jobChanged));
      return;
    }
    response.writeHead(404, { 'content-type': 'application/json' });
    response.end(
      JSON.stringify({
        error: { code: 'not_found', message: 'No such route.' },
      }),
    );
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const address = server.address();
  assert.ok(address !== null && typeof address === 'object');
  return {
    url: `http://127.0.0.1:${address.port}`,
    requests,
    async close() {
      if (feed !== undefined && !feed.destroyed) feed.end();
      server.closeAllConnections();
      await new Promise((resolve) => {
        server.close(resolve);
      });
    },
  };
}

/**
 * Runs `artifree job <args>` against `stub` and resolves with its exit status
 * and output. A run that has not exited by the timeout fails the test.
 */
async function runJob(args, stub) {
  const env = { ...process.env, ARTIFREE_URL: stub.url, ARTIFREE_TOKEN: TOKEN };
  delete env.ARTIFREE_SERVER;
  delete env.ARTIFREE_TOKEN_FILE;
  try {
    const { stdout, stderr } = await execFileAsync(
      process.execPath,
      ['apps/cli/dist/main.js', 'job', ...args],
      { cwd: root, env, timeout: TIMEOUT_MS },
    );
    return { status: 0, stdout, stderr };
  } catch (error) {
    if (error.killed === true)
      assert.fail(
        `artifree job ${args.join(' ')} did not exit within ${TIMEOUT_MS} ms`,
      );
    return { status: error.code, stdout: error.stdout, stderr: error.stderr };
  }
}

test('job show --wait prints only the ended job once the feed reports its change', async () => {
  const stub = await startStub({
    replies: [
      { status: 200, body: runningJob },
      { status: 200, body: succeededJob },
    ],
    changeAfterRead: 1,
  });
  try {
    const result = await runJob(['show', JOB_ID, '--wait'], stub);
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stderr, '');
    assert.equal(result.stdout, `${JSON.stringify(succeededJob, null, 2)}\n`);
    assert.deepEqual(stub.requests, [
      'GET /v1/changes',
      `GET /v1/jobs/${JOB_ID}`,
      `GET /v1/jobs/${JOB_ID}`,
    ]);
  } finally {
    await stub.close();
  }
});

test('job show --wait exits with the API error when a job read answers 404', async () => {
  const stub = await startStub({
    replies: [
      {
        status: 404,
        body: { error: { code: 'not_found', message: 'No such job.' } },
      },
    ],
  });
  try {
    // main.ts reports an API error on stderr with exit status 1 (README).
    const result = await runJob(['show', JOB_ID, '--wait'], stub);
    assert.equal(result.status, 1);
    assert.equal(result.stdout, '');
    assert.equal(result.stderr, 'error[not_found]: No such job.\n');
    assert.deepEqual(stub.requests, [
      'GET /v1/changes',
      `GET /v1/jobs/${JOB_ID}`,
    ]);
  } finally {
    await stub.close();
  }
});

test('job show without --wait reads the job once and prints it as read', async () => {
  const stub = await startStub({
    replies: [{ status: 200, body: runningJob }],
  });
  try {
    const result = await runJob(['show', JOB_ID], stub);
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stderr, '');
    assert.equal(result.stdout, `${JSON.stringify(runningJob, null, 2)}\n`);
    assert.deepEqual(stub.requests, [`GET /v1/jobs/${JOB_ID}`]);
  } finally {
    await stub.close();
  }
});

test('job show with --wait and an extra argument is a usage error before any request', async () => {
  const stub = await startStub({
    replies: [{ status: 200, body: succeededJob }],
  });
  try {
    // Usage errors exit with status 2 (README).
    const result = await runJob(['show', JOB_ID, '--wait', 'extra'], stub);
    assert.equal(result.status, 2, result.stderr);
    assert.equal(result.stdout, '');
    assert.match(result.stderr, /^Expected <job-id>\./);
    assert.deepEqual(stub.requests, []);
  } finally {
    await stub.close();
  }
});
