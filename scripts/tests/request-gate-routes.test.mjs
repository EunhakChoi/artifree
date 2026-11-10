import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';
import {
  FRAME_ROUTE_TAILS,
  frameRoute,
} from '../../packages/client/dist/runtime-frame.js';

// The gate is web code without DOM or React imports; bundle it as it ships.
// The metafile lists the modules in the bundle, for the rule 9 check.
const bundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL('../../apps/web/src/artifacts/request-gate.ts', import.meta.url),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
  metafile: true,
});
const { requestKind } = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);

/** The twelve frame route tails the client names, in their order. */
const EXPECTED_TAILS = [
  'metadata',
  'source/list',
  'source/read',
  'source/offer',
  'source/download',
  'source/write',
  'assets/list',
  'assets/read',
  'assets/upload',
  'assets/delete',
  'assets/offer',
  'assets/download',
];

/**
 * Tails that are not frame routes: a thirteenth name, and near misses of the
 * twelve, such as a longer tail, a shorter one, or one sharing a prefix.
 */
const NON_FRAME_TAILS = [
  'assets/rename',
  'source/delete',
  'metadata/extra',
  'assets',
  'source',
  'source/list/x',
  'assets/listx',
];

/** The configured server address, without and with a path prefix. */
const BASES = ['http://127.0.0.1:4100', 'https://api.example/prefix'];

/** The view whose frame routes the gate classifies. */
const view = {
  artifact_id: `art_${'0'.repeat(32)}`,
  revision: 3,
  view_id: 'd'.repeat(32),
  generation: 1,
  initialization: 'c'.repeat(64),
};

const revisionPath = `/v1/artifacts/${view.artifact_id}/revisions/${view.revision}`;

/** Requests outside the frame routes, with the kind the gate gives each. */
const OTHER_REQUESTS = [
  {
    name: 'a preview',
    method: 'POST',
    path: `${revisionPath}/previews`,
    kind: 'read',
  },
  {
    name: 'a runtime view',
    method: 'POST',
    path: `${revisionPath}/runtime-views`,
    kind: 'read',
  },
  {
    name: 'a view path with no tail, which is not a frame route',
    method: 'POST',
    path: `${revisionPath}/runtime-views/${view.view_id}/${view.generation}`,
    kind: 'write',
  },
  {
    name: 'a DELETE of a frame route',
    method: 'DELETE',
    path: frameRoute(view, 'metadata'),
    kind: 'write',
  },
];

/** Host module paths; the gate bundle must not contain them (rule 9). */
const HOST_MODULE = /frame-rpc|frame-calls|runtime-host|offer-desk/;
/** The gate, and the client module that names its routes. */
const GATE = 'apps/web/src/artifacts/request-gate.ts';
const CLIENT_ROUTES = 'packages/client/dist/runtime-frame.js';

it('The client names exactly the twelve recorded frame route tails', () => {
  assert.deepEqual([...FRAME_ROUTE_TAILS], EXPECTED_TAILS);
});

describe('Every frame route tail is a frame call', () => {
  for (const base of BASES) {
    for (const tail of EXPECTED_TAILS) {
      it(`${tail} under ${base}`, () => {
        const url = base + frameRoute(view, tail);
        assert.equal(requestKind('POST', url), 'frame', url);
        assert.equal(requestKind('GET', url), 'read', url);
      });
    }
  }
});

describe('Any other tail is classified as before', () => {
  for (const base of BASES) {
    for (const tail of NON_FRAME_TAILS) {
      it(`POST ${tail} under ${base} is a write`, () => {
        const url = base + frameRoute(view, tail);
        assert.equal(requestKind('POST', url), 'write', url);
      });
    }
  }
});

describe('Requests outside the frame routes keep their kinds', () => {
  for (const base of BASES) {
    for (const request of OTHER_REQUESTS) {
      it(`${request.name} under ${base} is a ${request.kind}`, () => {
        const url = base + request.path;
        assert.equal(requestKind(request.method, url), request.kind, url);
      });
    }
  }
});

it('The gate bundle carries no host code (rule 9)', () => {
  const inputs = Object.keys(bundle.metafile.inputs);
  // Without these two entries the check below could pass vacuously.
  assert.ok(
    inputs.some((input) => input.endsWith(GATE)),
    'gate not bundled',
  );
  assert.ok(
    inputs.some((input) => input.endsWith(CLIENT_ROUTES)),
    'client routes not bundled',
  );
  assert.deepEqual(
    inputs.filter((input) => HOST_MODULE.test(input)),
    [],
  );
});
