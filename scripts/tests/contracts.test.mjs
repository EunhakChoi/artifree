import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import Ajv2020 from 'ajv/dist/2020.js';

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
const validate = new Ajv2020({ strict: true, allErrors: true }).compile(schema);

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
      value.http_listening = 'false';
    },
    '/http_listening',
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
      value.components[0].status = 'implemented';
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
