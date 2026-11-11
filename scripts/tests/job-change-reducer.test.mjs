import assert from 'node:assert/strict';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The reducer shares its module with React hooks and the client. Bundle the
// module as it ships and import only the pure reducer and its start state.
const bundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL(
        '../../apps/web/src/artifacts/use-live-workspace.ts',
        import.meta.url,
      ),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { START, learnFrom } = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);

const ARTIFACT = `art_${'a'.repeat(32)}`;
const JOB = `job_${'c'.repeat(32)}`;
const CURSOR = `${'f'.repeat(32)}.1`;

/**
 * A committed change of `ARTIFACT` as the change feed delivers it. A job
 * change names its job and carries the artifact's current versions.
 */
function changeEvent(kind, extra = {}) {
  return {
    type: 'change',
    change: {
      artifact_id: ARTIFACT,
      change: kind,
      cursor: CURSOR,
      committed_at_ms: 1,
      revision: 2,
      metadata_revision: 1,
      ...extra,
    },
  };
}

test('a job change for an artifact the page does not know changes nothing', () => {
  // Decision 0055, rule 11: the web's reducer leaves artifact state as it is
  // for a job change, as it does for a data change.
  const next = learnFrom(START, {
    type: 'event',
    event: changeEvent('job_changed', { job_id: JOB }),
  });
  assert.equal(next.news, 0);
  assert.equal(next, START);
});

test('a revision of an artifact the page does not know is news', () => {
  // The control: a revision of the same artifact is news, so the case above
  // does not pass merely because the reducer ignores every change.
  const next = learnFrom(START, {
    type: 'event',
    event: changeEvent('revised'),
  });
  assert.equal(next.news, 1);
  assert.deepEqual(next.known, { [ARTIFACT]: { revision: 2, metadata: 1 } });
});
