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
const { JobRow, createElement, renderToStaticMarkup } = await loadRow();

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

/** The markup of one row with the Cancel slot `cancel`, inside its list. */
function rowMarkup(job, cancel) {
  return renderToStaticMarkup(
    createElement(
      'ul',
      null,
      createElement(JobRow, {
        job,
        overdue: false,
        background: undefined,
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
