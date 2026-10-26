import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import { parse } from 'yaml';
import { checkWorkflow } from '../lib/workflow-policy.mjs';

const workflow = parse(
  await readFile(
    new URL('../../.github/workflows/ci.yml', import.meta.url),
    'utf8',
  ),
);

test('Browser image matches the installed Playwright version and has an immutable digest', async () => {
  const manifest = JSON.parse(
    await readFile(new URL('../../package.json', import.meta.url), 'utf8'),
  );
  const [tag, digest] =
    workflow.jobs.browsers.container.image.split('@sha256:');
  assert.equal(
    tag,
    `mcr.microsoft.com/playwright:v${manifest.devDependencies['@playwright/test']}-noble`,
  );
  assert.match(digest, /^[a-f\d]{64}$/);
});
test('The actual CI workflow enforces every verification job', () =>
  checkWorkflow(workflow));
test('An unpinned action is rejected', () => {
  const invalid = structuredClone(workflow);
  invalid.jobs.checks.steps[0].uses = 'actions/checkout@v4';
  assert.throws(() => checkWorkflow(invalid), /pinned to a commit/);
});
test('A newly added job cannot bypass the required aggregate', () => {
  const invalid = structuredClone(workflow);
  invalid.jobs.unreviewed = {
    'runs-on': 'ubuntu-latest',
    'timeout-minutes': 10,
    steps: [],
  };
  assert.throws(() => checkWorkflow(invalid), /include every verification job/);
});
test('A step cannot silently ignore failure', () => {
  const invalid = structuredClone(workflow);
  invalid.jobs.checks.steps[0]['continue-on-error'] = true;
  assert.throws(() => checkWorkflow(invalid), /Fail-open step/);
});
