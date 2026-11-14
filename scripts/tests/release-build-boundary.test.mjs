import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { checkReleaseBuilds } from '../lib/boundaries.mjs';

// Decision 0057 rule 2: release builds use the default features, while test lines of the
// workflows may name the enforcement fixture feature or --all-features.
const root = fileURLToPath(new URL('../../', import.meta.url));
const forbiddenNames = ['enforcement-fixture', '--all-features'];
// The same sources that scripts/check-boundaries.mjs passes to checkReleaseBuilds.
const wholePaths = [
  'scripts/lib/server-process.mjs',
  'scripts/package-release.mjs',
  'scripts/verify-container.mjs',
  'deploy/container/Dockerfile',
];
const workflowPaths = [
  '.github/workflows/ci.yml',
  '.github/workflows/release.yml',
];

const readRepositoryFile = async (path) => ({
  path,
  text: await readFile(join(root, path), 'utf8'),
});

test('the repository release build sources pass the release build check', async () => {
  checkReleaseBuilds({
    whole: await Promise.all(wholePaths.map(readRepositoryFile)),
    workflows: await Promise.all(workflowPaths.map(readRepositoryFile)),
  });
});

for (const name of forbiddenNames)
  for (const path of wholePaths)
    test(`a whole release source naming ${name} is rejected: ${path}`, () => {
      assert.throws(
        () =>
          checkReleaseBuilds({
            whole: [{ path, text: `release build with ${name} here\n` }],
            workflows: [],
          }),
        (error) => error.message.includes(`${path} names ${name}:`),
      );
    });

const buildLines = [
  {
    name: 'enforcement-fixture',
    line: '      - run: cargo build --locked -p artifree-server --features enforcement-fixture',
  },
  {
    name: '--all-features',
    line: '      - run: cargo build --release --all-features',
  },
  {
    name: 'enforcement-fixture',
    line: [
      '        run: |',
      '          cargo build --locked -p artifree-server \\',
      '            --features enforcement-fixture',
    ].join('\n'),
  },
];
for (const path of workflowPaths)
  for (const [index, { name, line }] of buildLines.entries())
    test(`a workflow cargo build line naming ${name} is rejected (${index + 1}): ${path}`, () => {
      assert.throws(
        () =>
          checkReleaseBuilds({
            whole: [],
            workflows: [{ path, text: line }],
          }),
        (error) => error.message.includes(`${path} builds with ${name}:`),
      );
    });

test('workflow test lines may name the enforcement fixture and --all-features', () => {
  checkReleaseBuilds({
    whole: [],
    workflows: [
      {
        path: '.github/workflows/ci.yml',
        text: [
          '      - run: cargo +1.89.0 test --workspace --all-features --locked',
          '      - run: cargo test -p artifree-server --features enforcement-fixture --test execution_jobs --no-run --locked',
        ].join('\n'),
      },
    ],
  });
});
