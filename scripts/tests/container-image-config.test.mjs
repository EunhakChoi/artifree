import assert from 'node:assert/strict';
import { test } from 'node:test';
import { checkImageConfig } from '../verify-container.mjs';

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
