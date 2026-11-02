import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { expect } from '@playwright/test';
import { api, bytes } from './smoke-workspace.mjs';

const NOTES = Buffer.from('héllo — 日本語\n', 'utf8');
const SPRITE = Buffer.from([0x89, 0x50, 0x00, 0xff, 0x0d, 0x0a, 0x1a]);
const SAVED_NOTES = Buffer.from('ünï — 保存\n', 'utf8');
const SAVED_SPRITE = Buffer.from([0x00, 0xff, 0x01, 0xfe]);

// The frame shows where it runs and what each request answered, so the check
// reads what the frame itself saw, not only what the workspace did.
const entry = `<!doctype html><html lang="en"><head><!-- artifree-runtime -->
<meta charset="utf-8"><title>Source probe</title></head><body>
<h1>Source probe</h1><p id="where"></p><output id="out">idle</output>
<button data-act="list">List</button>
<button data-act="read">Read notes</button>
<button data-act="save">Save</button>
<button data-act="file">Download notes</button>
<button data-act="archive">Download archive</button>
<button data-act="twice">Ask twice</button>
<button data-act="flood">Flood</button>
<script>
document.querySelector('#where').textContent = location.pathname;
const out = document.querySelector('#out');
const show = (value) => { out.textContent = JSON.stringify(value); };
const acts = {
  list: async () => show(await window.artifree.listSource()),
  read: async () => {
    const part = await window.artifree.readSource('notes.txt');
    show({ text: new TextDecoder().decode(part.bytes), sha256: part.sha256, size: part.size, nextOffset: part.nextOffset });
  },
  save: async () => show(await window.artifree.saveSource({
    key: 'smoke-source-save-1',
    changes: [
      { op: 'put', path: 'notes.txt', content: '\\u00fcn\\u00ef \\u2014 \\u4fdd\\u5b58\\n' },
      { op: 'put', path: 'sprite.png', content: new Uint8Array([0, 255, 1, 254]) },
    ],
  })),
  file: async () => show(await window.artifree.requestDownload({ path: 'notes.txt' })),
  archive: async () => show(await window.artifree.requestDownload()),
  // Asks again the moment the first question is refused.
  twice: async () => {
    const codes = [];
    for (let attempt = 0; attempt < 2; attempt += 1) {
      try { await window.artifree.requestDownload({ path: 'notes.txt' }); codes.push('started'); }
      catch (error) { codes.push(error.code); }
    }
    show({ first: codes[0], second: codes[1] });
  },
  // Sixty requests in one breath.
  flood: async () => {
    const codes = await Promise.all(Array.from({ length: 60 }, () =>
      window.artifree.listSource().then(() => 'ok', (error) => error.code)));
    const counts = {};
    for (const code of codes) counts[code] = (counts[code] ?? 0) + 1;
    show(counts);
  },
};
for (const button of document.querySelectorAll('button')) {
  button.addEventListener('click', async () => {
    try { await acts[button.dataset.act](); }
    catch (error) { show({ error: error.code ?? 'unexpected', currentRevision: error.currentRevision ?? null }); }
  });
}
</script></body></html>`;

const manifest = JSON.stringify({
  sdk: 'artifree.app.v1',
  capabilities: ['source.read', 'source.write', 'source.download'],
});

const digest = (data) => createHash('sha256').update(data).digest('hex');

/**
 * Browser checks for the source and download capabilities (decision 0032)
 * against a real artifree-server: a frame sees nothing without a grant; each
 * grant is a separate button; the bytes a frame reads and the bytes the user
 * downloads equal the stored revision, and a frame's save arrives as the
 * stored revision; a download is never saved before the user confirms, and
 * an unanswered question dies with its frame; an old view generation is
 * refused by the server. The page has its own browser context.
 */
