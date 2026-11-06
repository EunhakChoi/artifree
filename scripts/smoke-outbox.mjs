import { checkOutboxStore } from './smoke-outbox-store.mjs';
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
export async function checkOutbox(browser, live) {
  await checkOutboxStore(browser);
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
  if (live !== undefined)
    await checkLiveBinding(browser, live, bundle.outputFiles[0].text);
  let currentBinding = binding;
  let mode = 'normal';
  let redirectStatus = 307;
  let redirectTarget = '/collect';
  let bindingStatus = 200;
  let bindingBody;
  const heldBindings = [];
  let onBindingHeld;
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
      if (mode === 'binding_hold') {
        heldBindings.push(response);
        onBindingHeld?.();
        onBindingHeld = undefined;
        return;
      }
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
      response.setHeader('Retry-After', '999999');
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
        bindingTimers.push({
          callback: (...invocation) => {
            const timer = bindingTimers.find((entry) => entry.id === id);
            timer.active = false;
            callback(...invocation);
          },
          milliseconds,
          id,
          args,
          active: true,
        });
        return id;
      };
      window.clearTimeout = (id) => {
        const timer = bindingTimers.find((entry) => entry.id === id);
        if (timer) timer.active = false;
        originalClear(id);
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
  async function bindingFailure(target) {
    const before = requests.length;
    const result = await target.evaluate(async () => {
      try {
        await outboxApi.openBrowserOutbox({
          baseUrl: '',
          accessToken: 'test-token',
        });
        return 'accepted';
      } catch (error) {
        if (
          error.message !== error.code ||
          error.body !== undefined ||
          error.cause !== undefined
        )
          throw new Error('binding diagnostic leaked response details');
        return { name: error.name, code: error.code, status: error.status };
      }
    });
    if (mode !== 'network_failure')
      assert.equal(
        requests.length,
        before + 1,
        'binding read must dispatch once without retry',
      );
    return result;
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
      assert.deepEqual(await bindingFailure(original), {
        name: 'ArtifreeApiError',
        code: 'network_error',
        status: 0,
      });
      redirectTarget = crossOriginTarget;
      assert.deepEqual(await bindingFailure(original), {
        name: 'ArtifreeApiError',
        code: 'network_error',
        status: 0,
      });
    }
    assert.equal(
      collector,
      0,
      'native Fetch must not follow metadata redirects',
    );
    mode = 'network_failure';
    assert.deepEqual(await bindingFailure(original), {
      name: 'ArtifreeApiError',
      code: 'network_error',
      status: 0,
    });
    mode = 'oversize';
    await assert.rejects(open(original), /invalid_response/);
    mode = 'normal';
    for (const status of [401, 403, 404, 429, 500, 503]) {
      bindingStatus = status;
      const before = requests.length;
      assert.deepEqual(await bindingFailure(original), {
        name: 'ArtifreeApiError',
        code: status === 429 ? 'rate_limited' : 'invalid_response',
        status,
      });
      assert.equal(
        requests.length,
        before + 1,
        'a failed binding read must not retry',
      );
      assert.equal(await original.evaluate(() => sessionStorage.length), 0);
    }
    for (const [status, code] of [
      [401, 'unauthenticated'],
      [403, 'forbidden'],
      [404, 'not_found'],
      [429, 'rate_limited'],
      [500, 'internal'],
      [503, 'unavailable'],
    ]) {
      bindingStatus = status;
      bindingBody = JSON.stringify({
        error: { code, message: 'PRIVATE server diagnostic' },
      });
      assert.deepEqual(await bindingFailure(original), {
        name: 'ArtifreeApiError',
        code,
        status,
      });
      bindingBody = '{invalid';
      assert.deepEqual(await bindingFailure(original), {
        name: 'ArtifreeApiError',
        code: status === 429 ? 'rate_limited' : 'invalid_response',
        status,
      });
      mode = 'oversize';
      assert.deepEqual(await bindingFailure(original), {
        name: 'ArtifreeApiError',
        code: status === 429 ? 'rate_limited' : 'invalid_response',
        status,
      });
      mode = 'body_stall';
      await controlBinding(original);
      await original.evaluate(() => {
        globalThis.bindingAttempt = outboxApi
          .openBrowserOutbox({ baseUrl: '', accessToken: 'test-token' })
          .then(
            () => 'accepted',
            (error) => ({
              name: error.name,
              code: error.code,
              status: error.status,
              retryAfterSeconds: error.retryAfterSeconds,
            }),
          );
      });
      await original.waitForFunction(
        () => bindingHeaders && bindingTimers.length >= 3,
      );
      assert.deepEqual(
        await original.evaluate(() =>
          bindingTimers.slice(0, 2).map((timer) => timer.milliseconds),
        ),
        [15000, 60000],
      );
      const retainedStatus = await original.evaluate(async () => {
        const timer = bindingTimers.find(
          (entry) => entry.active && entry.milliseconds === 60000,
        );
        timer.callback(...timer.args);
        const result = await bindingAttempt;
        const fetches = bindingFetches;
        restoreBinding();
        return { result, fetches };
      });
      assert.deepEqual(retainedStatus, {
        result: {
          name: 'ArtifreeApiError',
          code: status === 429 ? 'rate_limited' : 'network_error',
          status,
          retryAfterSeconds: 3600,
        },
        fetches: 1,
      });
      assert.equal(await original.evaluate(() => sessionStorage.length), 0);
      mode = 'normal';
    }
    bindingBody = undefined;
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
      assert.deepEqual(await bindingFailure(original), {
        name: 'ArtifreeApiError',
        code: 'invalid_response',
        status: 200,
      });
      assert.equal(requests.length, before + 1);
      assert.equal(await original.evaluate(() => sessionStorage.length), 0);
    }
    bindingBody = undefined;
    for (const stalled of ['headers_stall', 'whole_body', 'body_stall']) {
      mode = stalled === 'whole_body' ? 'body_stall' : stalled;
      await controlBinding(original);
      await original.evaluate(() => {
        globalThis.bindingAttempt = outboxApi
          .openBrowserOutbox({ baseUrl: '', accessToken: 'test-token' })
          .then(
            () => 'accepted',
            (error) => ({ code: error.code, status: error.status }),
          );
      });
      if (stalled !== 'headers_stall')
        await original.waitForFunction(
          () => bindingHeaders && bindingTimers.length >= 2,
        );
      else
        await original.waitForFunction(
          () => bindingFetches === 1 && bindingTimers.length >= 1,
        );
      assert.deepEqual(
        await original.evaluate(() =>
          bindingTimers.slice(0, 2).map((timer) => timer.milliseconds),
        ),
        [15000, 60000],
      );
      const deadline = await original.evaluate(async (stalled) => {
        const delays = bindingTimers.map((timer) => timer.milliseconds);
        const timer = bindingTimers.findLast(
          (entry) =>
            entry.active &&
            entry.milliseconds ===
              (stalled === 'headers_stall'
                ? 15000
                : stalled === 'whole_body'
                  ? 60000
                  : 45000),
        );
        if (stalled !== 'headers_stall' && bindingTimers[0].active)
          throw new Error('header deadline was not cancelled');
        timer.callback(...timer.args);
        const result = await bindingAttempt;
        for (const timer of bindingTimers) timer.callback(...timer.args);
        const answer = {
          result,
          delays,
          fetches: bindingFetches,
          entries: sessionStorage.length,
          activeTimers: bindingTimers.filter((timer) => timer.active).length,
        };
        restoreBinding();
        return answer;
      }, stalled);
      assert.deepEqual(deadline.result, {
        code: 'network_error',
        status: stalled === 'headers_stall' ? 0 : 200,
      });
      assert.deepEqual(deadline.delays.slice(0, 2), [15000, 60000]);
      assert.ok(deadline.delays.slice(2).every((delay) => delay === 45000));
      assert.equal(deadline.fetches, 1);
      assert.equal(deadline.entries, 0);
      assert.equal(deadline.activeTimers, 0);
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
    assert.deepEqual(
      await clone.evaluate(() =>
        box.listUnsentTargets(false, { mode: 'offline' }),
      ),
      [],
    );
    await assert.rejects(
      clone.evaluate(
        (artifactId) =>
          box.selectUnsent(artifactId, false, {
            mode: 'offline',
            storageEpoch: '1'.repeat(32),
          }),
        ARTIFACT,
      ),
      /invalid_selection/,
    );
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
    assert.deepEqual(
      await clone.evaluate(() =>
        box.listUnsentTargets(false, { mode: 'offline' }),
      ),
      [],
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
      await owners[0].evaluate(() => box.close());
      await owners[0].evaluate(() => {
        globalThis.recoveryReadLimits = [];
        const objectGet = IDBObjectStore.prototype.getAll;
        const indexGet = IDBIndex.prototype.getAll;
        IDBObjectStore.prototype.getAll = function (...args) {
          recoveryReadLimits.push(args[1]);
          return Reflect.apply(objectGet, this, args);
        };
        IDBIndex.prototype.getAll = function (...args) {
          recoveryReadLimits.push(args[1]);
          return Reflect.apply(indexGet, this, args);
        };
        globalThis.restoreRecoveryReads = () => {
          IDBObjectStore.prototype.getAll = objectGet;
          IDBIndex.prototype.getAll = indexGet;
        };
      });
      assert.equal(await open(owners[0]), 'recovery_required');
      const readLimits = await owners[0].evaluate(() => {
        restoreRecoveryReads();
        return recoveryReadLimits;
      });
      assert.ok(readLimits.length > 0);
      assert.ok(
        readLimits.every(
          (count) => Number.isInteger(count) && count > 0 && count <= 32,
        ),
        'recovery must bound every materialized IDB page to32 rows',
      );
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
          const pending = indexedDB.open('artifree-data-outbox', 3);
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
      'unauthenticated_stall',
      'forbidden',
      'missing',
      'server_error',
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
          globalThis.offlineDiscard = await box.selectUnsent(
            artifactId,
            false,
            { mode: 'offline' },
          );
        }, ARTIFACT);
        await controlBinding(failurePage);
        const unauthorized = failure.startsWith('unauthenticated');
        mode = [
          'unauthenticated',
          'forbidden',
          'missing',
          'server_error',
        ].includes(failure)
          ? 'normal'
          : failure === 'unauthenticated_stall'
            ? 'body_stall'
            : failure;
        bindingStatus =
          {
            unauthenticated: 401,
            unauthenticated_stall: 401,
            forbidden: 403,
            missing: 404,
            server_error: 500,
          }[failure] ?? 200;
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
        if (failure === 'body_stall' || failure === 'unauthenticated_stall') {
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
            [
              'unauthenticated',
              'forbidden',
              'missing',
              'server_error',
            ].includes(failure)
              ? 'invalid_response'
              : 'network_error',
            'owner_not_ready',
            'owner_not_ready',
            unauthorized ? 'owner_not_ready' : 'invalid_selection',
          ],
          state: { kind: 'binding_paused' },
          fetches: 1,
        });
        assert.equal(collector, collectorBefore);
        assert.deepEqual(await rows(failurePage), retained);
        await assert.rejects(enqueue(failurePage), /owner_not_ready/);
        await assert.rejects(
          failurePage.evaluate(
            (artifactId) =>
              box.selectUnsent(artifactId, false, { mode: 'offline' }),
            ARTIFACT,
          ),
          /owner_not_ready/,
        );
        await assert.rejects(
          failurePage.evaluate(() =>
            box.listUnsentTargets(false, { mode: 'offline' }),
          ),
          /owner_not_ready/,
        );
        await assert.rejects(
          failurePage.evaluate(() => box.discardUnsent(offlineDiscard)),
          unauthorized ? /owner_not_ready/ : /invalid_selection/,
        );
        mode = 'normal';
        bindingStatus = 200;
        await assert.rejects(
          failurePage.evaluate(() => box.discardUnsent(nextDiscard)),
          unauthorized ? /owner_not_ready/ : /invalid_selection/,
        );
        if (unauthorized) {
          const before = requests.length;
          await assert.rejects(
            failurePage.evaluate(
              (artifactId) => box.selectUnsent(artifactId),
              ARTIFACT,
            ),
            /owner_not_ready/,
          );
          assert.equal(
            requests.length,
            before,
            '401 terminal handle must not reauthenticate',
          );
        }
        const pausedOwner = await failurePage.evaluate(
          (key) => sessionStorage.getItem(key),
          OWNER,
        );
        const overlapping = await page(failureContext);
        await overlapping.evaluate(
          ({ key, owner }) => sessionStorage.setItem(key, owner),
          { key: OWNER, owner: pausedOwner },
        );
        assert.equal(
          await open(overlapping),
          'owner_busy',
          'disconnect/auth pause must retain the live coordinator lock',
        );
        assert.equal(
          await overlapping.evaluate(
            async () => (await box.retryRecoveryCandidate()).kind,
          ),
          'owner_busy',
        );
        await failurePage.evaluate(() => box.close());
        assert.equal(
          await overlapping.evaluate(
            async () => (await box.retryRecoveryCandidate()).kind,
          ),
          'recovery_required',
          'explicit coordinator disposal releases the lock without automatic adoption',
        );
        assert.deepEqual(await rows(overlapping), retained);
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
    for (const feature of ['indexedDB', 'sessionStorage']) {
      const blocked = await page();
      const refused = await blocked.evaluate(async (feature) => {
        const session = window.sessionStorage;
        let fetches = 0;
        const original = window.fetch;
        window.fetch = (...args) => {
          fetches += 1;
          return Reflect.apply(original, window, args);
        };
        if (feature === 'indexedDB')
          Object.defineProperty(window, 'indexedDB', { value: undefined });
        else
          Object.defineProperty(window, 'sessionStorage', {
            get() {
              throw new DOMException('injected', 'SecurityError');
            },
          });
        let result;
        try {
          await outboxApi.openBrowserOutbox({
            baseUrl: '',
            accessToken: 'test-token',
          });
          result = 'accepted';
        } catch (error) {
          result = error.code;
        }
        return { result, fetches, references: session.length };
      }, feature);
      assert.deepEqual(refused, {
        result: 'outbox_unavailable',
        fetches: 0,
        references: 0,
      });
    }

    // Offline and original-epoch choices are deliberate local cleanup only.
    const localChoices = {};
    for (const choice of ['offline', 'pastEpoch', 'offlinePastEpoch']) {
      const choiceContext = await browser.newContext();
      try {
        const choicePage = await page(choiceContext);
        assert.equal(await open(choicePage), 'ready');
        await enqueue(choicePage);
        await choicePage.evaluate(() => box.close());
        if (choice !== 'offline')
          currentBinding = { ...binding, storage_epoch: '2'.repeat(32) };
        assert.equal(
          await open(choicePage),
          choice === 'offline'
            ? 'recovery_required'
            : 'owner_binding_quarantined',
        );
        await choicePage.evaluate(() => {
          const original = window.fetch;
          globalThis.cleanupFetches = 0;
          window.fetch = (...args) => {
            cleanupFetches += 1;
            return Reflect.apply(original, window, args);
          };
        });
        const offline = choice !== 'pastEpoch';
        if (offline) await choiceContext.setOffline(true);
        const cleanup = await choicePage.evaluate(
          async ({ artifactId, mode, storageEpoch }) => {
            try {
              const targets = await box.listUnsentTargets(false, { mode });
              const selected = await box.selectUnsent(artifactId, false, {
                mode,
                storageEpoch,
              });
              return {
                kind: (await box.discardUnsent(selected)).kind,
                targets,
                target: selected.target,
                mode: selected.mode,
                fetches: cleanupFetches,
              };
            } catch (error) {
              return { kind: error.code };
            }
          },
          {
            artifactId: ARTIFACT,
            mode: offline ? 'offline' : 'online',
            storageEpoch: binding.storage_epoch,
          },
        );
        localChoices[choice] = cleanup.kind;
        assert.deepEqual(cleanup, {
          kind: 'discarded_unsent',
          targets: [
            {
              apiBaseUrl: origin,
              actorId: 'local',
              artifactId: ARTIFACT,
              storageEpoch: binding.storage_epoch,
              admittedGeneration: '1',
              headSequence: 1,
              recordVersion: 1,
            },
          ],
          target: {
            apiBaseUrl: origin,
            actorId: 'local',
            artifactId: ARTIFACT,
            storageEpoch: binding.storage_epoch,
            admittedGeneration: '1',
            headSequence: 1,
            recordVersion: 1,
          },
          mode: offline ? 'offline' : 'online',
          fetches: offline ? 0 : 3,
        });
        assert.equal((await rows(choicePage)).length, 0);
        await assert.rejects(enqueue(choicePage), /owner_not_ready/);
      } finally {
        currentBinding = binding;
        await choiceContext.close();
      }
    }
    assert.deepEqual(
      localChoices,
      {
        offline: 'discarded_unsent',
        pastEpoch: 'discarded_unsent',
        offlinePastEpoch: 'discarded_unsent',
      },
      'explicit cleanup must support retained page proof and the original same-actor/URL epoch',
    );

    // An external logout/profile switch terminally detaches the old proof.
    const detachedContext = await browser.newContext();
    try {
      const detachedPage = await page(detachedContext);
      assert.equal(await open(detachedPage), 'ready');
      await enqueue(detachedPage);
      const retained = await rows(detachedPage);
      const detached = await detachedPage.evaluate(async (artifactId) => {
        const selected = await box.selectUnsent(artifactId, false, {
          mode: 'offline',
        });
        const original = window.fetch;
        let fetches = 0;
        window.fetch = (...args) => {
          fetches += 1;
          return Reflect.apply(original, window, args);
        };
        box.invalidateAuthentication();
        const outcome = async (pending) => {
          try {
            await pending;
            return 'accepted';
          } catch (error) {
            return error.code;
          }
        };
        const results = await Promise.all([
          outcome(box.listUnsentTargets(false, { mode: 'offline' })),
          outcome(box.selectUnsent(artifactId, false, { mode: 'offline' })),
          outcome(box.discardUnsent(selected)),
          outcome(box.listUnsentTargets()),
          outcome(
            box.enqueue({
              artifactId,
              path: '',
              base: { version: 0 },
              operation: 'delete',
            }),
          ),
        ]);
        return { results, fetches, state: box.state };
      }, ARTIFACT);
      assert.deepEqual(detached, {
        results: Array(5).fill('owner_not_ready'),
        fetches: 0,
        state: { kind: 'binding_paused' },
      });
      assert.deepEqual(await rows(detachedPage), retained);
      const owner = await detachedPage.evaluate(
        (key) => sessionStorage.getItem(key),
        OWNER,
      );
      const duplicate = await page(detachedContext);
      await duplicate.evaluate(
        ({ key, owner }) => sessionStorage.setItem(key, owner),
        { key: OWNER, owner },
      );
      assert.equal(await open(duplicate), 'owner_busy');
      await detachedPage.evaluate(() => box.close());
      assert.equal(
        await duplicate.evaluate(
          async () => (await box.retryRecoveryCandidate()).kind,
        ),
        'recovery_required',
      );
    } finally {
      await detachedContext.close();
    }

    // Revoking authentication aborts even a delete whose first request succeeded.
    const deletionLifecycle = {};
    for (const action of ['logout', 'close']) {
      const lifecycleContext = await browser.newContext();
      try {
        const lifecyclePage = await page(lifecycleContext);
        assert.equal(await open(lifecyclePage), 'ready');
        const first = await enqueue(lifecyclePage);
        const second = await enqueue(lifecyclePage, ARTIFACT, {
          after: first.sequence,
        });
        await enqueue(lifecyclePage, ARTIFACT, { after: second.sequence });
        await enqueue(lifecyclePage, OTHER);
        await lifecyclePage.evaluate(() => {
          globalThis.inspectLifecycle = async () => {
            const db = await new Promise((resolve) => {
              const pending = indexedDB.open('artifree-data-outbox');
              pending.onsuccess = () => resolve(pending.result);
            });
            try {
              const result = {};
              for (const name of [
                'intents',
                'receipts',
                'checkpoints',
                'usage',
              ])
                result[name] = await new Promise((resolve) => {
                  const request = db
                    .transaction(name)
                    .objectStore(name)
                    .getAll();
                  request.onsuccess = () => resolve(request.result);
                });
              return result;
            } finally {
              db.close();
            }
          };
        });
        const before = await lifecyclePage.evaluate(() => inspectLifecycle());
        const result = await lifecyclePage.evaluate(
          async ({ artifactId, action }) => {
            const selection = await box.selectUnsent(artifactId, false, {
              mode: 'offline',
            });
            const original = IDBObjectStore.prototype.delete;
            IDBObjectStore.prototype.delete = function (...args) {
              const pending = Reflect.apply(original, this, args);
              if (this.name === 'intents') {
                IDBObjectStore.prototype.delete = original;
                pending.addEventListener(
                  'success',
                  () => {
                    if (action === 'logout') box.invalidateAuthentication();
                    else globalThis.lifecycleClose = box.close();
                  },
                  { once: true },
                );
              }
              return pending;
            };
            let result;
            try {
              result = (await box.discardUnsent(selection)).kind;
            } catch (error) {
              result = error.code;
            }
            if (action === 'close') await lifecycleClose;
            return result;
          },
          { artifactId: ARTIFACT, action },
        );
        const after = await lifecyclePage.evaluate(() => inspectLifecycle());
        deletionLifecycle[action] = {
          result,
          unchanged: JSON.stringify(after) === JSON.stringify(before),
          rows: after.intents.length,
          usageRows: after.usage[0].rows,
        };
      } finally {
        await lifecycleContext.close();
      }
    }
    assert.deepEqual(
      deletionLifecycle,
      {
        logout: {
          result: 'owner_not_ready',
          unchanged: true,
          rows: 4,
          usageRows: 4,
        },
        close: {
          result: 'discarded_unsent',
          unchanged: false,
          rows: 1,
          usageRows: 1,
        },
      },
      'auth invalidation must abort the whole deleting transaction; close settles an already-started transaction',
    );

    // Close may run after revalidate's own check, before its caller resumes.
    for (const depth of [2, 3]) {
      const microtaskContext = await browser.newContext();
      try {
        const microtaskPage = await page(microtaskContext);
        assert.equal(await open(microtaskPage), 'ready');
        await enqueue(microtaskPage);
        const retained = await rows(microtaskPage);
        const outcome = await microtaskPage.evaluate(
          async ({ artifactId, depth }) => {
            const selection = await box.selectUnsent(artifactId);
            globalThis.microtaskClosing = false;
            let writes = 0;
            const transaction = IDBDatabase.prototype.transaction;
            IDBDatabase.prototype.transaction = function (...args) {
              if (microtaskClosing && args[1] === 'readwrite') writes += 1;
              return Reflect.apply(transaction, this, args);
            };
            const parse = JSON.parse;
            JSON.parse = function (text, ...args) {
              const value = Reflect.apply(parse, JSON, [text, ...args]);
              if (text.includes('"actor_generation"')) {
                JSON.parse = parse;
                const schedule = (remaining) =>
                  queueMicrotask(() => {
                    if (remaining > 1) schedule(remaining - 1);
                    else {
                      microtaskClosing = true;
                      globalThis.microtaskClose = box.close();
                    }
                  });
                schedule(depth);
              }
              return value;
            };
            let result;
            try {
              result = (await box.discardUnsent(selection)).kind;
            } catch (error) {
              result = error.code;
            }
            await microtaskClose;
            return { result, writes };
          },
          { artifactId: ARTIFACT, depth },
        );
        assert.deepEqual(
          outcome,
          { result: 'outbox_closed', writes: 0 },
          'the final transaction boundary must recheck close after revalidate resolves',
        );
        assert.deepEqual(await rows(microtaskPage), retained);
      } finally {
        await microtaskContext.close();
      }
    }

    // close() fences actions not yet in a local transaction, including late GETs.
    const closingContext = await browser.newContext();
    try {
      const closingPage = await page(closingContext);
      assert.equal(await open(closingPage), 'ready');
      await enqueue(closingPage);
      const retained = await rows(closingPage);
      await closingPage.evaluate(async (artifactId) => {
        globalThis.otherHandle = await outboxApi.openBrowserOutbox({
          baseUrl: '',
          accessToken: 'test-token',
        });
        globalThis.closingSelection = await box.selectUnsent(artifactId);
        globalThis.closingMarker = false;
        globalThis.postCloseWrites = 0;
        const original = IDBDatabase.prototype.transaction;
        IDBDatabase.prototype.transaction = function (...args) {
          if (closingMarker && args[1] === 'readwrite') postCloseWrites += 1;
          return Reflect.apply(original, this, args);
        };
        globalThis.closingOutcome = async (pending) => {
          try {
            const result = await pending;
            return result?.kind ?? 'accepted';
          } catch (error) {
            return error.code;
          }
        };
      }, ARTIFACT);
      assert.equal(await closingPage.evaluate(() => otherHandle === box), true);
      mode = 'binding_hold';
      const entered = new Promise((resolve) => {
        onBindingHeld = resolve;
      });
      await closingPage.evaluate(() => {
        globalThis.pendingDiscard = closingOutcome(
          box.discardUnsent(closingSelection),
        );
      });
      await entered;
      await closingPage.evaluate((artifactId) => {
        globalThis.pendingEnqueue = closingOutcome(
          box.enqueue({
            artifactId,
            path: '',
            base: { version: 0 },
            operation: 'set',
            value: 1,
          }),
        );
        globalThis.pendingSelection = closingOutcome(
          box.selectUnsent(artifactId),
        );
        closingMarker = true;
        globalThis.pendingClose = otherHandle.close();
      }, ARTIFACT);
      mode = 'normal';
      for (const response of heldBindings.splice(0))
        response.end(JSON.stringify(currentBinding));
      const closed = await closingPage.evaluate(async () => {
        const outcomes = await Promise.all([
          pendingDiscard,
          pendingEnqueue,
          pendingSelection,
        ]);
        await pendingClose;
        return {
          outcomes,
          writes: postCloseWrites,
          state: box.state,
          other: await closingOutcome(
            otherHandle.enqueue({
              artifactId: 'art_' + 'a'.repeat(32),
              path: '',
              base: { version: 0 },
              operation: 'delete',
            }),
          ),
        };
      });
      assert.equal(
        closed.writes,
        0,
        'close must fence newly started write transactions after a held binding GET',
      );
      assert.deepEqual(closed, {
        outcomes: ['outbox_closed', 'outbox_closed', 'outbox_closed'],
        writes: 0,
        state: { kind: 'closed' },
        other: 'outbox_closed',
      });
      assert.deepEqual(await rows(closingPage), retained);
    } finally {
      mode = 'normal';
      onBindingHeld = undefined;
      await closingContext.close();
    }

    // An IDB transaction already in progress settles before explicit close.
    const settlingContext = await browser.newContext();
    try {
      const settlingPage = await page(settlingContext);
      assert.equal(await open(settlingPage), 'ready');
      const settled = await settlingPage.evaluate(async (artifactId) => {
        const original = IDBObjectStore.prototype.add;
        IDBObjectStore.prototype.add = function (...args) {
          const pending = Reflect.apply(original, this, args);
          if (this.name === 'intents') {
            IDBObjectStore.prototype.add = original;
            pending.addEventListener(
              'success',
              () => {
                globalThis.settlingClose = box.close();
              },
              { once: true },
            );
          }
          return pending;
        };
        const result = await box.enqueue({
          artifactId,
          path: '',
          base: { version: 0 },
          operation: 'set',
          value: 1,
        });
        await settlingClose;
        return { result, state: box.state };
      }, ARTIFACT);
      assert.equal(settled.result.kind, 'durably_enqueued');
      assert.deepEqual(settled.state, { kind: 'closed' });
      assert.equal((await rows(settlingPage)).length, 1);
      assert.equal(
        (await rows(settlingPage))[0].key,
        settled.result.idempotencyKey,
      );
    } finally {
      await settlingContext.close();
    }

    // Explicit unsent cleanup is independent of confirming a recovered queue.
    const cleanupContext = await browser.newContext();
    try {
      const cleanupPage = await page(cleanupContext);
      assert.equal(await open(cleanupPage), 'ready');
      const head = await enqueue(cleanupPage);
      await enqueue(cleanupPage, ARTIFACT, { after: head.sequence });
      await enqueue(cleanupPage, ARTIFACT, { version: 7 });
      const owner = await cleanupPage.evaluate(
        (key) => sessionStorage.getItem(key),
        OWNER,
      );
      await cleanupPage.evaluate(() => box.close());
      assert.equal(await open(cleanupPage), 'recovery_required');
      await cleanupPage.evaluate(() => {
        globalThis.priorRecovery = box.state.selection;
      });
      await assert.rejects(enqueue(cleanupPage), /owner_not_ready/);
      assert.deepEqual(
        await cleanupPage.evaluate(
          async (artifactId) =>
            box.discardUnsent(await box.selectUnsent(artifactId)),
          ARTIFACT,
        ),
        { kind: 'discarded_unsent' },
      );
      assert.equal((await rows(cleanupPage)).length, 1);
      assert.deepEqual((await rows(cleanupPage))[0].base, { version: 7 });
      assert.equal(
        await cleanupPage.evaluate((key) => sessionStorage.getItem(key), OWNER),
        owner,
      );
      await assert.rejects(enqueue(cleanupPage), /owner_not_ready/);
      await assert.rejects(
        cleanupPage.evaluate(() => box.confirmRecovery(priorRecovery)),
        /invalid_selection/,
      );
      const savedCleanup = await page(cleanupContext);
      await savedCleanup.evaluate(
        ({ key, owner }) => sessionStorage.setItem(key, owner),
        { key: OWNER, owner },
      );
      assert.equal(await open(savedCleanup), 'owner_busy');
      await cleanupPage.evaluate(() => box.close());
      assert.equal(
        await savedCleanup.evaluate(
          async () => (await box.retryRecoveryCandidate()).kind,
        ),
        'recovery_required',
      );
      assert.deepEqual(
        await savedCleanup.evaluate(
          async (artifactId) =>
            box.discardUnsent(await box.selectUnsent(artifactId, true)),
          ARTIFACT,
        ),
        { kind: 'discarded_unsent' },
      );
      assert.equal((await rows(savedCleanup)).length, 0);
      assert.equal(
        await savedCleanup.evaluate(
          (key) => sessionStorage.getItem(key),
          CANDIDATE,
        ),
        owner,
      );
      await savedCleanup.evaluate(() => {
        const original = Storage.prototype.removeItem;
        Storage.prototype.removeItem = function () {
          Storage.prototype.removeItem = original;
          throw new DOMException('injected', 'SecurityError');
        };
      });
      await assert.rejects(
        savedCleanup.evaluate(() => box.clearRecoveryCandidate()),
        /storage_failed/,
      );
      assert.equal(
        await savedCleanup.evaluate(
          (key) => sessionStorage.getItem(key),
          CANDIDATE,
        ),
        owner,
      );
      await savedCleanup.evaluate(() => box.clearRecoveryCandidate());
      assert.equal(
        (await enqueue(savedCleanup)).kind,
        'durably_enqueued',
        'saved-queue disposal does not adopt it; independent fresh-owner admission remains available',
      );
    } finally {
      await cleanupContext.close();
    }

    const generationCleanupContext = await browser.newContext();
    try {
      const generationCleanup = await page(generationCleanupContext);
      currentBinding = { ...binding, actor_id: `u-${'e'.repeat(32)}` };
      assert.equal(await open(generationCleanup), 'ready');
      await enqueue(generationCleanup);
      await generationCleanup.evaluate(() => box.close());
      currentBinding = { ...currentBinding, actor_generation: '2' };
      assert.equal(await open(generationCleanup), 'generation_changed');
      assert.equal((await rows(generationCleanup))[0].admittedGeneration, '1');
      await assert.rejects(enqueue(generationCleanup), /owner_not_ready/);
      assert.deepEqual(
        await generationCleanup.evaluate(
          async (artifactId) =>
            box.discardUnsent(await box.selectUnsent(artifactId)),
          ARTIFACT,
        ),
        { kind: 'discarded_unsent' },
      );
      assert.equal((await rows(generationCleanup)).length, 0);
      await assert.rejects(enqueue(generationCleanup), /owner_not_ready/);
    } finally {
      currentBinding = binding;
      await generationCleanupContext.close();
    }

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

