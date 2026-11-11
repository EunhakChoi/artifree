import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import {
  ServerClosureUnconfirmedError,
  startServer,
} from './server-process.mjs';

/** Exercises real process restart and backup/restore; no database rows are injected. */
export async function checkDataRecovery(
  previousBinary,
  binary,
  { upgrade = false, startProcess = startServer } = {},
) {
  const scratch = await mkdtemp(join(tmpdir(), 'artifree-data-recovery-'));
  const data = join(scratch, 'data');
  let server;
  let artifact;
  const errors = [];
  async function request(
    path,
    { method = 'GET', body, key, binding, status = 200 } = {},
  ) {
    const response = await fetch(server.api_url + path, {
      method,
      headers: {
        Authorization: `Bearer ${server.token}`,
        ...(body === undefined ? {} : { 'Content-Type': 'application/json' }),
        ...(key === undefined ? {} : { 'Idempotency-Key': key }),
        ...(binding === undefined
          ? {}
          : {
              'artifree-outbox-actor': binding.actor_id,
              'artifree-outbox-epoch': binding.storage_epoch,
              'artifree-outbox-actor-generation': binding.actor_generation,
            }),
      },
      ...(body === undefined ? {} : { body }),
      signal: AbortSignal.timeout(15_000),
    });
    const bytes = Buffer.from(await response.arrayBuffer());
    const text = bytes.toString('utf8');
    assert.equal(response.status, status, `${method} ${path}: ${text}`);
    return { bytes, text, json: JSON.parse(text) };
  }
  const root = () => request(`/v1/artifacts/${artifact}/data`);
  const at = (path, status = 200) =>
    request(`/v1/artifacts/${artifact}/data?${new URLSearchParams({ path })}`, {
      status,
    });
  const write = (key, body, options = {}) =>
    request(`/v1/artifacts/${artifact}/data`, {
      method: 'PUT',
      key,
      body,
      ...options,
    });
  const binding = async () => (await request('/v1/data-outbox/binding')).json;
  const members = (query) =>
    request(`/v1/artifacts/${artifact}/data/members?${query}`);
  function command(args, event) {
    const result = spawnSync(binary, args, {
      encoding: 'utf8',
      timeout: 30_000,
    });
    if (result.error) throw result.error;
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stderr, '');
    const lines = result.stdout.trim().split('\n');
    assert.equal(lines.length, 1);
    const report = JSON.parse(lines[0]);
    assert.equal(report.event, event);
    assert.equal(report.schema_version, 13);
  }
  // Read one complete SSE event, then close the stream before stopping its server.
  async function position(cursor) {
    const response = await fetch(server.api_url + '/v1/changes', {
      headers: {
        Authorization: `Bearer ${server.token}`,
        ...(cursor === undefined ? {} : { 'Last-Event-ID': cursor }),
      },
      signal: AbortSignal.timeout(15_000),
    });
    assert.equal(response.status, 200);
    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let text = '';
    try {
      while (!text.includes('\n\n')) {
        const chunk = await reader.read();
        assert.equal(
          chunk.done,
          false,
          'change stream ended before its first event',
        );
        text += decoder.decode(chunk.value, { stream: true });
      }
      const event = text.slice(0, text.indexOf('\n\n'));
      return {
        kind: event.match(/^event: ?(.+)$/m)?.[1],
        id: event.match(/^id: ?(.+)$/m)?.[1],
        data: JSON.parse(event.match(/^data: ?(.+)$/m)?.[1]),
      };
    } finally {
      await reader.cancel();
    }
  }
  try {
    server = await startProcess(previousBinary, data);
    artifact = (
      await request('/v1/artifacts', {
        method: 'POST',
        key: 'recovery-create',
        status: 201,
        body: JSON.stringify({
          title: 'Data recovery example',
          files: [
            {
              path: 'index.html',
              encoding: 'utf8',
              content: '<p>Recovery example</p>',
            },
          ],
        }),
      })
    ).json.artifact.id;
    const original = {
      a: {},
      z: 0,
      é: 1,
      'e\u0301': 2,
      한글: { 값: 3 },
      '𝄞': 4,
      'bad/name': 5,
      ['長'.repeat(22)]: 6,
    };
    await write(
      'recovery-root',
      JSON.stringify({ path: '', base_data_version: 0, value: original }),
    );
    const oldBinding = await binding();
    const asciiBody = '{"path":"a/detail","base_data_version":1,"value":7}';
    const asciiReceipt = (
      await write('recovery-ascii', asciiBody, { binding: oldBinding })
    ).json;
    assert.equal(asciiReceipt.data_version, 2);
    await write(
      'recovery-head',
      '{"path":"z","base_data_version":2,"value":8}',
    );
    const before = await root();
    assert.deepEqual(before.json.value, {
      ...original,
      a: { detail: 7 },
      z: 8,
    });
    const oldPosition = await position();
    assert.equal(oldPosition.kind, 'ready');
    const oldPage = (await members('limit=1&at_data_version=3')).json;
    assert.deepEqual(oldPage.names, ['a']);
    assert.equal(typeof oldPage.next_page_token, 'string');
    if (upgrade) {
      assert.equal(
        (await at('한글/값', 422)).json.error.code,
        'invalid_request',
      );
      assert.deepEqual((await members('limit=100')).json.names, ['a', 'z']);
    }
    await server.stop();
    server = await startProcess(binary, data);
    assert.deepEqual(
      await binding(),
      oldBinding,
      'ordinary upgrade preserves storage identity',
    );
    assert.deepEqual(
      (await root()).bytes,
      before.bytes,
      'upgrade preserves canonical UTF-8 response bytes',
    );
    assert.deepEqual(
      await position(),
      oldPosition,
      'upgrade does not create a feed change',
    );
    assert.equal((await at('é')).json.value, 1);
    assert.equal((await at('e\u0301')).json.value, 2);
    assert.equal((await at('한글/값')).json.value, 3);
    assert.equal((await at('𝄞')).json.value, 4);
    const page = (await members('limit=1&at_data_version=3')).json;
    assert.equal(
      page.next_page_token,
      oldPage.next_page_token,
      'ASCII token bytes stay unchanged',
    );
    const continuation = (
      await members(
        new URLSearchParams({
          limit: '100',
          at_data_version: '3',
          page_token: oldPage.next_page_token,
        }),
      )
    ).json;
    assert.deepEqual(continuation.names, ['e\u0301', 'z', 'é', '한글', '𝄞']);
    const replay = (
      await write('recovery-ascii', asciiBody, { binding: oldBinding })
    ).json;
    assert.deepEqual(replay, { ...asciiReceipt, replayed: true });
    const reused = await write(
      'recovery-ascii',
      asciiBody.replace('a/detail', 'é'),
      { status: 422 },
    );
    assert.equal(reused.json.error.code, 'request_key_reused');
    const unicodeBody = '{"path":"e\u0301","base_data_version":3,"value":9}';
    const unicodeReceipt = (await write('recovery-unicode', unicodeBody)).json;
    assert.equal(unicodeReceipt.data_version, 4);
    assert.equal((await at('é')).json.value, 1);
    assert.equal((await at('e\u0301')).json.value, 9);
    const savedRoot = await root();
    const savedPosition = await position();
    await server.kill();
    server = await startProcess(binary, data);
    assert.deepEqual(
      (await root()).bytes,
      savedRoot.bytes,
      'committed UTF-8 bytes survive process kill',
    );
    assert.deepEqual(await binding(), oldBinding);
    assert.deepEqual(await position(), savedPosition);
    assert.deepEqual((await write('recovery-unicode', unicodeBody)).json, {
      ...unicodeReceipt,
      replayed: true,
    });
    const backup = join(scratch, 'backup.sqlite3');
    command(['backup', '--data-dir', data, '--output', backup], 'backup');
    await write(
      'recovery-after-backup',
      '{"path":"z","base_data_version":4,"value":10}',
    );
    assert.equal((await root()).json.data_version, 5);
    await server.stop();
    const restored = join(scratch, 'restored');
    command(['restore', '--input', backup, '--data-dir', restored], 'restore');
    server = await startProcess(binary, restored);
    assert.deepEqual(
      (await root()).bytes,
      savedRoot.bytes,
      'restore preserves snapshot bytes and version',
    );
    const restoredBinding = await binding();
    assert.equal(restoredBinding.actor_id, oldBinding.actor_id);
    assert.equal(restoredBinding.actor_generation, oldBinding.actor_generation);
    assert.notEqual(restoredBinding.storage_epoch, oldBinding.storage_epoch);
    const reset = await position(savedPosition.id);
    assert.equal(reset.kind, 'reset');
    assert.notEqual(reset.id, savedPosition.id);
    assert.equal(reset.data.cursor, reset.id);
    assert.equal(
      reset.id.split('.')[1],
      savedPosition.id.split('.')[1],
      'restore keeps the feed sequence head',
    );
    const fenced = await write('recovery-ascii', asciiBody, {
      binding: oldBinding,
      status: 409,
    });
    assert.equal(fenced.json.error.code, 'outbox_binding_changed');
    assert.deepEqual(
      (await write('recovery-ascii', asciiBody, { binding: restoredBinding }))
        .json,
      { ...asciiReceipt, replayed: true },
    );
    assert.deepEqual((await write('recovery-unicode', unicodeBody)).json, {
      ...unicodeReceipt,
      replayed: true,
    });
    const stale = await write(
      'recovery-stale',
      '{"path":"z","base_data_version":5,"value":11}',
      { status: 409 },
    );
    assert.equal(stale.json.error.code, 'data_conflict');
    assert.equal(stale.json.error.current_data_version, 4);
    assert.deepEqual(
      (await root()).bytes,
      savedRoot.bytes,
      'replay and refusals never write',
    );
    assert.deepEqual(await position(), { ...reset, kind: 'ready' });
  } catch (error) {
    errors.push(error);
  } finally {
    try {
      await server?.stop();
    } catch (error) {
      errors.push(error);
    }
    const unclosed = errors.flatMap((error) => unclosedProcesses(error));
    if (unclosed.length > 0) {
      const pids = unclosed.map((error) => error.pid ?? 'unknown').join(', ');
      errors.push(
        new Error(
          `Retained data-recovery directory ${scratch}: child closure is unconfirmed (PID ${pids}). Confirm those owned processes have exited before removing this directory.`,
        ),
      );
    } else {
      try {
        await rm(scratch, { recursive: true, force: true });
      } catch (error) {
        errors.push(
          new Error(`Cannot remove data-recovery directory ${scratch}.`, {
            cause: error,
          }),
        );
      }
    }
  }
  if (errors.length === 1) throw errors[0];
  if (errors.length > 1)
    throw new AggregateError(
      errors,
      'Data recovery verification and cleanup failed.',
    );
}

function unclosedProcesses(error) {
  if (error instanceof ServerClosureUnconfirmedError)
    return error.isClosureConfirmed() ? [] : [error];
  if (error instanceof AggregateError)
    return error.errors.flatMap((nested) => unclosedProcesses(nested));
  return [];
}
