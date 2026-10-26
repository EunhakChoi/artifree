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
