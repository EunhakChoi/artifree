import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { once } from 'node:events';
import { build } from 'esbuild';

const OWNER = 'artifree-data-outbox-owner-v1';
const CANDIDATE = 'artifree-data-outbox-candidate-v1';
const ARTIFACT = `art_${'a'.repeat(32)}`;
const OTHER = `art_${'b'.repeat(32)}`;
const binding = {
  actor_id: 'local',
  actor_generation: '1',
  storage_epoch: '1'.repeat(32),
};

/** Real origin storage and Web Locks, with an owned HTTP metadata-only fixture. */
export async function checkOutbox(browser) {
  const bundle = await build({
    stdin: {
      contents: `import {openBrowserOutbox} from './packages/client/src/outbox/index.ts';
        import {JsonText} from './packages/client/src/client.ts';
        globalThis.outboxApi = {openBrowserOutbox, JsonText};`,
      resolveDir: process.cwd(),
      loader: 'ts',
    },
    bundle: true,
    format: 'esm',
    write: false,
    platform: 'browser',
  });
  let currentBinding = binding;
  let mode = 'normal';
  let redirectStatus = 307;
  let redirectTarget = '/collect';
  let bindingStatus = 200;
  let bindingBody;
  let requests = [];
  let collector = 0;
  const target = createServer((request, response) => {
    collector += 1;
    response.setHeader('Access-Control-Allow-Origin', '*');
    response.setHeader('Access-Control-Allow-Headers', 'Authorization');
    response.setHeader('Content-Type', 'application/json');
    response.end(JSON.stringify(currentBinding));
  });
  target.listen(0, '127.0.0.1');
  await once(target, 'listening');
  const crossOriginTarget = `http://127.0.0.1:${target.address().port}/collect`;
  const server = createServer((request, response) => {
    if (
      request.url === '/module.js' ||
      request.url === '/module-duplicate.js'
    ) {
      response.setHeader('Content-Type', 'text/javascript');
      response.end(
        request.url === '/module-duplicate.js'
          ? bundle.outputFiles[0].text.replace(
              'globalThis.outboxApi',
              'globalThis.outboxDuplicate',
            )
          : bundle.outputFiles[0].contents,
      );
      return;
    }
    if (request.url === '/collect') {
      collector += 1;
      response.end('{}');
      return;
    }
    if (request.url?.includes('/v1/')) {
      requests.push({ method: request.method, url: request.url });
    }
    if (request.url?.endsWith('/v1/data-outbox/binding')) {
      if (request.headers.authorization !== 'Bearer test-token') {
        response.statusCode = 401;
        response.end(JSON.stringify(currentBinding));
        return;
      }
      if (mode === 'headers_stall') return;
      if (mode === 'network_failure') {
        request.destroy();
        return;
      }
      if (mode === 'redirect') {
        response.writeHead(redirectStatus, { Location: redirectTarget });
        response.end();
        return;
      }
      response.setHeader('Cache-Control', 'no-store');
      response.setHeader('Content-Type', 'application/json');
      response.statusCode = bindingStatus;
      if (mode === 'body_stall') {
        response.write('{"actor_id":');
        return;
      }
      response.end(
        mode === 'oversize'
          ? JSON.stringify(currentBinding) + ' '.repeat(4097)
          : (bindingBody ?? JSON.stringify(currentBinding)),
      );
      return;
    }
    response.setHeader('Content-Type', 'text/html');
    response.end(
      '<!doctype html><script type="module" src="/module.js"></script>',
    );
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const origin = `http://127.0.0.1:${server.address().port}`;
  const context = await browser.newContext();
  const pages = [];
  async function page(selectedContext = context) {
    const opened = await selectedContext.newPage();
    pages.push(opened);
    await opened.goto(origin);
    await opened.waitForFunction(() => globalThis.outboxApi !== undefined);
    return opened;
  }
  async function open(target, baseUrl = '') {
    return target.evaluate(async (baseUrl) => {
      globalThis.box = await outboxApi.openBrowserOutbox({
        baseUrl,
        accessToken: 'test-token',
      });
      return box.state.kind;
    }, baseUrl);
  }
  async function controlBinding(target) {
    await target.evaluate(() => {
      const originalSet = window.setTimeout;
      const originalClear = window.clearTimeout;
      const originalFetch = window.fetch;
      globalThis.bindingTimers = [];
      globalThis.bindingFetches = 0;
      globalThis.bindingHeaders = false;
      window.setTimeout = (callback, milliseconds, ...args) => {
        const id = originalSet(() => undefined, 60000);
        bindingTimers.push({ callback, milliseconds, id, args });
        return id;
      };
      window.fetch = async (...args) => {
        bindingFetches += 1;
        const response = await Reflect.apply(originalFetch, window, args);
        bindingHeaders = true;
        return response;
      };
      globalThis.restoreBinding = () => {
        for (const timer of bindingTimers) originalClear(timer.id);
        window.setTimeout = originalSet;
        window.clearTimeout = originalClear;
        window.fetch = originalFetch;
      };
    });
  }
  async function enqueue(target, artifactId = ARTIFACT, base = { version: 0 }) {
    return target.evaluate(
      async ({ artifactId, base }) =>
        box.enqueue({
          artifactId,
          base,
          path: 'raw//path',
          operation: 'set',
          value: new outboxApi.JsonText('{"a":9007199254740993,"a":2}'),
        }),
      { artifactId, base },
    );
  }
  async function rows(target) {
    return target.evaluate(async () => {
      const db = await new Promise((resolve, reject) => {
        const request = indexedDB.open('artifree-data-outbox');
        request.onsuccess = () => resolve(request.result);
        request.onerror = () => reject(request.error);
      });
      try {
        return await new Promise((resolve, reject) => {
          const request = db
            .transaction('intents')
            .objectStore('intents')
            .getAll();
          request.onsuccess = () => resolve(request.result);
          request.onerror = () => reject(request.error);
        });
      } finally {
        db.close();
      }
    });
  }
  try {
    const original = await page();
    // URL rejection happens before Fetch or any owner/session writes.
    const invalid = await original.evaluate(async () => {
      const results = [];
      for (const baseUrl of [
        'http://user:secret@localhost',
        '/?',
        '/#',
        'relative',
        '//elsewhere',
        'file:///x',
      ]) {
        try {
          await outboxApi.openBrowserOutbox({ baseUrl, accessToken: 'never' });
          results.push('accepted');
        } catch (error) {
          results.push(error.code);
        }
      }
      return { results, entries: sessionStorage.length };
    });
    assert.deepEqual(invalid, {
      results: Array(6).fill('invalid_outbox_configuration'),
      entries: 0,
    });
    assert.equal(requests.length, 0);
    const authenticated = await page();
    assert.equal(await open(authenticated), 'ready');
    await authenticated.evaluate(() => box.close());
    await authenticated.close();
    mode = 'redirect';
    for (const status of [301, 302, 303, 307, 308]) {
      redirectStatus = status;
      redirectTarget = '/collect';
      await assert.rejects(open(original), /binding_failed/);
      redirectTarget = crossOriginTarget;
      await assert.rejects(open(original), /binding_failed/);
    }
    assert.equal(
      collector,
      0,
      'native Fetch must not follow metadata redirects',
    );
    mode = 'oversize';
    await assert.rejects(open(original), /binding_failed/);
    mode = 'normal';
    for (const status of [401, 403, 500]) {
      bindingStatus = status;
      const before = requests.length;
      await assert.rejects(open(original), /binding_failed/);
      assert.equal(
        requests.length,
        before + 1,
        'a failed binding read must not retry',
      );
      assert.equal(await original.evaluate(() => sessionStorage.length), 0);
    }
    bindingStatus = 200;
    for (const body of [
      '{invalid',
      JSON.stringify({ ...binding, extra: true }),
      JSON.stringify({ ...binding, actor_id: 'u-invalid' }),
      JSON.stringify({ ...binding, storage_epoch: 'A'.repeat(32) }),
      JSON.stringify({ ...binding, actor_generation: '01' }),
      JSON.stringify({ ...binding, actor_generation: '2' }),
      JSON.stringify({
        ...binding,
        actor_id: `u-${'a'.repeat(32)}`,
        actor_generation: '9223372036854775808',
      }),
    ]) {
      bindingBody = body;
      const before = requests.length;
      await assert.rejects(open(original), /binding_failed/);
      assert.equal(requests.length, before + 1);
      assert.equal(await original.evaluate(() => sessionStorage.length), 0);
    }
    bindingBody = undefined;
    for (const stalled of ['headers_stall', 'body_stall']) {
      mode = stalled;
      await controlBinding(original);
      await original.evaluate(() => {
        globalThis.bindingAttempt = outboxApi
          .openBrowserOutbox({ baseUrl: '', accessToken: 'test-token' })
          .then(
            () => 'accepted',
            (error) => error.code,
          );
      });
      if (stalled === 'body_stall')
        await original.waitForFunction(
          () => bindingHeaders && bindingTimers.length >= 2,
        );
      else
        await original.waitForFunction(
          () => bindingFetches === 1 && bindingTimers.length >= 1,
        );
      const deadline = await original.evaluate(async (stalled) => {
        const delays = bindingTimers.map((timer) => timer.milliseconds);
        const timer =
          stalled === 'body_stall' ? bindingTimers.at(-1) : bindingTimers[0];
        timer.callback(...timer.args);
        const result = await bindingAttempt;
        for (const timer of bindingTimers) timer.callback(...timer.args);
        const answer = {
          result,
          delays,
          fetches: bindingFetches,
          entries: sessionStorage.length,
        };
        restoreBinding();
        return answer;
      }, stalled);
      assert.equal(deadline.result, 'binding_failed');
      assert.deepEqual(
        deadline.delays,
        Array(deadline.delays.length).fill(45000),
      );
      assert.equal(deadline.fetches, 1);
      assert.equal(deadline.entries, 0);
    }
    mode = 'normal';
    assert.equal(await open(original), 'ready');
    assert.equal(
      await original.evaluate(async () => {
        await import('/module-duplicate.js');
        return (
          (await outboxDuplicate.openBrowserOutbox({
            baseUrl: '',
            accessToken: 'test-token',
          })) === box
        );
      }),
      true,
    );
    assert.equal(
      await original.evaluate(
        async () =>
          (await outboxApi.openBrowserOutbox({
            baseUrl: '/',
            accessToken: 'test-token',
          })) === box,
      ),
      true,
    );
    const owner = await original.evaluate(
      (key) => sessionStorage.getItem(key),
      OWNER,
    );
    const accepted = await enqueue(original);
    assert.equal(accepted.kind, 'durably_enqueued');
    assert.equal(accepted.sequence, 1);
    assert.equal(
      (await rows(original))[0].text,
      '{"a":9007199254740993,"a":2}',
    );
    assert.equal((await rows(original))[0].path, 'raw//path');
    await original.evaluate(async (artifactId) => {
      const value = { nested: { x: 1 } };
      const pending = box.enqueue({
        artifactId,
        path: '',
        base: { version: 0 },
        operation: 'set',
        value,
      });
      value.nested.x = 2;
      await pending;
    }, OTHER);
    assert.equal((await rows(original))[1].text, '{"nested":{"x":1}}');
    await assert.rejects(
      original.evaluate(
        async (artifactId) =>
          box.enqueue({
            artifactId,
            path: '',
            base: { version: 0 },
            operation: 'set',
            value: {
              get x() {
                throw new Error('getter called');
              },
            },
          }),
        OTHER,
      ),
      /getter or setter/,
    );
    // A copied identity cannot steal the live owner's lock.
    const clone = await page();
    await clone.evaluate(
      ({ key, owner }) => sessionStorage.setItem(key, owner),
      { key: OWNER, owner },
    );
    assert.equal(await open(clone), 'owner_busy');
    const cloneOwner = await clone.evaluate(
      (key) => sessionStorage.getItem(key),
      OWNER,
    );
    assert.notEqual(cloneOwner, owner);
    assert.equal(
      await clone.evaluate((key) => sessionStorage.getItem(key), CANDIDATE),
      owner,
    );
    assert.equal(
      await clone.evaluate(
        async () => (await box.retryRecoveryCandidate()).kind,
      ),
      'owner_busy',
    );
    assert.equal((await rows(original)).length, 2);
    // One retained slot: do not overwrite either reference when another owner is busy.
    const full = await page();
    await full.evaluate(
      ({ owner, active, candidate }) => {
        sessionStorage.setItem(active, owner);
        sessionStorage.setItem(candidate, 'f'.repeat(32));
      },
      { owner, active: OWNER, candidate: CANDIDATE },
    );
    await assert.rejects(open(full), /recovery_reference_full/);
    assert.equal(
      await full.evaluate((key) => sessionStorage.getItem(key), OWNER),
      owner,
    );
    assert.equal(
      await full.evaluate((key) => sessionStorage.getItem(key), CANDIDATE),
      'f'.repeat(32),
    );
    const persistFailure = await page();
    await persistFailure.evaluate(
      ({ key, owner }) => {
        sessionStorage.setItem(key, owner);
        const original = Storage.prototype.setItem;
        Storage.prototype.setItem = function (key, value) {
          if (key === 'artifree-data-outbox-candidate-v1')
            throw new DOMException('injected', 'QuotaExceededError');
          return Reflect.apply(original, this, [key, value]);
        };
      },
      { key: OWNER, owner },
    );
    await assert.rejects(open(persistFailure), /storage_failed/);
    assert.equal(
      await persistFailure.evaluate(
        (key) => sessionStorage.getItem(key),
        OWNER,
      ),
      owner,
    );
    assert.equal(
      await persistFailure.evaluate(
        (key) => sessionStorage.getItem(key),
        CANDIDATE,
      ),
      null,
    );
    await original.evaluate(() => box.close());
    assert.equal(
      await clone.evaluate(async () => {
        const state = await box.retryRecoveryCandidate();
        globalThis.choice = state.selection;
        return state.kind;
      }),
      'recovery_required',
    );
    await assert.rejects(
      clone.evaluate(() => box.confirmRecovery({})),
      /invalid_selection/,
    );
    await clone.evaluate(() => box.confirmRecovery(choice));
    await assert.rejects(
      clone.evaluate(() => box.confirmRecovery(choice)),
      /invalid_selection/,
    );
    await assert.rejects(
      clone.evaluate(() => box.clearRecoveryCandidate()),
      /owner_not_ready/,
    );
    // Disposal token kinds, consumed/stale snapshots and independent rows.
    await clone.evaluate(async (artifactId) => {
      globalThis.discard = await box.selectUnsent(artifactId, true);
    }, ARTIFACT);
    await assert.rejects(
      clone.evaluate(() => box.discardUnsent(choice)),
      /invalid_selection/,
    );
    assert.deepEqual(await clone.evaluate(() => box.discardUnsent(discard)), {
      kind: 'discarded_unsent',
    });
    assert.equal((await rows(clone)).length, 1);
    await assert.rejects(
      clone.evaluate(() => box.discardUnsent(discard)),
      /invalid_selection/,
    );
    await clone.evaluate(
      async (artifactId) =>
        box.discardUnsent(await box.selectUnsent(artifactId, true)),
      OTHER,
    );
    assert.equal(
      await clone.evaluate(
        async () => (await box.retryRecoveryCandidate()).kind,
      ),
      'recovery_empty',
    );
    await clone.evaluate(() => box.clearRecoveryCandidate());
    assert.equal(
      await clone.evaluate((key) => sessionStorage.getItem(key), CANDIDATE),
      null,
    );
    // Recovered active owner is always ambiguous, including after a clean close.
    await enqueue(clone);
    const retained = await rows(clone);
    await clone.reload();
    await clone.waitForFunction(() => globalThis.outboxApi !== undefined);
    assert.equal(await open(clone), 'recovery_required');
    const priorSelection = await clone.evaluate(() => box.state.selection);
    await clone.reload();
    await clone.waitForFunction(() => globalThis.outboxApi !== undefined);
    assert.equal(await open(clone), 'recovery_required');
    assert.equal(
      await clone.evaluate((key) => sessionStorage.getItem(key), OWNER),
      cloneOwner,
    );
    await assert.rejects(
      clone.evaluate(
        (selection) => box.confirmRecovery(selection),
        priorSelection,
      ),
      /invalid_selection/,
    );
    await assert.rejects(enqueue(clone), /owner_not_ready/);
    assert.deepEqual(await rows(clone), retained);
    await clone.evaluate(() => box.close());
    // No current binding rows does not mean an empty owner.
    currentBinding = { ...binding, actor_id: `u-${'c'.repeat(32)}` };
    assert.equal(await open(clone), 'owner_binding_quarantined');
    assert.deepEqual(await clone.evaluate(() => box.state), {
      kind: 'owner_binding_quarantined',
    });
    assert.equal(
      await clone.evaluate((key) => sessionStorage.getItem(key), OWNER),
      cloneOwner,
    );
    await assert.rejects(enqueue(clone), /owner_not_ready/);
    assert.deepEqual(await rows(clone), retained);
    await clone.evaluate(() => box.close());
    const savedForeign = await page();
    await savedForeign.evaluate(
      ({ key, owner }) => sessionStorage.setItem(key, owner),
      { key: CANDIDATE, owner: cloneOwner },
    );
    assert.equal(await open(savedForeign), 'ready');
    const independentOwner = await savedForeign.evaluate(
      (key) => sessionStorage.getItem(key),
      OWNER,
    );
    assert.deepEqual(
      await savedForeign.evaluate(() => box.retryRecoveryCandidate()),
      { kind: 'owner_binding_quarantined' },
    );
    await assert.rejects(
      savedForeign.evaluate(() => box.clearRecoveryCandidate()),
      /owner_not_ready/,
    );
    assert.equal(
      await savedForeign.evaluate((key) => sessionStorage.getItem(key), OWNER),
      independentOwner,
    );
    assert.equal(
      await savedForeign.evaluate(
        (key) => sessionStorage.getItem(key),
        CANDIDATE,
      ),
      cloneOwner,
    );
    await savedForeign.evaluate(() => box.close());
    currentBinding = binding;
    assert.equal(await open(savedForeign), 'ready');
    assert.equal(
      await savedForeign.evaluate(
        async () => (await box.retryRecoveryCandidate()).kind,
      ),
      'recovery_required',
    );
    await savedForeign.evaluate(() => box.close());
    assert.equal(await open(clone), 'recovery_required');
    await clone.evaluate(() => box.confirmRecovery(box.state.selection));
    // A newly appended row invalidates the complete-lane disposal snapshot.
    await clone.evaluate(async (artifactId) => {
      globalThis.discard = await box.selectUnsent(artifactId);
    }, ARTIFACT);
    const dependent = await enqueue(clone, ARTIFACT, {
      after: retained[0].sequence,
    });
    await assert.rejects(
      clone.evaluate(() => box.discardUnsent(discard)),
      /invalid_selection/,
    );
    await enqueue(clone, ARTIFACT, { after: dependent.sequence });
    await enqueue(clone, ARTIFACT, { version: 12 });
    await clone.evaluate(
      async (artifactId) =>
        box.discardUnsent(await box.selectUnsent(artifactId)),
      ARTIFACT,
    );
    assert.equal((await rows(clone)).length, 1);
    assert.deepEqual((await rows(clone))[0].base, { version: 12 });

    // Request success followed by transaction abort is never durable acceptance.
    const beforeAbort = await rows(clone);
    await clone.evaluate(() => {
      const original = IDBObjectStore.prototype.add;
      IDBObjectStore.prototype.add = function (...args) {
        const pending = Reflect.apply(original, this, args);
        if (this.name === 'intents') {
          IDBObjectStore.prototype.add = original;
          pending.addEventListener('success', () => this.transaction.abort(), {
            once: true,
          });
        }
        return pending;
      };
    });
    await assert.rejects(enqueue(clone, OTHER), /not_enqueued/);
    assert.deepEqual(await rows(clone), beforeAbort);
    const committed = await clone.evaluate(async (artifactId) => {
      let complete = false;
      const original = IDBDatabase.prototype.transaction;
      IDBDatabase.prototype.transaction = function (...args) {
        const tx = Reflect.apply(original, this, args);
        if (args[1] === 'readwrite')
          tx.addEventListener('complete', () => {
            complete = true;
          });
        return tx;
      };
      try {
        const accepted = await box.enqueue({
          artifactId,
          path: '',
          base: { version: 0 },
          operation: 'delete',
        });
        return { complete, sequence: accepted.sequence };
      } finally {
        IDBDatabase.prototype.transaction = original;
      }
    }, OTHER);
    assert.deepEqual(committed, {
      complete: true,
      sequence: beforeAbort[0].sequence + 1,
    });
    const beforeDiscard = await rows(clone);
    await clone.evaluate(() => {
      const original = IDBObjectStore.prototype.delete;
      IDBObjectStore.prototype.delete = function (...args) {
        const pending = Reflect.apply(original, this, args);
        if (this.name === 'intents') {
          IDBObjectStore.prototype.delete = original;
          pending.addEventListener('success', () => this.transaction.abort(), {
            once: true,
          });
        }
        return pending;
      };
    });
    await assert.rejects(
      clone.evaluate(
        async (artifactId) =>
          box.discardUnsent(await box.selectUnsent(artifactId)),
        OTHER,
      ),
      /storage_failed/,
    );
    assert.deepEqual(await rows(clone), beforeDiscard);
    assert.equal(await clone.evaluate(() => box.state.kind), 'storage_paused');
    await assert.rejects(enqueue(clone, OTHER), /owner_not_ready/);
    await clone.evaluate(
      async (artifactId) =>
        box.discardUnsent(await box.selectUnsent(artifactId)),
      OTHER,
    );

    // Learning an epoch mismatch must revoke local admission authority too.
    await clone.evaluate(async (artifactId) => {
      globalThis.mismatchedChoice = await box.selectUnsent(artifactId);
    }, ARTIFACT);
    const beforeMismatch = await rows(clone);
    currentBinding = { ...binding, storage_epoch: '2'.repeat(32) };
    await assert.rejects(
      clone.evaluate(() => box.discardUnsent(mismatchedChoice)),
      /owner_not_ready/,
    );
    assert.equal(
      await clone.evaluate(() => box.state.kind),
      'owner_binding_quarantined',
    );
    await assert.rejects(enqueue(clone), /owner_not_ready/);
    assert.deepEqual(await rows(clone), beforeMismatch);
    currentBinding = binding;

    // Reference-probe failures and foreign bindings never rotate an occupied owner.
    await clone.evaluate(() => box.close());
    const anchored = await clone.evaluate(
      (key) => sessionStorage.getItem(key),
      OWNER,
    );
    await clone.evaluate(() => {
      const original = IDBIndex.prototype.getKey;
      IDBIndex.prototype.getKey = function () {
        IDBIndex.prototype.getKey = original;
        throw new DOMException('injected failure', 'UnknownError');
      };
    });
    await assert.rejects(open(clone), /storage_failed/);
    assert.equal(
      await clone.evaluate((key) => sessionStorage.getItem(key), OWNER),
      anchored,
    );
    assert.equal(
      await open(clone, '/other-base/'),
      'owner_binding_quarantined',
    );
    assert.equal(
      await clone.evaluate((key) => sessionStorage.getItem(key), OWNER),
      anchored,
    );
    await clone.evaluate(() => box.close());
    currentBinding = { ...binding, storage_epoch: '2'.repeat(32) };
    assert.equal(await open(clone), 'owner_binding_quarantined');
    await clone.evaluate(() => box.close());
    currentBinding = binding;

    // Origin quota is serialized by IDB across competing owners, not page locks.
    const quotaContext = await browser.newContext();
    try {
      const owners = await Promise.all([
        page(quotaContext),
        page(quotaContext),
        page(quotaContext),
        page(quotaContext),
        page(quotaContext),
      ]);
      await Promise.all(owners.map((owner) => open(owner)));
      const fill = async (owner, count) =>
        owner.evaluate(
          async ({ artifactId, count }) => {
            for (let index = 0; index < count; index += 1)
              await box.enqueue({
                artifactId,
                path: '',
                base: { version: 0 },
                operation: 'set',
                value: 0,
              });
          },
          { artifactId: ARTIFACT, count },
        );
      await Promise.all([
        fill(owners[0], 64),
        fill(owners[1], 64),
        fill(owners[2], 64),
        fill(owners[3], 63),
      ]);
      await assert.rejects(enqueue(owners[0]), /capacity_exceeded/);
      const race = await Promise.allSettled([
        enqueue(owners[3]),
        enqueue(owners[4]),
      ]);
      assert.equal(
        race.filter((result) => result.status === 'fulfilled').length,
        1,
      );
      assert.equal(
        race.filter(
          (result) =>
            result.status === 'rejected' &&
            /capacity_exceeded/.test(String(result.reason)),
        ).length,
        1,
      );
      assert.equal((await rows(owners[0])).length, 256);
    } finally {
      await quotaContext.close();
    }

    const byteContext = await browser.newContext();
    try {
      const bytePage = await page(byteContext);
      assert.equal(await open(bytePage), 'ready');
      await assert.rejects(
        bytePage.evaluate(
          async (artifactId) =>
            box.enqueue({
              artifactId,
              path: 'x'.repeat(20000),
              base: { version: 0 },
              operation: 'set',
              value: new outboxApi.JsonText('x'.repeat(262144)),
            }),
          OTHER,
        ),
        /capacity_exceeded/,
      );
      assert.equal((await rows(bytePage)).length, 0);
      const byteLimits = await bytePage.evaluate(async (artifactId) => {
        const value = new outboxApi.JsonText('"' + 'x'.repeat(262142) + '"');
        for (let index = 0; index < 7; index += 1)
          await box.enqueue({
            artifactId,
            path: '',
            base: { version: 0 },
            operation: 'set',
            value,
          });
        try {
          await box.enqueue({
            artifactId,
            path: '',
            base: { version: 0 },
            operation: 'set',
            value,
          });
        } catch (error) {
          return error.code;
        }
        return 'accepted';
      }, ARTIFACT);
      assert.equal(byteLimits, 'capacity_exceeded');
      assert.equal((await rows(bytePage)).length, 7);
      // Account raw UTF-8 once, including maximum-size text rich in escapes.
      const globalBytes = await bytePage.evaluate(
        async (artifactId) => {
          const value = new outboxApi.JsonText('"'.repeat(262144));
          for (const suffix of ['b', 'c', 'd']) {
            for (let index = 0; index < 7; index += 1)
              await box.enqueue({
                artifactId: `art_${suffix.repeat(32)}`,
                path: '',
                base: { version: 0 },
                operation: 'set',
                value,
              });
          }
          for (let index = 0; index < 3; index += 1)
            await box.enqueue({
              artifactId,
              path: '',
              base: { version: 0 },
              operation: 'set',
              value,
            });
          try {
            await box.enqueue({
              artifactId,
              path: '',
              base: { version: 0 },
              operation: 'set',
              value,
            });
          } catch (error) {
            return error.code;
          }
          return 'accepted';
        },
        `art_${'e'.repeat(32)}`,
      );
      assert.equal(globalBytes, 'capacity_exceeded');
      assert.equal((await rows(bytePage)).length, 31);
      await assert.rejects(
        bytePage.evaluate(
          async (artifactId) =>
            box.enqueue({
              artifactId,
              path: '',
              base: { version: 0 },
              operation: 'set',
              value: new outboxApi.JsonText('x'.repeat(131072)),
            }),
          `art_${'f'.repeat(32)}`,
        ),
        /capacity_exceeded/,
      );
      const bytePeer = await page(byteContext);
      assert.equal(await open(bytePeer), 'ready');
      const racePayload = async (target) =>
        target.evaluate(
          async (artifactId) =>
            box.enqueue({
              artifactId,
              path: '',
              base: { version: 0 },
              operation: 'set',
              value: new outboxApi.JsonText('"' + 'x'.repeat(51198) + '"'),
            }),
          `art_${'f'.repeat(32)}`,
        );
      const byteRace = await Promise.allSettled([
        racePayload(bytePage),
        racePayload(bytePeer),
      ]);
      assert.equal(
        byteRace.filter((result) => result.status === 'fulfilled').length,
        1,
        'competing owners cannot both consume the last origin byte reservation',
      );
      assert.equal(
        byteRace.filter(
          (result) =>
            result.status === 'rejected' &&
            /capacity_exceeded/.test(String(result.reason)),
        ).length,
        1,
      );
      assert.equal((await rows(bytePage)).length, 32);
      await assert.rejects(
        bytePage.evaluate(
          async (artifactId) =>
            box.enqueue({
              artifactId,
              path: '',
              base: { version: 0 },
              operation: 'set',
              value: new outboxApi.JsonText('x'.repeat(262145)),
            }),
          OTHER,
        ),
        /not_enqueued/,
      );
      await bytePage.evaluate(() => box.close());
      assert.equal(await open(bytePage), 'recovery_required');
      assert.equal((await rows(bytePage)).length, 32);
      await bytePage.evaluate(() => box.close());
      // Corrupt envelopes remain retained and fail closed; payload syntax was accepted above.
      await bytePage.evaluate(async () => {
        const db = await new Promise((resolve) => {
          const request = indexedDB.open('artifree-data-outbox');
          request.onsuccess = () => resolve(request.result);
        });
        try {
          await new Promise((resolve, reject) => {
            const tx = db.transaction('intents', 'readwrite');
            const store = tx.objectStore('intents');
            const pending = store.get(1);
            pending.onsuccess = () =>
              store.put({ ...pending.result, version: 2 });
            tx.oncomplete = resolve;
            tx.onabort = () => reject(tx.error);
          });
        } finally {
          db.close();
        }
      });
      await assert.rejects(open(bytePage), /storage_corrupt/);
      assert.equal((await rows(bytePage)).length, 32);
      assert.equal((await rows(bytePage))[0].version, 2);
    } finally {
      await byteContext.close();
    }

    // Reserve actual future variable copies, including a full receipt's path.
    const envelopeContext = await browser.newContext();
    try {
      currentBinding = {
        ...binding,
        actor_id: `u-${'a'.repeat(32)}`,
        actor_generation: '9223372036854775807',
      };
      const envelopePage = await page(envelopeContext);
      const longBase = '/api/' + '%22%5C'.repeat(200);
      assert.equal(await open(envelopePage, longBase), 'ready');
      await envelopePage.evaluate(
        async (artifactId) =>
          box.enqueue({
            artifactId,
            path: '\\"'.repeat(128),
            base: { version: 0 },
            operation: 'update',
            patch: new outboxApi.JsonText('"' + 'x'.repeat(262142) + '"'),
          }),
        ARTIFACT,
      );
      const maximum = (await rows(envelopePage))[0];
      assert.equal(new TextEncoder().encode(maximum.text).byteLength, 262144);
      assert.ok(maximum.bytes <= 278528);
      await assert.rejects(
        envelopePage.evaluate(
          async (artifactId) =>
            box.enqueue({
              artifactId,
              path: '\\"'.repeat(1024),
              base: { version: 0 },
              operation: 'update',
              patch: new outboxApi.JsonText('"' + 'x'.repeat(262142) + '"'),
            }),
          OTHER,
        ),
        /capacity_exceeded/,
      );
      assert.equal((await rows(envelopePage)).length, 1);
      assert.deepEqual((await rows(envelopePage))[0], maximum);
      await envelopePage.evaluate(() => box.close());
      // Fresh owner/binding for a very long URI plus a much larger escaped path.
      await envelopePage.evaluate(() => sessionStorage.clear());
      assert.equal(
        await open(envelopePage, '/api/' + '%22%5C'.repeat(800)),
        'ready',
      );
      await envelopePage.evaluate(
        async (artifactId) =>
          box.enqueue({
            artifactId,
            path: '\\"'.repeat(4096),
            base: { version: 0 },
            operation: 'update',
            patch: 'x'.repeat(1024),
          }),
        OTHER,
      );
      const large = (await rows(envelopePage))[1];
      assert.ok(large.bytes <= 278528);
      // Build the proposed footprints independently, with all bounded numeric
      // fields at their maximum spelling. Do not call production accounting.
      function futureFootprint(row) {
        const { text, bytes, ...metadata } = row;
        const max = 9007199254740991;
        const generation = '9223372036854775807';
        const body = `{"base_data_version":${max},"path":${JSON.stringify(row.path)},"patch":${text}}`;
        const prepared = {
          ...metadata,
          state: 'prepared',
          version: max,
          base: { version: max },
          method: 'PATCH',
          authorizedGeneration: generation,
          bytes,
        };
        const receipt = {
          lane: row.lane,
          owner: row.owner,
          sequence: max,
          key: row.key,
          receipt: {
            artifact_id: row.artifactId,
            path: row.path,
            data_version: max,
            changed: false,
            replayed: false,
          },
        };
        const checkpoint = {
          lane: row.lane,
          owner: row.owner,
          generation,
          rows: 64,
          bytes: 2097152,
          predecessor: { sequence: max, dataVersion: max },
        };
        const size = (text) => new TextEncoder().encode(text).byteLength;
        return (
          size(body) +
          size(JSON.stringify(prepared)) +
          size(JSON.stringify(receipt)) +
          size(JSON.stringify(checkpoint))
        );
      }
      assert.ok(
        futureFootprint(maximum) <= maximum.bytes,
        'maximum payload plus future envelopes must fit the admitted reservation',
      );
      assert.ok(
        futureFootprint(large) <= large.bytes,
        'large variable path/URL copies must fit without using fixed overhead space',
      );
    } finally {
      await envelopeContext.close();
      currentBinding = binding;
    }

    const newerContext = await browser.newContext();
    try {
      const newerPage = await page(newerContext);
      await newerPage.evaluate(async () => {
        const db = await new Promise((resolve, reject) => {
          const pending = indexedDB.open('artifree-data-outbox', 2);
          pending.onupgradeneeded = () =>
            pending.result.createObjectStore('future');
          pending.onsuccess = () => resolve(pending.result);
          pending.onerror = () => reject(pending.error);
        });
        db.close();
      });
      await assert.rejects(open(newerPage), /storage_failed/);
      assert.equal(
        await newerPage.evaluate((key) => sessionStorage.getItem(key), OWNER),
        null,
      );
    } finally {
      await newerContext.close();
    }

    const generationContext = await browser.newContext();
    try {
      const generationPage = await page(generationContext);
      currentBinding = { ...binding, actor_id: `u-${'d'.repeat(32)}` };
      assert.equal(await open(generationPage), 'ready');
      await enqueue(generationPage);
      await generationPage.evaluate(() => box.close());
      currentBinding = { ...currentBinding, actor_generation: '2' };
      assert.equal(await open(generationPage), 'generation_changed');
      await assert.rejects(enqueue(generationPage), /owner_not_ready/);
      assert.equal((await rows(generationPage))[0].admittedGeneration, '1');
    } finally {
      await generationContext.close();
      currentBinding = binding;
    }

    const corruptContext = await browser.newContext();
    try {
      const corruptPage = await page(corruptContext);
      assert.equal(await open(corruptPage), 'ready');
      await corruptPage.evaluate(async (artifactId) => {
        await box.enqueue({
          artifactId,
          path: '',
          base: { version: 0 },
          operation: 'update',
          patch: 0,
        });
        await box.close();
        const db = await new Promise((resolve) => {
          const opening = indexedDB.open('artifree-data-outbox');
          opening.onsuccess = () => resolve(opening.result);
        });
        try {
          await new Promise((resolve, reject) => {
            const tx = db.transaction('intents', 'readwrite');
            const store = tx.objectStore('intents');
            const pending = store.get(1);
            pending.onsuccess = () =>
              store.put({ ...pending.result, operation: ['update'] });
            tx.oncomplete = resolve;
            tx.onabort = () => reject(tx.error);
          });
        } finally {
          db.close();
        }
      }, ARTIFACT);
      await assert.rejects(open(corruptPage), /storage_corrupt/);
      assert.deepEqual((await rows(corruptPage))[0].operation, ['update']);
      // Return to a valid envelope, then corrupt only the global sequence counter.
      await corruptPage.evaluate(async () => {
        const db = await new Promise((resolve) => {
          const pending = indexedDB.open('artifree-data-outbox');
          pending.onsuccess = () => resolve(pending.result);
        });
        try {
          await new Promise((resolve, reject) => {
            const tx = db.transaction(['intents', 'usage'], 'readwrite');
            const store = tx.objectStore('intents');
            const pending = store.get(1);
            pending.onsuccess = () =>
              store.put({ ...pending.result, operation: 'update' });
            const global = tx.objectStore('usage').get('global');
            global.onsuccess = () =>
              tx.objectStore('usage').put({ ...global.result, next: 1 });
            tx.oncomplete = resolve;
            tx.onabort = () => reject(tx.error);
          });
        } finally {
          db.close();
        }
      });
      await assert.rejects(open(corruptPage), /storage_corrupt/);
      assert.equal((await rows(corruptPage)).length, 1);
      await corruptPage.evaluate(async () => {
        const db = await new Promise((resolve) => {
          const pending = indexedDB.open('artifree-data-outbox');
          pending.onsuccess = () => resolve(pending.result);
        });
        try {
          await new Promise((resolve, reject) => {
            const tx = db.transaction(['intents', 'usage'], 'readwrite');
            const store = tx.objectStore('intents');
            const pending = store.get(1);
            pending.onsuccess = () => {
              const row = pending.result;
              delete row.admittedGeneration;
              store.put(row);
            };
            const global = tx.objectStore('usage').get('global');
            global.onsuccess = () =>
              tx.objectStore('usage').put({ ...global.result, next: 2 });
            tx.oncomplete = resolve;
            tx.onabort = () => reject(tx.error);
          });
        } finally {
          db.close();
        }
      });
      await assert.rejects(open(corruptPage), /storage_corrupt/);
      assert.equal(
        Object.hasOwn((await rows(corruptPage))[0], 'admittedGeneration'),
        false,
      );
    } finally {
      await corruptContext.close();
    }

    // Schema/index validation precedes reference rotation and owner emptiness.
    const schemaContext = await browser.newContext();
    try {
      const schemaPage = await page(schemaContext);
      await schemaPage.evaluate(async (ownerKey) => {
        const owner = 'e'.repeat(32);
        sessionStorage.setItem(ownerKey, owner);
        const db = await new Promise((resolve, reject) => {
          const opening = indexedDB.open('artifree-data-outbox', 1);
          opening.onupgradeneeded = () => {
            const db = opening.result;
            const intents = db.createObjectStore('intents', {
              keyPath: 'sequence',
            });
            intents.createIndex('owner', 'wrongOwner');
            intents.createIndex('binding', 'binding');
            intents.createIndex('laneSequence', ['lane', 'sequence']);
            intents.createIndex('key', 'key', { unique: true });
            intents.createIndex('artifact', 'artifactId');
            intents.add({ sequence: 1, owner });
            for (const name of ['receipts', 'checkpoints'])
              db.createObjectStore(name, { keyPath: 'lane' }).createIndex(
                'owner',
                'owner',
              );
            db.createObjectStore('usage', { keyPath: 'id' }).add({
              id: 'global',
              next: 2,
              rows: 1,
              bytes: 10,
            });
          };
          opening.onsuccess = () => resolve(opening.result);
          opening.onerror = () => reject(opening.error);
        });
        db.close();
      }, OWNER);
      await assert.rejects(open(schemaPage), /storage_corrupt/);
      assert.equal(
        await schemaPage.evaluate((key) => sessionStorage.getItem(key), OWNER),
        'e'.repeat(32),
      );
      assert.equal((await rows(schemaPage)).length, 1);
    } finally {
      await schemaContext.close();
    }

    // A retained receipt anchor alone must prevent the empty-owner rotation.
    const anchorContext = await browser.newContext();
    try {
      const anchorPage = await page(anchorContext);
      assert.equal(await open(anchorPage), 'ready');
      const anchorOwner = await anchorPage.evaluate(
        (key) => sessionStorage.getItem(key),
        OWNER,
      );
      await anchorPage.evaluate(
        async ({ owner, artifactId }) => {
          await box.close();
          sessionStorage.setItem('artifree-data-outbox-candidate-v1', owner);
          const db = await new Promise((resolve) => {
            const pending = indexedDB.open('artifree-data-outbox');
            pending.onsuccess = () => resolve(pending.result);
          });
          try {
            await new Promise((resolve, reject) => {
              const tx = db.transaction('receipts', 'readwrite');
              tx.objectStore('receipts').add({
                owner,
                lane: [
                  owner,
                  'http://other.invalid',
                  'local',
                  '3'.repeat(32),
                  artifactId,
                ],
              });
              tx.oncomplete = resolve;
              tx.onabort = () => reject(tx.error);
            });
          } finally {
            db.close();
          }
          IDBObjectStore.prototype.get = function () {
            throw new Error('foreign payload read');
          };
        },
        { owner: anchorOwner, artifactId: ARTIFACT },
      );
      assert.equal(await open(anchorPage), 'owner_binding_quarantined');
      await assert.rejects(
        anchorPage.evaluate(() => box.clearRecoveryCandidate()),
        /owner_not_ready/,
      );
      assert.equal(
        await anchorPage.evaluate(
          (key) => sessionStorage.getItem(key),
          CANDIDATE,
        ),
        anchorOwner,
      );
      assert.equal(
        await anchorPage.evaluate((key) => sessionStorage.getItem(key), OWNER),
        anchorOwner,
      );
      await assert.rejects(enqueue(anchorPage), /owner_not_ready/);
    } finally {
      await anchorContext.close();
    }

    // Failed revalidation revokes an already-open handle before queued actions run.
    for (const failure of [
      'unauthenticated',
      'redirect',
      'network_failure',
      'body_stall',
    ]) {
      const failureContext = await browser.newContext();
      try {
        const failurePage = await page(failureContext);
        assert.equal(await open(failurePage), 'ready');
        await enqueue(failurePage);
        const retained = await rows(failurePage);
        await failurePage.evaluate(async (artifactId) => {
          globalThis.firstDiscard = await box.selectUnsent(artifactId);
          globalThis.nextDiscard = await box.selectUnsent(artifactId);
        }, ARTIFACT);
        await controlBinding(failurePage);
        mode = failure === 'unauthenticated' ? 'normal' : failure;
        bindingStatus = failure === 'unauthenticated' ? 401 : 200;
        const collectorBefore = collector;
        await failurePage.evaluate((artifactId) => {
          const outcome = async (pending) => {
            try {
              await pending;
              return 'accepted';
            } catch (error) {
              return error.code;
            }
          };
          globalThis.localAttemptResults = Promise.all([
            outcome(box.discardUnsent(firstDiscard)),
            outcome(
              box.enqueue({
                artifactId,
                path: '',
                base: { version: 0 },
                operation: 'set',
                value: 1,
              }),
            ),
            outcome(box.selectUnsent(artifactId)),
            outcome(box.discardUnsent(nextDiscard)),
          ]);
        }, ARTIFACT);
        if (failure === 'body_stall') {
          await failurePage.waitForFunction(
            () => bindingHeaders && bindingTimers.length >= 2,
          );
          // A broken authority gate's extra read must fail promptly, not hide in a stall.
          mode = 'normal';
          bindingStatus = 401;
          await failurePage.evaluate(() => {
            const timer = bindingTimers.at(-1);
            timer.callback(...timer.args);
          });
        }
        const outcomes = await failurePage.evaluate(async () => {
          const results = await localAttemptResults;
          for (const timer of bindingTimers) timer.callback(...timer.args);
          const result = { results, state: box.state, fetches: bindingFetches };
          restoreBinding();
          return result;
        });
        assert.deepEqual(outcomes, {
          results: [
            'binding_failed',
            'owner_not_ready',
            'owner_not_ready',
            'invalid_selection',
          ],
          state: { kind: 'binding_paused' },
          fetches: 1,
        });
        assert.equal(collector, collectorBefore);
        assert.deepEqual(await rows(failurePage), retained);
      } finally {
        mode = 'normal';
        bindingStatus = 200;
        await failureContext.close();
      }
    }

    const unavailable = await page();
    await unavailable.evaluate(() =>
      Object.defineProperty(navigator, 'locks', { value: undefined }),
    );
    await assert.rejects(open(unavailable), /outbox_unavailable/);
    assert.equal(await unavailable.evaluate(() => sessionStorage.length), 0);

    const aliasContext = await browser.newContext();
    try {
      const aliasPage = await page(aliasContext);
      assert.equal(await open(aliasPage), 'ready');
      await enqueue(aliasPage);
      await aliasPage.evaluate(
        async ({ active, candidate }) => {
          await box.close();
          sessionStorage.setItem(candidate, sessionStorage.getItem(active));
        },
        { active: OWNER, candidate: CANDIDATE },
      );
      assert.equal(await open(aliasPage), 'recovery_required');
      await aliasPage.evaluate(() => box.confirmRecovery(box.state.selection));
      await aliasPage.evaluate(
        async (artifactId) =>
          box.discardUnsent(await box.selectUnsent(artifactId)),
        ARTIFACT,
      );
      await aliasPage.evaluate(() => box.clearRecoveryCandidate());
      assert.equal((await enqueue(aliasPage)).kind, 'durably_enqueued');
      assert.equal(
        await aliasPage.evaluate(
          (key) => sessionStorage.getItem(key),
          CANDIDATE,
        ),
        null,
      );
    } finally {
      await aliasContext.close();
    }

    assert.ok(requests.every((request) => request.method === 'GET'));
    console.log(
      'outbox: native metadata redirect, durable snapshot, owner lock/candidate, ambiguity, quarantine, typed atomic disposal passed',
    );
  } finally {
    await context.close();
    await new Promise((resolve) => server.close(resolve));
    await new Promise((resolve) => target.close(resolve));
  }
}
