import assert from 'node:assert/strict';
import { existsSync } from 'node:fs';
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
test('Browser container user matches the GitHub-mounted home owner', () => {
  assert.match(
    workflow.jobs.browsers.container.options,
    /(?:^|\s)--user\s+1001(?:\s|$)/,
    'Firefox requires the process user to own the mounted home directory; keep it.',
  );
});
test('Browser jobs run Chromium and Firefox', () => {
  assert.deepEqual(
    [...workflow.jobs.browsers.strategy.matrix.browser].sort(),
    ['chromium', 'firefox'],
    'docs/quality.md "Browser engines": CI runs the smoke in both engines.',
  );
});
test('Browser jobs run both smoke shards', () => {
  assert.deepEqual(
    [...workflow.jobs.browsers.strategy.matrix.shard].sort(),
    ['outbox', 'ui'],
    'scripts/smoke.mjs runs as the outbox and ui shards in CI; a missing shard leaves its checks unrun.',
  );
});
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
test('Browser jobs test the server binary built for them', () => {
  const browsers = workflow.jobs.browsers;
  assert.ok(
    browsers.needs.includes('server-binary'),
    'Browser tests must wait for the server binary.',
  );
  assert.equal(
    browsers.env.ARTIFREE_SERVER_BIN,
    '.artifree/bin/artifree-server',
  );
  assert.ok(
    browsers.steps.some((step) =>
      step.uses?.startsWith('actions/download-artifact@'),
    ),
    'Browser tests must download the binary instead of skipping the server.',
  );
});
test('A CI job cannot hold a write token', () => {
  const invalid = structuredClone(workflow);
  invalid.jobs.checks.permissions = { contents: 'write' };
  assert.throws(() => checkWorkflow(invalid), /must not hold a write token/);
});

const LINUX_NEXTEST = 'Install pinned cargo-nextest (Linux)';
const WINDOWS_NEXTEST = 'Install pinned cargo-nextest (Windows)';
const LINUX_VERSION = /^\s*nextest_version=(\S+)$/m;
const WINDOWS_VERSION = /^\s*\$nextestVersion = '([^']+)'$/m;
const RUST_CHECKS = /^npm run (?:check|check:rust)$/m;
const NEXTEST_TOML = new URL('../../.config/nextest.toml', import.meta.url);

function checksStep(name) {
  const step = workflow.jobs.checks.steps.find(
    (candidate) => candidate.name === name,
  );
  assert.ok(step, `The checks job needs the step "${name}".`);
  return step;
}

/** Index of a fragment that the install step must contain. */
function requiredIndex(text, fragment) {
  const index = text.indexOf(fragment);
  assert.notEqual(index, -1, `The install step must contain: ${fragment}`);
  return index;
}

function occurrences(text, fragment) {
  return text.split(fragment).length - 1;
}

test('The checks job installs cargo-nextest on Linux and Windows before npm run check', () => {
  const steps = workflow.jobs.checks.steps;
  const check = steps.findIndex((step) => step.run === 'npm run check');
  assert.notEqual(check, -1, 'The checks job must run npm run check.');
  const linux = checksStep(LINUX_NEXTEST);
  const windows = checksStep(WINDOWS_NEXTEST);
  assert.equal(linux.if, "${{ runner.os == 'Linux' }}");
  assert.equal(linux.shell, 'bash');
  assert.equal(windows.if, "${{ runner.os == 'Windows' }}");
  assert.equal(windows.shell, 'pwsh');
  assert.ok(
    steps.indexOf(linux) < check && steps.indexOf(windows) < check,
    'cargo-nextest must be installed before npm run check, which runs check:rust.',
  );
});

test('The Linux cargo-nextest install fetches over HTTPS, verifies sha256, then extracts', () => {
  const run = checksStep(LINUX_NEXTEST).run;
  assert.match(
    run,
    /^\s*set -euo pipefail$/m,
    'A failed checksum must stop the step.',
  );
  assert.match(
    run,
    /^\s*nextest_sha256=[a-f\d]{64}$/m,
    'The expected sha256 must be pinned in the step.',
  );
  const download = requiredIndex(run, "curl --proto '=https' --tlsv1.2 --fail");
  const verify = requiredIndex(
    run,
    'echo "$nextest_sha256  $nextest_archive" | sha256sum -c -',
  );
  const extract = requiredIndex(run, 'tar -xzf "$nextest_archive"');
  assert.ok(
    download < verify && verify < extract,
    'The archive must be downloaded, verified, and only then extracted.',
  );
});

