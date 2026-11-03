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
    'a publishing job with its own environment (a second approval)',
    (w) => {
      w.jobs.release.environment = 'release';
    },
    /Exactly one job, approve, uses an environment/,
  ],
  [
    'an approval job without the environment',
    (w) => {
      delete w.jobs.approve.environment;
    },
    /Exactly one job, approve, uses an environment/,
  ],
  [
    'an approval job in another environment',
    (w) => {
      w.jobs.approve.environment = 'staging';
    },
    /approve needs the release environment/,
  ],
  [
    'an approval job with token rights',
    (w) => {
      w.jobs.approve.permissions = { contents: 'read' };
    },
    /approval job needs no token rights/,
  ],
  [
    'an approval job that also runs on dry runs',
    (w) => {
      w.jobs.approve.if = "${{ github.ref_type == 'tag' }}";
    },
    /approve must require: github.event_name == 'push'/,
  ],
  [
    'a publishing job that does not wait for the approval',
    (w) => {
      w.jobs.release.needs = ['build', 'image-check'];
    },
    /wait for the build and image checks and the approval/,
  ],
  [
    'a publishing job that does not wait for the checks',
    (w) => {
      w.jobs['publish-image'].needs = ['approve', 'release'];
    },
    /wait for the build and image checks and the approval/,
  ],
  [
    'the image pushed before the draft release',
    (w) => {
      w.jobs['publish-image'].needs = ['build', 'image-check', 'approve'];
    },
    /after the draft release/,
  ],
  [
    'a release job that no longer rechecks the tag',
    (w) => {
      for (const step of w.jobs.release.steps)
        if (step.run)
          step.run = step.run.replace('release-assets.mjs check-tag', 'true');
    },
    /must run before publishing: release-assets.mjs check-tag/,
  ],
  [
    'a release job without an archive attestation',
    (w) => {
      w.jobs.release.steps = w.jobs.release.steps.filter(
        (step) => !step.uses?.startsWith('actions/attest-build-provenance@'),
      );
    },
    /each attested, once/,
  ],
  [
    'a release job that no longer rechecks CI',
    (w) => {
      for (const step of w.jobs.release.steps)
        if (step.run)
          step.run = step.run.replace('release-assets.mjs check-ci', 'true');
    },
    /must run before publishing: release-assets.mjs check-ci/,
  ],
  [
    'a release job that no longer rechecks main',
    (w) => {
      for (const step of w.jobs.release.steps)
        if (step.run)
          step.run = step.run.replace('git merge-base --is-ancestor', 'true');
    },
    /must run before publishing: git merge-base --is-ancestor/,
  ],
  [
    'a release job that does not verify the asset set',
    (w) => {
      for (const step of w.jobs.release.steps)
        if (step.run) step.run = step.run.replace('check-set', 'sums');
    },
    /must run before publishing: release-assets.mjs check-set/,
  ],
  [
    'a release that is not a draft',
    (w) => {
      for (const step of w.jobs.release.steps)
        if (step.run) step.run = step.run.replace('--draft', '');
    },
    /must run before publishing: --draft/,
  ],
  [
    'an image push that may overwrite a tag',
    (w) => {
      for (const step of w.jobs['publish-image'].steps)
        if (step.run)
          step.run = step.run.replace('docker manifest inspect', 'true');
    },
    /refuse to overwrite an existing tag/,
  ],
  [
    'a storage record on the image attestation',
    (w) => {
      delete w.jobs['publish-image'].steps.at(-1).with['create-storage-record'];
    },
    /Storage records need an organization-owned repository/,
  ],
  [
    'an image attestation that is not pushed to the registry',
    (w) => {
      w.jobs['publish-image'].steps.at(-1).with['push-to-registry'] = false;
    },
    /pushed to the registry too/,
  ],
  [
    'an archive attestation pushed to a registry',
    (w) => {
      w.jobs.release.steps.find((step) =>
        step.uses?.startsWith('actions/attest-build-provenance@'),
      ).with['push-to-registry'] = true;
    },
    /not pushed to a registry/,
  ],
  [
    'an archive attestation of other files',
    (w) => {
      w.jobs.release.steps.find((step) =>
        step.uses?.startsWith('actions/attest-build-provenance@'),
      ).with['subject-checksums'] = 'dist/OTHER';
    },
    /dist\/SHA256SUMS/,
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

test('the documented verification commands match what the workflow attests', async () => {
  const docs = await readFile(
    new URL('../../docs/release-packages.md', import.meta.url),
    'utf8',
  );
  const signer = 'EunhakChoi/artifree/.github/workflows/release.yml';
  const flags = `--repo EunhakChoi/artifree --signer-workflow ${signer}`;
  // Default retrieval (the attestations API), no --bundle: the action always
  // uploads there. The workflow's own policy test pins the attest settings.
  assert.deepEqual(
    docs.split('\n').filter((line) => line.startsWith('gh attestation verify')),
    [
      `gh attestation verify artifree-<version>-linux-x64.tar.gz ${flags}`,
      `gh attestation verify oci://ghcr.io/eunhakchoi/artifree:<version> ${flags}`,
    ],
  );
});
