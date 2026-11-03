import assert from 'node:assert/strict';

/** Keep the required aggregate check fail-closed as CI gains jobs. */
export function checkWorkflow(workflow) {
  assert.deepEqual(
    workflow.permissions,
    { contents: 'read' },
    'CI must default to read-only permissions.',
  );
  assert.ok(
    Object.hasOwn(workflow.on, 'pull_request'),
    'CI must run on pull requests.',
  );
  const pullRequest = workflow.on.pull_request ?? {};
  assert.ok(
    !Object.hasOwn(pullRequest, 'paths') &&
      !Object.hasOwn(pullRequest, 'paths-ignore'),
    'Required CI cannot be skipped by path filters.',
  );
  const gate = workflow.jobs['quality-gate'];
  assert.equal(
    gate?.name,
    'Quality gate',
    'Keep the required status check name stable.',
  );
  assert.equal(
    gate.if,
    '${{ always() }}',
    'The aggregate must report failure when a dependency fails.',
  );
  assert.deepEqual(
    [...gate.needs].sort(),
    Object.keys(workflow.jobs)
      .filter((name) => name !== 'quality-gate')
      .sort(),
    'The quality gate must include every verification job.',
  );
  for (const [name, job] of Object.entries(workflow.jobs)) {
    assert.ok(
      !Object.values(job.permissions ?? {}).includes('write'),
      `CI job ${name} must not hold a write token; publishing lives in release.yml.`,
    );
    assert.ok(
      !Object.hasOwn(job, 'continue-on-error'),
      `Fail-open job: ${name}`,
    );
    assert.ok(
      Number.isInteger(job['timeout-minutes']) && job['timeout-minutes'] > 0,
      `Missing job timeout: ${name}`,
    );
    for (const step of job.steps ?? []) {
      assert.ok(
        !Object.hasOwn(step, 'continue-on-error'),
        `Fail-open step in ${name}`,
      );
      if (step.uses)
        assert.match(
          step.uses,
          /^[\w.-]+\/[\w./-]+@[a-f\d]{40}$/,
          `Action must be pinned to a commit: ${step.uses}`,
        );
    }
  }
}

const PUBLISHING_JOBS = ['publish-image', 'release'];
const PUBLISH_CONDITIONS = [
  "github.event_name == 'push'",
  "github.ref_type == 'tag'",
  "github.repository == 'EunhakChoi/artifree'",
];

/**
 * Rules for .github/workflows/release.yml (decision 0037). Publishing must be
 * impossible from a pull request, a fork, a branch push, or a dry run; only the
 * two named publishing jobs may hold a write token, and only after the build
 * and image checks; no secret except the job token exists; and nothing from the
 * tag or event is interpolated into a shell script.
 */
export function checkReleaseWorkflow(workflow) {
  assert.deepEqual(
    Object.keys(workflow.on).sort(),
    ['push', 'workflow_dispatch'],
    'The release workflow runs only on a tag push and a manual dry run.',
  );
  assert.deepEqual(
    workflow.on.push,
    { tags: ['v*.*.*'] },
    'A push may trigger a release only through version tags.',
  );
  assert.ok(
    !Object.hasOwn(workflow.on.workflow_dispatch ?? {}, 'inputs'),
    'The manual run has no inputs; it is always a dry run.',
  );
  assert.deepEqual(
    workflow.permissions,
    {},
    'The release workflow grants nothing by default; each job lists its own.',
  );
  assert.ok(
    !/secrets\.(?!GITHUB_TOKEN\b)/.test(JSON.stringify(workflow)),
    'Only the job token may be used; no repository secret.',
  );
  const privileged = [];
  for (const [name, job] of Object.entries(workflow.jobs)) {
    assert.ok(
      job.permissions && typeof job.permissions === 'object',
      `Job ${name} must declare its permissions.`,
    );
    assert.ok(
      !Object.hasOwn(job, 'continue-on-error'),
      `Fail-open job: ${name}`,
    );
    assert.ok(
      Number.isInteger(job['timeout-minutes']) && job['timeout-minutes'] > 0,
      `Missing job timeout: ${name}`,
    );
    if (Object.values(job.permissions).includes('write')) {
      privileged.push(name);
      assert.ok(
        PUBLISHING_JOBS.includes(name),
        `Only the publishing jobs may hold a write token: ${name}`,
      );
      for (const condition of PUBLISH_CONDITIONS)
        assert.ok(
          String(job.if).includes(condition),
          `Publishing job ${name} must require: ${condition}`,
        );
      assert.equal(
        job.environment,
        'release',
        `${name} needs the release environment.`,
      );
      assert.ok(
        ['build', 'image-check'].every((need) => job.needs?.includes(need)),
        `${name} must wait for the build and image checks.`,
      );
    } else {
      assert.ok(
        !Object.hasOwn(job.permissions, 'id-token') &&
          !Object.hasOwn(job.permissions, 'attestations'),
        `Only a publishing job may sign or attest: ${name}`,
      );
    }
    for (const step of job.steps ?? []) {
      assert.ok(
        !Object.hasOwn(step, 'continue-on-error'),
        `Fail-open step in ${name}`,
      );
      assert.ok(
        !/\$\{\{[^}]*github\.(ref|ref_name|head_ref|event)\b/.test(
          step.run ?? '',
        ),
        `Pass ref data to a script through env, not by interpolation: ${name}`,
      );
      if (step.uses) {
        assert.match(
          step.uses,
          /^[\w.-]+\/[\w./-]+@[a-f\d]{40}$/,
          `Action must be pinned to a commit: ${step.uses}`,
        );
        if (step.uses.startsWith('actions/checkout@'))
          assert.equal(
            step.with?.['persist-credentials'],
            false,
            `Checkout must not keep the token: ${name}`,
          );
      }
    }
  }
  assert.deepEqual(
    privileged.sort(),
    PUBLISHING_JOBS,
    'Exactly the publishing jobs may hold a write token.',
  );
}