test('The Windows cargo-nextest install fetches, verifies sha256, then extracts', () => {
  const run = checksStep(WINDOWS_NEXTEST).run;
  assert.match(
    run,
    /^\s*\$ErrorActionPreference = 'Stop'$/m,
    'A failed download or checksum must stop the step.',
  );
  assert.match(
    run,
    /^\s*\$nextestSha256 = '[a-f\d]{64}'$/m,
    'The expected sha256 must be pinned in the step.',
  );
  const download = requiredIndex(
    run,
    'Invoke-WebRequest -Uri "https://github.com/nextest-rs/nextest/releases/download/',
  );
  const hash = requiredIndex(
    run,
    '(Get-FileHash -Algorithm SHA256 -LiteralPath $nextestArchive).Hash',
  );
  const compare = requiredIndex(
    run,
    'if ($actualSha256 -ne $nextestSha256) { throw',
  );
  const extract = requiredIndex(
    run,
    'Expand-Archive -LiteralPath $nextestArchive',
  );
  assert.ok(
    download < hash && hash < compare && compare < extract,
    'The archive must be downloaded, hashed, compared, and only then extracted.',
  );
});

test('Each cargo-nextest install writes its version once, and both platforms agree', () => {
  const linux = checksStep(LINUX_NEXTEST).run;
  const windows = checksStep(WINDOWS_NEXTEST).run;
  const linuxVersion = LINUX_VERSION.exec(linux)?.[1];
  const windowsVersion = WINDOWS_VERSION.exec(windows)?.[1];
  assert.match(
    linuxVersion ?? '',
    /^\d+\.\d+\.\d+$/,
    'The Linux install must pin an exact version.',
  );
  assert.equal(
    windowsVersion,
    linuxVersion,
    'Linux and Windows must install the same cargo-nextest version.',
  );
  assert.equal(occurrences(linux, linuxVersion), 1);
  assert.equal(occurrences(windows, windowsVersion), 1);
  const comment = /# Must match nextest-version in \.config\/nextest\.toml\./;
  assert.match(linux, comment, 'Document the Linux version link.');
  assert.match(windows, comment, 'Document the Windows version link.');
});

test('.config/nextest.toml declares the cargo-nextest version that both CI installs pin', async () => {
  assert.ok(
    existsSync(NEXTEST_TOML),
    'The repository must contain .config/nextest.toml, which declares the cargo-nextest version CI installs.',
  );
  const toml = await readFile(NEXTEST_TOML, 'utf8');
  const tomlVersion = /^\s*nextest-version\s*=\s*"([^"]+)"/m.exec(toml)?.[1];
  assert.match(
    tomlVersion ?? '',
    /^\d+\.\d+\.\d+$/,
    '.config/nextest.toml must declare nextest-version as an exact version.',
  );
  assert.equal(
    tomlVersion,
    LINUX_VERSION.exec(checksStep(LINUX_NEXTEST).run)?.[1],
    'The Linux cargo-nextest install must pin the version in .config/nextest.toml.',
  );
  assert.equal(
    tomlVersion,
    WINDOWS_VERSION.exec(checksStep(WINDOWS_NEXTEST).run)?.[1],
    'The Windows cargo-nextest install must pin the version in .config/nextest.toml.',
  );
});

test('Every job that runs the Rust checks installs cargo-nextest', () => {
  const missing = Object.entries(workflow.jobs)
    .filter(([, job]) =>
      (job.steps ?? []).some((step) => RUST_CHECKS.test(step.run ?? '')),
    )
    .filter(
      ([, job]) =>
        !(job.steps ?? []).some((step) =>
          step.name?.startsWith('Install pinned cargo-nextest'),
        ),
    )
    .map(([name]) => name);
  assert.deepEqual(
    missing,
    [],
    'These jobs run the Rust checks without installing cargo-nextest.',
  );
});
