import { execFileSync } from 'node:child_process';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { compile } from 'json-schema-to-typescript';
import { checkMcpCatalog, createAjv } from './lib/schema.mjs';
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
const mcpCatalog = schemas.mcp;
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
const rulesSchema = schemas.package_rules;
const rules = JSON.parse(
  run([
    'run',
    '--quiet',
    '--locked',
    '-p',
    'artifree-server',
    '--',
    '--package-rules',
  ]),
);
const ajv = createAjv();
const validate = ajv.compile(schema);
ajv.compile(apiSchema);
checkMcpCatalog(mcpCatalog);
if (!validate(inventory))
  throw new Error(
    `Rust inventory violates its schema: ${ajv.errorsText(validate.errors)}`,
  );
const validateRules = ajv.compile(rulesSchema);
if (!validateRules(rules))
  throw new Error(
    `Package rules violate their schema: ${ajv.errorsText(validateRules.errors)}`,
  );

const generatedRules = await compile(rulesSchema, 'PackageRules', {
  bannerComment:
    '/* Generated from artifree-server --package-rules. Run npm run generate:contracts; do not edit. */',
  style: { singleQuote: true, tabWidth: 2 },
});
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
// Definitions the root does not reach: the list query, which is not a body,
// and the duplicate body, whose fields are all optional. The generator emits
// only what a schema reaches.
const unreachable = ['ListArtifactsQuery', 'DuplicateArtifactRequest'];
const generatedParts = await Promise.all(
  unreachable.map((name) =>
    compile(
      {
        ...withoutRefDescriptions(apiSchema.$defs[name]),
        $defs: withoutRefDescriptions(apiSchema.$defs),
      },
      name,
      { bannerComment: '', style: { singleQuote: true, tabWidth: 2 } },
    ),
  ),
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
    'contracts/generated/mcp-tools.json',
    `${JSON.stringify(mcpCatalog, null, 2)}\n`,
  ],
  [
    'contracts/generated/scaffold-inventory.json',
    `${JSON.stringify(inventory, null, 2)}\n`,
  ],
  [
    'contracts/generated/package-rules.json',
    `${JSON.stringify(rules, null, 2)}\n`,
  ],
  [
    'packages/contracts/src/generated/package-rules.ts',
    `${generatedRules}\n/** The rules this build's server enforces; it checks every request again. */\nexport const packageRules: PackageRules = ${JSON.stringify(rules, null, 2)};\n`,
  ],
  ['packages/contracts/src/generated/schema.ts', generated],
  [
    'packages/contracts/src/generated/api.ts',
    [generatedApi, ...generatedParts].join('\n'),
  ],
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
  `${checking ? 'Verified' : 'Generated'} ${outputs.size} contract files; inventory and package rules validate against Rust schemas.`,
);
