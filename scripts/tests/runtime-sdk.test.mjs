import { execFileSync } from 'node:child_process';
import { test } from 'node:test';

test('A TypeScript consumer sees the optional runtime global', () => {
  execFileSync(
    process.execPath,
    [
      'node_modules/typescript/bin/tsc',
      '--noEmit',
      '--strict',
      '--target',
      'ES2022',
      '--module',
      'NodeNext',
      '--moduleResolution',
      'NodeNext',
      'scripts/tests/fixtures/runtime-sdk-consumer.ts',
    ],
    { stdio: 'inherit' },
  );
});
