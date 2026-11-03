import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import {
  ArtifreeApiError,
  ArtifreeClient,
  FileConflictError,
  editFiles,
} from '../../packages/client/dist/index.js';
import { createAjv } from '../lib/schema.mjs';
import { buildServer, startServer } from '../lib/server-process.mjs';

// Per-file conditional edits (decision 0035): the client's checks, and the
// command line and the HTTP API over a real server process.

const root = fileURLToPath(new URL('../../', import.meta.url));
const sha = (text) => createHash('sha256').update(text).digest('hex');
const artifact = {
  id: `art_${'0'.repeat(32)}`,
  kind: 'app',
  title: 'Demo',
  pinned: false,
  current_revision: 3,
  metadata_revision: 1,
  deleted_at_ms: null,
  created_at_ms: 1,
  updated_at_ms: 1,
};
const listing = (path, content) => ({
  path,
  size: Buffer.byteLength(content),
  sha256: sha(content),
  media_type: 'text/plain; charset=utf-8',
});
const commit = (files, revision = 3) => ({
  artifact,
  revision: { revision, created_by: 'owner', created_at_ms: 1, files },
  replayed: false,
});

function clientAnswering(status, body) {
  const sent = [];
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:7410',
    accessToken: 'token',
    fetch: async (url, init) => {
      sent.push([init.method, String(url), init.headers, init.body]);
      return Response.json(typeof body === 'function' ? body() : body, {
        status,
      });
    },
  });
  return { client, sent };
}

const failure = (promise) =>
  promise.then(
    () => assert.fail('Expected the request to fail.'),
    (error) => error,
  );

const request = (edits, base = 1) => ({ base_revision: base, edits });
const put = (path, content, expect) => ({
  op: 'put',
  path,
  encoding: 'utf8',
  content,
  expect,
});

test('An edit goes to its own route with its key and returns the commit', async () => {
  const { client, sent } = clientAnswering(
    201,
    commit([listing('a.txt', 'new')]),
  );
  const edits = request([put('a.txt', 'new', sha('old'))]);

  const result = await editFiles(client, artifact.id, edits, {
    idempotencyKey: 'edit-key-1',
  });

  assert.equal(result.revision.revision, 3);
  const [method, url, headers, body] = sent[0];
  assert.equal(method, 'POST');
  assert.equal(url, `http://127.0.0.1:7410/v1/artifacts/${artifact.id}/edits`);
  assert.equal(new Headers(headers).get('idempotency-key'), 'edit-key-1');
  assert.deepEqual(JSON.parse(body), edits);
});

test('An answer that does not match the edit is refused and says to retry', async () => {
  const cases = [
    [
      'a revision that does not follow the base',
      commit([listing('a.txt', 'new')], 1),
    ],
    ['a put file missing from the listing', commit([])],
    ['a put file of other bytes', commit([listing('a.txt', 'other')])],
    [
      'a put file of the size but not the digest',
      commit([{ ...listing('a.txt', 'new'), sha256: sha('zzz') }]),
    ],
    [
      'a put file of the digest but not the size',
      commit([{ ...listing('a.txt', 'new'), size: 99 }]),
    ],
    [
      'a deleted file still listed',
      commit([listing('a.txt', 'new'), listing('gone.txt', 'x')]),
    ],
  ];
  for (const [what, answer] of cases) {
    const { client } = clientAnswering(201, answer);
    const error = await failure(
      editFiles(
        client,
        artifact.id,
        request([
          put('a.txt', 'new', sha('old')),
          { op: 'delete', path: 'gone.txt', expect: sha('x') },
        ]),
        { idempotencyKey: 'edit-key-1' },
      ),
    );
    assert.equal(error.code, 'invalid_response', what);
    assert.match(error.message, /retry with the same key/, what);
  }
});

