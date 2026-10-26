import { execFileSync } from 'node:child_process';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { compile } from 'json-schema-to-typescript';
import { createAjv } from './lib/schema.mjs';
import assert from 'node:assert/strict';

const root = fileURLToPath(new URL('../', import.meta.url));
const args = process.argv.slice(2);
assert.ok(
  args.length === 0 || (args.length === 1 && args[0] === '--check'),
  'Usage: generate-contracts.mjs [--check]',
);
const checking = args.includes('--check');
const run = (args) =>
  execFileSync('cargo', args, {
    cwd: root,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'inherit'],
  });
const schemas = JSON.parse(
  run([
    'run',
    '--quiet',
    '--locked',
    '-p',
    'artifree-contracts',
    '--bin',
    'export-schema',
  ]),
);
const schema = schemas.service_description;
const apiSchema = schemas.api;
const inventory = JSON.parse(
  run([
    'run',
    '--quiet',
    '--locked',
    '-p',
    'artifree-server',
    '--',
    '--describe',
  ]),
);
const ajv = createAjv();
const validate = ajv.compile(schema);
ajv.compile(apiSchema);
if (!validate(inventory))
  throw new Error(
    `Rust inventory violates its schema: ${ajv.errorsText(validate.errors)}`,
  );

const generated = await compile(schema, 'ServiceDescription', {
  bannerComment:
    '/* Generated from artifree-contracts. Run npm run generate:contracts; do not edit. */',
  style: { singleQuote: true, tabWidth: 2 },
});
// A description beside $ref makes the generator emit a duplicate type
// (Artifact1). Descriptions stay in the published JSON Schema.
function withoutRefDescriptions(node) {
  if (Array.isArray(node)) return node.map(withoutRefDescriptions);
  if (node === null || typeof node !== 'object') return node;
  const copy = Object.fromEntries(
    Object.entries(node).map(([key, value]) => [
      key,
      withoutRefDescriptions(value),
    ]),
  );
  if (Object.hasOwn(copy, '$ref')) delete copy.description;
  return copy;
}
const generatedApi = await compile(
  withoutRefDescriptions(apiSchema),
  'ApiBody',
  {
    bannerComment:
      '/* Generated from artifree-contracts. Run npm run generate:contracts; do not edit. */',
    style: { singleQuote: true, tabWidth: 2 },
    unreachableDefinitions: true,
  },
);
const outputs = new Map([
  [
    'contracts/generated/service-description.schema.json',
    `${JSON.stringify(schema, null, 2)}\n`,
  ],
  [
    'contracts/generated/api-v1.schema.json',
    `${JSON.stringify(apiSchema, null, 2)}\n`,
  ],
  [
    'contracts/generated/scaffold-inventory.json',
    `${JSON.stringify(inventory, null, 2)}\n`,
  ],
  ['packages/contracts/src/generated/schema.ts', generated],
  ['packages/contracts/src/generated/api.ts', generatedApi],
  [
    'packages/contracts/src/generated/inventory.ts',
    `/* Generated build-time inventory; this is not live server health. */\nimport type { ServiceDescription } from './schema.js';\n\nexport const scaffoldDescription: ServiceDescription = ${JSON.stringify(inventory, null, 2)};\n`,
  ],
]);
for (const [path, content] of outputs) {
  const url = new URL(`../${path}`, import.meta.url);
  if (checking) {
    const existing = await readFile(url, 'utf8');
    if (existing !== content)
      throw new Error(
        `Generated contract drift: ${path}. Run npm run generate:contracts.`,
      );
  } else {
    await mkdir(new URL('.', url), { recursive: true });
    await writeFile(url, content);
  }
}
console.log(
  `${checking ? 'Verified' : 'Generated'} ${outputs.size} contract files; inventory validates against Rust schema.`,
);
