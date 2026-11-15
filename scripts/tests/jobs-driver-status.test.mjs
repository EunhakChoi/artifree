import assert from 'node:assert/strict';
import { test } from 'node:test';
import { setImmediate as nextTurn } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The head build status of the Jobs tab (decision 0058, rule 16, and the R03ea
// note): it is read when the tab opens, with each read of the list, and again on
// each `job_changed` and `revised` change of the artifact. An answer that a later
// change overtook is read again, not shown, and a `profile_unknown` refusal is
// not read again until the change feed resets. The driver is bundled as it
// ships, on ports that record each read in order, and a clock that stands still.

// The driver and the client's error class come from one bundle, so the
// driver's `instanceof` checks hold for the errors these cases throw.
const bundle = await build({
  stdin: {
    contents:
      "export { JobsDriver } from './jobs-driver.ts'; export { ArtifreeApiError } from '@artifree/client';",
    resolveDir: fileURLToPath(
      new URL('../../apps/web/src/artifacts/', import.meta.url),
    ),
  },
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { ArtifreeApiError, JobsDriver } = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);

const ARTIFACT = `art_${'a'.repeat(32)}`;
const OTHER = `art_${'b'.repeat(32)}`;
const JOB = `job_${'c'.repeat(32)}`;

/** A build job of `ARTIFACT` in `state`, as the list reads it. */
const BUILD_JOB = {
  id: JOB,
  artifact_id: ARTIFACT,
  kind: 'run_profile',
  profile: 'react-v1',
  revision: 2,
  state: 'running',
  attempt: 1,
  max_attempts: 3,
  cancel_requested: false,
  created_at_ms: 1_000,
};

/** The head status of revision 2 in `status`, with `fields` added. */
function headStatus(status, fields = {}) {
  return {
    profile: 'react-v1',
    head_revision: 2,
    status,
    diagnostics: [],
    ...fields,
  };
}

/** `GET /v1/background` of a server whose worker runs. */
const RUNNING = {
  worker: {
    dropped_lines: 0,
    mode: 'supervised',
    restarts: 0,
    state: 'running',
  },
  execution: { ready: true },
};

/** A clock that stands still, whose timers never fire. */
const STILL_CLOCK = { now: () => 0, after: () => () => {} };

/** A change of the feed of `artifact`, with the fields the client gives it. */
function change(kind, fields = {}) {
  return {
    cursor: `${'f'.repeat(32)}.1`,
    artifact_id: ARTIFACT,
    revision: 2,
    metadata_revision: 1,
    committed_at_ms: 1,
    change: kind,
    ...fields,
  };
}

/** A promise with the function that settles it. */
function deferred() {
  let resolve;
  const promise = new Promise((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

/**
 * A driver of `ARTIFACT` whose list shows the build job. `statuses` are the
 * answers of the status reads in order, the last one repeating; each is a
 * value, an `Error` that the read rejects with, or a promise the case settles.
 * `reads` names every read in order.
 */
function driverWith(statuses) {
  const reads = [];
  let served = 0;
  const ports = {
    listJobs: async () => {
      reads.push('list');
      return { jobs: [BUILD_JOB] };
    },
    getJob: async () => {
      reads.push('job');
      return BUILD_JOB;
    },
    getBackground: async () => {
      reads.push('background');
      return RUNNING;
    },
    getBuildStatus: (profile) => {
      reads.push(`status ${profile}`);
      const answer = statuses[Math.min(served, statuses.length - 1)];
      served += 1;
      if (answer instanceof Error) return Promise.reject(answer);
      return Promise.resolve(answer);
    },
    enqueueJob: () => new Promise(() => {}),
    cancelJob: () => new Promise(() => {}),
  };
  const driver = new JobsDriver(ports, ARTIFACT, STILL_CLOCK);
  return { driver, reads };
}

/** Lets every read the driver has started or owes settle. */
async function settle() {
  for (let turn = 0; turn < 10; turn += 1) await nextTurn();
}

/** A started driver whose first reads have settled, and its reads since. */
async function started(statuses, resets = 0) {
  const made = driverWith(statuses);
  made.driver.start(resets);
  await settle();
  return made;
}

/** The reads made since `from`. */
function since(reads, from) {
  return reads.slice(from);
}

const UNKNOWN = new ArtifreeApiError('The profile is unknown.', {
  status: 422,
  code: 'profile_unknown',
});

test('start() reads the list, the head status of react-v1, then the worker, and shows the status', async () => {
  const built = headStatus('built', { latest_build_revision: 2 });
  const { driver, reads } = await started([built]);
  assert.deepEqual(reads, ['list', 'status react-v1', 'background']);
  assert.deepEqual(driver.snapshot().buildStatus, {
    kind: 'read',
    status: built,
  });
});

// Each change of the feed and the reads it makes after the start's reads.
const CHANGE_CASES = [
  {
    name: 'a job_changed of a listed job reads that job and the status',
    change: change('job_changed', { job_id: JOB }),
    reads: ['job', 'status react-v1'],
  },
  {
    name: 'a revised change reads the status only',
    change: change('revised'),
    reads: ['status react-v1'],
  },
  {
    name: 'a recovered change reads the list and the status with it',
    change: change('recovered'),
    reads: ['list', 'status react-v1'],
  },
  {
    name: 'a metadata change reads nothing',
    change: change('metadata_updated'),
    reads: [],
  },
  {
    name: 'a revised change of another artifact reads nothing',
    change: change('revised', { artifact_id: OTHER }),
    reads: [],
  },
];

for (const kase of CHANGE_CASES) {
  test(`following the feed: ${kase.name}`, async () => {
    const { driver, reads } = await started([headStatus('none')]);
    const before = reads.length;
    driver.follow(kase.change);
    await settle();
    assert.deepEqual(since(reads, before), kase.reads);
  });
}

test('a status answer that a revised change overtook is read again, and only the later answer is shown', async () => {
  const late = deferred();
  const pending = headStatus('pending', { job_id: JOB });
  const failed = headStatus('failed', {
    job_id: JOB,
    failure: 'build_failed',
    head_revision: 3,
  });
  const later = deferred();
  const made = driverWith([late.promise, later.promise]);
  made.driver.start(0);
  await settle();
  assert.equal(made.driver.snapshot().buildStatus.kind, 'unread');

  made.driver.follow(change('revised', { revision: 3 }));
  late.resolve(pending);
  await settle();
  // The overtaken answer is not shown while the read it owes is in flight.
  assert.equal(made.driver.snapshot().buildStatus.kind, 'unread');

  later.resolve(failed);
  await settle();
  assert.deepEqual(made.driver.snapshot().buildStatus, {
    kind: 'read',
    status: failed,
  });
  assert.equal(
    made.reads.filter((read) => read === 'status react-v1').length,
    2,
  );
});

test('a profile_unknown status is not read again by changes or a ready, only after a reset', async () => {
  const { driver, reads } = await started([UNKNOWN, headStatus('none')], 0);
  assert.deepEqual(driver.snapshot().buildStatus, { kind: 'profile_unknown' });

  const before = reads.length;
  driver.follow(change('revised'));
  driver.follow(change('job_changed', { job_id: JOB }));
  await settle();
  driver.resync(0);
  await settle();
  assert.deepEqual(since(reads, before), ['job', 'list']);
  assert.deepEqual(driver.snapshot().buildStatus, { kind: 'profile_unknown' });

  const reset = reads.length;
  driver.resync(1);
  await settle();
  assert.deepEqual(since(reads, reset), ['list', 'status react-v1']);
  assert.equal(driver.snapshot().buildStatus.kind, 'read');
});

test('a failed status read is shown in words, and retryStatus() reads it again', async () => {
  const down = new ArtifreeApiError('The server is unavailable.', {
    status: 503,
    code: 'unavailable',
  });
  const { driver, reads } = await started([down, headStatus('none')]);
  const failed = driver.snapshot().buildStatus;
  assert.equal(failed.kind, 'failed');
  assert.match(failed.message, /\S/);

  const before = reads.length;
  driver.retryStatus();
  await settle();
  assert.deepEqual(since(reads, before), ['status react-v1']);
  assert.equal(driver.snapshot().buildStatus.kind, 'read');
});
