import assert from 'node:assert/strict';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The Jobs tab's rules and words are web code without DOM or React imports;
// bundle them as they ship. The bundle carries its own copy of the client, so
// the errors below are built from that copy for `instanceof` to hold.
const bundle = await build({
  stdin: {
    contents: [
      "export * from './jobs-model.ts';",
      "export * from './jobs-text.ts';",
      "export { ArtifreeApiError } from '@artifree/client';",
    ].join('\n'),
    resolveDir: fileURLToPath(
      new URL('../../apps/web/src/artifacts/', import.meta.url),
    ),
  },
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  tsconfig: fileURLToPath(
    new URL('../../apps/web/tsconfig.json', import.meta.url),
  ),
  write: false,
});
const {
  ArtifreeApiError,
  afterFailure,
  appendPage,
  isEnded,
  isOlder,
  mergeFirstPage,
  moreFindings,
  nextBackgroundRead,
  nextOverdueAt,
  overdueJobs,
  pressCancel,
  pressVerify,
  reactionTo,
  refusalText,
  replaceJob,
  seenQueued,
  showsNoWorker,
  upsertJob,
  workerNoticeOf,
  workerText,
} = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);

const ARTIFACT = 'art_one';
const OTHER_ARTIFACT = 'art_two';

// A verify job created at `createdAt` (Unix milliseconds) and in `state`.
const jobAt = (id, createdAt, state = 'succeeded') => ({
  id,
  artifact_id: ARTIFACT,
  kind: 'verify_revision',
  state,
  revision: 1,
  attempt: 1,
  max_attempts: 3,
  cancel_requested: false,
  created_at_ms: createdAt,
});

// The server lists jobs newest first: f, e, d, c, b, a.
const jobF = jobAt('job_f', 6000);
const jobE = jobAt('job_e', 5000);
const jobD = jobAt('job_d', 4000);
const jobC = jobAt('job_c', 3000);
const jobB = jobAt('job_b', 2000);
const jobA = jobAt('job_a', 1000);
const jobG = jobAt('job_g', 7000);

const ids = (rows) => rows.jobs.map((job) => job.id);

const apiError = (status, code, message) =>
  new ArtifreeApiError(message, { status, code });

const feedChange = (kind, fields = {}) => ({
  artifact_id: ARTIFACT,
  change: kind,
  committed_at_ms: 1000,
  cursor: 'cursor-1',
  metadata_revision: 1,
  revision: 1,
  ...fields,
});

const verifyResult = (mismatchCount, mismatches, truncated) => ({
  verdict: 'mismatch',
  files_checked: 100,
  bytes_checked: 4096,
  mismatch_count: mismatchCount,
  mismatches,
  truncated,
});

const listedMismatches = (count) =>
  Array.from({ length: count }, (_, index) => ({
    path: `file-${index}.txt`,
    problem: 'digest_mismatch',
  }));

const background = (state, mode = 'supervised') => ({
  worker: { mode, state, restarts: 0, dropped_lines: 0 },
});

test('isOlder: a job created earlier is listed after a job created later', () => {
  assert.equal(isOlder(jobA, jobB), true);
  assert.equal(isOlder(jobB, jobA), false);
});

test('isOlder: at the same creation time the larger id is listed after', () => {
  const smaller = jobAt('job_a', 1000);
  const larger = jobAt('job_b', 1000);
  assert.equal(isOlder(larger, smaller), true);
  assert.equal(isOlder(smaller, larger), false);
});

test('isOlder: a job is not older than itself', () => {
  assert.equal(isOlder(jobA, jobA), false);
});

test('appendPage adds the jobs not loaded yet after the loaded ones', () => {
  const loaded = { jobs: [jobF, jobE], next: 'tok-1' };
  const page = {
    jobs: [jobAt('job_e', 5000), jobD],
    next_page_token: 'tok-2',
  };
  const appended = appendPage(loaded, page);
  assert.deepEqual(ids(appended), ['job_f', 'job_e', 'job_d']);
  assert.equal(appended.jobs[1], jobE);
  assert.equal(appended.next, 'tok-2');
});

test('appendPage has no next page when the page has no next token', () => {
  const loaded = { jobs: [jobF], next: 'tok-1' };
  const appended = appendPage(loaded, { jobs: [jobE], next_page_token: null });
  assert.deepEqual(ids(appended), ['job_f', 'job_e']);
  assert.equal(appended.next, undefined);
});

