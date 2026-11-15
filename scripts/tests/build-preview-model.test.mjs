import assert from 'node:assert/strict';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The build choice of an artifact's preview (decision 0058, rules 14 to 16, and
// the R03e split note, R03eb): which build is shown for the shown revision, the
// notice that names an older build, and what is said when there is none. The
// rules are bundled as they ship, with the client's error class from the same
// bundle so the `instanceof` checks hold.
const bundle = await build({
  stdin: {
    contents:
      "export * from './build-preview-model.ts'; export { ArtifreeApiError } from '@artifree/client';",
    resolveDir: fileURLToPath(
      new URL('../../apps/web/src/artifacts/', import.meta.url),
    ),
  },
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const {
  ArtifreeApiError,
  PROFILE_UNKNOWN_TEXT,
  buildTarget,
  missingBuildText,
  noBuildText,
  noticeText,
  statusOutcomeOf,
  ticketOutcomeOf,
} = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);

/** A read head status of revision `head` in `status`, with `fields`. */
function read(head, status, fields = {}) {
  return {
    kind: 'read',
    status: { profile: 'react-v1', head_revision: head, status, ...fields },
  };
}

const apiError = (status, code) =>
  new ArtifreeApiError(`Refused with ${code}.`, { status, code });

const CASES = [
  {
    name: 'an older revision asks for its own build and needs no status',
    shown: 2,
    newest: 5,
    status: undefined,
    target: { kind: 'ticket', revision: 2, notice: undefined },
  },
  {
    name: 'an older revision never takes the latest build, even when one is newer',
    shown: 2,
    newest: 5,
    status: read(5, 'built', { latest_build_revision: 5 }),
    target: { kind: 'ticket', revision: 2, notice: undefined },
  },
  {
    name: 'the newest revision waits for the head status',
    shown: 5,
    newest: 5,
    status: undefined,
    target: { kind: 'wait' },
  },
  {
    name: 'a built head is shown without a notice',
    shown: 5,
    newest: 5,
    status: read(5, 'built', { latest_build_revision: 5 }),
    target: { kind: 'ticket', revision: 5, notice: undefined },
  },
  {
    name: 'a pending head shows the latest build with a notice',
    shown: 5,
    newest: 5,
    status: read(5, 'pending', { job_id: 'job_x', latest_build_revision: 3 }),
    target: {
      kind: 'ticket',
      revision: 3,
      notice: { latest: 3, head: 5, headStatus: 'pending' },
    },
  },
  {
    name: 'a failed head shows the latest build with a notice',
    shown: 5,
    newest: 5,
    status: read(5, 'failed', {
      job_id: 'job_x',
      failure: 'build_failed',
      latest_build_revision: 4,
    }),
    target: {
      kind: 'ticket',
      revision: 4,
      notice: { latest: 4, head: 5, headStatus: 'failed' },
    },
  },
  {
    name: 'a head without any build asks for no ticket',
    shown: 5,
    newest: 5,
    status: read(5, 'none'),
    target: { kind: 'none', head: 'none' },
  },
  {
    name: 'a status of a newer head makes the shown revision an older one',
    shown: 5,
    newest: 5,
    status: read(6, 'pending', { job_id: 'job_x', latest_build_revision: 3 }),
    target: { kind: 'ticket', revision: 5, notice: undefined },
  },
  {
    name: 'a status of an older head is stale, so the choice waits',
    shown: 5,
    newest: 5,
    status: read(4, 'built', { latest_build_revision: 4 }),
    target: { kind: 'wait' },
  },
  {
    name: 'a profile the server cannot build is said as such',
    shown: 5,
    newest: 5,
    status: { kind: 'profile_unknown' },
    target: { kind: 'profile_unknown' },
  },
  {
    name: 'a failed status read is shown in words',
    shown: 5,
    newest: 5,
    status: { kind: 'failed', message: 'Down.' },
    target: { kind: 'failed', message: 'Down.' },
  },
];

for (const kase of CASES) {
  test(`buildTarget: ${kase.name}`, () => {
    assert.deepEqual(
      buildTarget(kase.shown, kase.newest, kase.status),
      kase.target,
    );
  });
}

test('a ticket refused as not_found or profile_unknown is kept apart from a failure', () => {
  assert.deepEqual(ticketOutcomeOf(apiError(404, 'not_found')), {
    kind: 'not_found',
  });
  assert.deepEqual(ticketOutcomeOf(apiError(422, 'profile_unknown')), {
    kind: 'profile_unknown',
  });
  assert.deepEqual(ticketOutcomeOf(apiError(503, 'unavailable')), {
    kind: 'failed',
    message: 'Refused with unavailable.',
  });
});

test('a status refused as profile_unknown is kept apart from a failure', () => {
  assert.deepEqual(statusOutcomeOf(apiError(422, 'profile_unknown')), {
    kind: 'profile_unknown',
  });
  assert.deepEqual(statusOutcomeOf(apiError(404, 'not_found')), {
    kind: 'failed',
    message: 'Refused with not_found.',
  });
});

test('the words name the build shown, the head and its status', () => {
  assert.equal(
    noticeText({ latest: 3, head: 5, headStatus: 'pending' }),
    'Showing the build of revision 3, the latest build. Revision 5, the newest, has a build pending.',
  );
  assert.equal(
    noticeText({ latest: 4, head: 5, headStatus: 'failed' }),
    'Showing the build of revision 4, the latest build. Revision 5, the newest, failed to build.',
  );
  assert.equal(
    noBuildText('none'),
    'No revision has a build yet. The newest revision has no build.',
  );
  assert.equal(missingBuildText(2), 'Revision 2 has no build.');
  assert.equal(
    PROFILE_UNKNOWN_TEXT,
    'This server cannot build the react-v1 profile (profile_unknown).',
  );
});
