#!/usr/bin/env node
import { parseArgs } from 'node:util';
import { scaffoldDescription } from '@artifree/contracts';

const help = `Artifree — open artifacts, any agent.

Usage:
  artifree describe [--json]  Inspect the build-time scaffold inventory
  artifree --version         Print the scaffold version
  artifree --help            Show this help

Scaffolding only: publish, serve, storage, and agent connections are not implemented.
The description is bundled at build time; it does not probe a running server.
`;

function main(): void {
  const { values, positionals } = parseArgs({
    options: {
      json: { type: 'boolean', default: false },
      version: { type: 'boolean', short: 'v', default: false },
      help: { type: 'boolean', short: 'h', default: false },
    },
    allowPositionals: true,
    strict: true,
  });
  if (positionals.length > 1) throw new Error('Expected at most one command.');
  const command = positionals[0];
  if (values.help) {
    process.stdout.write(help);
    return;
  }
  if (values.version) {
    if (command || values.json)
      throw new Error('--version does not accept other arguments.');
    console.log(scaffoldDescription.version);
    return;
  }
  if (command === undefined) {
    if (values.json) throw new Error('--json requires the describe command.');
    process.stdout.write(help);
    return;
  }
  if (command !== 'describe')
    throw new Error(`Unknown command: ${command}. Use --help.`);
  if (values.json) {
    console.log(JSON.stringify(scaffoldDescription, null, 2));
    return;
  }
  console.log(`Artifree ${scaffoldDescription.version} · scaffolding`);
  console.log(
    'Build-time inventory. HTTP/MCP are not running; framework and async runtime undecided.',
  );
  for (const item of scaffoldDescription.components) {
    console.log(
      `${item.status.padEnd(9)} ${item.name.padEnd(23)} ${item.path}`,
    );
  }
}

try {
  main();
} catch (error: unknown) {
  console.error(error instanceof Error ? error.message : 'CLI failed.');
  process.exitCode = 2;
}