export async function checkSource({
  browser,
  origin,
  server,
  checkAccessibility,
}) {
  const { artifact } = await api(server, 'POST', '/v1/artifacts', {
    title: 'Source probe',
    files: [
      { path: 'index.html', encoding: 'utf8', content: entry },
      { path: 'artifree-runtime.json', encoding: 'utf8', content: manifest },
      {
        path: 'notes.txt',
        encoding: 'base64',
        content: NOTES.toString('base64'),
      },
      {
        path: 'sprite.png',
        encoding: 'base64',
        content: SPRITE.toString('base64'),
      },
    ],
  });
  const revision = `/v1/artifacts/${artifact.id}/revisions`;

  const context = await browser.newContext({ acceptDownloads: true });
  try {
    const page = await context.newPage();
    // Firefox on a loaded runner is slow; nothing here depends on speed.
    page.setDefaultTimeout(30_000);
    const check = expect.configure({ timeout: 30_000 });
    const errors = [];
    page.on('pageerror', (error) => errors.push(error.message));
    const views = [];
    const downloadRequests = [];
    const pending = [];
    page.on('response', (response) => {
      const url = new URL(response.url());
      if (response.request().method() !== 'POST') return;
      if (url.pathname.endsWith('/runtime-views') && response.ok())
        pending.push(response.json().then((view) => views.push(view)));
    });
    page.on('request', (request) => {
      if (
        request.method() === 'POST' &&
        new URL(request.url()).pathname.endsWith('/download')
      )
        downloadRequests.push(request.url());
    });
    const downloads = [];
    page.on('download', (download) => downloads.push(download));
    // The one response to the next save the workspace sends for a frame.
    const nextSave = () =>
      page.waitForResponse(
        (response) =>
          response.request().method() === 'POST' &&
          new URL(response.url()).pathname.endsWith('/source/save'),
      );

    await page.goto(origin);
    const workspace = page.locator('#artifacts');
    await workspace.getByLabel('Access token').fill(server.token);
    await workspace.getByRole('button', { name: 'Connect' }).click();
    await workspace.getByLabel('Search titles').fill('Source probe');
    await workspace
      .getByRole('navigation', { name: 'Artifacts' })
      .getByRole('button', { name: /^Source probe/ })
      .click();
    const permissions = workspace.getByRole('region', {
      name: 'Runtime permissions',
    });
    await check(
      permissions.getByText(
        'Declared: source.download, source.read, source.write',
      ),
    ).toBeVisible();
    await check(permissions.getByText('Granted: none')).toBeVisible();
    const frame = workspace.frameLocator('iframe[title^="Preview of"]');
    const element = workspace.locator('iframe[title^="Preview of"]');
    const offer = workspace.getByRole('alert', { name: 'Download request' });
    const answer = frame.locator('#out');

    // Waits until the frame on screen is the newest view the server issued,
    // so a click never lands in a frame that is about to be replaced.
    const ready = async () => {
      await check
        .poll(async () => {
          await Promise.all(pending);
          const last = views.at(-1);
          return last === undefined
            ? false
            : (await element.getAttribute('src')) === last.url &&
                (await frame.locator('#where').textContent()) ===
                  new URL(last.url).pathname;
        })
        .toBe(true);
    };
    // Clears the frame's answer first, so that what is read afterwards is the
    // answer to this action and not to an earlier one.
    const act = async (name) => {
      await answer.evaluate((output) => {
        output.textContent = 'idle';
      });
      await frame.locator(`[data-act="${name}"]`).click();
    };
    const grant = async (label, revoke) => {
      const before = views.length;
      await permissions.getByRole('button', { name: label }).click();
      await permissions.getByRole('button', { name: revoke }).waitFor();
      await check.poll(() => views.length).toBeGreaterThan(before);
      await ready();
    };
    const answered = async () => JSON.parse(await answer.textContent());
    const refusedAs = async (code) => {
      await check(answer).toHaveText(
        JSON.stringify({ error: code, currentRevision: null }),
      );
    };

    // 1. Nothing is granted: the frame is told so and learns nothing.
    await ready();
    await act('list');
    await refusedAs('not_granted');
    await act('read');
    await refusedAs('not_granted');
    // A frame never allowed to ask cannot even put a question to the user.
    await act('file');
    await refusedAs('not_granted');
    await check(offer).toHaveCount(0);

    // 2. Reading is one grant. It lists exactly the stored files and reads
    // the stored bytes; editing and downloading are still refused.
    await grant('Allow reading source', 'Revoke reading source');
    await act('list');
    await check(answer).toContainText('"revision":1');
    const listed = await answered();
    const stored = await api(server, 'GET', `${revision}/1`);
    assert.deepEqual(
      listed.files.map(({ path, size, sha256 }) => ({ path, size, sha256 })),
      stored.files.map(({ path, size, sha256 }) => ({ path, size, sha256 })),
    );
    assert.equal(
      listed.files.find(({ path }) => path === 'notes.txt').sha256,
      digest(NOTES),
    );
    await act('read');
    await check(answer).toContainText('"nextOffset":null');
    const read = await answered();
    assert.equal(read.text, NOTES.toString('utf8'));
    assert.equal(read.sha256, digest(NOTES));
    // The workspace forwards the frame's save and the server refuses it.
    const refusal = nextSave();
    await act('save');
    assert.equal((await refusal).status(), 403);
    await refusedAs('not_granted');
    await act('file');
    await refusedAs('not_granted');
    assert.deepEqual(downloadRequests, []);
    assert.equal(
      (await api(server, 'GET', `/v1/artifacts/${artifact.id}`))
        .current_revision,
      1,
    );

    // 3. Downloads are a third grant. Asking only opens a question for the
    // user: nothing is fetched or saved until the user confirms.
    await grant('Allow downloads', 'Revoke downloads');
    await act('file');
    await check(offer).toContainText('notes.txt');
    await check(offer).toContainText('revision 1');
    await checkAccessibility('runtime-download-request', page);
    assert.deepEqual(downloadRequests, [], 'No bytes before the user agrees.');
    assert.equal(downloads.length, 0);
    // The user agrees: the browser saves the stored bytes. The button takes
    // presses only a moment after the question appears; clicking waits for it.
    const saving = page.waitForEvent('download');
    await offer.getByRole('button', { name: 'Download' }).click();
    const file = await saving;
    assert.equal(file.suggestedFilename(), 'notes.txt');
    assert.deepEqual(await readDownload(file), NOTES);
    await check(answer).toContainText('"status":"started"');
    const receipt = await answered();
    assert.deepEqual(
      [receipt.kind, receipt.name, receipt.size, receipt.revision],
      ['file', 'notes.txt', NOTES.length, 1],
    );
    assert.equal(downloadRequests.length, 1);

    // The whole revision is the same archive the owner's export gives.
    await act('archive');
    const archiveSaving = page.waitForEvent('download');
    await offer.getByRole('button', { name: 'Download' }).click();
    const archive = await archiveSaving;
    assert.equal(archive.suggestedFilename(), `${artifact.id}-r1.zip`);
    assert.deepEqual(
      await readDownload(archive),
      await bytes(server, `${revision}/1/export`),
    );

    // A question left open dies with its frame: replacing the frame by
    // changing a grant leaves no pending offer and saves nothing.
    await act('file');
    await check(offer).toBeVisible();
    const sent = downloadRequests.length;
    const saved = downloads.length;
    await grant('Allow editing source', 'Revoke editing source');
    await check(offer).toBeHidden();
    assert.equal(downloadRequests.length, sent);
    assert.equal(downloads.length, saved);

    // A frame that asks again the moment it is refused is refused again, and
    // the user is not asked a second time.
    await act('twice');
    await check(offer).toBeVisible();
    await offer.getByRole('button', { name: 'Decline' }).click();
    await check(answer).toContainText('"second"');
    assert.deepEqual(await answered(), {
      first: 'declined',
      second: 'declined',
    });
    await check(offer).toBeHidden();
    assert.equal(downloadRequests.length, sent);
    assert.equal(downloads.length, saved);

    // 4. An old generation of the view is refused by the server, with the
    // real initialization the frame once had.
    await Promise.all(pending);
    const old = views.find(
      (view) =>
        view.view_id === views.at(-1).view_id &&
        view.generation < views.at(-1).generation,
    );
    assert.ok(old, 'An earlier generation of the same view must exist.');
    const replay = await fetch(
      `${server.api_url}/v1/artifacts/${artifact.id}/revisions/1/runtime-views/${old.view_id}/${old.generation}/source/read`,
      {
        method: 'POST',
        headers: {
          authorization: `Bearer ${server.token}`,
          'content-type': 'application/json',
        },
        body: JSON.stringify({
          initialization: old.initialization,
          path: 'notes.txt',
        }),
      },
    );
    assert.equal(replay.status, 404);
    assert.doesNotMatch(await replay.text(), /notes|héllo/);

    // 5. With the edit grant a frame saves, once, as a new revision of the
    // stored bytes, from the revision it is pinned to.
    const committing = nextSave();
    await act('save');
    const response = await committing;
    assert.equal(response.status(), 200);
    const body = await response.json();
    const request = response.request().postDataJSON();
    assert.equal(body.revision, 2);
    assert.equal(body.replayed, false);
    assert.deepEqual(request.changes.map(({ path }) => path).sort(), [
      'notes.txt',
      'sprite.png',
    ]);
    assert.deepEqual(
      await bytes(server, `${revision}/2/files/notes.txt`),
      SAVED_NOTES,
    );
    assert.deepEqual(
      await bytes(server, `${revision}/2/files/sprite.png`),
      SAVED_SPRITE,
    );
    assert.deepEqual(
      await bytes(server, `${revision}/1/files/notes.txt`),
      NOTES,
      'The pinned revision is unchanged.',
    );
    // The answer describes the files this save wrote and no others.
    assert.deepEqual(body.files.map(({ path }) => path).sort(), [
      'notes.txt',
      'sprite.png',
    ]);
    assert.equal(
      body.files.find(({ path }) => path === 'notes.txt').sha256,
      digest(SAVED_NOTES),
    );

    // 6. The permissions belonged to revision 1. Once the source has a new
    // head the preview starts over without them, and the user allows again.
    await check(permissions.getByText('Granted: none')).toBeVisible();
    await grant('Allow reading source', 'Revoke reading source');
    await act('read');
    await check(answer).toContainText('"text"');
    assert.equal((await answered()).text, SAVED_NOTES.toString('utf8'));

    // 7. A frame that floods is slowed by the workspace, and the user can
    // still take the permission away at once.
    await act('flood');
    await check(answer).toContainText('"ok"');
    const counts = await answered();
    assert.equal(
      Object.values(counts).reduce((sum, value) => sum + value, 0),
      60,
    );
    const slowed = (counts.rate_limited ?? 0) + (counts.busy ?? 0);
    assert.ok(
      slowed >= 1,
      `A flood must be limited: ${JSON.stringify(counts)}`,
    );
    for (const code of Object.keys(counts))
      assert.ok(['ok', 'rate_limited', 'busy'].includes(code), code);
    await permissions
      .getByRole('button', { name: 'Revoke reading source' })
      .click();
    await check(permissions.getByText('Granted: none')).toBeVisible();
    assert.deepEqual(errors, []);
  } finally {
    await context.close();
  }
  return {
    status: 'passed',
    grants: ['source.read', 'source.write', 'source.download'],
  };
}

async function readDownload(download) {
  return readFile(await download.path());
}
