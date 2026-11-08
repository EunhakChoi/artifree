import assert from 'node:assert/strict';
import { expect } from '@playwright/test';
import { raw } from './smoke-assets.mjs';
import { forgeReset } from './smoke-history.mjs';
import { api } from './smoke-workspace.mjs';

const FILE_LIMIT = 5 * 1024 * 1024;
const SPACE_FILES = 5;

/**
 * Browser checks for duplicates (decisions 0012 and 0045, rule 10) against a
 * real artifree-server: a copy of the revision the preview shows, a retry
 * after a lost answer that creates one copy, the source left as it was, the
 * asset copy that is off by default and on request copies the same asset
 * IDs under its own key, a refused asset copy that creates nothing, and the
 * draft lock. The page has its own browser context.
 */
export async function checkDuplicate({
  browser,
  origin,
  server,
  checkAccessibility,
}) {
  const { artifact } = await api(server, 'POST', '/v1/artifacts', {
    title: 'Duplicate demo',
    files: [
      { path: 'index.html', encoding: 'utf8', content: '<p>first</p>\r\n' },
      {
        path: 'logo.png',
        encoding: 'base64',
        content: Buffer.from([0x89, 0x50, 0x4e, 0x47, 0, 0xff]).toString(
          'base64',
        ),
      },
    ],
  });
  const source = `/v1/artifacts/${artifact.id}`;
  await api(server, 'POST', `${source}/revisions`, {
    base_revision: 1,
    changes: [
      { op: 'put', path: 'index.html', encoding: 'utf8', content: '<p>2</p>' },
    ],
  });
  await api(server, 'PATCH', source, {
    base_metadata_revision: 1,
    pinned: true,
  });
  let rawRequests = 0;
  async function seedUpload(id, base, content, key) {
    rawRequests += 1;
    return raw(
      server,
      'POST',
      `/v1/artifacts/${id}/assets`,
      {
        base_asset_version: base,
        content_type: 'text/plain',
        content_base64: content.toString('base64'),
      },
      key,
    );
  }
  async function listAssets(id) {
    rawRequests += 1;
    const { status, json } = await raw(
      server,
      'GET',
      `/v1/artifacts/${id}/assets`,
    );
    assert.equal(status, 200, `Listing assets failed: ${status}`);
    return json;
  }
  const seeded = await seedUpload(
    artifact.id,
    0,
    Buffer.from('duplicate notes\n'),
    'smoke-duplicate-asset-0',
  );
  assert.equal(seeded.status, 201, `Seeding an asset failed: ${seeded.status}`);
  const sourceAssets = (await listAssets(artifact.id)).assets.map(
    (asset) => asset.id,
  );
  const manifest = async (path) =>
    (await api(server, 'GET', path)).files.map(({ path, sha256 }) => [
      path,
      sha256,
    ]);

  const context = await browser.newContext();
  try {
    const tab = await context.newPage();
    tab.setDefaultTimeout(10_000);
    const errors = [];
    tab.on('pageerror', (error) => errors.push(error.message));
    const sent = [];
    tab.on('request', (request) => {
      if (request.method() === 'POST' && request.url().endsWith('/duplicate'))
        sent.push({
          key: request.headers()['idempotency-key'],
          body: request.postData(),
        });
    });
    await tab.goto(origin);
    const workspace = tab.locator('#artifacts');
    await workspace.getByLabel('Access token').fill(server.token);
    await workspace.getByRole('button', { name: 'Connect' }).click();
    await workspace.getByLabel('Search titles').fill('Duplicate demo');
    const nav = workspace.getByRole('navigation', { name: 'Artifacts' });
    await nav.getByRole('button', { name: /^Duplicate demo/ }).click();
    await expect(
      workspace.getByRole('heading', { name: 'Duplicate demo', exact: true }),
    ).toBeVisible();

    // The dialog offers the revision the preview shows.
    await workspace.locator('#preview-revision').selectOption('1');
    await workspace
      .getByRole('button', { name: 'Duplicate', exact: true })
      .click();
    const dialog = tab.getByRole('dialog', {
      name: 'Duplicate Duplicate demo',
    });
    await expect(dialog.getByLabel('Revision')).toHaveValue('1');
    // The asset copy is off by default and again each time the dialog opens.
    const copyAssets = dialog.getByRole('checkbox', {
      name: 'Copy uploaded assets',
    });
    await expect(copyAssets).not.toBeChecked();
    await expect(dialog).toContainText(
      'application data and uploaded assets are not',
    );
    await expect(dialog).toContainText('this dialog does not copy it');
    await copyAssets.check();
    await checkAccessibility('workspace-duplicate-dialog', tab);

    // A dialog opened before the server's data may have been replaced
    // sends nothing; opened again, it offers what the page reads now. The
    // preview's choice belonged to the earlier read, so the latest is
    // offered (0011 decision 8).
    await forgeReset(tab);
    await expect(
      dialog.getByText("The server's data may have been replaced"),
    ).toBeVisible();
    await expect(
      dialog.getByRole('button', { name: 'Duplicate', exact: true }),
    ).toBeDisabled();
    await dialog.getByRole('button', { name: 'Cancel' }).click();
    await workspace
      .getByRole('button', { name: 'Duplicate', exact: true })
      .click();
    await expect(dialog.getByLabel('Revision')).toHaveValue('2');
    await expect(copyAssets).not.toBeChecked();
    await dialog.getByLabel('Revision').selectOption('1');

    // The first answer is lost; sending again creates no second copy.
    let lost = false;
    await tab.route('**/duplicate', async (route) => {
      if (lost) return route.fallback();
      lost = true;
      await route.fetch();
      return route.fulfill({
        status: 503,
        json: { error: { code: 'unavailable', message: 'Response lost.' } },
      });
    });
    const send = dialog.getByRole('button', { name: 'Duplicate', exact: true });
    await send.click();
    await expect(dialog.getByText('Outcome unknown')).toBeVisible();
    await send.click();
    await expect(dialog).toBeHidden();
    await tab.unrouteAll({ behavior: 'ignoreErrors' });
    assert.equal(sent.length, 2);
    assert.equal(sent[1].key, sent[0].key, 'The retry used a new key.');
    assert.deepEqual(
      sent.map((request) => JSON.parse(request.body)),
      [{}, {}],
      'A default duplicate asked for more than the files.',
    );

    // The copy opens: revision 1 with the files of the source's revision 1.
    await expect(
      workspace.getByRole('heading', { name: 'Duplicate demo (Copy)' }),
    ).toBeVisible();
    await expect(
      workspace.getByText('Revision 1', { exact: true }),
    ).toBeVisible();
    const copies = await api(
      server,
      'GET',
      `/v1/artifacts?q=${encodeURIComponent('Duplicate demo (Copy)')}`,
    );
    assert.equal(copies.artifacts.length, 1, 'The retry created one copy.');
    const [copy] = copies.artifacts;
    assert.equal(copy.pinned, false);
    assert.equal(copy.current_revision, 1);
    assert.deepEqual(
      await manifest(`/v1/artifacts/${copy.id}/revisions/1`),
      await manifest(`${source}/revisions/1`),
    );
    const left = await api(server, 'GET', source);
    assert.deepEqual(
      [left.current_revision, left.pinned, left.title],
      [2, true, 'Duplicate demo'],
      'The source stays as it was.',
    );

    // The default copy has no assets, though the source has one.
    const views = workspace.getByRole('tablist', { name: 'Artifact views' });
    const assetsTab = views.getByRole('tab', { name: 'Assets', exact: true });
    const panel = workspace.getByRole('tabpanel', { name: 'Assets' });
    await assetsTab.click();
    await expect(panel.getByText('Asset version 0')).toBeVisible();
    await expect(panel.getByText('No assets')).toBeVisible();
    assert.deepEqual(
      (await listAssets(copy.id)).assets,
      [],
      'A default duplicate copied assets.',
    );

    // Asked for, the copy has the same asset IDs, also from an older
    // revision. Its first answer is cut; the same request is sent again
    // under the same key, which differs from the default duplicate's.
    // Opens the artifact named exactly `title`, which has no regex syntax.
    async function open(title) {
      await workspace.getByLabel('Search titles').fill(title);
      await nav
        .getByRole('button', {
          name: new RegExp(`^${title}\\s*(Pinned,\\s*)?r\\d+$`),
        })
        .click();
      await expect(
        workspace.getByRole('heading', { name: title, exact: true }),
      ).toBeVisible();
    }
    await open('Duplicate demo');
    await workspace
      .getByRole('button', { name: 'Duplicate', exact: true })
      .click();
    await expect(copyAssets).not.toBeChecked();
    await dialog.getByLabel('Revision').selectOption('1');
    await copyAssets.check();
    let cut = 0;
    await tab.route('**/duplicate', async (route) => {
      if (cut > 0) return route.fallback();
      cut += 1;
      await route.fetch();
      return route.abort('connectionreset');
    });
    await send.click();
    await expect(dialog.getByText('Outcome unknown')).toBeVisible();
    await send.click();
    await expect(dialog).toBeHidden();
    await tab.unrouteAll({ behavior: 'ignoreErrors' });
    assert.equal(sent.length, 4);
    assert.notEqual(
      sent[2].key,
      sent[0].key,
      'An asset copy reused the default duplicate key.',
    );
    assert.equal(sent[3].key, sent[2].key, 'The retry used a new key.');
    assert.deepEqual(
      sent.slice(2).map((request) => JSON.parse(request.body)),
      [{ copy_assets: true }, { copy_assets: true }],
      'The asset copy was not asked for as it was chosen.',
    );
    await expect(
      workspace.getByRole('heading', { name: 'Duplicate demo (Copy)' }),
    ).toBeVisible();
    const both = await api(
      server,
      'GET',
      `/v1/artifacts?q=${encodeURIComponent('Duplicate demo (Copy)')}`,
    );
    assert.equal(both.artifacts.length, 2, 'The retry created one copy.');
    const withAssets = both.artifacts.find((item) => item.id !== copy.id);
    const copied = await listAssets(withAssets.id);
    assert.deepEqual(
      copied.assets.map((asset) => asset.id),
      sourceAssets,
      'The copy has other assets.',
    );
    assert.equal(copied.asset_version, 1);
    await assetsTab.click();
    await expect(panel.getByText('Asset version 1')).toBeVisible();
    await expect(
      panel.getByRole('list', { name: 'Assets' }).getByRole('listitem'),
    ).toHaveCount(sourceAssets.length);
    await expect(
      panel.getByRole('button', {
        name: `Download asset ${sourceAssets[0].slice(0, 12)}`,
      }),
    ).toBeVisible();

    // An asset copy over the limits is refused whole: nothing is created.
    // The source fills its space; other artifacts fill the actor's quota
    // until one more 5 MiB upload is refused, so the 25 MiB copy cannot fit.
    const { artifact: full } = await api(server, 'POST', '/v1/artifacts', {
      title: 'Duplicate quota',
      files: [{ path: 'index.html', encoding: 'utf8', content: '<p>q</p>' }],
    });
    const filler = Buffer.alloc(FILE_LIMIT, 'f');
    for (let index = 0; index < SPACE_FILES; index += 1) {
      const { status } = await seedUpload(
        full.id,
        index,
        filler,
        `smoke-duplicate-full-${index}`,
      );
      assert.equal(status, 201, `Filling the source failed: ${status}`);
    }
    const fillers = [];
    for (let index = 0; ; index += 1) {
      if (index % SPACE_FILES === 0) {
        const created = await api(server, 'POST', '/v1/artifacts', {
          title: `Duplicate filler ${fillers.length}`,
          files: [
            { path: 'index.html', encoding: 'utf8', content: '<p>f</p>' },
          ],
        });
        fillers.push({ id: created.artifact.id, version: 0, assets: [] });
      }
      const space = fillers.at(-1);
      const { status, json } = await seedUpload(
        space.id,
        space.version,
        filler,
        `smoke-duplicate-fill-${index}`,
      );
      if (status === 422 && json.error.code === 'asset_limit_exceeded') break;
      assert.equal(status, 201, `Filling the quota failed: ${status}`);
      assert.ok(index < 20, 'The actor quota never filled.');
      space.version = json.asset_version;
      space.assets.push(json.asset.id);
    }
    await open('Duplicate quota');
    await workspace
      .getByRole('button', { name: 'Duplicate', exact: true })
      .click();
    const fullDialog = tab.getByRole('dialog', {
      name: 'Duplicate Duplicate quota',
    });
    await fullDialog
      .getByRole('checkbox', { name: 'Copy uploaded assets' })
      .check();
    await fullDialog
      .getByRole('button', { name: 'Duplicate', exact: true })
      .click();
    const refused = fullDialog
      .getByRole('alert')
      .filter({ hasText: 'Not changed' });
    await expect(refused).toContainText(
      'Over the asset limit: 5 MiB per file, 64 files and 25 MiB per artifact',
    );
    await expect(refused).toContainText('so no copy was made');
    const none = await api(
      server,
      'GET',
      `/v1/artifacts?q=${encodeURIComponent('Duplicate quota (Copy)')}`,
    );
    assert.deepEqual(none.artifacts, [], 'A refused asset copy created one.');
    await fullDialog.getByRole('button', { name: 'Cancel' }).click();
    // Frees the actor's quota for the checks after this one.
    let unfill = 0;
    for (const space of fillers)
      for (const id of space.assets) {
        rawRequests += 1;
        const { status, json } = await raw(
          server,
          'DELETE',
          `/v1/artifacts/${space.id}/assets/${id}`,
          { base_asset_version: space.version },
          `smoke-duplicate-unfill-${unfill}`,
        );
        unfill += 1;
        assert.equal(status, 200, `Freeing the quota failed: ${status}`);
        space.version = json.asset_version;
      }

    // A draft locks duplicating, since the copy would open in its place.
    await workspace.getByRole('tab', { name: 'Source' }).click();
    await workspace
      .getByLabel('index.html', { exact: true })
      .fill('<p>draft</p>');
    await expect(
      workspace.getByRole('button', { name: 'Duplicate', exact: true }),
    ).toBeDisabled();
    await expect(
      workspace.getByText(/Save or discard your changes to duplicate/),
    ).toBeVisible();

    assert.deepEqual(errors, [], 'Page errors.');
    return {
      cases: [
        'default-off-no-assets',
        'lost-default-retry',
        'asset-copy-same-ids',
        'lost-asset-copy-same-key',
        'asset-copy-quota-refused',
        'draft-lock',
      ],
      rawRequests,
    };
  } finally {
    await context.close();
  }
}
