import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import { createAjv } from '../lib/schema.mjs';

// Rule 8 of decision 0055: the published api-v1 schema requires job_id on a
// job_changed change and forbids it on every other kind. Each refusal is
// checked against a body that differs from an accepted one only in the field
// under test, so a refusal cannot come from any other part of the body.

const schema = JSON.parse(
  await readFile(
    new URL('../../contracts/generated/api-v1.schema.json', import.meta.url),
    'utf8',
  ),
);
const validate = createAjv().compile({
  $schema: schema.$schema,
  $defs: schema.$defs,
  $ref: '#/$defs/ArtifactChange',
});

const VALID_JOB_ID = 'job_0123456789abcdef0123456789abcdef';
const UPPERCASE_JOB_ID = 'job_0123456789ABCDEF0123456789ABCDEF';

// The seven kinds other than job_changed, with the members each one requires.
const OTHER_KINDS = [
  { kind: 'created', fields: {} },
  { kind: 'revised', fields: {} },
  { kind: 'metadata_updated', fields: {} },
  { kind: 'deleted', fields: {} },
  { kind: 'recovered', fields: {} },
  { kind: 'purged', fields: {} },
  { kind: 'data_changed', fields: { data_version: 1 } },
];

/** The members every ArtifactChange has, whatever its kind. */
function artifactChange(kind, fields = {}) {
  return {
    cursor: 'cursor-1',
    artifact_id: 'art_0123456789abcdef0123456789abcdef',
    change: kind,
    revision: 1,
    metadata_revision: 1,
    committed_at_ms: 1767225600000,
    ...fields,
  };
}

/**
 * Asserts that `accepted` is valid and that `refused`, which differs from it
 * only in the field under test, is not.
 */
function assertRefusedOnlyFor(accepted, refused, description) {
  assert.equal(
    validate(accepted),
    true,
    `control for ${description} must be valid: ${JSON.stringify(validate.errors)}`,
  );
  assert.equal(validate(refused), false, `${description} must be refused`);
}

test('the other-kinds table covers every change kind but job_changed', () => {
  const published = schema.$defs.ChangeKind.oneOf.map((entry) => entry.const);
  assert.deepEqual(
    [...published].sort(),
    [...OTHER_KINDS.map((entry) => entry.kind), 'job_changed'].sort(),
  );
});

test('job_changed with a valid job_id passes', () => {
  assert.equal(
    validate(artifactChange('job_changed', { job_id: VALID_JOB_ID })),
    true,
    JSON.stringify(validate.errors),
  );
});

for (const [label, fields] of [
  ['without job_id', {}],
  ['with a null job_id', { job_id: null }],
  ['with an uppercase job_id', { job_id: UPPERCASE_JOB_ID }],
]) {
  test(`job_changed ${label} fails`, () => {
    assertRefusedOnlyFor(
      artifactChange('job_changed', { job_id: VALID_JOB_ID }),
      artifactChange('job_changed', fields),
      `job_changed ${label}`,
    );
  });
}

for (const { kind, fields } of OTHER_KINDS) {
  test(`${kind} with a job_id fails`, () => {
    assertRefusedOnlyFor(
      artifactChange(kind, fields),
      artifactChange(kind, { ...fields, job_id: VALID_JOB_ID }),
      `${kind} with a job_id`,
    );
  });
}

test('data_changed without data_version still fails', () => {
  assertRefusedOnlyFor(
    artifactChange('data_changed', { data_version: 1 }),
    artifactChange('data_changed'),
    'data_changed without data_version',
  );
});