test('Put bytes sent as base64 are checked as the bytes they decode to', async () => {
  const bytes = Buffer.from([0, 255, 1, 2]);
  const base64 = {
    op: 'put',
    path: 'logo.bin',
    encoding: 'base64',
    content: bytes.toString('base64'),
    expect: 'absent',
  };
  const file = {
    path: 'logo.bin',
    size: 4,
    sha256: createHash('sha256').update(bytes).digest('hex'),
    media_type: 'application/octet-stream',
  };
  const ok = clientAnswering(201, commit([file]));
  await editFiles(ok.client, artifact.id, request([base64]), {
    idempotencyKey: 'edit-key-1',
  });
  const wrong = clientAnswering(201, commit([{ ...file, size: 3 }]));
  const error = await failure(
    editFiles(wrong.client, artifact.id, request([base64]), {
      idempotencyKey: 'edit-key-1',
    }),
  );
  assert.equal(error.code, 'invalid_response');
});

const conflictBody = (overrides = {}) => ({
  error: {
    code: 'file_conflict',
    message: '2 edited files are not as expected; current revision is 5',
    current_revision: 5,
    conflicting_files: [
      { path: 'a.txt', current_sha256: sha('now') },
      { path: 'b.txt', current_sha256: null },
    ],
    conflicting_files_total: 2,
    ...overrides,
  },
});

test('A file conflict carries the files, their digests, and the current revision', async () => {
  const { client } = clientAnswering(409, conflictBody());
  const error = await failure(
    editFiles(client, artifact.id, request([put('a.txt', 'x', sha('old'))]), {
      idempotencyKey: 'edit-key-1',
    }),
  );
  assert.ok(error instanceof FileConflictError);
  assert.ok(error instanceof ArtifreeApiError);
  assert.deepEqual(
    [error.status, error.code, error.currentRevision, error.total],
    [409, 'file_conflict', 5, 2],
  );
  assert.deepEqual(error.files, [
    { path: 'a.txt', current_sha256: sha('now') },
    { path: 'b.txt', current_sha256: null },
  ]);
});

test('The most files a conflict lists is the contract bound, and the client accepts exactly that many', async () => {
  const schema = JSON.parse(
    await readFile(
      new URL('../../contracts/generated/api-v1.schema.json', import.meta.url),
      'utf8',
    ),
  );
  const bound = schema.$defs.ErrorBody.properties.conflicting_files.maxItems;
  const listing = (count) =>
    Array.from({ length: count }, (_, index) => ({
      path: `f${index}.txt`,
      current_sha256: null,
    }));
  const answers = [];
  for (const count of [bound, bound + 1]) {
    const { client } = clientAnswering(
      409,
      conflictBody({
        conflicting_files: listing(count),
        conflicting_files_total: count,
      }),
    );
    answers.push(
      await failure(
        editFiles(
          client,
          artifact.id,
          request([put('a.txt', 'x', sha('old'))]),
          { idempotencyKey: 'edit-key-1' },
        ),
      ),
    );
  }
  assert.deepEqual(
    [bound, ...answers.map((error) => error instanceof FileConflictError)],
    [32, true, false],
  );
});

test('A conflict list whose numbers do not fit stays a plain error', async () => {
  const many = Array.from({ length: 33 }, (_, index) => ({
    path: `f${index}.txt`,
    current_sha256: null,
  }));
  const cases = [
    ['a total smaller than the list', { conflicting_files_total: 1 }],
    ['no total', { conflicting_files_total: undefined }],
    ['a negative total', { conflicting_files_total: -1 }],
    [
      'more files than a conflict lists',
      { conflicting_files: many, conflicting_files_total: 33 },
    ],
    [
      'a digest in upper case',
      {
        conflicting_files: [
          { path: 'a.txt', current_sha256: sha('x').toUpperCase() },
        ],
        conflicting_files_total: 1,
      },
    ],
    [
      'a file with no digest member',
      { conflicting_files: [{ path: 'a.txt' }], conflicting_files_total: 1 },
    ],
    ['no list', { conflicting_files: undefined }],
  ];
  for (const [what, overrides] of cases) {
    const { client } = clientAnswering(409, conflictBody(overrides));
    const error = await failure(
      editFiles(client, artifact.id, request([put('a.txt', 'x', sha('old'))]), {
        idempotencyKey: 'edit-key-1',
      }),
    );
    assert.ok(!(error instanceof FileConflictError), what);
    assert.ok(error instanceof ArtifreeApiError, what);
    assert.equal(error.code, 'file_conflict', what);
  }
});

