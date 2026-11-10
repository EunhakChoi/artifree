import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// Rule 21 of decision 0053: the layers of the frame bridge agree with each
// other. The client, the host, the server and the SDK each keep their own list
// of what the bridge carries, and these tests compare those lists.
//
// Rule 21 also pairs the events the host can send with the events the SDK
// handles. The frame bridge carries no event before R14c, so there is nothing
// to pair yet and no event test is written here; R14c adds that comparison.

const readSource = (path) => readFile(new URL(path, import.meta.url), 'utf8');

// The names in `names` that `others` does not have, in the order of `names`.
const absentFrom = (names, others) =>
  names.filter((name) => !others.includes(name));

// The client's route tails, as the built client ships them.
const client = await import('../../packages/client/dist/runtime-frame.js');
const clientTails = [...client.FRAME_ROUTE_TAILS].sort();

// The server's route tails: each `format!("{VIEW}/...")` in the frame route
// builder.
const runtimeRoutes = await readSource(
  '../../crates/artifree-server/src/http/runtime.rs',
);
const serverTails = [
  ...runtimeRoutes.matchAll(/format!\("\{VIEW\}\/([a-z/]+)"\)/g),
]
  .map((match) => match[1])
  .sort();

test('Each client route tail is a route the server registers, and the reverse', () => {
  assert.deepEqual(absentFrom(clientTails, serverTails), []);
  assert.deepEqual(absentFrom(serverTails, clientTails), []);
  assert.deepEqual(serverTails, clientTails);
  // Thirteen in R14a: the twelve of R13 and data/read. The count keeps two
  // empty lists from agreeing.
  assert.equal(clientTails.length, 13);
});

// The host's call registry, bundled as the web app ships it, the way the
// frame-call registry test reads it.
const registry = await build({
  entryPoints: [
    fileURLToPath(
      new URL('../../apps/web/src/artifacts/frame-rpc.ts', import.meta.url),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { FRAME_CALL_TYPES } = await import(
  `data:text/javascript;base64,${Buffer.from(registry.outputFiles[0].text).toString('base64')}`
);

// The SDK writes the message of each call it sends as `type: '<type>'`.
const bootstrap = await readSource(
  '../../packages/runtime-sdk/src/bootstrap.ts',
);

test('Each host call type is sent by an SDK method', () => {
  const unsent = FRAME_CALL_TYPES.filter(
    (type) => !bootstrap.includes(`type: '${type}'`),
  );
  assert.deepEqual(unsent, []);
  // Ten calls of decision 0052 and data.get of decision 0053 (R14a). The count
  // keeps an empty registry from passing.
  assert.equal(FRAME_CALL_TYPES.length, 11);
});

test('Each call the SDK sends is one the host serves', () => {
  // A message the SDK builds for a call names its type as `type: 'family.verb'`.
  const sent = [...bootstrap.matchAll(/type: '([a-z]+\.[A-Za-z]+)'/g)].map(
    ([, type]) => type,
  );
  assert.ok(sent.length > 0, 'the SDK sends calls');
  assert.deepEqual(
    sent.filter((type) => !FRAME_CALL_TYPES.includes(type)),
    [],
  );
});

const policy = await readSource(
  '../../crates/artifree-server/src/runtime_policy.rs',
);
const permissions = await readSource(
  '../../apps/web/src/artifacts/runtime-permissions.tsx',
);

// The supported capabilities: the constants that the match arms of
// `Capability::name` return, each resolved to the string it declares.
function supportedCapabilities() {
  const start = policy.indexOf('fn name(self)');
  assert.notEqual(start, -1, 'Capability::name is in runtime_policy.rs');
  const end = policy.indexOf('\n    }\n', start);
  assert.notEqual(end, -1, 'Capability::name ends in runtime_policy.rs');
  const constants = [
    ...policy.slice(start, end).matchAll(/=> ([A-Z_]+),/g),
  ].map((match) => match[1]);
  const declared = new Map(
    [
      ...policy.matchAll(/pub\(crate\) const ([A-Z_]+): &str = "([^"]+)";/g),
    ].map((match) => [match[1], match[2]]),
  );
  return constants.map((constant) => {
    assert.ok(declared.has(constant), `${constant} is declared as a string`);
    return declared.get(constant);
  });
}

// The names of the permission panel's controls: every `name` in
// CAPABILITY_CONTROLS.
function controlledCapabilities() {
  const start = permissions.indexOf('const CAPABILITY_CONTROLS = [');
  assert.notEqual(
    start,
    -1,
    'CAPABILITY_CONTROLS is in runtime-permissions.tsx',
  );
  const end = permissions.indexOf('] as const;', start);
  assert.notEqual(
    end,
    -1,
    'CAPABILITY_CONTROLS ends in runtime-permissions.tsx',
  );
  return [...permissions.slice(start, end).matchAll(/name: '([a-z.]+)'/g)].map(
    (match) => match[1],
  );
}

test('Each supported capability has a permission control, and each control is a supported capability', () => {
  const capabilities = supportedCapabilities();
  const controls = controlledCapabilities();
  assert.deepEqual(absentFrom(capabilities, controls), []);
  assert.deepEqual(absentFrom(controls, capabilities), []);
  assert.deepEqual([...controls].sort(), [...capabilities].sort());
  // Eight in R14a: the seven of R13 and data.read.
  assert.equal(capabilities.length, 8);
});
