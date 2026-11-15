import assert from 'node:assert/strict';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// Decision 0060 rule 14: a change of an artifact's access keeps its source
// and metadata revisions, yet may change what this user reaches of it. The
// web's reducer takes it as news of the artifact, to be read again.

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
const CURSOR = `${'f'.repeat(32)}.1`;

/** A change of `ARTIFACT` at revision 2 and metadata revision 1. */
function changeEvent(kind) {
  return {
    type: 'change',
    change: {
      artifact_id: ARTIFACT,
      change: kind,
      cursor: CURSOR,
      type: null,
      committed_at_ms: 1,
      revision: 2,
      metadata_revision: 1,
    },
  };
}

/** The start state once a read showed `ARTIFACT` at the event's versions. */
const KNOWN = learnFrom(START, {
  type: 'event',
  event: changeEvent('revised'),
});

for (const kind of ['access_granted', 'access_changed', 'access_revoked']) {
  test(`${kind} of a known artifact at its versions is news to read again`, () => {
    const next = learnFrom(KNOWN, { type: 'event', event: changeEvent(kind) });
    assert.equal(next.news, KNOWN.news + 1);
    assert.deepEqual(next.unknown, { [ARTIFACT]: 1 });
    assert.deepEqual(next.known, KNOWN.known);
  });
}

test('a revision the page already knows is not news', () => {
  // The control: the same versions under a kind that changes them are
  // nothing new, so the cases above do not pass by the versions alone.
  const next = learnFrom(KNOWN, {
    type: 'event',
    event: changeEvent('revised'),
  });
  assert.equal(next, KNOWN);
});
