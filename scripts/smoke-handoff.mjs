import assert from 'node:assert/strict';
import {
  Client,
  StreamableHTTPClientTransport,
} from '@modelcontextprotocol/client';
import { expect } from '@playwright/test';
import { api, loseResponse } from './smoke-workspace.mjs';

/** Exercises consent and actual MCP inbox delivery, without running a model. */
export async function checkHandoff({
  browser,
  origin,
  server,
  checkAccessibility,
}) {
  const source = '<p>left 😀 right</p>\r\n';
  await api(server, 'POST', '/v1/artifacts', {
    title: 'Handoff smoke',
    files: [{ path: 'index.html', encoding: 'utf8', content: source }],
  });
  const agent = new Client({ name: 'handoff-smoke', version: '1' });
  const transport = new StreamableHTTPClientTransport(
    new URL(`${server.api_url}/mcp`),
    {
      requestInit: { headers: { authorization: `Bearer ${server.token}` } },
    },
  );
  await agent.connect(transport);
  const call = async (name, args) => {
    const result = await agent.callTool({ name, arguments: args });
    assert.equal(result.isError, false, `${name}: ${JSON.stringify(result)}`);
    return result.structuredContent;
  };
  const inbox = await call('register_handoff_session', {
    name: 'Smoke editor',
  });
  const sameName = await call('register_handoff_session', {
    name: 'Smoke editor',
  });
  const poll = () => call('poll_handoffs', inbox);
  const context = await browser.newContext({
    viewport: { width: 1440, height: 1000 },
    reducedMotion: 'reduce',
  });
  try {
    const page = await context.newPage();
    page.setDefaultTimeout(10_000);
    const errors = [];
    const stages = [];
    const sends = [];
    page.on('pageerror', (error) => errors.push(error.message));
    page.on('request', (request) => {
      if (
        request.method() === 'POST' &&
        new URL(request.url()).pathname === '/v1/handoffs'
      )
        stages.push(request.url());
      if (
        request.method() === 'POST' &&
        new URL(request.url()).pathname.endsWith('/send')
      )
        sends.push(new URL(request.url()).pathname);
    });
    await page.goto(origin);
    const workspace = page.locator('#artifacts');
    await workspace.getByLabel('Access token').fill(server.token);
    const connectionResponse = page.waitForResponse(
      (response) =>
        new URL(response.url()).pathname === '/v1/runtime-sessions' &&
        response.request().method() === 'POST',
    );
    await workspace.getByRole('button', { name: 'Connect' }).click();
    const connection = (await (await connectionResponse).json()).session_id;
    await workspace
      .getByRole('navigation', { name: 'Artifacts' })
      .getByRole('button', { name: /^Handoff smoke / })
      .click();
    await workspace.getByRole('tab', { name: 'Source' }).click();
    const editor = workspace.getByLabel('index.html', { exact: true });
    await expect(editor).toHaveValue('<p>left 😀 right</p>\n');
    const start = '<p>left '.length;
    const select = () =>
      editor.evaluate((element, offset) => {
        element.focus();
        element.setSelectionRange(offset, offset + 2);
      }, start);
    const open = workspace.getByRole('button', {
      name: 'Stage selection for agent',
    });
    await select();
    await open.evaluate((element) => element.click());
    await expect(
      page.getByRole('dialog', { name: 'Send a source selection' }),
    ).toHaveCount(0);
    await open.click();
    const dialog = page.getByRole('dialog', {
      name: 'Send a source selection',
    });
    await expect(dialog).toBeVisible();
    await expect(dialog.getByText('😀', { exact: true })).toBeVisible();
    const agentChoice = dialog.getByLabel('Agent session');
    await expect(
      agentChoice.locator(`option[value="${inbox.session_id}"]`),
    ).toContainText(inbox.session_id);
    await expect(
      agentChoice.locator(`option[value="${sameName.session_id}"]`),
    ).toContainText(sameName.session_id);
    await agentChoice.selectOption(inbox.session_id);
    await dialog.getByLabel('Edit intent').fill('Explain this selection');
    await checkAccessibility('handoff-proposal', page);
    const stage = dialog.getByRole('button', {
      name: 'Stage selection',
      exact: true,
    });
    await stage.evaluate((element) => element.click());
    assert.deepEqual((await poll()).deliveries, []);
    assert.deepEqual(
      stages,
      [],
      'A synthetic stage must not submit a proposal.',
    );
    await expect(stage).toBeVisible();
    await stage.click();
    await expect(
      dialog.getByText('Staged. Nothing has been sent to the agent.'),
    ).toBeVisible();
    await expect(
      dialog.getByText(`Agent: Smoke editor (${inbox.session_id})`),
    ).toBeVisible();
    const statusButton = dialog.getByRole('button', { name: 'Check status' });
    await expect(statusButton).toBeEnabled();
    assert.equal(
      await statusButton.evaluate(
        (element) => getComputedStyle(element).opacity,
      ),
      '1',
    );
    await checkAccessibility('handoff-staged', page);
    assert.deepEqual((await poll()).deliveries, []);
    await dialog.getByRole('button', { name: 'Cancel handoff' }).click();
    await expect(dialog).toBeHidden();
    assert.deepEqual((await poll()).deliveries, []);

    await select();
    await open.click();
    await dialog.getByLabel('Agent session').selectOption(inbox.session_id);
    await dialog.getByLabel('Edit intent').fill('Explain this selection');
    const response = page.waitForResponse(
      (item) =>
        new URL(item.url()).pathname === '/v1/handoffs' &&
        item.request().method() === 'POST',
    );
    await dialog
      .getByRole('button', { name: 'Stage selection', exact: true })
      .click();
    const staged = await (await response).json();
    await expect(
      dialog.getByText('Staged. Nothing has been sent to the agent.'),
    ).toBeVisible();
    const send = dialog.getByRole('button', { name: 'Send to agent' });
    await send.evaluate((element) => element.click());
    const beforeSend = await poll();
    assert.deepEqual(sends, [], 'A synthetic send must not submit a delivery.');
    assert.deepEqual(beforeSend.deliveries, []);
    const restore = await loseResponse(page, '**/v1/handoffs/*/send');
    await send.click();
    await expect(
      dialog.getByText('Send was not confirmed.', { exact: false }),
    ).toBeVisible();
    await expect(
      dialog.getByText('Send is unconfirmed.', { exact: false }),
    ).toBeVisible();
    await restore();
    await send.click();
    await expect(
      dialog.getByText(
        'Sent to the inbox; waiting for the agent to receive it.',
      ),
    ).toBeVisible();
    await expect(dialog.getByRole('button', { name: 'Close' })).toHaveCount(0);
    assert.deepEqual(sends, [
      `/v1/handoffs/${staged.id}/send`,
      `/v1/handoffs/${staged.id}/send`,
    ]);
    const delivery = (await poll()).deliveries;
    assert.equal(delivery.length, 1);
    assert.equal(delivery[0].id, staged.id);
    assert.equal(delivery[0].selection.text, '😀');
    assert.equal(delivery[0].intent, 'Explain this selection');
    await call('acknowledge_handoff', { ...inbox, delivery_id: staged.id });
    await expect(
      dialog.getByText(
        'The agent acknowledged receipt. Model work is not confirmed.',
      ),
    ).toBeVisible();
    await checkAccessibility('handoff-receipt', page);
    await dialog.getByRole('button', { name: 'Close' }).click();
    await expect(dialog).toBeHidden();

    // The MCP integration exercises the real unavailable source. Inject its
    // status here to check presentation while the editor remains open.
    await select();
    await open.click();
    await dialog.getByLabel('Agent session').selectOption(inbox.session_id);
    await dialog.getByLabel('Edit intent').fill('Explain a blocked selection');
    const stagedResponse = page.waitForResponse(
      (item) =>
        new URL(item.url()).pathname === '/v1/handoffs' &&
        item.request().method() === 'POST',
    );
    await dialog
      .getByRole('button', { name: 'Stage selection', exact: true })
      .click();
    const blockedStage = await (await stagedResponse).json();
    await dialog.getByRole('button', { name: 'Send to agent' }).click();
    await expect(
      dialog.getByText(
        'Sent to the inbox; waiting for the agent to receive it.',
      ),
    ).toBeVisible();
    const statusPath = (url) =>
      url.pathname === `/v1/handoffs/${blockedStage.id}`;
    const blockedStatus = async (route) => {
      if (route.request().method() !== 'GET') return route.fallback();
      const response = await route.fetch();
      const body = await response.json();
      return route.fulfill({ response, json: { ...body, state: 'blocked' } });
    };
    await page.route(statusPath, blockedStatus);
    await dialog.getByRole('button', { name: 'Check status' }).click();
    await expect(
      dialog.getByText(
        'The saved source is unavailable. The agent may have received it; retries have stopped.',
      ),
    ).toBeVisible();
    await expect(
      dialog.getByRole('button', { name: 'Cancel handoff' }),
    ).toHaveCount(0);
    await checkAccessibility('handoff-blocked', page);
    await page.unroute(statusPath, blockedStatus);
    const cancelled = await api(
      server,
      'POST',
      `/v1/handoffs/${blockedStage.id}/cancel`,
      { workspace_session_id: connection },
    );
    assert.equal(cancelled.state, 'cancelled');
    await dialog.getByRole('button', { name: 'Close' }).click();
    await expect(dialog).toBeHidden();

    // A disconnected workspace must report expiry without requiring an agent
    // to poll; otherwise a delivery that cannot arrive appears to be waiting.
    await select();
    await open.click();
    await dialog.getByLabel('Agent session').selectOption(inbox.session_id);
    await dialog.getByLabel('Edit intent').fill('Explain again');
    await dialog
      .getByRole('button', { name: 'Stage selection', exact: true })
      .click();
    await dialog.getByRole('button', { name: 'Send to agent' }).click();
    await expect(
      dialog.getByText(
        'Sent to the inbox; waiting for the agent to receive it.',
      ),
    ).toBeVisible();
    const closed = await fetch(
      `${server.api_url}/v1/runtime-sessions/${connection}`,
      {
        method: 'DELETE',
        headers: { authorization: `Bearer ${server.token}` },
      },
    );
    assert.equal(closed.status, 204);
    await expect(
      dialog.getByText(
        'The workspace connection or agent session ended before delivery.',
      ),
    ).toBeVisible();
    assert.deepEqual((await poll()).deliveries, []);
    await dialog.getByRole('button', { name: 'Close' }).click();
    await expect(dialog).toBeHidden();
    await editor.fill('<p>unsaved</p>');
    await expect(open).toHaveCount(0);
    assert.deepEqual(errors, []);
    return {
      staged: true,
      cancelled: true,
      retriedSameDelivery: true,
      receiptOnly: true,
    };
  } finally {
    await context.close();
    await agent.close();
  }
}
