import assert from 'node:assert/strict';
import { readdir, readFile } from 'node:fs/promises';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';
import * as client from '../../packages/client/dist/index.js';

// The bounds that more than one layer applies to what a runtime frame sends or
// receives (decision 0052, rule 10). The client defines each one once. The SDK
// and the host keep copies of their own, so these tests pin every copy to the
// client's value, and pin the client's values to the generated API schema where
// the schema states them.

// The client's limits as they are pinned. A regular expression is compared by
// its source text.
const PINNED_LIMITS = {
  RETRY_ID: '^[A-Za-z0-9_-]{8,36}$',
  ASSET_ID: '^[0-9a-f]{64}$',
  MAX_SOURCE_PART_BYTES: 1_048_576,
  MAX_SOURCE_WRITE_BYTES: 1_048_576,
  MAX_ASSET_PART_BYTES: 262_144,
  MAX_ASSET_UPLOAD_BYTES: 1_048_576,
  MAX_ASSET_BYTES: 5_242_880,
  MAX_LISTED_FILES: 256,
  MAX_LISTED_ASSETS: 64,
};

// A limit as a comparable value: a regular expression is its source text.
function limitValue(value) {
  return value instanceof RegExp ? value.source : value;
}

test('Each client limit equals its pinned value', () => {
  const actual = Object.fromEntries(
    Object.keys(PINNED_LIMITS).map((name) => [name, limitValue(client[name])]),
  );
  assert.deepEqual(actual, PINNED_LIMITS);
});

const schema = JSON.parse(
  await readFile(
    new URL('../../contracts/generated/api-v1.schema.json', import.meta.url),
    'utf8',
  ),
);

// The schema node at a path below `$defs`; undefined when the path is gone.
function schemaAt(path) {
  return path.reduce((node, key) => node?.[key], schema.$defs);
}

// The length bounds RETRY_ID states, `^[A-Za-z0-9_-]{min,max}$`. Another
// alphabet fails here as well as in the pinned-value test.
function retryIdBounds() {
  const source = limitValue(client.RETRY_ID);
  const match = /^\^\[A-Za-z0-9_-\]\{(\d+),(\d+)\}\$$/.exec(source);
  assert.ok(match, `RETRY_ID is not the class with length bounds: ${source}`);
  return { min: Number(match[1]), max: Number(match[2]) };
}

// The length of padded base64 text for this many bytes.
const base64Length = (bytes) => Math.ceil(bytes / 3) * 4;

const RETRY_ID_DEFS = [
  'RuntimeSourceWriteRequest',
  'RuntimeAssetUploadRequest',
  'RuntimeAssetDeleteRequest',
];

// Each bound the schema states: the path to it below `$defs` and the value the
// client implies for it.
const SCHEMA_ROWS = [
  {
    name: 'source part',
    path: ['RuntimeSourceReadRequest', 'properties', 'length', 'maximum'],
    expected: () => client.MAX_SOURCE_PART_BYTES,
  },
  {
    name: 'asset part',
    path: ['RuntimeAssetReadRequest', 'properties', 'length', 'maximum'],
    expected: () => client.MAX_ASSET_PART_BYTES,
  },
  {
    name: 'listed assets',
    path: ['AssetList', 'properties', 'assets', 'maxItems'],
    expected: () => client.MAX_LISTED_ASSETS,
  },
  {
    name: 'source write',
    path: ['RuntimeSourceWriteRequest', 'properties', 'content', 'maxLength'],
    expected: () => base64Length(client.MAX_SOURCE_WRITE_BYTES),
  },
  {
    name: 'asset upload',
    path: [
      'RuntimeAssetUploadRequest',
      'properties',
      'content_base64',
      'maxLength',
    ],
    expected: () => base64Length(client.MAX_ASSET_UPLOAD_BYTES),
  },
  ...RETRY_ID_DEFS.flatMap((def) => [
    {
      name: `retry id minLength in ${def}`,
      path: [def, 'properties', 'retry_id', 'minLength'],
      expected: () => retryIdBounds().min,
    },
    {
      name: `retry id maxLength in ${def}`,
      path: [def, 'properties', 'retry_id', 'maxLength'],
      expected: () => retryIdBounds().max,
    },
  ]),
];

for (const row of SCHEMA_ROWS) {
  test(`The schema states ${row.name}`, () => {
    assert.equal(row.expected(), schemaAt(row.path));
  });
}

const bootstrap = await readFile(
  new URL('../../packages/runtime-sdk/src/bootstrap.ts', import.meta.url),
  'utf8',
);

