import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';

const NEXTEST_CONFIG = new URL('../.config/nextest.toml', import.meta.url);

function pinnedNextestVersion() {
  const version = /^\s*nextest-version\s*=\s*"([^"]+)"/m.exec(
    readFileSync(NEXTEST_CONFIG, 'utf8'),
  )?.[1];
  if (!version) {
    throw new Error(
      '.config/nextest.toml must pin nextest-version = "<version>".',
    );
  }
  return version;
}

// The tests run only under cargo-nextest. The probe runs first, so a missing or broken install fails before the
// minutes-long clippy step. It never falls back to cargo test.
const probe = spawnSync('cargo', ['nextest', '--version'], {
  encoding: 'utf8',
});
if (probe.error) {
  process.stderr.write(
    `Could not run \`cargo nextest --version\`: ${probe.error.message}\n`,
  );
  process.exit(1);
}
if (probe.status !== 0) {
  const version = pinnedNextestVersion();
  const outcome = probe.signal
    ? `killed by ${probe.signal}`
    : `exit code ${probe.status}`;
  // Show the probe's own output, so a failure other than a missing install is not hidden.
  const probeOutput = [probe.stdout, probe.stderr]
    .map((text) => text.trim())
    .filter((text) => text !== '');
  const lines = [
    `cargo-nextest ${version} is required to run the Rust tests, and \`cargo nextest --version\` failed (${outcome}).`,
    ...probeOutput,
    `Install the pinned version: cargo install cargo-nextest --version ${version} --locked`,
    'CI installs the same release archive from https://github.com/nextest-rs/nextest/releases.',
  ];
  process.stderr.write(`${lines.join('\n')}\n`);
  process.exit(1);
}

// Keep the same gate on Windows and Unix; shell env assignments are not portable.
const commands = [
  ['fmt', '--all', '--', '--check'],
  [
    'clippy',
    '--workspace',
    '--all-targets',
    '--all-features',
    '--locked',
    '--',
    '-D',
    'warnings',
  ],
  // --no-fail-fast runs every test even after one fails, so one run reports all failures. The gate sets
  // fail-fast here, not in .config/nextest.toml.
  [
    'nextest',
    'run',
    '--workspace',
    '--all-features',
    '--locked',
    '--no-fail-fast',
  ],
  // nextest does not run documentation tests, so they run as their own step.
  ['test', '--doc', '--workspace', '--all-features', '--locked'],
  ['doc', '--workspace', '--all-features', '--no-deps', '--locked'],
];
for (const args of commands) {
  const result = spawnSync('cargo', args, {
    stdio: 'inherit',
    env: {
      ...process.env,
      RUSTDOCFLAGS: `${process.env.RUSTDOCFLAGS ?? ''} -D warnings`.trim(),
    },
  });
  if (result.error) throw result.error;
  if (result.status !== 0) process.exit(result.status ?? 1);
}
