import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { expect } from '@playwright/test';
import { ArtifreeClient, newRandomId } from '../packages/client/dist/index.js';
import { api, bytes } from './smoke-workspace.mjs';

/**
 * Browser checks for the source capabilities of a runtime frame (decision
 * 0032) against a real artifree-server. A frame lists and reads the source of
 * its pinned revision in parts, and asks the workspace to offer a stored file
 * or the export; the user confirms in the workspace, never in the frame. Bytes
 * the frame read and the files the browser saved are compared by digest with
 * the bytes the server stores. The refusals are checked in the browser (no
 * grant; a page whose own snapshot says "granted" after the grant was
 * revoked) and over the protocol with the real client (an old generation, a
 * head that moved).
 */

const text = 'héllo wörld — 日本語 ✓\nsecond line with "quotes" & <tags>\n';
const binary = Buffer.from([
  0x89, 0x50, 0x4e, 0x47, 0x00, 0xff, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0xfe,
  0x80, 0x7f, 0xc3, 0x28, 0xa0, 0xa1, 0xe2, 0x28, 0xa1, 0x00, 0x01, 0x02,
]);
const manifest = JSON.stringify({
  sdk: 'artifree.app.v1',
  capabilities: ['source.read', 'source.download'],
});

// One page that drives the runtime: each button writes one JSON answer to the
// shared output, so a test clears it, acts, and polls for the next answer.
const entry = `<!doctype html><html lang="en"><head><!-- artifree-runtime -->
<meta charset="utf-8"><title>Source probe</title></head><body>
<h1>Source probe</h1><output id="result">waiting</output>
<div>
<button id="list">List files</button>
<button id="read-text">Read notes</button>
<button id="read-binary">Read blob</button>
<button id="read-empty">Read empty</button>
<button id="read-missing">Read missing</button>
<button id="dl-text">Save notes</button>
<button id="dl-binary">Save blob</button>
<button id="dl-archive">Save archive</button>
<button id="dl-missing">Save missing</button>
</div>
<script>
const out = document.querySelector('#result');
const show = (value) => { out.textContent = JSON.stringify(value); };
const failure = (error) => show({ error: error && error.code ? error.code : 'unknown' });
async function readAll(path) {
  const joined = [];
  let offset = 0;
  let parts = 0;
  let size = 0;
  for (;;) {
    const part = await window.artifree.source.read({ path, offset, length: 7 });
    parts += 1;
    size = part.size;
    for (const byte of part.bytes) joined.push(byte);
    if (part.nextOffset === null) break;
    offset = part.nextOffset;
  }
  return { path, parts, size, base64: btoa(String.fromCharCode(...joined)) };
}
const run = (id, action) => document.querySelector(id).addEventListener('click', () => {
  action().then(show, failure);
});
run('#list', () => window.artifree.source.list());
run('#read-text', () => readAll('notes.txt'));
run('#read-binary', () => readAll('data/blob.png'));
run('#read-empty', () => readAll('empty.txt'));
run('#read-missing', () => readAll('nope.txt'));
run('#dl-text', () => window.artifree.source.requestDownload({ path: 'notes.txt' }));
run('#dl-binary', () => window.artifree.source.requestDownload({ path: 'data/blob.png' }));
run('#dl-archive', () => window.artifree.source.requestDownload());
run('#dl-missing', () => window.artifree.source.requestDownload({ path: 'nope.txt' }));
</script></body></html>`;

const sha256 = (buffer) => createHash('sha256').update(buffer).digest('hex');

async function createProbe(server, title) {
  const { artifact } = await api(server, 'POST', '/v1/artifacts', {
    title,
    files: [
      { path: 'index.html', encoding: 'utf8', content: entry },
      { path: 'artifree-runtime.json', encoding: 'utf8', content: manifest },
      { path: 'notes.txt', encoding: 'utf8', content: text },
      {
        path: 'data/blob.png',
        encoding: 'base64',
        content: binary.toString('base64'),
      },
      { path: 'empty.txt', encoding: 'utf8', content: '' },
    ],
  });
  return artifact;
}