const mergeCases = [
  {
    name: 'without loaded rows, the first page is the list',
    rows: undefined,
    list: { jobs: [jobF, jobE], next_page_token: 'tok-p2' },
    expected: ['job_f', 'job_e'],
    next: 'tok-p2',
  },
  {
    name: 'a first page without a next token replaces every loaded row',
    rows: { jobs: [jobF, jobE, jobD, jobC], next: 'tok-x' },
    list: { jobs: [jobF, jobE], next_page_token: null },
    expected: ['job_f', 'job_e'],
    next: undefined,
  },
  {
    name: 'loaded rows older than the new first page stay after it',
    rows: { jobs: [jobE, jobD, jobC, jobB], next: 'tok-old' },
    list: { jobs: [jobF, jobE], next_page_token: 'tok-new' },
    expected: ['job_f', 'job_e', 'job_d', 'job_c', 'job_b'],
    next: 'tok-old',
  },
  {
    name: 'a loaded row newer than the first page no longer lists is dropped',
    rows: { jobs: [jobG, jobE, jobD], next: 'tok-old' },
    list: { jobs: [jobF, jobE], next_page_token: 'tok-new' },
    expected: ['job_f', 'job_e', 'job_d'],
    next: 'tok-old',
  },
  {
    name: 'with no older loaded rows, the new page gives the next token',
    rows: { jobs: [jobF, jobE], next: 'tok-old' },
    list: { jobs: [jobF, jobE, jobD], next_page_token: 'tok-new' },
    expected: ['job_f', 'job_e', 'job_d'],
    next: 'tok-new',
  },
  {
    name: 'loaded rows that no job of the new first page reaches are dropped',
    rows: { jobs: [jobC, jobB], next: undefined },
    list: { jobs: [jobG, jobF], next_page_token: 'tok-new' },
    expected: ['job_g', 'job_f'],
    next: 'tok-new',
  },
  {
    name: 'a loaded tail with a next page is dropped when no job of the new first page reaches it',
    rows: { jobs: [jobD], next: 'tok-old' },
    list: { jobs: [jobG, jobF], next_page_token: 'tok-new' },
    expected: ['job_g', 'job_f'],
    next: 'tok-new',
  },
];
for (const { name, rows, list, expected, next } of mergeCases) {
  test(`mergeFirstPage: ${name}`, () => {
    const merged = mergeFirstPage(rows, list);
    assert.deepEqual(ids(merged), expected);
    assert.equal(merged.next, next);
  });
}

// The server lists six jobs, newest first: g, f, e and d were created after the
// loaded c and b. Its pages hold two jobs each; a token names the next page.
const listingPageOne = { jobs: [jobG, jobF], next_page_token: 'tok-2' };
const listingPageTwo = { jobs: [jobE, jobD], next_page_token: 'tok-4' };
const listingPageThree = { jobs: [jobC, jobB], next_page_token: null };
test('after a reread past loaded jobs, Load more reaches every job in listing order', () => {
  const reread = mergeFirstPage(
    { jobs: [jobC, jobB], next: undefined },
    listingPageOne,
  );
  assert.equal(reread.next, 'tok-2');
  const second = appendPage(reread, listingPageTwo);
  const third = appendPage(second, listingPageThree);
  assert.deepEqual(ids(third), [
    'job_g',
    'job_f',
    'job_e',
    'job_d',
    'job_c',
    'job_b',
  ]);
  assert.equal(third.next, undefined);
});

test('replaceJob returns the same rows when no row has the job id', () => {
  const rows = { jobs: [jobF, jobE], next: 'tok' };
  assert.equal(replaceJob(rows, jobA), rows);
});

test('replaceJob swaps a listed row in place and keeps the next token', () => {
  const rows = { jobs: [jobF, jobE], next: 'tok' };
  const changed = jobAt('job_e', 5000, 'failed');
  const replaced = replaceJob(rows, changed);
  assert.deepEqual(ids(replaced), ['job_f', 'job_e']);
  assert.equal(replaced.jobs[1], changed);
  assert.equal(replaced.next, 'tok');
});