test('Other failures of an edit are not turned into conflicts', async () => {
  const { client } = clientAnswering(409, {
    error: { code: 'revision_conflict', message: 'moved', current_revision: 9 },
  });
  const error = await failure(
    editFiles(client, artifact.id, request([put('a.txt', 'x', 'absent')]), {
      idempotencyKey: 'edit-key-1',
    }),
  );
  assert.ok(!(error instanceof FileConflictError));
  assert.deepEqual(
    [error.code, error.currentRevision],
    ['revision_conflict', 9],
  );
});

// -- Over a real server ------------------------------------------------

let binary;
let scratch;

before(async () => {
  binary = buildServer(root);
  scratch = await mkdtemp(join(tmpdir(), 'artifree-edit-test-'));
});

after(async () => {
  await rm(scratch, { recursive: true, force: true });
});

function cli(server, args) {
  const env = {
    ...process.env,
    ARTIFREE_URL: server.api_url,
    ARTIFREE_TOKEN: server.token,
  };
  delete env.ARTIFREE_TOKEN_FILE;
  const result = spawnSync(
    process.execPath,
    ['apps/cli/dist/main.js', 'artifact', ...args],
    { cwd: root, encoding: 'utf8', timeout: 20_000, env },
  );
  if (result.error) throw result.error;
  return result;
}

function json(result) {
  assert.equal(result.status, 0, result.stderr);
  return JSON.parse(result.stdout);
}

test('The CLI edits a file the head moved past, refuses a stale one, and never merges', async () => {
  const site = join(scratch, 'site');
  const inputs = join(scratch, 'inputs');
  await mkdir(site, { recursive: true });
  await mkdir(inputs, { recursive: true });
  await writeFile(join(site, 'index.html'), '<h1>v1</h1>');
  await writeFile(join(site, 'notes.txt'), 'notes 1');
  await writeFile(join(site, 'style.css'), 'h1{}');
  // Local files the edits read; they are not part of the artifact.
  await writeFile(join(inputs, 'index-v2.html'), '<h1>v2</h1>');
  await writeFile(join(inputs, 'notes-new.txt'), 'notes 2');
  await writeFile(join(inputs, 'mine.html'), '<h1>mine</h1>');
  await writeFile(join(inputs, 'new.js'), 'new');
  const server = await startServer(binary, join(scratch, 'data'));
  try {
    const id = json(cli(server, ['create', '--title', 'Edits', '--dir', site]))
      .artifact.id;
    // Another writer moves the head: index.html changes in revision 2.
    json(
      cli(server, [
        'save',
        id,
        '--base',
        '1',
        '--put',
        `index.html=${join(inputs, 'index-v2.html')}`,
      ]),
    );

    // The strict save of the stale base is refused; the edit of a file that
    // did not change applies, and carries over revision 2's index.html.
    const strict = cli(server, [
      'save',
      id,
      '--base',
      '1',
      '--put',
      `notes.txt=${join(inputs, 'notes-new.txt')}`,
    ]);
    assert.match(strict.stderr, /^error\[revision_conflict\]/);
    const edit = ['edit', id, '--base', '1', '--idempotency-key', 'cli-edit-1'];
    const notes = [
      ...edit,
      '--put',
      `notes.txt=${join(inputs, 'notes-new.txt')}`,
    ];
    const edited = json(cli(server, notes));
    assert.deepEqual([edited.revision.revision, edited.replayed], [3, false]);
    assert.equal(cli(server, ['cat', id, 'index.html']).stdout, '<h1>v2</h1>');
    assert.equal(cli(server, ['cat', id, 'notes.txt']).stdout, 'notes 2');
    assert.equal(
      json(cli(server, notes)).replayed,
      true,
      'the same key replays',
    );

    // A file that did change since the base: refused, with the files named.
    const stale = cli(server, [
      'edit',
      id,
      '--base',
      '1',
      '--put',
      `style.css=${join(inputs, 'new.js')}`,
      '--put',
      `index.html=${join(inputs, 'mine.html')}`,
      '--put',
      `new.js=${join(inputs, 'new.js')}`,
    ]);
    assert.equal(stale.status, 1);
    assert.equal(stale.stdout, '');
    assert.match(stale.stderr, /^error\[file_conflict\]/);
    assert.match(stale.stderr, /^current_revision=3$/m);
    assert.match(
      stale.stderr,
      new RegExp(`^conflict index.html ${sha('<h1>v2</h1>')}$`, 'm'),
    );
    assert.match(stale.stderr, /^conflicts=1$/m);
    const history = json(cli(server, ['history', id]));
    assert.equal(history.revisions.length, 3, 'nothing was written');
    assert.equal(cli(server, ['cat', id, 'style.css']).stdout, 'h1{}');

    // A create needs the path to be absent now.
    const created = json(
      cli(server, [
        'edit',
        id,
        '--base',
        '3',
        '--put',
        `new.js=${join(inputs, 'new.js')}`,
      ]),
    );
    assert.equal(created.revision.revision, 4);
    const taken = cli(server, [
      'edit',
      id,
      '--base',
      '3',
      '--put',
      `new.js=${join(inputs, 'mine.html')}`,
    ]);
    assert.match(taken.stderr, /^conflict new.js [0-9a-f]{64}$/m);

    // A delete of a file the base lacks, no change at all, and a base that
    // does not exist are refused before or by the server.
    const missing = cli(server, [
      'edit',
      id,
      '--base',
      '1',
      '--delete',
      'new.js',
    ]);
    assert.equal(missing.status, 2);
    assert.equal(cli(server, ['edit', id, '--base', '1']).status, 2);
    const future = cli(server, [
      'edit',
      id,
      '--base',
      '99',
      '--put',
      `new.js=${join(inputs, 'new.js')}`,
    ]);
    assert.match(future.stderr, /^error\[not_found\]/);
    const deleted = json(
      cli(server, ['edit', id, '--base', '4', '--delete', 'style.css']),
    );
    assert.deepEqual(
      deleted.revision.files.map((file) => file.path),
      ['index.html', 'new.js', 'notes.txt'],
    );
  } finally {
    await server.stop();
  }
});