// The SDK's copy of a bound, written on its own line as `const NAME = 1_048_576;`
// or `const NAME = /.../;`. A copy that is renamed or removed fails here.
function sdkValue(name) {
  const match = new RegExp(`^const ${name} = (?:/(.*)/|([0-9_]+));$`, 'm').exec(
    bootstrap,
  );
  assert.ok(match, `${name} is written in bootstrap.ts`);
  return match[1] ?? Number(match[2].replaceAll('_', ''));
}

// Each SDK copy and the client constant it must equal.
const SDK_PAIRS = [
  ['MAX_PART_BYTES', 'MAX_SOURCE_PART_BYTES'],
  ['MAX_WRITE_BYTES', 'MAX_SOURCE_WRITE_BYTES'],
  ['MAX_WRITE_BYTES', 'MAX_ASSET_UPLOAD_BYTES'],
  ['RETRY_ID', 'RETRY_ID'],
  ['ASSET_ID', 'ASSET_ID'],
  ['MAX_ASSET_PART_BYTES', 'MAX_ASSET_PART_BYTES'],
  ['MAX_ASSET_BYTES', 'MAX_ASSET_BYTES'],
  ['MAX_LISTED_FILES', 'MAX_LISTED_FILES'],
  ['MAX_LISTED_ASSETS', 'MAX_LISTED_ASSETS'],
];

for (const [sdkName, clientName] of SDK_PAIRS) {
  test(`The SDK's ${sdkName} equals the client's ${clientName}`, () => {
    assert.equal(sdkValue(sdkName), limitValue(client[clientName]));
  });
}

// call.ts as the web app bundles it, so the values read are the ones that run.
// Its imports are types only, so the bundle has no dependencies to resolve.
const hostBundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL(
        '../../apps/web/src/artifacts/frame-calls/call.ts',
        import.meta.url,
      ),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const host = await import(
  `data:text/javascript;base64,${Buffer.from(hostBundle.outputFiles[0].text).toString('base64')}`
);

// The literals call.ts keeps, each with the client constant it must equal. The
// host writes its own copies rather than importing the client's (decision 0052,
// rule 10).
const HOST_PAIRS = [
  ['RETRY_ID', 'RETRY_ID'],
  ['ASSET_ID', 'ASSET_ID'],
  ['MAX_PART', 'MAX_SOURCE_PART_BYTES'],
  ['MAX_WRITE', 'MAX_SOURCE_WRITE_BYTES'],
  ['MAX_WRITE', 'MAX_ASSET_UPLOAD_BYTES'],
  ['MAX_ASSET_PART', 'MAX_ASSET_PART_BYTES'],
];

for (const [hostName, clientName] of HOST_PAIRS) {
  test(`The host's ${hostName} equals the client's ${clientName}`, () => {
    assert.ok(hostName in host, `${hostName} is exported by call.ts`);
    assert.equal(limitValue(host[hostName]), limitValue(client[clientName]));
  });
}

// The pattern limits carry no flags. A flag changes what a pattern matches, and
// `g` or `y` makes `test` keep state between calls, which the source comparisons
// above cannot see. The SDK's pattern lines end at the slash, which sdkValue
// requires, so a flag there already fails the SDK checks.
const PATTERN_LIMITS = ['RETRY_ID', 'ASSET_ID'];

for (const name of PATTERN_LIMITS) {
  test(`The client's ${name} carries no flags`, () => {
    assert.equal(client[name].flags, '');
  });

  test(`The host's ${name} carries no flags`, () => {
    assert.equal(host[name].flags, '');
  });
}

const FRAME_CALLS = new URL(
  '../../apps/web/src/artifacts/frame-calls/',
  import.meta.url,
);

// The sizes, the retry identity's length bounds and the asset ID's digits as a
// host module would spell them.
const SHARED_LITERAL =
  /1_?048_?576|262_?144|5_?242_?880|\{8,36\}|\[0-9a-f\]\{64\}/;

const OTHER_HOST_FILES = (await readdir(FRAME_CALLS))
  .filter((name) => /\.tsx?$/.test(name) && name !== 'call.ts')
  .sort();

test('The host frame calls other than call.ts are scanned', () => {
  assert.ok(
    OTHER_HOST_FILES.includes('source.ts') &&
      OTHER_HOST_FILES.includes('assets.ts'),
    `scanned: ${OTHER_HOST_FILES.join(', ')}`,
  );
});

for (const name of OTHER_HOST_FILES) {
  test(`The host frame call ${name} repeats none of the shared literals`, async () => {
    const text = await readFile(new URL(name, FRAME_CALLS), 'utf8');
    assert.equal(SHARED_LITERAL.exec(text)?.[0], undefined);
  });
}
