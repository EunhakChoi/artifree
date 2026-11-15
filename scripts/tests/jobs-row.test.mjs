import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { test } from 'node:test';
import { build } from 'esbuild';

// One row of the Jobs tab (decision 0055, rules 13 and 14), bundled as it ships
// and rendered to markup. Cancel is offered for a job that has not ended, and so
// is the retry of a Cancel whose answer was lost. An ended job offers neither,
// but still shows a refusal of its Cancel as job_ended.
const web = new URL('../../apps/web/', import.meta.url);
const { BuildStatusBanner, JobRow, createElement, renderToStaticMarkup } =
  await loadRow();

/**
 * Bundles the row with React and its server renderer, then loads the bundle
 * from a file. The browser build of react-dom/server opens a message port that
 * keeps the process alive, so the node build is bundled instead.
 */
async function loadRow() {
  const dir = await mkdtemp(join(tmpdir(), 'jobs-row-'));
  try {
    const outfile = join(dir, 'jobs-row.mjs');
    await build({
      stdin: {
        contents: [
          "export { JobRow } from './src/artifacts/jobs-row.tsx';",
          "export { BuildStatusBanner } from './src/artifacts/jobs-build.tsx';",
          "export { createElement } from 'react';",
          "export { renderToStaticMarkup } from 'react-dom/server';",
        ].join('\n'),
        resolveDir: fileURLToPath(web),
      },
      bundle: true,
      format: 'esm',
      platform: 'node',
      jsx: 'automatic',
      // esbuild's ESM output cannot require Node builtins, and react-dom's node
      // build requires some, so the bundle gets a require of its own.
      banner: {
        js: "import { createRequire } from 'node:module'; const require = createRequire(import.meta.url);",
      },
      // The '@/' alias of the web app's components is read from its tsconfig.
      tsconfig: fileURLToPath(new URL('tsconfig.json', web)),
      outfile,
    });
    return await import(pathToFileURL(outfile).href);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
}

const JOB = 'job_one';
const ENDED_STATES = ['succeeded', 'failed', 'cancelled'];
const OPEN_STATES = ['queued', 'running', 'retry_scheduled'];

/** A verify job of revision 2 in `state`, as the list reads it. */
function jobIn(state) {
  const ended = ENDED_STATES.includes(state);
  return {
    id: JOB,
    artifact_id: 'art_one',
    kind: 'verify_revision',
    revision: 2,
    state,
    attempt: 1,
    max_attempts: 3,
    cancel_requested: false,
    created_at_ms: 1000,
    finished_at_ms: ended ? 2000 : null,
    failure: state === 'failed' ? 'internal' : null,
    result: null,
    retry_at_ms: null,
  };
}

/** A Cancel whose answer was lost: the press may have committed. */
const LOST_CANCEL = {
  status: 'unknown',
  press: { kind: 'cancel', jobId: JOB, key: 'key_one' },
  message: 'The answer to the request did not arrive.',
};

/**
 * The markup of one row with the Cancel slot `cancel`, inside its list.
 * `queued` sets whether the job is overdue and the worker's last answer.
 */
function rowMarkup(job, cancel, queued = {}) {
  return renderToStaticMarkup(
    createElement(
      'ul',
      null,
      createElement(JobRow, {
        job,
        overdue: queued.overdue ?? false,
        background: queued.background,
        backgroundError: undefined,
        cancel,
        onCancel: () => {},
        onRetryCancel: () => {},
        onForgetCancel: () => {},
      }),
    ),
  );
}

for (const state of ENDED_STATES) {
  test(`a ${state} job offers no Cancel and no retry of a lost Cancel`, () => {
    const markup = rowMarkup(jobIn(state), LOST_CANCEL);
    assert.doesNotMatch(markup, />Cancel<\/button>/);
    assert.doesNotMatch(markup, /Outcome unknown/);
    assert.doesNotMatch(markup, /Retry cancel/);
    assert.doesNotMatch(markup, /Forget this attempt/);
  });
}

for (const state of OPEN_STATES) {
  test(`a ${state} job keeps Cancel and the retry of a lost Cancel`, () => {
    const markup = rowMarkup(jobIn(state), LOST_CANCEL);
    assert.match(markup, />Cancel<\/button>/);
    assert.match(markup, /Outcome unknown/);
    assert.match(markup, /The answer to the request did not arrive\./);
    assert.match(markup, /Retry cancel/);
    assert.match(markup, /Forget this attempt/);
  });
}

test('a Cancel refused as job_ended is still shown on the ended job', () => {
  const markup = rowMarkup(jobIn('cancelled'), {
    status: 'refused',
    refusal: 'job_ended',
    message: 'The job ended first.',
  });
  assert.match(
    markup,
    /This job has already ended, so it cannot be cancelled \(job_ended\)\./,
  );
});

// A build (decision 0058 rule 17, slice R03da) is named as such, and each of
// its failures has words of its own; R03ea adds the build action.
const BUILD_FAILURES = {
  execution_unavailable: /No execution worker could run the build/,
  build_failed: /The build reported errors\./,
  limit_exceeded: /The build ran past a limit of its sandbox/,
  output_rejected: /does not fit the template/,
};

for (const [failure, words] of Object.entries(BUILD_FAILURES)) {
  test(`a build that failed as ${failure} is named and explained`, () => {
    const markup = rowMarkup(
      {
        ...jobIn('failed'),
        kind: 'run_profile',
        profile: 'react-v1',
        failure,
      },
      { status: 'idle' },
    );
    assert.match(markup, /Build 2/);
    assert.match(markup, words);
  });
}

/** A build job of revision 2 in `state`, with `fields` added. */
function buildIn(state, fields = {}) {
  return {
    ...jobIn(state),
    kind: 'run_profile',
    profile: 'react-v1',
    ...fields,
  };
}

test('a succeeded build shows its profile and its file and byte counts', () => {
  const markup = rowMarkup(
    buildIn('succeeded', { build: { file_count: 3, byte_count: 2048 } }),
    { status: 'idle' },
  );
  assert.match(markup, /Profile react-v1/);
  assert.match(markup, /3 files built, 2048 bytes/);
});

// Decision 0058, R03ea: diagnostics come from the bundler and can quote the
// artifact's source, so they are text, never markup or links.
test("a failed build lists its diagnostics in order, as plain text with each one's location", () => {
  const markup = rowMarkup(
    buildIn('failed', {
      failure: 'build_failed',
      diagnostics: [
        {
          location: { path: 'src/App.tsx', line: 4, column: 9 },
          message: 'Expected "}" but found <img src=x onerror=alert(1)>',
        },
        { message: 'Build failed with 1 error' },
      ],
    }),
    { status: 'idle' },
  );
  const list = /<ul aria-label="Diagnostics"[^>]*>(.*?)<\/ul>/.exec(markup);
  assert.ok(list, 'the row lists its diagnostics');
  const items = [...list[1].matchAll(/<li[^>]*>(.*?)<\/li>/g)].map(
    (item) => item[1],
  );
  assert.deepEqual(items, [
    '<code>src/App.tsx:4:9</code>: Expected &quot;}&quot; but found &lt;img src=x onerror=alert(1)&gt;',
    'Build failed with 1 error',
  ]);
  assert.doesNotMatch(markup, /<img/);
  assert.doesNotMatch(markup, /<a /);
});

test('a failed build without diagnostics lists none', () => {
  const markup = rowMarkup(buildIn('failed', { failure: 'limit_exceeded' }), {
    status: 'idle',
  });
  assert.doesNotMatch(markup, /aria-label="Diagnostics"/);
});

/** `GET /v1/background` with the server worker in `state`. */
function backgroundWith(state, executionReady) {
  return {
    worker: { mode: 'supervised', state, restarts: 0, dropped_lines: 0 },
    execution: { ready: executionReady },
  };
}

test('an overdue queued build says that no execution worker is ready and how it ends', () => {
  const markup = rowMarkup(
    buildIn('queued'),
    { status: 'idle' },
    {
      overdue: true,
      background: backgroundWith('running', false),
    },
  );
  assert.match(markup, /No execution worker is ready, so this build waits/);
  assert.match(markup, /execution_unavailable after ten minutes/);
  assert.doesNotMatch(markup, /No worker is running/);
});

test('an overdue queued build shows no notice while an execution worker is ready', () => {
  const markup = rowMarkup(
    buildIn('queued'),
    { status: 'idle' },
    {
      overdue: true,
      background: backgroundWith('stopped', true),
    },
  );
  assert.doesNotMatch(markup, /No execution worker is ready/);
  assert.doesNotMatch(markup, /No worker is running/);
});

/** The markup of the head build status banner in `state`. */
function bannerMarkup(state) {
  return renderToStaticMarkup(
    createElement(BuildStatusBanner, { state, onRetry: () => {} }),
  );
}

test('the banner names the head as the newest revision, its failure and its job, and no diagnostic', () => {
  const markup = bannerMarkup({
    kind: 'read',
    status: {
      profile: 'react-v1',
      head_revision: 5,
      latest_build_revision: 3,
      status: 'failed',
      job_id: 'job_x',
      failure: 'build_failed',
      diagnostics: [{ message: 'Unexpected token in App.tsx' }],
    },
  });
  assert.match(markup, /Revision 5, the newest: build failed/);
  assert.match(markup, /The build reported errors\./);
  assert.match(markup, /Job job_x\./);
  assert.match(markup, /The latest build is of revision 3\./);
  assert.doesNotMatch(markup, /Unexpected token/);
});

test('the banner says when this server cannot build the profile', () => {
  assert.match(
    bannerMarkup({ kind: 'profile_unknown' }),
    /This server cannot build the react-v1 profile \(profile_unknown\)\./,
  );
});

test('the banner shows a failed read in words with a way to try again', () => {
  const markup = bannerMarkup({ kind: 'failed', message: 'Server down.' });
  assert.match(markup, /Could not read the build status/);
  assert.match(markup, /Server down\./);
  assert.match(markup, /Try again/);
});
