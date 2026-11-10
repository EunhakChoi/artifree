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
// handles. R14c adds that event pairing below, with data.changed as the one
// event the frame bridge carries.

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
  // Sixteen in R14b: the twelve of R13, data/read of R14a, and data/set,
  // data/update and data/delete of R14b. The count keeps two empty lists from
  // agreeing.
  assert.equal(clientTails.length, 16);
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
  // Ten calls of decision 0052, data.get of decision 0053 (R14a), and
  // data.set, data.update and data.remove of R14b. The count keeps an empty
  // registry from passing.
  assert.equal(FRAME_CALL_TYPES.length, 14);
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

// The change hint module, bundled as the web app ships it: the events the host
// sends a frame and the sender of its change hints.
const hints = await build({
  entryPoints: [
    fileURLToPath(
      new URL('../../apps/web/src/artifacts/data-hint.ts', import.meta.url),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { FRAME_EVENTS, createDataHintSender } = await import(
  `data:text/javascript;base64,${Buffer.from(hints.outputFiles[0].text).toString('base64')}`
);

test('Each event the host can send is one the SDK handles, and the reverse', () => {
  // The SDK's guards compare an event name as `event.data.event === 'name'`.
  const handled = [
    ...bootstrap.matchAll(/event\.data\.event === '([a-z]+\.[A-Za-z]+)'/g),
  ].map(([, event]) => event);
  assert.deepEqual(absentFrom(FRAME_EVENTS, handled), []);
  assert.deepEqual(absentFrom(handled, FRAME_EVENTS), []);
  // data.changed of R14c. The count keeps two empty lists from agreeing.
  assert.equal(FRAME_EVENTS.length, 1);

  // The hint the host sends names one of those events. The schedule runs its
  // callback at once, so the single offer is sent without waiting.
  const sent = [];
  const sender = createDataHintSender({
    send: (message) => {
      sent.push(message);
    },
    schedule: (run) => {
      run();
      return () => {};
    },
  });
  sender.offer(1);
  assert.equal(sent.length, 1);
  assert.equal(sent[0].type, 'event');
  assert.deepEqual(absentFrom([sent[0].event], FRAME_EVENTS), []);
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
  // Nine in R14b: the seven of R13, data.read of R14a, and data.write of R14b.
  // The count keeps two empty lists from agreeing.
  assert.equal(capabilities.length, 9);
});