/** Real R08a server binding plus browser-local admission; no data HTTP mutation. */
async function checkLiveBinding(browser, { origin, token }, module) {
  const context = await browser.newContext();
  try {
    await context.route(`${origin}/__outbox-entry.js`, (route) =>
      route.fulfill({ contentType: 'text/javascript', body: module }),
    );
    await context.route(`${origin}/__outbox-entry.html`, (route) =>
      route.fulfill({
        contentType: 'text/html',
        body: '<!doctype html><script type="module" src="/__outbox-entry.js"></script>',
      }),
    );
    const page = await context.newPage();
    const apiRequests = [];
    page.on('request', (request) => {
      if (request.url().startsWith(`${origin}/v1/`))
        apiRequests.push({ method: request.method(), url: request.url() });
    });
    await page.goto(`${origin}/__outbox-entry.html`);
    await page.waitForFunction(() => globalThis.outboxApi !== undefined);
    assert.equal(
      await page.evaluate(async () => {
        try {
          await outboxApi.openBrowserOutbox({
            baseUrl: '',
            accessToken: 'deliberately-invalid',
          });
          return 'accepted';
        } catch (error) {
          return error.code;
        }
      }),
      'unauthenticated',
    );
    assert.equal(await page.evaluate(() => sessionStorage.length), 0);
    assert.equal(
      await page.evaluate(async (token) => {
        globalThis.box = await outboxApi.openBrowserOutbox({
          baseUrl: '',
          accessToken: token,
        });
        return box.state.kind;
      }, token),
      'ready',
    );
    const accepted = await page.evaluate(
      async (artifactId) =>
        box.enqueue({
          artifactId,
          path: 'raw//path',
          base: { version: 0 },
          operation: 'set',
          value: new outboxApi.JsonText('{"n":9007199254740993}'),
        }),
      ARTIFACT,
    );
    assert.equal(accepted.kind, 'durably_enqueued');
    const owner = await page.evaluate(
      (key) => sessionStorage.getItem(key),
      OWNER,
    );
    await page.reload();
    await page.waitForFunction(() => globalThis.outboxApi !== undefined);
    assert.equal(
      await page.evaluate(async (token) => {
        globalThis.box = await outboxApi.openBrowserOutbox({
          baseUrl: '',
          accessToken: token,
        });
        return box.state.kind;
      }, token),
      'recovery_required',
    );
    assert.equal(
      await page.evaluate((key) => sessionStorage.getItem(key), OWNER),
      owner,
    );
    assert.deepEqual(
      await page.evaluate(
        async (artifactId) =>
          box.discardUnsent(await box.selectUnsent(artifactId)),
        ARTIFACT,
      ),
      { kind: 'discarded_unsent' },
    );
    assert.deepEqual(
      apiRequests,
      Array(5).fill({ method: 'GET', url: `${origin}/v1/data-outbox/binding` }),
    );
    console.log(
      'outbox: real server binding/auth, browser durable admission, reload and cleanup-only recovery passed',
    );
  } finally {
    await context.close();
  }
}
