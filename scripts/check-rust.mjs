import { spawnSync } from 'node:child_process';

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
  ['test', '--workspace', '--all-features', '--locked'],
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