test('upsertJob puts a new job before the first older row', () => {
  const rows = { jobs: [jobC, jobA], next: 'tok' };
  const upserted = upsertJob(rows, jobB);
  assert.deepEqual(ids(upserted), ['job_c', 'job_b', 'job_a']);
  assert.equal(upserted.next, 'tok');
});

test('upsertJob orders a job of the same creation time by its id', () => {
  const rows = {
    jobs: [jobAt('job_a', 2000), jobAt('job_c', 1000)],
    next: 'tok',
  };
  const upserted = upsertJob(rows, jobAt('job_b', 1000));
  assert.deepEqual(ids(upserted), ['job_a', 'job_b', 'job_c']);
});

test('upsertJob appends an older job when every job is loaded', () => {
  const rows = { jobs: [jobC, jobA], next: undefined };
  const upserted = upsertJob(rows, jobAt('job_0', 500));
  assert.deepEqual(ids(upserted), ['job_c', 'job_a', 'job_0']);
});

test('upsertJob leaves the rows alone for an older job of an unloaded page', () => {
  const rows = { jobs: [jobC, jobA], next: 'tok' };
  assert.equal(upsertJob(rows, jobAt('job_0', 500)), rows);
});

test('upsertJob replaces a listed job in place', () => {
  const rows = { jobs: [jobC, jobB, jobA], next: 'tok' };
  const upserted = upsertJob(rows, jobAt('job_b', 2000, 'failed'));
  assert.deepEqual(ids(upserted), ['job_c', 'job_b', 'job_a']);
  assert.equal(upserted.jobs[1].state, 'failed');
  assert.equal(upserted.next, 'tok');
});

const loadedRows = { jobs: [jobC, jobB, jobA], next: 'tok' };
const quietKinds = [
  'created',
  'revised',
  'metadata_updated',
  'deleted',
  'purged',
  'data_changed',
  'unknown',
];
const reactionCases = [
  {
    name: 'a job_changed for a listed job reads that job',
    change: feedChange('job_changed', { job_id: 'job_b' }),
    rows: loadedRows,
    expected: { type: 'job', id: 'job_b' },
  },
  {
    name: 'a job_changed for an unlisted job reads the first page',
    change: feedChange('job_changed', { job_id: 'job_z' }),
    rows: loadedRows,
    expected: { type: 'first page' },
  },
  {
    name: 'a job_changed without a job id reads the first page',
    change: feedChange('job_changed'),
    rows: loadedRows,
    expected: { type: 'first page' },
  },
  {
    name: 'a job_changed before any row is loaded reads the first page',
    change: feedChange('job_changed', { job_id: 'job_b' }),
    rows: undefined,
    expected: { type: 'first page' },
  },
  {
    name: 'a recovered change reads the first page',
    change: feedChange('recovered'),
    rows: loadedRows,
    expected: { type: 'first page' },
  },
  {
    name: 'a job_changed of another artifact reads nothing',
    change: feedChange('job_changed', {
      artifact_id: OTHER_ARTIFACT,
      job_id: 'job_b',
    }),
    rows: loadedRows,
    expected: { type: 'none' },
  },
  {
    name: 'a recovered change of another artifact reads nothing',
    change: feedChange('recovered', { artifact_id: OTHER_ARTIFACT }),
    rows: loadedRows,
    expected: { type: 'none' },
  },
  ...quietKinds.map((kind) => ({
    name: `a ${kind} change reads nothing`,
    change: feedChange(kind),
    rows: loadedRows,
    expected: { type: 'none' },
  })),
];
for (const { name, change, rows, expected } of reactionCases) {
  test(`reactionTo: ${name}`, () => {
    assert.deepEqual(reactionTo(change, ARTIFACT, rows), expected);
  });
}

const pressKinds = [
  {
    name: 'Verify revision',
    make: (newKey) => pressVerify(4, newKey),
    target: { kind: 'verify', revision: 4 },
  },
  {
    name: 'Cancel',
    make: (newKey) => pressCancel('job_b', newKey),
    target: { kind: 'cancel', jobId: 'job_b' },
  },
];
for (const { name, make, target } of pressKinds) {
  test(`${name} press: each press takes a new request key and keeps its target`, () => {
    let calls = 0;
    const newKey = () => {
      calls += 1;
      return `key-${calls}`;
    };
    assert.deepEqual(make(newKey), { ...target, key: 'key-1' });
    assert.deepEqual(make(newKey), { ...target, key: 'key-2' });
    assert.equal(calls, 2);
  });
}

