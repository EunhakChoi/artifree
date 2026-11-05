import assert from 'node:assert/strict';
import { expect } from '@playwright/test';
import { setVisible } from './smoke-list.mjs';
import { api } from './smoke-workspace.mjs';

/** Follows application data only while its tab and the page are visible. */
export async function checkData({ browser, origin, server }) {
  const { artifact } = await api(server, 'POST', '/v1/artifacts', {
    title: 'Data snapshot',
    files: [{ path: 'index.html', encoding: 'utf8', content: '<p>Data</p>' }],
  });
  const path = `/v1/artifacts/${artifact.id}/data`;
  const context = await browser.newContext();
  try {
    const page = await context.newPage();
    page.setDefaultTimeout(10_000);
    let reads = 0;
    page.on('request', (request) => {
      if (
        request.method() === 'GET' &&
        new URL(request.url()).pathname === path
      )
        reads++;
    });
    await page.goto(origin);
    const workspace = page.locator('#artifacts');
    await workspace.getByLabel('Access token').fill(server.token);
    await workspace.getByRole('button', { name: 'Connect' }).click();
    await workspace.getByLabel('Search titles').fill('Data snapshot');
    await workspace.getByRole('button', { name: /^Data snapshot / }).click();
    await expect(
      workspace.getByRole('tab', { name: 'Data', exact: true }),
    ).toBeVisible();
    assert.equal(reads, 0, 'the data tab does not read before it is opened');
    await workspace.getByRole('tab', { name: 'Data', exact: true }).click();
    await expect(workspace.getByText('Data version 0')).toBeVisible();
    const panel = workspace.getByRole('tabpanel', { name: 'Data' });
    await expect(panel.locator('pre')).toHaveText('{}');
    await api(server, 'PUT', path, {
      path: '',
      base_data_version: 0,
      value: { text: '<b>safe</b>' },
    });
    await expect(workspace.getByText('Data version 1')).toBeVisible();
    await expect(panel.locator('pre')).toContainText('<b>safe</b>');
    await expect(panel.locator('pre b')).toHaveCount(0);
    const beforeIdle = reads;
    await page.waitForTimeout(20_000);
    assert.equal(reads, beforeIdle, 'an unchanged follower does not poll');
    const beforeHidden = reads;
    await setVisible(page, false);
    await expect(panel.locator('pre')).toHaveCount(0);
    await setVisible(page, true);
    await expect(workspace.getByText('Data version 1')).toBeVisible({
      timeout: 20_000,
    });
    assert.equal(
      reads,
      beforeHidden + 1,
      'showing the page reads a fresh snapshot',
    );
    await workspace.getByRole('tab', { name: 'Preview' }).click();
    let release;
    let arrived;
    const held = new Promise((resolve) => {
      release = resolve;
    });
    const started = new Promise((resolve) => {
      arrived = resolve;
    });
    await page.route(
      `**${path}`,
      async (route) => {
        arrived();
        await held;
        await route
          .fulfill({
            json: {
              artifact_id: artifact.id,
              path: '',
              data_version: 0,
              exists: true,
              value: { text: 'late stale response' },
            },
          })
          .catch(() => undefined);
      },
      { times: 1 },
    );
    await workspace.getByRole('tab', { name: 'Data', exact: true }).click();
    await started;
    const whileHeld = reads;
    await workspace.getByRole('tab', { name: 'Preview' }).click();
    await workspace.getByRole('tab', { name: 'Data', exact: true }).click();
    await page.waitForTimeout(500);
    assert.equal(reads, whileHeld, 'the old read retains the shared gate');
    // An already-sent read keeps the shared gate until its ten-second stall
    // bound. The next read can then finish before the held old answer.
    await expect(workspace.getByText('Data version 1')).toBeVisible({
      timeout: 15_000,
    });
    release();
    await page.waitForTimeout(200);
    await expect(panel.locator('pre')).toContainText('<b>safe</b>');
    await workspace.getByRole('tab', { name: 'Preview' }).click();
    await page.route(
      `**${path}`,
      (route) =>
        route.fulfill({
          status: 401,
          json: {
            error: { code: 'unauthenticated', message: 'Token rejected.' },
          },
        }),
      { times: 1 },
    );
    await workspace.getByRole('tab', { name: 'Data', exact: true }).click();
    await expect(panel.getByRole('alert')).toContainText(
      'access token was rejected',
    );
    await expect(panel.locator('pre')).toHaveCount(0);
    await workspace.getByRole('tab', { name: 'Preview' }).click();
    await page.route(
      `**${path}`,
      (route) =>
        route.fulfill({
          status: 404,
          json: { error: { code: 'not_found', message: 'Not found.' } },
        }),
      { times: 1 },
    );
    await workspace.getByRole('tab', { name: 'Data', exact: true }).click();
    await expect(panel.getByRole('alert')).toContainText('Data unavailable');
    await expect(panel.locator('pre')).toHaveCount(0);
    await workspace.getByRole('tab', { name: 'Preview' }).click();
    await workspace.getByRole('tab', { name: 'Data', exact: true }).click();
    await expect(workspace.getByText('Data version 1')).toBeVisible();
    await api(server, 'PATCH', `/v1/artifacts/${artifact.id}`, {
      base_metadata_revision: artifact.metadata_revision,
      deleted: true,
    });
    await expect(
      workspace.getByText('The open artifact was moved to the trash.'),
    ).toBeVisible();
    await expect(panel.locator('pre')).toHaveCount(0);
  } finally {
    await context.close();
  }
}
