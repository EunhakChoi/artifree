import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import {
  checkPins,
  findDockerImages,
  findPins,
  parseReference,
  readPinFiles,
  resolveDigest,
  run,
} from '../refresh-base-digests.mjs';

const root = fileURLToPath(new URL('../../', import.meta.url));
const A = 'a'.repeat(64);
const B = 'b'.repeat(64);

test('the repository pins every container image by tag and digest', async () => {
  const pins = checkPins(await readPinFiles(root));
  const references = pins.map((pin) => pin.reference);
  assert.ok(references.includes('debian:bookworm-slim'));
  assert.ok(
    references.some((r) => r.startsWith('mcr.microsoft.com/playwright:')),
  );
});

test('pins are found with their line, tag, and digest', () => {
  assert.deepEqual(findPins(`x\nFROM debian:bookworm-slim@sha256:${A}\n`), [
    {
      line: 2,
      reference: 'debian:bookworm-slim',
      name: 'debian',
      tag: 'bookworm-slim',
      digest: A,
    },
  ]);
});

const file = (path, text) => ({ path, text });

test('a pin without a tag, with latest, or with a short digest is refused', () => {
  assert.throws(
    () => checkPins([file('d/Dockerfile', `FROM debian@sha256:${A}`)]),
    /needs a version tag/,
  );
  assert.throws(
    () => checkPins([file('d/Dockerfile', `FROM debian:latest@sha256:${A}`)]),
    /needs a version tag/,
  );
  assert.throws(
    () => checkPins([file('d/Dockerfile', 'FROM debian:12@sha256:abc123')]),
    /64-digit sha256/,
  );
});

test('one image reference with two digests is refused', () => {
  assert.throws(
    () =>
      checkPins([
        file('d/Dockerfile', `FROM debian:12@sha256:${A}`),
        file(
          '.github/workflows/w.yml',
          `run: docker run debian:12@sha256:${B}`,
        ),
      ]),
    /two different digests/,
  );
});

test('an unpinned Dockerfile base or workflow container image is refused', () => {
  const pinned = file('d/Dockerfile', `FROM debian:12@sha256:${A}`);
  assert.throws(
    () => checkPins([pinned, file('e/Dockerfile', 'FROM debian:12')]),
    /FROM debian:12 is not pinned/,
  );
  const workflow = (image) =>
    file(
      '.github/workflows/w.yml',
      `jobs:\n  j:\n    container:\n      image: ${image}\n`,
    );
  checkPins([pinned, workflow(`node:22@sha256:${A}`)]);
  assert.throws(
    () => checkPins([pinned, workflow('node:22')]),
    /image node:22 is not pinned/,
  );
});

test('no pins at all is an error, not a pass', () => {
  assert.throws(
    () => checkPins([file('d/Dockerfile', '# nothing')]),
    /No image pins/,
  );
});

test('references resolve to the right registry and repository', () => {
  assert.deepEqual(parseReference('debian:bookworm-slim'), {
    registry: 'registry-1.docker.io',
    repository: 'library/debian',
    tag: 'bookworm-slim',
  });
  assert.deepEqual(parseReference('mcr.microsoft.com/playwright:v1-noble'), {
    registry: 'mcr.microsoft.com',
    repository: 'playwright',
    tag: 'v1-noble',
  });
  assert.deepEqual(parseReference('someone/tool:1'), {
    registry: 'registry-1.docker.io',
    repository: 'someone/tool',
    tag: '1',
  });
});

/** A fetch double for a registry that wants an anonymous bearer token. */
function registry({ digest = `sha256:${B}`, challenge = true } = {}) {
  const calls = [];
  const fetchImpl = async (url, options = {}) => {
    calls.push({ url: String(url), options });
    if (String(url).startsWith('https://auth.test/token'))
      return Response.json({ token: 't0k' });
    if (challenge && options.headers?.authorization !== 'Bearer t0k')
      return new Response(null, {
        status: 401,
        headers: {
          'www-authenticate':
            'Bearer realm="https://auth.test/token",service="reg.test"',
        },
      });
    return new Response(null, {
      status: 200,
      headers: { 'docker-content-digest': digest },
    });
  };
  return { fetchImpl, calls };
}

test('a digest is read from the registry after the anonymous token exchange', async () => {
  const { fetchImpl, calls } = registry();
  assert.equal(await resolveDigest('debian:12', fetchImpl), B);
  assert.match(calls[1].url, /scope=repository%3Alibrary%2Fdebian%3Apull/);
  assert.equal(calls.at(-1).options.method, 'HEAD');
});

test('a registry that sends no usable digest is an error', async () => {
  await assert.rejects(
    resolveDigest(
      'debian:12',
      registry({ digest: 'garbage', challenge: false }).fetchImpl,
    ),
    /no content digest/,
  );
  await assert.rejects(
    resolveDigest('debian:12', async () => new Response(null, { status: 401 })),
    /anonymous pull is not available/,
  );
});

let scratch;
before(async () => {
  scratch = await mkdtemp(join(tmpdir(), 'artifree-digest-test-'));
  await mkdir(join(scratch, 'deploy', 'c'), { recursive: true });
  await mkdir(join(scratch, '.github', 'workflows'), { recursive: true });
  await writeFile(
    join(scratch, 'deploy', 'c', 'Dockerfile'),
    `FROM debian:12@sha256:${A}\n`,
  );
  await writeFile(join(scratch, '.github', 'workflows', 'w.yml'), 'jobs: {}\n');
});
after(async () => {
  await rm(scratch, { recursive: true, force: true });
});