/** Opens the artifact's preview in its own browser context. */
async function open(browser, origin, server, artifact) {
  const context = await browser.newContext({ acceptDownloads: true });
  try {
    const page = await context.newPage();
    page.setDefaultTimeout(15_000);
    const views = [];
    const errors = [];
    const downloads = [];
    page.on('pageerror', (error) => errors.push(error.message));
    page.on('download', (download) => downloads.push(download));
    page.on('response', (response) => {
      const request = response.request();
      if (
        request.method() === 'POST' &&
        new URL(response.url()).pathname.endsWith('/runtime-views') &&
        response.ok()
      )
        views.push(response.json().catch(() => undefined));
    });
    await page.goto(origin);
    const workspace = page.locator('#artifacts');
    await workspace.getByLabel('Access token').fill(server.token);
    await workspace.getByRole('button', { name: 'Connect' }).click();
    await workspace.getByLabel('Search titles').fill(artifact.title);
    await workspace
      .getByRole('navigation', { name: 'Artifacts' })
      .getByRole('button', { name: new RegExp(`^${artifact.title}`) })
      .click();
    const permissions = workspace.getByRole('region', {
      name: 'Runtime permissions',
    });
    await expect(
      permissions.getByText('Declared: source.download, source.read'),
    ).toBeVisible();
    await expect(permissions.getByText('Granted: none')).toBeVisible();
    return {
      context,
      page,
      workspace,
      permissions,
      errors,
      downloads,
      views: async () => (await Promise.all(views)).filter(Boolean),
      frame: workspace.frameLocator('iframe[title^="Preview of"]'),
      element: workspace.locator('iframe[title^="Preview of"]'),
    };
  } catch (error) {
    await context.close();
    throw error;
  }
}

/** Grants one capability and waits for the new view to be shown. */
async function allow(opened, button, granted) {
  const before = await opened.element.getAttribute('src');
  await opened.permissions.getByRole('button', { name: button }).click();
  await expect(
    opened.permissions.getByText(`Granted: ${granted}`),
  ).toBeVisible();
  await expect.poll(() => opened.element.getAttribute('src')).not.toBe(before);
  await expect(
    opened.frame.getByRole('button', { name: 'List files' }),
  ).toBeVisible();
}

/** Clears the shared answer, presses a button, and returns the next answer. */
async function act(opened, name) {
  const result = opened.frame.locator('#result');
  await result.evaluate((element) => {
    element.textContent = 'waiting';
  });
  await opened.frame.getByRole('button', { name }).click();
  await expect
    .poll(async () => result.textContent(), { timeout: 20_000 })
    .not.toBe('waiting');
  return JSON.parse(await result.textContent());
}

/** Presses a download button, then answers the workspace's prompt. */
async function offer(opened, name, answer, inspect = async () => {}) {
  const result = opened.frame.locator('#result');
  await result.evaluate((element) => {
    element.textContent = 'waiting';
  });
  const dialog = opened.page.getByRole('dialog', {
    name: 'Save a file from this preview?',
  });
  const box = await opened.element.boundingBox();
  await opened.frame.getByRole('button', { name }).click();
  await expect(dialog).toBeVisible();
  assert.deepEqual(
    await opened.element.boundingBox(),
    box,
    'The prompt must not move the preview.',
  );
  await inspect();
  const saved = opened.downloads.length;
  const download =
    answer === 'save' ? opened.page.waitForEvent('download') : undefined;
  await dialog
    .getByRole('button', { name: answer === 'save' ? 'Save file' : 'Decline' })
    .click();
  await expect(dialog).toBeHidden();
  await expect
    .poll(async () => result.textContent(), { timeout: 20_000 })
    .not.toBe('waiting');
  const answered = JSON.parse(await result.textContent());
  if (download === undefined) {
    assert.equal(opened.downloads.length, saved, 'Nothing was saved.');
    return { answered };
  }
  const file = await download;
  return { answered, file };
}

