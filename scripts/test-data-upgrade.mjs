import { parseArgs } from 'node:util';
import { resolve } from 'node:path';
import { checkDataRecovery } from './lib/data-recovery.mjs';

const { values } = parseArgs({
  options: {
    'previous-server': { type: 'string' },
    server: { type: 'string' },
  },
});
if (!values['previous-server'] || !values.server)
  throw new Error(
    'Pass --previous-server <pre-0046 executable> and --server <current executable>.',
  );
await checkDataRecovery(
  resolve(values['previous-server']),
  resolve(values.server),
  { upgrade: true },
);
console.log(
  JSON.stringify({
    event: 'data_upgrade_verified',
    previous: 'ASCII paths',
    current: '0046 UTF-8 paths',
  }),
);