test('check mode needs no network and resolve mode reports the lines to edit', async () => {
  const lines = [];
  const out = (line) => lines.push(line);
  const forbidden = async () =>
    assert.fail('check mode must not use the network');
  assert.equal(
    await run({
      root: scratch,
      check: true,
      env: {},
      fetchImpl: forbidden,
      out,
    }),
    0,
  );

  assert.equal(
    await run({
      root: scratch,
      check: false,
      env: {},
      fetchImpl: registry({ digest: `sha256:${B}` }).fetchImpl,
      out,
    }),
    1,
  );
  assert.ok(lines.some((l) => l.includes('CHANGED   debian:12')));
  assert.ok(lines.some((l) => l.includes(`latest  sha256:${B}`)));
  assert.ok(lines.some((l) => l.includes('edit    deploy/c/Dockerfile:1')));

  lines.length = 0;
  assert.equal(
    await run({
      root: scratch,
      check: false,
      env: {},
      fetchImpl: registry({ digest: `sha256:${A}` }).fetchImpl,
      out,
    }),
    0,
  );
  assert.ok(lines.some((l) => l.startsWith('current   debian:12')));
});

test('resolve mode refuses to run in CI', async () => {
  await assert.rejects(
    run({
      root: scratch,
      check: false,
      env: { CI: 'true' },
      fetchImpl: registry().fetchImpl,
      out: () => undefined,
    }),
    /does not run in CI/,
  );
});

const workflowWith = (script) =>
  file(
    '.github/workflows/w.yml',
    `jobs:\n  j:\n    steps:\n      - run: |\n${script
      .split('\n')
      .map((line) => `          ${line}`)
      .join('\n')}\n`,
  );
const pinnedBase = file('d/Dockerfile', `FROM debian:12@sha256:${A}`);

test('the image of docker run, create, and pull is found past options and continuations', () => {
  assert.deepEqual(
    findDockerImages(
      [
        'docker run --rm --network none --read-only --tmpfs /tmp:rw,size=1m \\',
        '  --volume "$PWD/a:/a:ro" --workdir /work \\',
        '  node:22@sha256:abc \\',
        "  sh -c 'ls'",
        'docker run "quoted:1" true',
        'docker pull -q --platform linux/amd64 debian:12 && echo done',
        'docker container create -e A=1 -p 80:80 nginx:1',
        'docker build --tag x .',
      ].join('\n'),
    ),
    [
      { command: 'run', image: 'node:22@sha256:abc' },
      { command: 'run', image: 'quoted:1' },
      { command: 'pull', image: 'debian:12' },
      { command: 'create', image: 'nginx:1' },
    ],
  );
});

test('an unpinned docker run image in a workflow step is refused', () => {
  checkPins([
    pinnedBase,
    workflowWith(`docker run --rm node:22@sha256:${A} node --version`),
  ]);
  assert.throws(
    () =>
      checkPins([
        pinnedBase,
        workflowWith(
          'docker run --rm --network none node:22.12.0-bookworm-slim node --version',
        ),
      ]),
    /docker run uses node:22.12.0-bookworm-slim without a literal @sha256 pin/,
  );
  assert.throws(
    () => checkPins([pinnedBase, workflowWith('docker pull debian:12')]),
    /docker pull uses debian:12 without/,
  );
  assert.throws(
    () => checkPins([pinnedBase, workflowWith('docker run "$IMAGE" true')]),
    /without a literal @sha256 pin/,
  );
  assert.throws(
    () => checkPins([pinnedBase, workflowWith('docker run --rm')]),
    /an image that cannot be found/,
  );
});

test('a matrix-expression image or an unpinned compose or --from image is refused', () => {
  assert.throws(
    () =>
      checkPins([
        pinnedBase,
        file(
          '.github/workflows/w.yml',
          'jobs:\n  j:\n    container:\n      image: ${{ matrix.image }}\n',
        ),
      ]),
    /not pinned by a literal digest/,
  );
  assert.throws(
    () =>
      checkPins([
        pinnedBase,
        file(
          '.github/workflows/w.yml',
          `jobs:\n  j:\n    container:\n      image: \${{ matrix.image }}@sha256:${A}\n`,
        ),
      ]),
    /not pinned by a literal digest/,
  );
  checkPins([
    pinnedBase,
    file(
      'deploy/c/compose.yml',
      `services:\n  a:\n    image: caddy:2@sha256:${A}\n`,
    ),
  ]);
  assert.throws(
    () =>
      checkPins([
        pinnedBase,
        file('deploy/c/compose.yml', 'services:\n  a:\n    image: caddy:2\n'),
      ]),
    /image caddy:2 is not pinned/,
  );
  assert.throws(
    () =>
      checkPins([
        pinnedBase,
        file('d/Dockerfile', 'COPY --from=busybox:1 /bin/true /true'),
      ]),
    /is not pinned by digest/,
  );
});

test('yaml files under deploy and every workflow are read as pin files', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'artifree-pinfiles-'));
  try {
    await mkdir(join(dir, 'deploy', 'c'), { recursive: true });
    await mkdir(join(dir, '.github', 'workflows'), { recursive: true });
    await writeFile(join(dir, 'deploy', 'c', 'Dockerfile.dev'), 'x\n');
    await writeFile(join(dir, 'deploy', 'c', 'compose.yml'), 'x\n');
    await writeFile(join(dir, 'deploy', 'c', 'other.yaml'), 'x\n');
    await writeFile(join(dir, 'deploy', 'c', 'notes.md'), 'x\n');
    await writeFile(join(dir, '.github', 'workflows', 'w.yml'), 'x\n');
    assert.deepEqual(
      (await readPinFiles(dir)).map((f) => f.path),
      [
        '.github/workflows/w.yml',
        'deploy/c/Dockerfile.dev',
        'deploy/c/compose.yml',
        'deploy/c/other.yaml',
      ],
    );
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});