async function checkFrame(browser, origin, server, artifact, inspect) {
  const opened = await open(browser, origin, server, artifact);
  try {
    const source = `/v1/artifacts/${artifact.id}/revisions/1`;
    const stored = {
      'notes.txt': await bytes(server, `${source}/files/notes.txt`),
      'data/blob.png': await bytes(server, `${source}/files/data/blob.png`),
      'empty.txt': await bytes(server, `${source}/files/empty.txt`),
    };
    assert.deepEqual(stored['notes.txt'], Buffer.from(text));
    assert.deepEqual(stored['data/blob.png'], binary);

    // Nothing is granted: both capabilities are refused, and no prompt opens.
    assert.deepEqual(await act(opened, 'List files'), { error: 'denied' });
    assert.deepEqual(await act(opened, 'Read notes'), { error: 'denied' });
    assert.deepEqual(await act(opened, 'Save notes'), { error: 'denied' });
    await expect(opened.page.getByRole('dialog')).toHaveCount(0);

    // Reading is its own grant; it does not allow a download request.
    await allow(opened, 'Allow source reading', 'source.read');
    const listing = await act(opened, 'List files');
    assert.equal(listing.revision, 1);
    const listed = await api(server, 'GET', source);
    assert.deepEqual(
      listing.files.map(({ path, size, sha256: digest }) => [
        path,
        size,
        digest,
      ]),
      listed.files.map(({ path, size, sha256: digest }) => [
        path,
        size,
        digest,
      ]),
    );
    for (const [button, path] of [
      ['Read notes', 'notes.txt'],
      ['Read blob', 'data/blob.png'],
      ['Read empty', 'empty.txt'],
    ]) {
      const read = await act(opened, button);
      const joined = Buffer.from(read.base64, 'base64');
      assert.deepEqual(joined, stored[path], `${path} read in parts`);
      assert.equal(read.size, stored[path].length);
      assert.equal(
        sha256(joined),
        listing.files.find((file) => file.path === path).sha256,
      );
      assert.equal(read.parts, Math.max(1, Math.ceil(stored[path].length / 7)));
    }
    assert.deepEqual(await act(opened, 'Read missing'), { error: 'not_found' });
    assert.deepEqual(await act(opened, 'Save notes'), { error: 'denied' });
    await expect(opened.page.getByRole('dialog')).toHaveCount(0);

    // A download needs its own grant and then the user's confirmation.
    await allow(
      opened,
      'Allow download requests',
      'source.download, source.read',
    );
    assert.deepEqual(await act(opened, 'Save missing'), { error: 'not_found' });
    await expect(opened.page.getByRole('dialog')).toHaveCount(0);
    let inspected = false;
    for (const [button, path, name] of [
      ['Save notes', 'notes.txt', 'notes.txt'],
      ['Save blob', 'data/blob.png', 'blob.png'],
    ]) {
      const { answered, file } = await offer(
        opened,
        button,
        'save',
        async () => {
          if (inspected) return;
          inspected = true;
          // Nothing is saved while the prompt waits for the user.
          assert.equal(opened.downloads.length, 0);
          await inspect('source-download-prompt', opened.page);
        },
      );
      assert.deepEqual(answered, {
        started: true,
        kind: 'file',
        bytes: stored[path].length,
      });
      assert.equal(file.suggestedFilename(), name);
      const saved = await readFile(await file.path());
      assert.deepEqual(saved, stored[path]);
      assert.equal(sha256(saved), sha256(stored[path]));
    }
    {
      const { answered, file } = await offer(opened, 'Save archive', 'save');
      assert.equal(answered.kind, 'archive');
      assert.equal(file.suggestedFilename(), `${artifact.id}-r1.zip`);
      const saved = await readFile(await file.path());
      assert.deepEqual(saved, await bytes(server, `${source}/export`));
      assert.equal(answered.bytes, saved.length);
    }

    // Declining is an answer; the next request is refused for a while.
    const { answered } = await offer(opened, 'Save notes', 'decline');
    assert.deepEqual(answered, { error: 'declined' });
    assert.deepEqual(await act(opened, 'Save notes'), {
      error: 'rate_limited',
    });
    await expect(opened.page.getByRole('dialog')).toHaveCount(0);
    assert.deepEqual(opened.errors, []);
    return { views: (await opened.views()).length };
  } finally {
    await opened.context.close();
  }
}

/**
 * The workspace's own snapshot says a capability is granted after the server
 * revoked it. The prompt asks the server first, so none opens.
 */