test('a press without an injected key takes a random key in the client format', () => {
  const first = pressVerify(4).key;
  const second = pressVerify(4).key;
  assert.match(first, /^key-[0-9a-f]{8}-[0-9a-f]{24}$/);
  assert.notEqual(second, first);
});

const lostAnswers = [
  { name: 'a network error', error: apiError(0, 'network_error', 'Lost.') },
  {
    name: 'a 503 from the server',
    error: apiError(503, 'unavailable', 'Busy.'),
  },
  {
    name: 'an unreadable answer',
    error: apiError(200, 'invalid_response', 'Unreadable.'),
  },
  {
    name: 'a failure that is not an API error',
    error: new TypeError('fetch failed'),
  },
];
for (const { name, error } of lostAnswers) {
  test(`afterFailure keeps the press unknown after ${name}`, () => {
    const press = pressVerify(4, () => 'key-1');
    const state = afterFailure(press, error);
    assert.equal(state.status, 'unknown');
    assert.equal(state.press, press);
  });
}

const refusals = [
  {
    name: 'a 409 too_many_jobs',
    status: 409,
    code: 'too_many_jobs',
    message: 'Too many jobs.',
    refusal: 'too_many_jobs',
  },
  {
    name: 'a 409 job_ended',
    status: 409,
    code: 'job_ended',
    message: 'Job ended.',
    refusal: 'job_ended',
  },
  {
    name: 'a 404 not_found',
    status: 404,
    code: 'not_found',
    message: 'Not found.',
    refusal: 'other',
  },
  {
    name: 'a 429 rate_limited',
    status: 429,
    code: 'rate_limited',
    message: 'Slow down.',
    refusal: 'other',
  },
];
for (const { name, status, code, message, refusal } of refusals) {
  test(`afterFailure refuses the press after ${name}`, () => {
    const press = pressVerify(4, () => 'key-1');
    const error = apiError(status, code, message);
    assert.deepEqual(afterFailure(press, error), {
      status: 'refused',
      refusal,
      message,
    });
  });
}

test('seenQueued keeps the first time a job was seen queued and marks a new one at now', () => {
  const seen = new Map([['job_a', 1000]]);
  const rows = {
    jobs: [jobAt('job_a', 1000, 'queued'), jobAt('job_b', 2000, 'queued')],
    next: undefined,
  };
  assert.deepEqual(Object.fromEntries(seenQueued(seen, rows, 4000)), {
    job_a: 1000,
    job_b: 4000,
  });
});

test('seenQueued drops the jobs that are not queued or not loaded', () => {
  const seen = new Map([
    ['job_c', 500],
    ['job_d', 600],
    ['job_gone', 700],
  ]);
  const rows = {
    jobs: [jobAt('job_c', 3000), jobAt('job_d', 4000, 'running')],
    next: undefined,
  };
  assert.deepEqual(Object.fromEntries(seenQueued(seen, rows, 4000)), {});
});

test('seenQueued is empty before the first page answers', () => {
  const seen = new Map([['job_a', 1000]]);
  assert.equal(seenQueued(seen, undefined, 4000).size, 0);
});

test('overdueJobs lists a job queued for exactly 5 seconds', () => {
  const seen = new Map([
    ['job_a', 1000],
    ['job_b', 3000],
  ]);
  assert.deepEqual([...overdueJobs(seen, 6000)], ['job_a']);
});

test('overdueJobs does not list a job queued for 4999 ms', () => {
  const seen = new Map([['job_a', 1000]]);
  assert.deepEqual([...overdueJobs(seen, 5999)], []);
});

test('nextOverdueAt is the earliest notice time after now', () => {
  const seen = new Map([
    ['job_a', 1000],
    ['job_b', 3000],
  ]);
  assert.equal(nextOverdueAt(seen, 2000), 6000);
  assert.equal(nextOverdueAt(seen, 6000), 8000);
  assert.equal(nextOverdueAt(seen, 8000), undefined);
  assert.equal(nextOverdueAt(new Map(), 2000), undefined);
});

