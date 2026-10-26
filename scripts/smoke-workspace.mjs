import assert from 'node:assert/strict';
import { resolve } from 'node:path';
import { expect } from '@playwright/test';

/**
 * Browser checks for the artifact workspace against a real artifree-server:
 * connect, create, sandboxed preview, save, conflict, and pinned revisions.
 */
export async function checkWorkspace({
  page,
  origin,
  server,
  qa,
  checkAccessibility,
}) {
  const workspace = page.locator('#artifacts');
  await workspace.scrollIntoViewIfNeeded();
  await expect(
    workspace.getByRole('heading', { name: 'Connect to your server' }),
  ).toBeVisible();
  await checkAccessibility('workspace-connect');

  await workspace.getByLabel('Access token').fill('0'.repeat(64));
  await workspace.getByRole('button', { name: 'Connect' }).click();
  await expect(
    workspace.getByText('The access token was rejected.', { exact: false }),
  ).toBeVisible();
  await workspace.getByRole('button', { name: 'Disconnect' }).click();
  await workspace.getByLabel('Access token').fill(server.token);
  await workspace.getByRole('button', { name: 'Connect' }).click();
  await expect(workspace.getByText('No artifacts yet')).toBeVisible();
  await checkAccessibility('workspace-empty');

  // The probe reports what sandboxed content could reach. It must not reach
  // the embedding page, persistent storage, or the API on the page origin.
  const probe = (label) => `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Probe</title></head>
<body><h1 id="label">${label}</h1><p id="probe">running</p>
<script>
const out = [];
try { void parent.document.title; out.push('parent:open'); } catch { out.push('parent:blocked'); }
try { localStorage.setItem('k', '1'); out.push('storage:open'); } catch { out.push('storage:blocked'); }
fetch('${origin}/v1/artifacts').then(
  () => out.push('api:open'),
  () => out.push('api:blocked'),
).finally(() => { document.getElementById('probe').textContent = out.join(' '); });
</script></body></html>
`;
  await workspace.getByRole('button', { name: 'New artifact' }).click();
  const dialog = page.getByRole('dialog', { name: 'New HTML artifact' });
  await expect(dialog).toBeVisible();
  await checkAccessibility('workspace-create-dialog');
  await dialog.getByLabel('Title').fill('Smoke artifact');
  await dialog.getByLabel('index.html').fill(probe('Revision one'));
  await dialog.getByRole('button', { name: 'Create' }).click();
  await expect(dialog).toBeHidden();

  await expect(
    workspace.getByText('Revision 1', { exact: true }),
  ).toBeVisible();
  const frameElement = workspace.locator('iframe[title^="Preview of"]');
  await expect(frameElement).toHaveAttribute(
    'src',
    new RegExp(`^${server.content_origin}/p/[0-9a-f]{64}/$`),
  );
  const sandbox = (await frameElement.getAttribute('sandbox')) ?? '';
  assert.ok(
    sandbox.includes('allow-scripts') && !sandbox.includes('allow-same-origin'),
    `Preview sandbox must keep content opaque: ${sandbox}`,
  );
  const frame = page.frameLocator('iframe[title^="Preview of"]');
  await expect(frame.locator('#label')).toHaveText('Revision one');
  await expect(frame.locator('#probe')).toHaveText(
    'parent:blocked storage:blocked api:blocked',
  );
  await checkAccessibility('workspace-preview');

  await workspace.getByRole('tab', { name: 'Source' }).click();
  const editor = workspace.getByLabel('index.html');
  await expect(editor).toHaveValue(/Revision one/);
  await editor.fill(probe('Revision two'));
  await checkAccessibility('workspace-source');
  await workspace.getByRole('button', { name: 'Save revision' }).click();
  await expect(
    workspace.getByText('Revision 2', { exact: true }),
  ).toBeVisible();
  await workspace.getByRole('tab', { name: 'Preview' }).click();
  await expect(frame.locator('#label')).toHaveText('Revision two');

  // Another client commits revision 3 while this page still edits revision 2.
  const [{ id }] = await api(server, 'GET', '/v1/artifacts').then(
    (body) => body.artifacts,
  );
  await workspace.getByRole('tab', { name: 'Source' }).click();
  await editor.fill(probe('Lost edit'));
  await api(server, 'POST', `/v1/artifacts/${id}/revisions`, {
    base_revision: 2,
    changes: [
      {
        op: 'put',
        path: 'index.html',
        encoding: 'utf8',
        content: probe('Revision three'),
      },
    ],
  });
  await workspace.getByRole('button', { name: 'Save revision' }).click();
  await expect(
    workspace.getByText('Not saved: the artifact changed'),
  ).toBeVisible();
  await expect(editor).toHaveValue(/Lost edit/);
  await checkAccessibility('workspace-conflict');
  await workspace.getByRole('button', { name: 'Load latest' }).click();
  await expect(
    workspace.getByText('Revision 3', { exact: true }),
  ).toBeVisible();
  await expect(workspace.getByLabel('index.html')).toHaveValue(
    /Revision three/,
  );

  await workspace.getByRole('tab', { name: 'Preview' }).click();
  await workspace.getByLabel('Revision', { exact: true }).selectOption('1');
  await expect(frame.locator('#label')).toHaveText('Revision one');
  await page.screenshot({ path: resolve(qa, 'workspace-desktop.png') });

  const history = await api(server, 'GET', `/v1/artifacts/${id}/revisions`);
  assert.deepEqual(
    history.revisions.map((revision) => revision.revision),
    [3, 2, 1],
    'The conflicting save must not create a revision.',
  );
  return { revisions: 3, conflict: 'rejected', sandbox: 'opaque' };
}

async function api(server, method, path, body) {
  const init = {
    method,
    headers: { authorization: `Bearer ${server.token}` },
  };
  if (body !== undefined) {
    init.headers['content-type'] = 'application/json';
    init.headers['idempotency-key'] = `smoke-${crypto.randomUUID()}`;
    init.body = JSON.stringify(body);
  }
  const response = await fetch(`${server.api_url}${path}`, init);
  const text = await response.text();
  assert.ok(
    response.ok,
    `${method} ${path} failed: ${response.status} ${text}`,
  );
  return JSON.parse(text);
}