test('The edit route answers with bodies the API schema accepts', async () => {
  const schema = JSON.parse(
    await readFile(
      new URL('../../contracts/generated/api-v1.schema.json', import.meta.url),
      'utf8',
    ),
  );
  const validate = createAjv().compile(schema);
  const server = await startServer(binary, join(scratch, 'schema-data'));
  try {
    let count = 0;
    const call = async (status, path, body) => {
      count += 1;
      const response = await fetch(`${server.api_url}${path}`, {
        method: 'POST',
        headers: {
          authorization: `Bearer ${server.token}`,
          'content-type': 'application/json',
          'idempotency-key': `schema-edit-${count}`,
        },
        body: JSON.stringify(body),
      });
      const value = await response.json();
      assert.equal(response.status, status, JSON.stringify(value));
      assert.ok(
        validate(body),
        `the request: ${JSON.stringify(validate.errors?.slice(0, 3))}`,
      );
      assert.ok(validate(value), JSON.stringify(validate.errors?.slice(0, 3)));
      return value;
    };
    const created = await call(201, '/v1/artifacts', {
      title: 'Schema',
      files: [
        { path: 'index.html', encoding: 'utf8', content: '<p>1</p>' },
        { path: 'a.txt', encoding: 'utf8', content: 'a' },
      ],
    });
    const id = created.artifact.id;
    const digest = (name) =>
      created.revision.files.find((file) => file.path === name).sha256;
    await call(201, `/v1/artifacts/${id}/revisions`, {
      base_revision: 1,
      changes: [{ op: 'put', path: 'a.txt', encoding: 'utf8', content: 'b' }],
    });
    const conflict = await call(
      409,
      `/v1/artifacts/${id}/edits`,
      request([
        put('a.txt', 'c', digest('a.txt')),
        { op: 'delete', path: 'gone.txt', expect: sha('x') },
      ]),
    );
    assert.equal(conflict.error.code, 'file_conflict');
    assert.deepEqual(
      conflict.error.conflicting_files.map((file) => file.path),
      ['a.txt', 'gone.txt'],
    );
    assert.equal(conflict.error.conflicting_files[1].current_sha256, null);
    await call(
      201,
      `/v1/artifacts/${id}/edits`,
      request([put('index.html', '<p>2</p>', digest('index.html'))]),
    );
  } finally {
    await server.stop();
  }
});