async function checkStaleSnapshot(browser, origin, server, artifact) {
  const opened = await open(browser, origin, server, artifact);
  try {
    await allow(opened, 'Allow download requests', 'source.download');
    let view;
    await expect
      .poll(async () => {
        const src = await opened.element.getAttribute('src');
        view = (await opened.views()).find(
          (candidate) => candidate.url === src,
        );
        return view !== undefined;
      })
      .toBe(true);
    await api(
      server,
      'PUT',
      `/v1/runtime-sessions/${view.session_id}/artifacts/${artifact.id}/revisions/1/grant`,
      {
        head_revision: view.head_revision,
        expected_version: view.grant_version,
        capabilities: [],
      },
    );
    await expect(
      opened.permissions.getByText('Granted: source.download'),
    ).toBeVisible();
    assert.deepEqual(await act(opened, 'Save notes'), { error: 'denied' });
    await expect(opened.page.getByRole('dialog')).toHaveCount(0);
    assert.equal(opened.downloads.length, 0);
    assert.deepEqual(opened.errors, []);
  } finally {
    await opened.context.close();
  }
}

/** Calls over the protocol, as a frame that kept using an old view could. */
async function checkOldViews(server, artifact) {
  const client = new ArtifreeClient({
    baseUrl: server.api_url,
    accessToken: server.token,
  });
  await client.openRuntimeSession();
  const refused = async (promise, status, code) => {
    const error = await promise.then(
      () => assert.fail('The call must be refused.'),
      (rejection) => rejection,
    );
    assert.equal(error.status, status);
    assert.equal(error.code, code);
  };
  const viewId = newRandomId();
  // A view is usable once its page was served, as a browser does by loading
  // the frame; the bootstrap is served once per view.
  const created = async (generation) => {
    const view = await client.createRuntimeView(
      artifact.id,
      1,
      viewId,
      generation,
    );
    const page = await fetch(view.url);
    assert.equal(page.status, 200);
    await page.arrayBuffer();
    return view;
  };
  const first = await created(1);
  await refused(client.runtimeSourceList(first), 403, 'forbidden');
  const reading = await client.setRuntimeGrant(first, ['source.read']);
  assert.ok(reading.version > first.grant_version);
  // The view was issued before the grant, so it still does not hold it.
  await refused(client.runtimeSourceList(first), 403, 'forbidden');
  const second = await created(2);
  assert.equal((await client.runtimeSourceList(second)).files.length, 5);
  await refused(
    client.runtimeSourceOffer(second, { kind: 'archive' }),
    403,
    'forbidden',
  );
  await client.setRuntimeGrant(second, ['source.read', 'source.download']);
  const third = await created(3);
  assert.equal(
    (await client.runtimeSourceOffer(third, { kind: 'archive' })).kind,
    'archive',
  );
  // The generation before the newest is gone.
  await refused(client.runtimeSourceList(second), 404, 'not_found');

  // The head moves: the pinned view is stale for every source call.
  await api(server, 'POST', `/v1/artifacts/${artifact.id}/revisions`, {
    base_revision: 1,
    changes: [
      { op: 'put', path: 'notes.txt', encoding: 'utf8', content: 'new' },
    ],
  });
  await refused(client.runtimeSourceList(third), 409, 'revision_conflict');
  await refused(
    client.runtimeSourceRead(third, {
      path: 'notes.txt',
      offset: 0,
      length: 4,
    }),
    409,
    'revision_conflict',
  );
  await refused(
    client.runtimeSourceOffer(third, { kind: 'file', path: 'notes.txt' }),
    409,
    'revision_conflict',
  );
  await refused(
    client.runtimeSourceDownload(third, {
      kind: 'archive',
      path: null,
      file_name: 'x.zip',
      size: null,
      sha256: null,
    }),
    409,
    'revision_conflict',
  );
  await client.closeRuntimeSession();
}

/** The checks of decision 0032 in a real browser. */
export async function checkSource({
  browser,
  origin,
  server,
  checkAccessibility,
}) {
  const main = await createProbe(server, 'Source probe');
  const frame = await checkFrame(
    browser,
    origin,
    server,
    main,
    checkAccessibility,
  );
  const other = await createProbe(server, 'Source probe snapshot');
  await checkStaleSnapshot(browser, origin, server, other);
  const old = await createProbe(server, 'Source probe protocol');
  await checkOldViews(server, old);
  return {
    read: 'parts joined by digest',
    saved: 'file, binary and archive by digest',
    refused: ['no grant', 'stale snapshot', 'old generation', 'moved head'],
    views: frame.views,
  };
}
