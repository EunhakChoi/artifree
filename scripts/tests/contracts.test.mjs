import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import { createAjv } from '../lib/schema.mjs';

const schema = JSON.parse(
  await readFile(
    new URL(
      '../../contracts/generated/service-description.schema.json',
      import.meta.url,
    ),
    'utf8',
  ),
);
const inventory = JSON.parse(
  await readFile(
    new URL(
      '../../contracts/generated/scaffold-inventory.json',
      import.meta.url,
    ),
    'utf8',
  ),
);
const validate = createAjv().compile(schema);

test('Generated inventory satisfies the actual Rust schema', () => {
  assert.equal(validate(inventory), true, JSON.stringify(validate.errors));
});

for (const [name, mutate, expectedPath] of [
  [
    'an unknown top-level field',
    (value) => {
      value.unreviewed = true;
    },
    '',
  ],
  [
    'an unknown component field',
    (value) => {
      value.components[0].unreviewed = true;
    },
    '/components/0',
  ],
  [
    'a missing required name',
    (value) => {
      delete value.name;
    },
    '',
  ],
  [
    'a wrong scalar type',
    (value) => {
      value.http_api = 'true';
    },
    '/http_api',
  ],
  [
    'an unknown layer',
    (value) => {
      value.components[0].layer = 'database';
    },
    '/components/0/layer',
  ],
  [
    'an unknown status',
    (value) => {
      value.components[0].status = 'deprecated';
    },
    '/components/0/status',
  ],
]) {
  test(`Schema rejects ${name}`, () => {
    const invalid = structuredClone(inventory);
    mutate(invalid);
    assert.equal(validate(invalid), false);
    assert.ok(
      validate.errors.some((error) => error.instancePath === expectedPath),
      JSON.stringify(validate.errors),
    );
  });
}

const api = JSON.parse(
  await readFile(
    new URL('../../contracts/generated/api-v1.schema.json', import.meta.url),
    'utf8',
  ),
);
const apiAjv = createAjv();
apiAjv.addSchema(api);
const body = (name) => {
  const check = apiAjv.getSchema(`${api.$id}#/$defs/${name}`);
  assert.ok(check, `Missing API body schema: ${name}`);
  return check;
};
const saveRequest = () => ({
  base_revision: 1,
  changes: [
    { op: 'put', path: 'index.html', encoding: 'utf8', content: '<p>x</p>' },
    { op: 'delete', path: 'old.css' },
  ],
});

test('API schema accepts a revision commit with put and delete changes', () => {
  const check = body('SaveRevisionRequest');
  assert.equal(check(saveRequest()), true, JSON.stringify(check.errors));
});

for (const [name, mutate] of [
  ['an unknown change operation', (value) => (value.changes[1].op = 'move')],
  ['content on a delete', (value) => (value.changes[1].content = 'x')],
  ['a missing encoding', (value) => delete value.changes[0].encoding],
  ['a negative base revision', (value) => (value.base_revision = -1)],
  ['an unknown top-level field', (value) => (value.force = true)],
]) {
  test(`API schema rejects ${name}`, () => {
    const invalid = saveRequest();
    mutate(invalid);
    assert.equal(body('SaveRevisionRequest')(invalid), false);
  });
}

test('API error bodies carry a known code and optional current revision', () => {
  const check = body('ErrorResponse');
  const conflict = {
    error: { code: 'revision_conflict', message: 'stale', current_revision: 2 },
  };
  assert.equal(check(conflict), true, JSON.stringify(check.errors));
  assert.equal(
    check({ error: { code: 'teapot', message: 'no' } }),
    false,
    'Unknown error codes must not validate.',
  );
});

test('A runtime view always states its SDK pin, including null for ordinary previews', () => {
  const check = body('RuntimeView');
  const view = {
    url: 'http://127.0.0.1:7411/p/example/',
    enabled: false,
    session_id: 'e'.repeat(32),
    artifact_id: 'art_0',
    revision: 1,
    head_revision: 1,
    sdk: null,
    declared: [],
    supported: [],
    granted: [],
    grant_version: 0,
    view_id: 'a'.repeat(32),
    generation: 1,
    initialization: 'b'.repeat(64),
    expires_at_ms: 1,
    lease_ms: 1,
  };
  assert.equal(check(view), true, JSON.stringify(check.errors));
  assert.equal(check({ ...view, sdk: 'artifree.app.v1' }), true);
  const missing = { ...view };
  delete missing.sdk;
  assert.equal(check(missing), false, 'A missing SDK field must not validate.');
  const missingSession = { ...view };
  delete missingSession.session_id;
  assert.equal(check(missingSession), false, 'A view must name its session.');
  assert.equal(check({ ...view, sdk: 12 }), false);
});
