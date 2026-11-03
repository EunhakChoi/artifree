import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import { parse } from 'yaml';
import { checkReleaseWorkflow } from '../lib/workflow-policy.mjs';

const read = async (name) =>
  parse(
    await readFile(
      new URL(`../../.github/workflows/${name}`, import.meta.url),
      'utf8',
    ),
  );
const release = await read('release.yml');
const clone = () => structuredClone(release);

test('the actual release workflow satisfies the publishing rules', () =>
  checkReleaseWorkflow(release));

test('the release workflow runs on version tags and manual dry runs only', () => {
  assert.deepEqual(Object.keys(release.on).sort(), [
    'push',
    'workflow_dispatch',
  ]);
  assert.deepEqual(release.on.push, { tags: ['v*.*.*'] });
});

test('the ordinary CI workflow never holds a write token', async () => {
  const ci = await read('ci.yml');
  assert.deepEqual(ci.permissions, { contents: 'read' });
  for (const job of Object.values(ci.jobs))
    assert.ok(!Object.values(job.permissions ?? {}).includes('write'));
});

const mutations = [
  [
    'a pull_request trigger',
    (w) => {
      w.on.pull_request = {};
    },
    /only on a tag push/,
  ],
  [
    'a branch push trigger',
    (w) => {
      w.on.push = { branches: ['main'] };
    },
    /only through version tags/,
  ],
  [
    'a dispatch input',
    (w) => {
      w.on.workflow_dispatch = { inputs: { publish: { type: 'boolean' } } };
    },
    /no inputs/,
  ],
  [
    'broad default permissions',
    (w) => {
      w.permissions = { contents: 'write' };
    },
    /grants nothing by default/,
  ],
  [
    'a repository secret',
    (w) => {
      w.jobs.build.env = { KEY: '${{ secrets.SIGNING_KEY }}' };
    },
    /no repository secret/,
  ],
  [
    'a job without declared permissions',
    (w) => {
      delete w.jobs.build.permissions;
    },
    /must declare its permissions/,
  ],
  [
    'a publishing job that also runs on dry runs',
    (w) => {
      w.jobs['publish-image'].if = "${{ github.ref_type == 'tag' }}";
    },
    /must require: github.event_name == 'push'/,
  ],
  [
    'a publishing job open to forks',
    (w) => {
      w.jobs.release.if =
        "${{ github.event_name == 'push' && github.ref_type == 'tag' }}";
    },
    /github.repository == 'EunhakChoi\/artifree'/,
  ],
  [
    'a publishing job without the release environment',
    (w) => {
      delete w.jobs.release.environment;
    },
    /needs the release environment/,
  ],
  [
    'a publishing job that does not wait for the checks',
    (w) => {
      w.jobs['publish-image'].needs = ['build'];
    },
    /wait for the build and image checks/,
  ],
  [
    'a write token in the build job',
    (w) => {
      w.jobs.build.permissions.contents = 'write';
    },
    /Only the publishing jobs may hold a write token: build/,
  ],
  [
    'a publishing job that lost its write token',
    (w) => {
      w.jobs.release.permissions = { contents: 'read' };
    },
    /Exactly the publishing jobs/,
  ],
  [
    'attestation rights outside the publishing jobs',
    (w) => {
      w.jobs.build.permissions['id-token'] = 'read';
    },
    /Only a publishing job may sign or attest/,
  ],
  [
    'a third publishing job',
    (w) => {
      w.jobs['image-check'].permissions.packages = 'write';
      w.jobs['image-check'].if = release.jobs.release.if;
      w.jobs['image-check'].environment = 'release';
      w.jobs['image-check'].needs = ['build', 'image-check'];
    },
    /Only the publishing jobs may hold a write token: image-check/,
  ],
  [
    'an unpinned action',
    (w) => {
      w.jobs.build.steps[0].uses = 'actions/checkout@v4';
    },
    /pinned to a commit/,
  ],
  [
    'a fail-open step',
    (w) => {
      w.jobs.build.steps[1]['continue-on-error'] = true;
    },
    /Fail-open step/,
  ],
  [
    'a checkout that keeps credentials',
    (w) => {
      w.jobs.build.steps[0].with['persist-credentials'] = true;
    },
    /must not keep the token/,
  ],
  [
    'the tag name interpolated into a script',
    (w) => {
      w.jobs['verify-tag'].steps.at(-1).run = 'echo ${{ github.ref_name }}';
    },
    /through env, not by interpolation/,
  ],
  [
    'a job without a timeout',
    (w) => {
      delete w.jobs.build['timeout-minutes'];
    },
    /Missing job timeout/,
  ],
];

for (const [name, mutate, pattern] of mutations) {
  test(`${name} is rejected`, () => {
    const invalid = clone();
    mutate(invalid);
    assert.throws(() => checkReleaseWorkflow(invalid), pattern);
  });
}
