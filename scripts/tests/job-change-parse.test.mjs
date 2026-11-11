import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  ArtifreeApiError,
  ArtifreeClient,
} from '../../packages/client/dist/index.js';

// Decision 0055, rules 8 and 11: a job_changed change names its job in job_id,
// which is `job_` and 32 lowercase hex digits. No other kind carries job_id,
// and a breach of either rule makes the event invalid.

const ARTIFACT_ID = `art_${'0'.repeat(32)}`;
const FEED = 'f'.repeat(32);
const JOB_ID = 'job_0123456789abcdef0123456789abcdef';

/** The data of a change of `kind` at sequence 3, with `fields` laid over it. */
function changeData(kind, fields) {
  return JSON.stringify({
    cursor: `${FEED}.3`,
    artifact_id: ARTIFACT_ID,
    change: kind,
    revision: 3,
    metadata_revision: 1,
    committed_at_ms: 1,
    ...fields,
  });
}

/** The server-sent text of one change event of `kind` with `fields`. */
function sseChange(kind, fields) {
  return `event: change\ndata: ${changeData(kind, fields)}\n\n`;
}

/** The event the client must deliver for that change, with `extra` kept. */
function expectedEvent(change, extra = {}) {
  return {
    type: 'change',
    change: {
      cursor: `${FEED}.3`,
      artifact_id: ARTIFACT_ID,
      change,
      revision: 3,
      metadata_revision: 1,
      committed_at_ms: 1,
      ...extra,
    },
  };
}

/** A successful event stream whose body is `text`. */
function eventStream(text) {
  return new Response(text, {
    status: 200,
    headers: { 'content-type': 'text/event-stream; charset=utf-8' },
  });
}

/** The feed events that one `openChanges` call delivers for `text`. */
async function eventsOf(text) {
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async () => eventStream(text),
  });
  const events = [];
  for await (const event of await client.openChanges()) events.push(event);
  return events;
}

/** The error that `promise` rejects with; a promise that resolves fails. */
async function failure(promise) {
  const error = await promise.then(
    () => assert.fail('The request must fail.'),
    (rejection) => rejection,
  );
  assert.ok(error instanceof ArtifreeApiError, String(error));
  return error;
}

test('A job change with a job id is delivered with that job id', async () => {
  const events = await eventsOf(sseChange('job_changed', { job_id: JOB_ID }));
  assert.deepEqual(events, [expectedEvent('job_changed', { job_id: JOB_ID })]);
});

/** Job changes that break rule 8, with one field wrong each. */
const BROKEN_JOB_CHANGES = [
  ['no job id', {}],
  ['a null job id', { job_id: null }],
  ['a numeric job id', { job_id: 7 }],
  ['a job id with uppercase hex digits', { job_id: `job_${'A'.repeat(32)}` }],
  ['a job id with 31 hex digits', { job_id: `job_${'a'.repeat(31)}` }],
  ['a job id with 33 hex digits', { job_id: `job_${'a'.repeat(33)}` }],
  ['a job id with a non-hex digit', { job_id: `job_${'g'.repeat(32)}` }],
  ['a job id with another prefix', { job_id: `art_${'a'.repeat(32)}` }],
  ['a data version', { job_id: JOB_ID, data_version: 1 }],
  ['a null data version', { job_id: JOB_ID, data_version: null }],
];

for (const [name, fields] of BROKEN_JOB_CHANGES) {
  test(`A job change with ${name} is invalid`, async () => {
    const broken = await failure(eventsOf(sseChange('job_changed', fields)));
    assert.deepEqual([broken.status, broken.code], [200, 'invalid_response']);
  });
}

/** The other known kinds, each with the fields that make its change valid. */
const OTHER_KINDS = [
  ['created', {}],
  ['revised', {}],
  ['metadata_updated', {}],
  ['deleted', {}],
  ['recovered', {}],
  ['purged', {}],
  ['data_changed', { data_version: 1 }],
];

for (const [kind, fields] of OTHER_KINDS) {
  // The valid form is delivered, so the invalid form below fails on job_id.
  test(`A ${kind} change without a job id is delivered`, async () => {
    const events = await eventsOf(sseChange(kind, fields));
    assert.deepEqual(events, [expectedEvent(kind, fields)]);
  });

  test(`A ${kind} change with a job id is invalid`, async () => {
    const broken = await failure(
      eventsOf(sseChange(kind, { ...fields, job_id: JOB_ID })),
    );
    assert.deepEqual([broken.status, broken.code], [200, 'invalid_response']);
  });
}

test('A change kind this client does not know is delivered as unknown', async () => {
  // A field beyond the contract is not read, so the job id is dropped.
  const events = await eventsOf(sseChange('renamed', { job_id: JOB_ID }));
  assert.deepEqual(events, [expectedEvent('unknown')]);
});
