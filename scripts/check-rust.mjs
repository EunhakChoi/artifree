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

// The tests run only under cargo-nextest. A missing install stops the gate; it never falls back to cargo test.
const probe = spawnSync('cargo', ['nextest', '--version'], { stdio: 'ignore' });
if (probe.error) throw probe.error;
if (probe.status !== 0) {
  const version = pinnedNextestVersion();
  process.stderr.write(
    `cargo-nextest ${version} is required to run the Rust tests, and it is not installed.\n` +
      `Install the pinned version: cargo install cargo-nextest --version ${version} --locked\n` +
      'CI installs the same release archive from https://github.com/nextest-rs/nextest/releases.\n',
  );
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