const queuedSince = new Map([
  ['job_b', 3000],
  ['job_a', 1000],
]);
const backgroundReads = [
  {
    name: 'no job is queued',
    seen: new Map(),
    lastRead: undefined,
    expected: undefined,
  },
  {
    name: 'no job is queued after a read',
    seen: new Map(),
    lastRead: 9000,
    expected: undefined,
  },
  {
    name: 'no read yet waits for the earliest queued job to pass 5 seconds',
    seen: queuedSince,
    lastRead: undefined,
    expected: 6000,
  },
  {
    name: 'a read before that time waits until it',
    seen: queuedSince,
    lastRead: 5000,
    expected: 6000,
  },
  {
    name: 'a read at that time repeats 15 seconds later',
    seen: queuedSince,
    lastRead: 6000,
    expected: 21000,
  },
  {
    name: 'a later read repeats 15 seconds after it',
    seen: queuedSince,
    lastRead: 6500,
    expected: 21500,
  },
];
for (const { name, seen, lastRead, expected } of backgroundReads) {
  test(`nextBackgroundRead: ${name}`, () => {
    assert.equal(nextBackgroundRead(seen, lastRead), expected);
  });
}

const workerStates = [
  ['running', false],
  ['restarting', true],
  ['stopped', true],
];
for (const [state, shows] of workerStates) {
  test(`showsNoWorker is ${shows} for a ${state} worker`, () => {
    assert.equal(showsNoWorker(background(state)), shows);
  });
}

// Rule 14: a failed read of the worker is shown even after an earlier answer,
// since that answer may no longer hold. Without a failure, the last answer
// decides.
const workerNotices = [
  {
    name: 'a failed read after an answer that no worker runs shows the failure',
    status: background('stopped', 'none'),
    error: 'Connection refused.',
    expected: { kind: 'unread', message: 'Connection refused.' },
  },
  {
    name: 'a failed read after an answer that a worker runs shows the failure',
    status: background('running'),
    error: 'Connection refused.',
    expected: { kind: 'unread', message: 'Connection refused.' },
  },
  {
    name: 'a failed read before any answer shows the failure',
    status: undefined,
    error: 'Connection refused.',
    expected: { kind: 'unread', message: 'Connection refused.' },
  },
  {
    name: 'an answer that no worker runs shows that worker',
    status: background('stopped', 'none'),
    error: undefined,
    expected: { kind: 'no_worker', status: background('stopped', 'none') },
  },
  {
    name: 'an answer that a running worker gives shows no notice',
    status: background('running'),
    error: undefined,
    expected: undefined,
  },
  {
    name: 'no answer and no failure show no notice',
    status: undefined,
    error: undefined,
    expected: undefined,
  },
];
for (const { name, status, error, expected } of workerNotices) {
  test(`workerNoticeOf: ${name}`, () => {
    assert.deepEqual(workerNoticeOf(status, error), expected);
  });
}

const endedStates = [
  ['queued', false],
  ['running', false],
  ['retry_scheduled', false],
  ['succeeded', true],
  ['failed', true],
  ['cancelled', true],
];
for (const [state, ended] of endedStates) {
  test(`isEnded over all six states: ${state} is ${ended ? 'ended' : 'not ended'}`, () => {
    assert.equal(isEnded(state), ended);
  });
}

test('moreFindings counts the mismatches a truncated result does not list', () => {
  const result = verifyResult(60, listedMismatches(50), true);
  assert.equal(moreFindings(result), 10);
});

test('moreFindings is zero when the result lists every mismatch', () => {
  const result = verifyResult(2, listedMismatches(2), false);
  assert.equal(moreFindings(result), 0);
});

test('refusalText names too_many_jobs and says what to do', () => {
  assert.equal(
    refusalText('too_many_jobs', 'unused'),
    'Too many jobs are waiting or running (too_many_jobs). Wait for one to end, or cancel one.',
  );
});

test('refusalText names job_ended', () => {
  assert.equal(
    refusalText('job_ended', 'unused'),
    'This job has already ended, so it cannot be cancelled (job_ended).',
  );
});

test('refusalText shows the server message for any other refusal', () => {
  assert.equal(refusalText('other', 'Not found.'), 'Not found.');
});

test('workerText states the worker mode and state', () => {
  assert.equal(
    workerText(background('restarting')),
    'Worker mode: supervised. Worker state: restarting.',
  );
  assert.equal(
    workerText(background('stopped', 'none')),
    'Worker mode: none. Worker state: stopped.',
  );
});
