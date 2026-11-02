import assert from 'node:assert/strict';
import { test } from 'node:test';
import { readFile } from 'node:fs/promises';
import {
  CONTAINER_FRAME_ANCESTORS,
  checkImageConfig,
  containerEnvironment,
  environmentOptions,
} from '../verify-container.mjs';

const good = {
  User: '10001:10001',
  ExposedPorts: { '7410/tcp': {}, '7411/tcp': {} },
  Volumes: { '/data': {} },
  Healthcheck: { Test: ['CMD', '/bin/bash', '-c', 'true'] },
  Entrypoint: ['/app/bin/artifree-server', 'serve'],
  Cmd: null,
};

test('the expected image configuration passes', () => {
  checkImageConfig(good);
});

for (const [name, change] of [
  ['root', { User: '' }],
  ['a missing preview port', { ExposedPorts: { '7410/tcp': {} } }],
  ['an extra port', { ExposedPorts: { ...good.ExposedPorts, '22/tcp': {} } }],
  ['no volume', { Volumes: null }],
  ['no health check', { Healthcheck: undefined }],
  ['a disabled health check', { Healthcheck: { Test: ['NONE'] } }],
  ['a default command', { Cmd: ['serve'] }],
  ['another entrypoint', { Entrypoint: ['/bin/sh'] }],
])
  test(`the image check rejects ${name}`, () => {
    assert.throws(() => checkImageConfig({ ...good, ...change }));
  });

// The CI container job declares the server settings as variables
// (decision 0028). It cannot run here, so these tests pin its inputs.
test('the checked container is configured by variables', () => {
  const environment = containerEnvironment({
    apiPort: 9100,
    contentPort: 9101,
  });
  assert.deepEqual(environment, {
    ARTIFREE_CONTENT_ORIGIN: 'http://127.0.0.1:9101',
    ARTIFREE_ALLOWED_HOSTS: '127.0.0.1:9100',
    ARTIFREE_FRAME_ANCESTORS: CONTAINER_FRAME_ANCESTORS,
  });
  assert.match(CONTAINER_FRAME_ANCESTORS, / /, 'The list has two sources.');
  assert.notEqual(
    CONTAINER_FRAME_ANCESTORS,
    'http://127.0.0.1:* http://localhost:* http://[::1]:*',
    'The list must differ from the default so that a lost variable shows.',
  );
});

test('each variable becomes one --env argument, spaces included', () => {
  assert.deepEqual(environmentOptions({ A: '1', B: 'x y' }), [
    '--env',
    'A=1',
    '--env',
    'B=x y',
  ]);
});

test('the container check does not pass these settings as flags', async () => {
  const source = await readFile(
    new URL('../verify-container.mjs', import.meta.url),
    'utf8',
  );
  for (const flag of [
    '--content-origin',
    '--frame-ancestors',
    '--allowed-host',
    '--api-origin',
  ])
    assert.ok(!source.includes(`'${flag}'`), `${flag} is passed as a flag`);
  assert.match(source, /ARTIFREE_CONTENT_ORIGIN: 'https:\/\/user:hunter2@/);
});
