import assert from 'node:assert/strict';
import { expect } from '@playwright/test';
import { forgeReset } from './smoke-history.mjs';
import { api, bytes, hold } from './smoke-workspace.mjs';

/**
 * Browser checks for the code workspace (decision 0017) against a real
 * artifree-server: a draft spans files, moves bytes it never shows, and is
 * saved as one revision; paths the server would refuse are refused before
 * anything is sent; a moved file that cannot be read sends nothing; and a
 * revision saved elsewhere leaves the draft as it is. The page has its own
 * browser context.
 */
export async function checkEditor({
  browser,
  origin,
  server,
  checkAccessibility,
}) {
  const logo = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0, 0xff, 0x0d, 0x0a]);
  const { artifact } = await api(server, 'POST', '/v1/artifacts', {
    title: 'Editor demo',
    files: [
      { path: 'index.html', encoding: 'utf8', content: '<p>one</p>\r\n' },
      { path: 'style.css', encoding: 'utf8', content: 'p { color: red }\n' },
      {
        path: 'img/logo.png',
        encoding: 'base64',
        content: logo.toString('base64'),
      },
    ],
  });
  const source = `/v1/artifacts/${artifact.id}`;

  const context = await browser.newContext();
  try {
    const tab = await context.newPage();
    tab.setDefaultTimeout(10_000);
    const errors = [];
    tab.on('pageerror', (error) => errors.push(error.message));
    await tab.goto(origin);
    const workspace = tab.locator('#artifacts');
    await workspace.getByLabel('Access token').fill(server.token);
    await workspace.getByRole('button', { name: 'Connect' }).click();
    await workspace.getByLabel('Search titles').fill('Editor demo');
    await workspace
      .getByRole('navigation', { name: 'Artifacts' })
      .getByRole('button', { name: /^Editor demo/ })
      .click();
    await expect(
      workspace.getByRole('heading', { name: 'Editor demo', exact: true }),
    ).toBeVisible();
    await workspace.getByRole('tab', { name: 'Source' }).click();

    const panel = workspace.getByRole('tabpanel', { name: 'Source' });
    const files = panel.getByRole('navigation', { name: 'Files' });
    const fileButton = (path) =>
      files.getByRole('button', {
        name: new RegExp(`^${path.replaceAll('.', '\\.')}( |$)`),
      });
    const save = workspace.getByRole('button', { name: 'Save revision' });

    // A draft spans files: switching files keeps every edit.
    const index = workspace.getByLabel('index.html');
    await expect(index).toHaveValue('<p>one</p>\n');
    await index.fill('<p>two</p>\n');
    await fileButton('style.css').click();
    const style = workspace.getByLabel('style.css');
    await style.fill('p { color: blue }\n');
    await fileButton('index.html').click();
    await expect(index).toHaveValue('<p>two</p>\n');
    await expect(fileButton('style.css')).toContainText('Edited');
    await expect(
      workspace.getByRole('button', { name: 'Disconnect' }),
    ).toBeDisabled();

    // A path the server would refuse is refused in the dialog.
    await fileButton('img/logo.png').click();
    await expect(
      workspace.getByText('img/logo.png is not a text file.'),
    ).toBeVisible();
    await panel.getByRole('button', { name: 'Rename file' }).click();
    const dialog = tab.getByRole('dialog', { name: 'Rename img/logo.png' });
    const pathField = dialog.getByLabel('Path');
    const confirm = dialog.getByRole('button', { name: 'Rename' });
    await pathField.fill('assets/logo.exe');
    await confirm.click();
    await expect(dialog.getByText('not one Artifree serves')).toBeVisible();
    await expect(pathField).toHaveAttribute('aria-invalid', 'true');
    await checkAccessibility('editor-invalid-path', tab);
    await pathField.fill('Index.html');
    await expect(
      dialog.getByText('index.html differs from it only by case.'),
    ).toBeVisible();
    await pathField.fill('assets/logo.png');
    await confirm.click();
    await expect(dialog).toBeHidden();
    await expect(fileButton('assets/logo.png')).toContainText('Added');
    await expect(fileButton('img/logo.png')).toContainText('Deleted');

    // A new file, a deleted one, and a draft the server would refuse.
    await files.getByRole('button', { name: 'New file' }).click();
    const added = tab.getByRole('dialog', { name: 'New file' });
    await added.getByLabel('Path').fill('pages/about.html');
    await added.getByRole('button', { name: 'Add' }).click();
    await workspace.getByLabel('pages/about.html').fill('<p>about</p>');
    await fileButton('style.css').click();
    await panel.getByRole('button', { name: 'Delete file' }).click();
    await expect(fileButton('style.css')).toContainText('Deleted');
    await fileButton('index.html').click();
    await panel.getByRole('button', { name: 'Rename file' }).click();
    const move = tab.getByRole('dialog', { name: 'Rename index.html' });
    await move.getByLabel('Path').fill('home.html');
    await move.getByRole('button', { name: 'Rename' }).click();
    await expect(
      workspace.getByText('Every artifact needs index.html'),
    ).toBeVisible();
    await expect(save).toBeDisabled();
    await checkAccessibility('editor-draft-problems', tab);
    await panel.getByRole('button', { name: 'Rename file' }).click();
    const back = tab.getByRole('dialog', { name: 'Rename home.html' });
    await back.getByLabel('Path').fill('index.html');
    await back.getByRole('button', { name: 'Rename' }).click();
    await expect(
      workspace.getByText('Every artifact needs index.html'),
    ).toBeHidden();
    await expect(index).toHaveValue('<p>two</p>\n');

    // The moved bytes are read when the draft is saved. If they cannot be
    // read, nothing is sent.
    const logoUrl = `**${source}/revisions/1/files/img/logo.png`;
    await tab.route(
      logoUrl,
      (route) =>
        route.fulfill({
          status: 503,
          json: { error: { code: 'unavailable', message: 'Busy.' } },
        }),
      { times: 1 },
    );
    await save.click();
    await expect(
      workspace.getByRole('alert').filter({ hasText: 'Not saved' }),
    ).toBeVisible();
    assert.deepEqual(
      (await api(server, 'GET', `${source}/revisions`)).revisions.map(
        ({ revision }) => revision,
      ),
      [1],
      'A failed read of moved bytes must send nothing.',
    );

    // Saved, every change is one revision; the moved file keeps its bytes.
    await save.click();
    await expect(
      workspace.getByText('Revision 2', { exact: true }),
    ).toBeVisible();
    const second = await api(server, 'GET', `${source}/revisions/2`);
    assert.deepEqual(
      second.files.map(({ path }) => path),
      ['assets/logo.png', 'index.html', 'pages/about.html'],
    );
    assert.deepEqual(
      await bytes(server, `${source}/revisions/2/files/assets/logo.png`),
      logo,
      'A moved file keeps its bytes.',
    );
    assert.deepEqual(
      await bytes(server, `${source}/revisions/2/files/index.html`),
      Buffer.from('<p>two</p>\r\n'),
      'An edit keeps CRLF line breaks.',
    );
    assert.deepEqual(
      (await api(server, 'GET', `${source}/revisions`)).revisions.map(
        ({ revision }) => revision,
      ),
      [2, 1],
      'The draft is saved as one revision.',
    );
    await expect(
      workspace.getByRole('button', { name: 'Disconnect' }),
    ).toBeEnabled();

    // A revision saved elsewhere leaves the draft as it is; saving it is
    // refused, and loading the latest revision replaces it.
    await index.fill('<p>mine</p>\n');
    await api(server, 'POST', `${source}/revisions`, {
      base_revision: 2,
      changes: [
        { op: 'put', path: 'index.html', encoding: 'utf8', content: 'agent' },
      ],
    });
    await expect(
      workspace.getByText('Revision 3 was saved elsewhere'),
    ).toBeVisible();
    await expect(index).toHaveValue('<p>mine</p>\n');
    await save.click();
    await expect(
      workspace.getByText('Not saved: the artifact changed'),
    ).toBeVisible();
    await expect(index).toHaveValue('<p>mine</p>\n');
    await workspace.getByRole('button', { name: 'Load latest' }).click();
    await expect(index).toHaveValue('agent');

    // Text moved onto a deleted file's path is edited there without
    // waiting for the file it replaces, whose read is held.
    const releaseAbout = await hold(
      tab,
      'GET',
      `**${source}/revisions/3/files/pages/about.html`,
    );
    await fileButton('pages/about.html').click();
    await panel.getByRole('button', { name: 'Delete file' }).click();
    await files.getByRole('button', { name: 'New file' }).click();
    const fresh = tab.getByRole('dialog', { name: 'New file' });
    await fresh.getByLabel('Path').fill('draft.html');
    await fresh.getByRole('button', { name: 'Add' }).click();
    await workspace.getByLabel('draft.html').fill('<p>moved</p>');
    await panel.getByRole('button', { name: 'Rename file' }).click();
    const onto = tab.getByRole('dialog', { name: 'Rename draft.html' });
    await onto.getByLabel('Path').fill('pages/about.html');
    await onto.getByRole('button', { name: 'Rename' }).click();
    const about = workspace.getByLabel('pages/about.html');
    await expect(about).toHaveValue('<p>moved</p>');
    await about.fill('<p>moved and edited</p>');
    await expect(about).toHaveValue('<p>moved and edited</p>');
    await releaseAbout();
    await workspace.getByRole('button', { name: 'Discard changes' }).click();
    await expect(fileButton('draft.html')).toBeHidden();

    // The saved revision can be previewed beside the editor.
    await workspace.getByRole('button', { name: 'Show saved preview' }).click();
    await expect(
      workspace.locator('iframe[title="Saved revision 3 of Editor demo"]'),
    ).toBeVisible();
    await checkAccessibility('editor-saved-preview', tab);
    await workspace.getByRole('button', { name: 'Hide saved preview' }).click();

    // A possible replacement of the server's data while moved bytes are
    // read: revision 3 may now name other content, so nothing is sent.
    await fileButton('assets/logo.png').click();
    await panel.getByRole('button', { name: 'Rename file' }).click();
    const late = tab.getByRole('dialog', { name: 'Rename assets/logo.png' });
    await late.getByLabel('Path').fill('logo.png');
    await late.getByRole('button', { name: 'Rename' }).click();
    const posts = [];
    const recordPost = (request) => {
      if (request.method() === 'POST' && request.url().endsWith('/revisions'))
        posts.push(request.url());
    };
    tab.on('request', recordPost);
    const releaseRead = await hold(
      tab,
      'GET',
      `**${source}/revisions/3/files/assets/logo.png`,
    );
    await save.click();
    await expect(
      workspace.getByRole('button', { name: 'Saving…' }),
    ).toBeVisible();
    await forgeReset(tab);
    await expect(
      workspace.getByText("The server's data may have been replaced"),
    ).toBeVisible();
    await releaseRead();
    await expect(save).toBeDisabled();
    await expect(
      workspace.getByRole('alert').filter({ hasText: 'Not saved' }),
    ).toBeHidden();
    tab.off('request', recordPost);
    assert.deepEqual(
      posts,
      [],
      'Nothing is sent after a possible replacement.',
    );
    assert.deepEqual(
      (await api(server, 'GET', `${source}/revisions`)).revisions.map(
        ({ revision }) => revision,
      ),
      [3, 2, 1],
    );

    assert.deepEqual(errors, [], 'Page errors.');
    return { revisions: 3, moved: 'bytes kept', refused: 3 };
  } finally {
    await context.close();
  }
}
