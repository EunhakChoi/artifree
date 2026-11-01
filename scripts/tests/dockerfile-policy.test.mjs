import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { checkDockerfile } from '../lib/dockerfile-policy.mjs';

const path = fileURLToPath(
  new URL('../../deploy/container/Dockerfile', import.meta.url),
);
const dockerfile = await readFile(path, 'utf8');

test('the committed Dockerfile satisfies the policy', () => {
  checkDockerfile(dockerfile);
});

const mutations = [
  [
    'a tag instead of a digest',
    (d) => d.replace(/@sha256:[0-9a-f]{64}/, ''),
    /pinned by digest/,
  ],
  [
    'a root user',
    (d) => d.replace('USER 10001:10001', 'USER root'),
    /numeric, non-root/,
  ],
  ['uid 0', (d) => d.replace('USER 10001:10001', 'USER 0'), /non-root/],
  [
    'no data volume',
    (d) => d.replace('VOLUME ["/data"]', 'VOLUME ["/var/data"]'),
    /\/data as a volume/,
  ],
  [
    'no preview port',
    (d) => d.replace('EXPOSE 7410 7411', 'EXPOSE 7410'),
    /7411/,
  ],
  [
    'no health check',
    (d) => d.replace(/HEALTHCHECK[^\n]*\n[^\n]*\n/, ''),
    /HEALTHCHECK/,
  ],
  [
    'a shell-form entrypoint',
    (d) =>
      d.replace(
        /ENTRYPOINT \[[\s\S]*$/,
        'ENTRYPOINT /app/bin/artifree-server serve\n',
      ),
    /exec form/,
  ],
  ['a CMD', (d) => `${d}CMD ["serve"]\n`, /no CMD/],
  ['ADD', (d) => `${d}ADD https://example.com/x /x\n`, /not ADD/],
  [
    'a package install',
    (d) => d.replace('COPY . /app', 'RUN apt-get install -y curl\nCOPY . /app'),
    /must not fetch/,
  ],
  [
    'a second stage',
    (d) => `FROM debian@sha256:${'a'.repeat(64)}\n${d}`,
    /exactly one FROM/,
  ],
];

for (const [name, mutate, pattern] of mutations)
  test(`the policy rejects ${name}`, () => {
    const changed = mutate(dockerfile);
    assert.notEqual(changed, dockerfile, 'The mutation must change the file.');
    assert.throws(() => checkDockerfile(changed), pattern);
  });
