import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { once } from 'node:events';
import { build } from 'esbuild';

/** Native IDB lifecycle checks. No sender or synthetic server ACK is claimed. */
export async function checkOutboxStore(browser) {
  const bundle = await build({
    stdin: {
      contents: `import {openStore} from './packages/client/src/outbox/store.ts';
        import {OutboxError, openBrowserOutbox} from './packages/client/src/outbox/index.ts';
        globalThis.openStore = openStore; globalThis.openBrowserOutbox = openBrowserOutbox;
        globalThis.OutboxError = OutboxError;`,
      resolveDir: process.cwd(),
      loader: 'ts',
    },
    bundle: true,
    format: 'esm',
    write: false,
    platform: 'browser',
  });
  const server = createServer((request, response) => {
    if (request.url === '/v1/data-outbox/binding') {
      response.setHeader('Content-Type', 'application/json');
      response.end(
        JSON.stringify({
          actor_id: 'local',
          actor_generation: '1',
          storage_epoch: 'b'.repeat(32),
        }),
      );
    } else if (request.url === '/module.js') {
      response.setHeader('Content-Type', 'text/javascript');
      response.end(bundle.outputFiles[0].contents);
    } else {
      response.setHeader('Content-Type', 'text/html');
      response.end(
        '<!doctype html><script type="module" src="/module.js"></script>',
      );
    }
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const context = await browser.newContext();
  const page = await context.newPage();
  try {
    await page.goto(`http://127.0.0.1:${server.address().port}`);
    await page.waitForFunction(() => globalThis.openStore !== undefined);
    await page.evaluate(async () => {
      globalThis.store = await openStore();
      globalThis.signal = new AbortController().signal;
      globalThis.authorize = () => {};
      globalThis.artifact = `art_${'a'.repeat(32)}`;
      globalThis.binding = [
        'a'.repeat(32),
        location.origin,
        'local',
        'b'.repeat(32),
      ];
      globalThis.lane = [...binding, artifact];
      globalThis.input = (
        key,
        base = { version: 0 },
        operation = 'set',
        text = '{"x":1}',
      ) => ({
        format: 2,
        state: 'unprepared',
        version: 1,
        key: key.repeat(32),
        owner: binding[0],
        binding,
        lane,
        artifactId: artifact,
        admittedGeneration: '1',
        authorizedGeneration: '1',
        operation,
        path: 'raw//path',
        text: operation === 'delete' ? null : text,
        base,
      });
      globalThis.snapshot = async () => {
        const db = await new Promise((resolve, reject) => {
          const opening = indexedDB.open('artifree-data-outbox');
          opening.onsuccess = () => resolve(opening.result);
          opening.onerror = () => reject(opening.error);
        });
        try {
          return await new Promise((resolve, reject) => {
            const tx = db.transaction(['intents', 'checkpoints', 'usage']);
            const rows = tx.objectStore('intents').getAll();
            const checkpoints = tx.objectStore('checkpoints').getAll();
            const usage = tx.objectStore('usage').get('global');
            tx.oncomplete = () =>
              resolve({
                rows: rows.result,
                checkpoints: checkpoints.result,
                usage: usage.result,
              });
            tx.onabort = () => reject(tx.error);
          });
        } finally {
          db.close();
        }
      };
      globalThis.reopen = async () => {
        store.close();
        store = await openStore();
      };
      globalThis.receipt = (row, changed = true, replayed = false) => ({
        artifact_id: row.artifactId,
        path: row.path,
        data_version: row.request.base + (changed ? 1 : 0),
        changed,
        replayed,
      });
      // Real requests succeed, then the native transaction aborts. Restoration is always explicit.
      globalThis.faultPut = (name, mode = 'abort') => {
        const original = IDBObjectStore.prototype.put;
        IDBObjectStore.prototype.put = function (...args) {
          if (this.name !== name) return Reflect.apply(original, this, args);
          if (mode === 'quota')
            throw new DOMException(
              'synthetic quota denial',
              'QuotaExceededError',
            );
          const pending = Reflect.apply(original, this, args);
          pending.addEventListener('success', () => this.transaction.abort(), {
            once: true,
          });
          return pending;
        };
        globalThis.restorePut = () => {
          IDBObjectStore.prototype.put = original;
        };
      };
      globalThis.head = await store.enqueue(input('1'));
      globalThis.child = await store.enqueue(
        input('2', { after: head.sequence }, 'update', '{"a":1,"a":2}'),
      );
      globalThis.independent = await store.enqueue(
        input('3', { version: 77 }, 'delete'),
      );
    });
    const admitted = await page.evaluate(() => snapshot());
    assert.equal(admitted.usage.rows, 3);
    await assert.rejects(
      page.evaluate(() => store.prepare(child, authorize, signal)),
      /invalid_selection/,
    );
    await assert.rejects(
      page.evaluate(() => store.enqueue(input('1'))),
      /storage_failed/,
    );
    assert.deepEqual(await page.evaluate(() => snapshot()), admitted);

    // Neither request success nor quota denial can promote an unprepared intent.
    await page.evaluate(() => faultPut('intents'));
    await assert.rejects(
      page.evaluate(() => store.prepare(head, authorize, signal)),
      /storage_failed/,
    );
    await page.evaluate(() => restorePut());
    assert.deepEqual(await page.evaluate(() => snapshot()), admitted);
    await page.evaluate(() => faultPut('intents', 'quota'));
    await assert.rejects(
      page.evaluate(() => store.prepare(head, authorize, signal)),
      /storage_failed/,
    );
    await page.evaluate(() => restorePut());
    assert.deepEqual(await page.evaluate(() => snapshot()), admitted);
    await page.evaluate(async () => {
      await reopen();
      globalThis.prepared = await store.prepare(head, authorize, signal);
    });
    const prepared = await page.evaluate(() => prepared);
    assert.deepEqual(prepared.request, {
      method: 'PUT',
      body: '{"base_data_version":0,"path":"raw//path","value":{"x":1}}',
      base: 0,
    });
    assert.equal(Object.hasOwn(prepared, 'text'), false);
    assert.equal(prepared.key, admitted.rows[0].key);
    assert.equal(prepared.bytes, admitted.rows[0].bytes);
    await assert.rejects(
      page.evaluate(() => store.prepare(head, authorize, signal)),
      /invalid_selection/,
    );
    await assert.rejects(
      page.evaluate(() =>
        store.discard([prepared, child, independent], authorize, signal),
      ),
      /invalid_selection/,
    );
    await assert.rejects(
      page.evaluate(() =>
        store.acknowledge(
          prepared,
          { ...receipt(prepared), data_version: 7 },
          authorize,
          signal,
        ),
      ),
      /storage_corrupt/,
    );
    await assert.rejects(
      page.evaluate(() =>
        store.acknowledge(
          prepared,
          { ...receipt(prepared), path: 'other' },
          authorize,
          signal,
        ),
      ),
      /storage_corrupt/,
    );
    const beforeAck = await page.evaluate(() => snapshot());
    await page.evaluate(() => faultPut('checkpoints'));
    await assert.rejects(
      page.evaluate(() =>
        store.acknowledge(prepared, receipt(prepared), authorize, signal),
      ),
      /storage_failed/,
    );
    await page.evaluate(() => restorePut());
    assert.deepEqual(await page.evaluate(() => snapshot()), beforeAck);
    await page.evaluate(async () => {
      await reopen();
      globalThis.prepared = (await store.lane(lane))[0];
    });
    assert.deepEqual(await page.evaluate(() => prepared), prepared);
    await assert.rejects(
      page.evaluate(() => store.prepare(child, authorize, signal)),
      /invalid_selection/,
    );

    // Replay receipt is persisted only by the new transaction, then allows its child.
    await page.evaluate(async () => {
      globalThis.ack = await store.acknowledge(
        prepared,
        receipt(prepared, true, true),
        authorize,
        signal,
      );
    });
    const acked = await page.evaluate(() => snapshot());
    assert.equal(acked.rows[0].state, 'acknowledged');
    assert.deepEqual(acked.checkpoints[0].predecessor, {
      sequence: 1,
      dataVersion: 1,
    });
    assert.deepEqual(acked.usage, admitted.usage);
    await assert.rejects(
      page.evaluate(() =>
        store.acknowledge(prepared, receipt(prepared), authorize, signal),
      ),
      /invalid_selection/,
    );
    await page.evaluate(() => faultPut('usage'));
    await assert.rejects(
      page.evaluate(() =>
        store.trim([ack, child, independent], authorize, signal),
      ),
      /storage_failed/,
    );
    await page.evaluate(() => restorePut());
    assert.deepEqual(await page.evaluate(() => snapshot()), acked);
    await page.evaluate(async () => {
      await reopen();
      await store.trim(await store.lane(lane), authorize, signal);
    });
    const trimmed = await page.evaluate(() => snapshot());
    assert.deepEqual(
      trimmed.rows.map((row) => row.sequence),
      [2, 3],
    );
    assert.equal(
      trimmed.usage.bytes,
      admitted.usage.bytes - admitted.rows[0].bytes,
    );
    assert.deepEqual(trimmed.checkpoints[0].predecessor, {
      sequence: 1,
      dataVersion: 1,
    });
    await assert.rejects(
      page.evaluate(() =>
        store.trim([ack, child, independent], authorize, signal),
      ),
      /invalid_selection/,
    );
    const next = await page.evaluate(async () => {
      await reopen();
      globalThis.next = await store.prepare(child, authorize, signal);
      return next;
    });
    assert.deepEqual(next.request, {
      method: 'PATCH',
      body: '{"base_data_version":1,"path":"raw//path","patch":{"a":1,"a":2}}',
      base: 1,
    });
    await page.evaluate(async () => {
      await store.acknowledge(next, receipt(next, false), authorize, signal);
      await store.trim(await store.lane(lane), authorize, signal);
      globalThis.last = await store.prepare(independent, authorize, signal);
    });
    assert.deepEqual((await page.evaluate(() => last)).request, {
      method: 'DELETE',
      body: '{"base_data_version":77,"path":"raw//path"}',
      base: 77,
    });
    await page.evaluate(async () => {
      await store.acknowledge(last, receipt(last, false), authorize, signal);
      await store.trim(await store.lane(lane), authorize, signal);
      await reopen();
    });
    const empty = await page.evaluate(() => snapshot());
    assert.deepEqual(empty, {
      rows: [],
      checkpoints: [],
      usage: { id: 'global', next: 4, rows: 0, bytes: 0 },
    });
    assert.equal(await page.evaluate(() => store.occupied(binding[0])), false);
    // Missing authority, aborted handles and stale identities cannot persist a request.
    await page.evaluate(async () => {
      globalThis.fresh = await store.enqueue(input('4'));
    });
    const fresh = await page.evaluate(() => snapshot());
    await assert.rejects(
      page.evaluate(() =>
        store.prepare(
          fresh,
          () => {
            throw new Error('authority revoked');
          },
          signal,
        ),
      ),
      /storage_failed/,
    );
    await assert.rejects(
      page.evaluate(() =>
        store.prepare({ ...fresh, key: '5'.repeat(32) }, authorize, signal),
      ),
      /invalid_selection/,
    );
    await assert.rejects(
      page.evaluate(() => store.prepare(fresh, authorize, AbortSignal.abort())),
      /owner_not_ready/,
    );
    assert.deepEqual(await page.evaluate(() => snapshot()), fresh);
    // Strict phase envelopes reject corruption without parsing raw application JSON.
    await page.evaluate(async () => {
      globalThis.freeze = await store.prepare(fresh, authorize, signal);
      globalThis.overwrite = async (value, checkpoint = false) => {
        const opening = indexedDB.open('artifree-data-outbox');
        const db = await new Promise((resolve, reject) => {
          opening.onsuccess = () => resolve(opening.result);
          opening.onerror = () => reject(opening.error);
        });
        try {
          await new Promise((resolve, reject) => {
            const tx = db.transaction(
              checkpoint ? 'checkpoints' : 'intents',
              'readwrite',
            );
            tx.objectStore(checkpoint ? 'checkpoints' : 'intents').put(value);
            tx.oncomplete = () => resolve();
            tx.onabort = () => reject(tx.error);
          });
        } finally {
          db.close();
        }
      };
    });
    const frozen = await page.evaluate(() => freeze);
    const unprepared = await page.evaluate(() => fresh);
    const missingAuthority = { ...unprepared };
    delete missingAuthority.authorizedGeneration;
    const missingFormat = { ...unprepared };
    delete missingFormat.format;
    for (const corrupted of [missingAuthority, missingFormat]) {
      await page.evaluate((value) => overwrite(value), corrupted);
      await assert.rejects(
        page.evaluate(() => store.lane(lane)),
        /storage_corrupt/,
      );
      await page.evaluate((value) => overwrite(value), frozen);
    }
    const downgradedPrepared = { ...frozen, format: 1 };
    delete downgradedPrepared.authorizedGeneration;
    const corruptions = [
      downgradedPrepared,
      { ...frozen, request: { ...frozen.request, method: 'DELETE' } },
      { ...frozen, request: { ...frozen.request, base: 99 } },
      { ...frozen, request: { ...frozen.request, body: '{}' } },
      { ...frozen, text: '{"shadow":true}' },
      { ...frozen, version: 3 },
      { ...frozen, authorizedGeneration: undefined },
      { ...frozen, format: undefined },
      {
        ...frozen,
        state: 'acknowledged',
        receipt: {
          artifact_id: frozen.artifactId,
          path: frozen.path,
          data_version: 99,
          changed: true,
          replayed: false,
        },
      },
    ];
    for (const corrupted of corruptions) {
      await page.evaluate((value) => overwrite(value), corrupted);
      await assert.rejects(
        page.evaluate(() => store.lane(lane)),
        /storage_corrupt/,
      );
      await page.evaluate((value) => overwrite(value), frozen);
      assert.deepEqual(
        (await page.evaluate(() => store.lane(lane)))[0],
        frozen,
      );
    }
    const checkpoint = (await page.evaluate(() => snapshot())).checkpoints[0];
    await page.evaluate(
      (value) =>
        overwrite(
          { ...value, predecessor: { sequence: 400, dataVersion: 1 } },
          true,
        ),
      checkpoint,
    );
    await assert.rejects(
      page.evaluate(() => store.lane(lane)),
      /storage_corrupt/,
    );
    await page.evaluate((value) => overwrite(value, true), checkpoint);
    await page.evaluate(async () => {
      await store.acknowledge(freeze, receipt(freeze), authorize, signal);
      await store.trim(await store.lane(lane), authorize, signal);
    });
    // These payloads intentionally violate Rust's domain rules. Storage preserves their bytes.
    for (const [key, text, path] of [
      ['5', '{', 'raw//path'],
      ['6', '{"n":9007199254740993}', 'raw//path'],
      ['7', '"' + 'x'.repeat(262142) + '"', 'raw//path'],
      ['8', '{"a":1,"a":2}', '\ud800'],
      ['9', '{"raw":1}', 'e\u0301/한글/𝄞'],
      ['a', '{', 'bad\u200dname'],
    ]) {
      const preserved = await page.evaluate(
        async ({ key, text, path }) => {
          const row = await store.enqueue({
            ...input(key, { version: 9007199254740990 }, 'set', text),
            path,
          });
          const prepared = await store.prepare(row, authorize, signal);
          await reopen();
          const recovered = (await store.lane(lane))[0];
          await store.acknowledge(
            recovered,
            receipt(recovered, false),
            authorize,
            signal,
          );
          await store.trim(await store.lane(lane), authorize, signal);
          return { prepared, recovered };
        },
        { key, text, path },
      );
      assert.deepEqual(preserved.recovered, preserved.prepared);
      assert.equal(
        preserved.prepared.request.body,
        '{"base_data_version":9007199254740990,"path":' +
          JSON.stringify(path) +
          ',"value":' +
          text +
          '}',
      );
      assert.ok(preserved.prepared.bytes <= 278528);
    }
    await page.evaluate(() => store.close());
    await page.evaluate(() => {
      globalThis.seedLegacy = async (
        base = 'https://example.test/api',
        actor = 'local',
      ) => {
        // Four base-URL copies per row in the fixed v1 reservation fixture.
        const delta = base.length - 24 + actor.length - 5;
        const firstBytes = 5060 + 4 * delta;
        const secondBytes = 5073 + 4 * delta;
        const totalBytes = firstBytes + secondBytes;
        await new Promise((resolve, reject) => {
          const deletion = indexedDB.deleteDatabase('artifree-data-outbox');
          deletion.onsuccess = () => resolve();
          deletion.onerror = () => reject(deletion.error);
        });
        globalThis.legacyBinding = [
          'a'.repeat(32),
          base,
          actor,
          'b'.repeat(32),
        ];
        globalThis.legacyLane = [...legacyBinding, artifact];
        const opening = indexedDB.open('artifree-data-outbox', 1);
        opening.onupgradeneeded = () => {
          const db = opening.result;
          for (const name of ['intents', 'receipts', 'checkpoints']) {
            const table = db.createObjectStore(name, {
              keyPath: name === 'intents' ? 'sequence' : 'lane',
            });
            table.createIndex('owner', 'owner');
            if (name === 'intents') {
              table.createIndex('binding', 'binding');
              table.createIndex('laneSequence', ['lane', 'sequence']);
              table.createIndex('key', 'key', { unique: true });
              table.createIndex('artifact', 'artifactId');
            }
          }
          db.createObjectStore('usage', { keyPath: 'id' }).add({
            id: 'global',
            next: 3,
            rows: 2,
            bytes: totalBytes,
          });
          const table = opening.transaction.objectStore('intents');
          // Independently fixed v1 allocations, including its original 4 KiB reserve.
          table.add({
            format: 1,
            state: 'unprepared',
            version: 1,
            sequence: 1,
            key: '1'.repeat(32),
            owner: legacyBinding[0],
            binding: legacyBinding,
            lane: legacyLane,
            artifactId: artifact,
            admittedGeneration: '1',
            operation: 'set',
            path: 'raw//path',
            text: '{',
            base: { version: 7 },
            bytes: firstBytes,
          });
          table.add({
            format: 1,
            state: 'unprepared',
            version: 1,
            sequence: 2,
            key: '2'.repeat(32),
            owner: legacyBinding[0],
            binding: legacyBinding,
            lane: legacyLane,
            artifactId: artifact,
            admittedGeneration: '1',
            operation: 'update',
            path: 'raw//path',
            text: '{"x":1,"x":2}',
            base: { after: 1 },
            bytes: secondBytes,
          });
          opening.transaction.objectStore('checkpoints').add({
            lane: legacyLane,
            owner: legacyBinding[0],
            generation: '1',
            rows: 2,
            bytes: totalBytes,
          });
        };
        const db = await new Promise((resolve, reject) => {
          opening.onsuccess = () => resolve(opening.result);
          opening.onerror = () => reject(opening.error);
        });
        db.close();
        sessionStorage.setItem(
          'artifree-data-outbox-owner-v1',
          legacyBinding[0],
        );
      };
      globalThis.version = async () => {
        const opening = indexedDB.open('artifree-data-outbox');
        const db = await new Promise((resolve, reject) => {
          opening.onsuccess = () => resolve(opening.result);
          opening.onerror = () => reject(opening.error);
        });
        const version = db.version;
        db.close();
        return version;
      };
    });
    await page.evaluate(async () => {
      await seedLegacy();
      const opening = indexedDB.open('artifree-data-outbox', 1);
      globalThis.blocker = await new Promise((resolve, reject) => {
        opening.onsuccess = () => resolve(opening.result);
        opening.onerror = () => reject(opening.error);
      });
    });
    const blockedOriginal = await page.evaluate(() => snapshot());
    await assert.rejects(
      page.evaluate(() => openStore()),
      /storage_failed/,
    );
    assert.equal(
      await page.evaluate(async () => {
        blocker.close();
        return version();
      }),
      1,
    );
    assert.deepEqual(await page.evaluate(() => snapshot()), blockedOriginal);
    // Opening upgrades layout metadata only, even with retained v1 payloads.
    await page.evaluate(async () => {
      globalThis.payloadReads = 0;
      const names = ['get', 'getAll', 'openCursor', 'put', 'add'];
      const originals = new Map(
        names.map((name) => [name, IDBObjectStore.prototype[name]]),
      );
      for (const name of names) {
        IDBObjectStore.prototype[name] = function (...args) {
          if (this.name === 'intents') {
            payloadReads += 1;
            throw new Error('pre-lock payload access');
          }
          return Reflect.apply(originals.get(name), this, args);
        };
      }
      globalThis.restorePayload = () => {
        for (const [name, original] of originals)
          IDBObjectStore.prototype[name] = original;
      };
      globalThis.store = await openStore();
      await reopen();
      restorePayload();
    });
    assert.equal(await page.evaluate(() => payloadReads), 0);
    assert.equal(await page.evaluate(() => version()), 2);
    assert.deepEqual(await page.evaluate(() => snapshot()), blockedOriginal);
    // A held lock is required by the coordinator before normalizing this binding.
    await page.evaluate(async () => {
      let held;
      const acquired = new Promise((resolve) => {
        held = resolve;
      });
      globalThis.lockLifetime = navigator.locks.request(
        'artifree-data-outbox:' + legacyBinding[0],
        async (lock) => {
          if (lock === null) throw new Error('missing owner lock');
          held();
          await new Promise((resolve) => {
            globalThis.releaseLegacyLock = resolve;
          });
        },
      );
      await acquired;
    });
    const beforeTransition = await page.evaluate(() => snapshot());
    await page.evaluate(() => faultPut('intents'));
    await assert.rejects(
      page.evaluate(async () => {
        const rows = await store.recover(legacyBinding);
        return store.prepare(rows[0], authorize, signal);
      }),
      /storage_failed/,
    );
    await page.evaluate(() => restorePut());
    assert.deepEqual(await page.evaluate(() => snapshot()), beforeTransition);
    // Checkpoint failure happens after the row put; both must still roll back.
    for (const mode of ['abort', 'quota']) {
      await page.evaluate((mode) => faultPut('checkpoints', mode), mode);
      await assert.rejects(
        page.evaluate(async () => {
          const rows = await store.recover(legacyBinding);
          return store.prepare(rows[0], authorize, signal);
        }),
        /storage_failed/,
      );
      await page.evaluate(() => restorePut());
      assert.deepEqual(await page.evaluate(() => snapshot()), beforeTransition);
      await page.evaluate(() => reopen());
      assert.deepEqual(await page.evaluate(() => snapshot()), beforeTransition);
    }
    await page.evaluate(() => faultPut('intents', 'quota'));
    await assert.rejects(
      page.evaluate(async () => {
        const rows = await store.recover(legacyBinding);
        return store.prepare(rows[0], authorize, signal);
      }),
      /storage_failed/,
    );
    await page.evaluate(() => restorePut());
    assert.deepEqual(await page.evaluate(() => snapshot()), beforeTransition);
    const migratedHead = await page.evaluate(async () => {
      const recovered = await store.recover(legacyBinding);
      return store.prepare(recovered[0], authorize, signal);
    });
    const afterPreparation = await page.evaluate(() => snapshot());
    assert.deepEqual(afterPreparation.checkpoints, [
      { ...beforeTransition.checkpoints[0], format: 2, predecessor: null },
    ]);
    assert.deepEqual(afterPreparation.rows, [
      migratedHead,
      beforeTransition.rows[1],
    ]);
    assert.deepEqual(afterPreparation.usage, beforeTransition.usage);
    assert.equal(migratedHead.version, 2);
    assert.equal(migratedHead.key, beforeTransition.rows[0].key);
    assert.equal(migratedHead.bytes, beforeTransition.rows[0].bytes);
    assert.deepEqual(migratedHead.request, {
      method: 'PUT',
      base: 7,
      body: '{"base_data_version":7,"path":"raw//path","value":{}',
    });
    // No ACK has happened: reopening must retain the exact prepared request.
    await page.evaluate(() => reopen());
    assert.deepEqual(await page.evaluate(() => store.recover(legacyBinding)), [
      migratedHead,
      {
        ...beforeTransition.rows[1],
        format: 2,
        authorizedGeneration: '1',
      },
    ]);
    assert.deepEqual(await page.evaluate(() => snapshot()), afterPreparation);
    // A migrated head does not make a corrupt retained legacy dependent safe.
    const wrongGeneration = {
      ...beforeTransition.rows[1],
      admittedGeneration: '2',
    };
    await page.evaluate((row) => overwrite(row), wrongGeneration);
    const corruptMixed = await page.evaluate(() => snapshot());
    await assert.rejects(
      page.evaluate(() => store.recover(legacyBinding)),
      /storage_corrupt/,
    );
    await assert.rejects(
      page.evaluate(
        (row) => store.prepare(row, authorize, signal),
        migratedHead,
      ),
      /storage_corrupt/,
    );
    assert.deepEqual(await page.evaluate(() => snapshot()), corruptMixed);
    await page.evaluate((row) => overwrite(row), beforeTransition.rows[1]);
    const migratedRequest = await page.evaluate(async () => {
      const [first] = await store.recover(legacyBinding);
      await store.acknowledge(
        first,
        receipt(first, false, true),
        authorize,
        signal,
      );
      await store.trim(await store.lane(legacyLane), authorize, signal);
      await reopen();
      const dependent = await store.prepare(
        (await store.recover(legacyBinding))[0],
        authorize,
        signal,
      );
      await store.acknowledge(dependent, receipt(dependent), authorize, signal);
      return dependent;
    });
    assert.deepEqual(migratedRequest.request, {
      method: 'PATCH',
      base: 7,
      body: '{"base_data_version":7,"path":"raw//path","patch":{"x":1,"x":2}}',
    });
    const durableDependent = (await page.evaluate(() => snapshot())).rows[0];
    assert.equal(durableDependent.state, 'acknowledged');
    const downgradedAck = { ...durableDependent, format: 1 };
    delete downgradedAck.authorizedGeneration;
    await page.evaluate((row) => overwrite(row), downgradedAck);
    await assert.rejects(
      page.evaluate(() => store.lane(legacyLane)),
      /storage_corrupt/,
    );
    await page.evaluate((row) => overwrite(row), durableDependent);
    for (const after of [2, 3]) {
      await page.evaluate(
        ({ row, after }) => overwrite({ ...row, base: { after } }),
        { row: durableDependent, after },
      );
      await assert.rejects(
        page.evaluate(() => store.lane(legacyLane)),
        /storage_corrupt/,
      );
      await page.evaluate((row) => overwrite(row), durableDependent);
    }
    await page.evaluate(() =>
      store
        .lane(legacyLane)
        .then((rows) => store.trim(rows, authorize, signal)),
    );
    assert.equal((await page.evaluate(() => snapshot())).usage.bytes, 0);
    await page.evaluate(() => store.close());
    await page.evaluate(async () => {
      await seedLegacy();
      const before = await snapshot();
      const unbound = { ...before.rows[0] };
      delete unbound.admittedGeneration;
      await overwrite(unbound);
    });
    const unbound = await page.evaluate(() => snapshot());
    await page.evaluate(async () => {
      globalThis.store = await openStore();
    });
    await assert.rejects(
      page.evaluate(() => store.recover(legacyBinding)),
      /storage_corrupt/,
    );
    assert.equal(await page.evaluate(() => version()), 2);
    assert.deepEqual(await page.evaluate(() => snapshot()), unbound);
    await page.evaluate(async () => {
      store.close();
      releaseLegacyLock();
      await lockLifetime;
    });
    // Busy and foreign-binding owners remain key-only probes through the actual public coordinator.
    await page.evaluate(() => {
      globalThis.guardPayload = () => {
        globalThis.payloadReads = 0;
        const names = ['get', 'getAll', 'openCursor', 'put', 'add'];
        const originals = new Map(
          names.map((name) => [name, IDBObjectStore.prototype[name]]),
        );
        for (const name of names)
          IDBObjectStore.prototype[name] = function (...args) {
            if (this.name === 'intents') {
              payloadReads += 1;
              throw new Error('unowned payload access');
            }
            return Reflect.apply(originals.get(name), this, args);
          };
        globalThis.restorePayload = () => {
          for (const [name, original] of originals)
            IDBObjectStore.prototype[name] = original;
        };
      };
      globalThis.holdLegacy = async () => {
        let held;
        const acquired = new Promise((resolve) => {
          held = resolve;
        });
        globalThis.lockLifetime = navigator.locks.request(
          'artifree-data-outbox:' + legacyBinding[0],
          async () => {
            held();
            await new Promise((resolve) => {
              globalThis.releaseLegacyLock = resolve;
            });
          },
        );
        await acquired;
      };
    });
    await page.evaluate(async () => {
      sessionStorage.clear();
      await seedLegacy();
      await holdLegacy();
    });
    const busyOriginal = await page.evaluate(() => snapshot());
    assert.equal(
      await page.evaluate(async () => {
        guardPayload();
        globalThis.publicBox = await openBrowserOutbox({
          baseUrl: location.origin,
          accessToken: 'test-token',
        });
        return publicBox.state.kind;
      }),
      'owner_busy',
    );
    assert.equal(await page.evaluate(() => payloadReads), 0);
    await page.evaluate(async () => {
      await publicBox.close();
      restorePayload();
      releaseLegacyLock();
      await lockLifetime;
    });
    assert.deepEqual(await page.evaluate(() => snapshot()), busyOriginal);
    await page.evaluate(async () => {
      sessionStorage.clear();
      await seedLegacy();
    });
    const foreignOriginal = await page.evaluate(() => snapshot());
    assert.equal(
      await page.evaluate(async () => {
        guardPayload();
        globalThis.publicBox = await openBrowserOutbox({
          baseUrl: location.origin,
          accessToken: 'test-token',
        });
        return publicBox.state.kind;
      }),
      'owner_binding_quarantined',
    );
    assert.equal(await page.evaluate(() => payloadReads), 0);
    await page.evaluate(async () => {
      await publicBox.close();
      restorePayload();
    });
    assert.deepEqual(await page.evaluate(() => snapshot()), foreignOriginal);
    await page.evaluate(async () => {
      sessionStorage.clear();
      await seedLegacy(location.origin, 'u-' + 'c'.repeat(32));
    });
    const otherActorOriginal = await page.evaluate(() => snapshot());
    assert.equal(
      await page.evaluate(async () => {
        guardPayload();
        globalThis.publicBox = await openBrowserOutbox({
          baseUrl: location.origin,
          accessToken: 'test-token',
        });
        return publicBox.state.kind;
      }),
      'owner_binding_quarantined',
    );
    assert.equal(await page.evaluate(() => payloadReads), 0);
    await page.evaluate(async () => {
      await publicBox.close();
      restorePayload();
    });
    assert.deepEqual(await page.evaluate(() => snapshot()), otherActorOriginal);
    // Matching legacy data normalizes only after this owner lock is held, and stays raw on read.
    await page.evaluate(async () => {
      sessionStorage.clear();
      await seedLegacy(location.origin);
    });
    const matchingOriginal = await page.evaluate(() => snapshot());
    const recovered = await page.evaluate(async () => {
      const original = IDBObjectStore.prototype.get;
      globalThis.heldPayloadReads = 0;
      const requestLock = navigator.locks.request.bind(navigator.locks);
      const heldOwners = new Set();
      navigator.locks.request = (name, options, callback) =>
        requestLock(name, options, async (lock) => {
          if (lock !== null) heldOwners.add(name);
          try {
            return await callback(lock);
          } finally {
            heldOwners.delete(name);
          }
        });
      IDBObjectStore.prototype.get = function (...args) {
        if (this.name === 'intents') {
          if (!heldOwners.has('artifree-data-outbox:' + legacyBinding[0]))
            throw new Error('payload before owner lock');
          heldPayloadReads += 1;
        }
        return Reflect.apply(original, this, args);
      };
      try {
        globalThis.publicBox = await openBrowserOutbox({
          baseUrl: location.origin,
          accessToken: 'test-token',
        });
        const locks = await navigator.locks.query();
        return {
          kind: publicBox.state.kind,
          held: locks.held.map((lock) => lock.name),
          reads: heldPayloadReads,
        };
      } finally {
        IDBObjectStore.prototype.get = original;
        navigator.locks.request = requestLock;
      }
    });
    assert.equal(recovered.kind, 'recovery_required');
    assert.ok(
      recovered.held.includes('artifree-data-outbox:' + 'a'.repeat(32)),
    );
    assert.ok(recovered.reads > 0);
    assert.deepEqual(await page.evaluate(() => snapshot()), matchingOriginal);
    await page.evaluate(async () => {
      await publicBox.confirmRecovery(publicBox.state.selection);
      await publicBox.close();
    });
    assert.deepEqual(await page.evaluate(() => snapshot()), matchingOriginal);

    // Rejected-head disposal (0049 rules 7, 9 and 10) through the scheduler-only
    // store operation. Each case starts from an empty database, so every counter
    // and checkpoint asserted here is absolute.
    const primaryArtifact = `art_${'a'.repeat(32)}`;
    const foreignArtifact = `art_${'c'.repeat(32)}`;
    const otherArtifact = `art_${'e'.repeat(32)}`;
    const LANE_BYTES = 2 * 1024 * 1024;
    const laneCheckpoint = (state, artifactId) =>
      state.checkpoints.find((row) => row.lane.at(-1) === artifactId);
    const sumBytes = (rows) => rows.reduce((sum, row) => sum + row.bytes, 0);
    await page.evaluate(() => {
      globalThis.resetDatabase = async () => {
        store.close();
        await new Promise((resolve, reject) => {
          const deleting = indexedDB.deleteDatabase('artifree-data-outbox');
          deleting.onsuccess = () => resolve();
          deleting.onerror = () => reject(deleting.error);
          deleting.onblocked = () =>
            reject(new Error('outbox database deletion blocked'));
        });
        store = await openStore();
      };
      globalThis.foreignLane = [...binding, `art_${'c'.repeat(32)}`];
      globalThis.keyFor = (index) =>
        index.toString(16).padStart(2, '0').repeat(16);
      globalThis.enqueueChainPair = async () => {
        globalThis.chainHead = await store.enqueue(input('1'));
        globalThis.chainChild = await store.enqueue(
          input('2', { after: chainHead.sequence }, 'update', '{"a":1,"a":2}'),
        );
      };
      globalThis.selectLane = () =>
        store.captureCleanup(lane, authorize, signal);
      globalThis.disposeRejected = (expected, dispatched) =>
        store.discardRejectedSnapshot(expected, dispatched, authorize, signal);
      globalThis.otherBinding = [
        'd'.repeat(32),
        location.origin,
        'local',
        'b'.repeat(32),
      ];
      globalThis.otherLane = [...otherBinding, `art_${'e'.repeat(32)}`];
      globalThis.rowOverhead = (row) => row.bytes - row.text.length;
      // Resolves to { value } or { error: code }, so each case asserts the exact outcome.
      globalThis.settle = (promise) =>
        promise.then(
          (value) => ({ value }),
          (error) => ({
            error: error instanceof OutboxError ? error.code : String(error),
          }),
        );
      globalThis.closureFixture = async () => {
        await resetDatabase();
        await enqueueChainPair();
        globalThis.chainGrand = await store.enqueue(
          input('4', { after: chainChild.sequence }),
        );
        globalThis.survivor = await store.enqueue(
          input('3', { version: 77 }, 'delete'),
        );
        globalThis.preparedHead = await store.prepare(
          chainHead,
          authorize,
          signal,
        );
        globalThis.selection = await selectLane();
      };
      globalThis.countAuthorizeCalls = (expected, dispatched) => {
        let calls = 0;
        return settle(
          store.discardRejectedSnapshot(
            expected,
            dispatched,
            () => {
              calls += 1;
            },
            signal,
          ),
        ).then((outcome) => ({ outcome, calls }));
      };
      // Interrupts the closure discard at authorize call `call`. A denial throws
      // invalid_selection; an abort invalidates the transaction.
      globalThis.interruptedDiscard = (call, mode) => {
        const controller = new AbortController();
        let calls = 0;
        return settle(
          store.discardRejectedSnapshot(
            selection,
            preparedHead,
            () => {
              calls += 1;
              if (calls !== call) {
                return;
              }
              if (mode === 'abort') {
                controller.abort();
              } else {
                throw new OutboxError('invalid_selection');
              }
            },
            controller.signal,
          ),
        );
      };
      globalThis.faultPutAbortFirst = (name) => {
        const original = IDBObjectStore.prototype.put;
        IDBObjectStore.prototype.put = function (...args) {
          if (this.name === name && this.transaction.mode === 'readwrite') {
            this.transaction.abort();
          }
          return Reflect.apply(original, this, args);
        };
        globalThis.restorePut = () => {
          IDBObjectStore.prototype.put = original;
        };
      };
      // Counts the readwrite transactions that the action opens.
      globalThis.countReadwrite = async (action) => {
        const original = IDBDatabase.prototype.transaction;
        let readwrite = 0;
        IDBDatabase.prototype.transaction = function (...args) {
          if (args[1] === 'readwrite') {
            readwrite += 1;
          }
          return Reflect.apply(original, this, args);
        };
        try {
          return { result: await settle(action()), readwrite };
        } finally {
          IDBDatabase.prototype.transaction = original;
        }
      };
      // Raw IndexedDB writes that bypass the store. A put adds a record; a field edit
      // sets one field of an existing record (`lane` addresses the lane's checkpoint).
      globalThis.rawEdit = async (edits) => {
        const db = await new Promise((resolve, reject) => {
          const opening = indexedDB.open('artifree-data-outbox');
          opening.onsuccess = () => resolve(opening.result);
          opening.onerror = () => reject(opening.error);
        });
        try {
          await new Promise((resolve, reject) => {
            const tx = db.transaction(
              ['intents', 'checkpoints', 'usage'],
              'readwrite',
            );
            tx.oncomplete = () => resolve();
            tx.onabort = () => reject(new Error('raw edit aborted'));
            for (const edit of edits) {
              const target = tx.objectStore(edit.store);
              if (edit.put !== undefined) {
                target.put(edit.put);
              } else {
                const read = target.get(edit.lane ? lane : edit.key);
                read.onsuccess = () => {
                  if (read.result === undefined) {
                    tx.abort();
                    return;
                  }
                  target.put({ ...read.result, [edit.field]: edit.value });
                };
              }
            }
          });
        } finally {
          db.close();
        }
      };
      // A lane of 63 chained rows: the first is about 28 KB, the others 32 KB each.
      globalThis.fillBoundPrefix = async () => {
        await resetDatabase();
        globalThis.boundPrefix = [];
        for (let index = 0; index < 63; index += 1) {
          const previous = boundPrefix.at(-1);
          const base = previous ? { after: previous.sequence } : { version: 0 };
          const size = previous ? 32768 - rowOverhead(previous) : 28000;
          boundPrefix.push(
            await store.enqueue({
              ...input('0', base, 'update', 'x'.repeat(size)),
              key: keyFor(index),
            }),
          );
        }
      };
      // The 64th row, sized so the lane totals LANE_BYTES plus `extra` bytes.
      globalThis.closingInput = (extra) => {
        const used = boundPrefix.reduce((sum, row) => sum + row.bytes, 0);
        const size =
          2 * 1024 * 1024 - used - rowOverhead(boundPrefix.at(-1)) + extra;
        return {
          ...input(
            '0',
            { after: boundPrefix.at(-1).sequence },
            'update',
            'x'.repeat(size),
          ),
          key: keyFor(63),
        };
      };
      globalThis.fillSmallLane = async (count) => {
        await resetDatabase();
        globalThis.smallRows = [];
        for (let index = 0; index < count; index += 1) {
          const previous = smallRows.at(-1);
          const base = previous ? { after: previous.sequence } : { version: 0 };
          smallRows.push(
            await store.enqueue({ ...input('0', base), key: keyFor(index) }),
          );
        }
      };
    });

    // B22 shape: head, two chained dependents, a numeric-base survivor, a row in
    // another lane of the same binding, and a row under a different owner.
    await page.evaluate(async () => {
      await resetDatabase();
      await enqueueChainPair();
      globalThis.chainGrand = await store.enqueue(
        input('4', { after: chainChild.sequence }),
      );
      globalThis.survivor = await store.enqueue(
        input('3', { version: 77 }, 'delete'),
      );
      globalThis.foreign = await store.enqueue({
        ...input('5'),
        artifactId: foreignLane.at(-1),
        lane: foreignLane,
      });
      globalThis.foreignBinding = await store.enqueue({
        ...input('6'),
        owner: otherBinding[0],
        binding: otherBinding,
        lane: otherLane,
        artifactId: otherLane.at(-1),
      });
      globalThis.preparedHead = await store.prepare(
        chainHead,
        authorize,
        signal,
      );
      globalThis.selection = await selectLane();
    });
    const closureBefore = await page.evaluate(() => snapshot());
    const survivorRow = await page.evaluate(() => survivor);
    const foreignRow = await page.evaluate(() => foreign);
    const foreignBindingRow = await page.evaluate(() => foreignBinding);
    assert.equal(closureBefore.rows.length, 6);
    assert.deepEqual(
      await page.evaluate(() => disposeRejected(selection, preparedHead)),
      { removedDependents: 2, survivors: [survivorRow] },
    );
    const closureAfter = await page.evaluate(() => snapshot());
    assert.deepEqual(closureAfter.rows, [
      survivorRow,
      foreignRow,
      foreignBindingRow,
    ]);
    // The survivor checkpoint keeps the lane's predecessor and counts only survivors.
    assert.deepEqual(laneCheckpoint(closureAfter, primaryArtifact), {
      ...laneCheckpoint(closureBefore, primaryArtifact),
      rows: 1,
      bytes: survivorRow.bytes,
    });
    assert.deepEqual(
      laneCheckpoint(closureAfter, foreignArtifact),
      laneCheckpoint(closureBefore, foreignArtifact),
    );
    assert.deepEqual(
      laneCheckpoint(closureAfter, otherArtifact),
      laneCheckpoint(closureBefore, otherArtifact),
    );
    // The head, its two dependents and nothing else leave usage, each with its
    // recorded bytes (every row's bytes include the 4096 reserve).
    const [headRow, childRow, grandRow] = closureBefore.rows;
    assert.deepEqual(
      [headRow, childRow, grandRow].map((row) => row.bytes >= 4096),
      [true, true, true],
    );
    assert.equal(closureBefore.usage.rows - closureAfter.usage.rows, 3);
    assert.equal(
      closureBefore.usage.bytes - closureAfter.usage.bytes,
      headRow.bytes + childRow.bytes + grandRow.bytes,
    );

    // Zero survivors: the lane checkpoint is deleted and usage returns to zero.
    await page.evaluate(async () => {
      await resetDatabase();
      await enqueueChainPair();
      globalThis.preparedHead = await store.prepare(
        chainHead,
        authorize,
        signal,
      );
      globalThis.selection = await selectLane();
    });
    const emptyBefore = await page.evaluate(() => snapshot());
    assert.equal(emptyBefore.rows.length, 2);
    assert.deepEqual(
      await page.evaluate(() => disposeRejected(selection, preparedHead)),
      { removedDependents: 1, survivors: [] },
    );
    const emptyAfter = await page.evaluate(() => snapshot());
    assert.deepEqual(emptyAfter.rows, []);
    assert.deepEqual(emptyAfter.checkpoints, []);
    assert.equal(emptyBefore.usage.rows - emptyAfter.usage.rows, 2);
    assert.equal(emptyAfter.usage.bytes, 0);
    assert.equal(
      emptyBefore.usage.bytes - emptyAfter.usage.bytes,
      sumBytes(emptyBefore.rows),
    );

    // A trimmed predecessor stays on the survivor checkpoint after the closure goes.
    await page.evaluate(async () => {
      await resetDatabase();
      // The closure is chained after the earlier row, so trimming that row keeps
      // it as the checkpoint predecessor. A lane emptied by trim would lose it.
      globalThis.earlier = await store.enqueue(input('6'));
      globalThis.chainHead = await store.enqueue(
        input('1', { after: earlier.sequence }),
      );
      globalThis.chainChild = await store.enqueue(
        input('2', { after: chainHead.sequence }, 'update', '{"a":1,"a":2}'),
      );
      globalThis.survivor = await store.enqueue(
        input('3', { version: 77 }, 'delete'),
      );
      const earlierPrepared = await store.prepare(earlier, authorize, signal);
      globalThis.earlierAck = await store.acknowledge(
        earlierPrepared,
        receipt(earlierPrepared),
        authorize,
        signal,
      );
      await store.trim(
        [earlierAck, chainHead, chainChild, survivor],
        authorize,
        signal,
      );
      globalThis.preparedHead = await store.prepare(
        chainHead,
        authorize,
        signal,
      );
      globalThis.selection = await selectLane();
    });
    const predecessorBefore = await page.evaluate(() => snapshot());
    const predecessorRow = await page.evaluate(() => survivor);
    assert.notEqual(
      laneCheckpoint(predecessorBefore, primaryArtifact).predecessor,
      null,
    );
    assert.deepEqual(
      await page.evaluate(() => disposeRejected(selection, preparedHead)),
      { removedDependents: 1, survivors: [predecessorRow] },
    );
    const predecessorAfter = await page.evaluate(() => snapshot());
    assert.deepEqual(laneCheckpoint(predecessorAfter, primaryArtifact), {
      ...laneCheckpoint(predecessorBefore, primaryArtifact),
      rows: 1,
      bytes: predecessorRow.bytes,
    });

    // Counter-step aborts leave rows, checkpoint and usage intact; a fresh
    // selection then commits once.
    await page.evaluate(async () => {
      await resetDatabase();
      await enqueueChainPair();
      globalThis.abortSurvivor = await store.enqueue(
        input('3', { version: 77 }, 'delete'),
      );
      globalThis.preparedHead = await store.prepare(
        chainHead,
        authorize,
        signal,
      );
      globalThis.selection = await selectLane();
    });
    const abortSurvivorRow = await page.evaluate(() => abortSurvivor);
    const abortBefore = await page.evaluate(() => snapshot());
    // The usage write is the last counter step: a quota denial there, and an
    // abort after its request succeeds, must each roll back everything.
    await page.evaluate(() => faultPut('usage', 'quota'));
    await assert.rejects(
      page.evaluate(() => disposeRejected(selection, preparedHead)),
      /storage_failed/,
    );
    await page.evaluate(() => restorePut());
    assert.deepEqual(await page.evaluate(() => snapshot()), abortBefore);
    await page.evaluate(() => faultPut('usage'));
    await assert.rejects(
      page.evaluate(() => disposeRejected(selection, preparedHead)),
      /storage_failed/,
    );
    await page.evaluate(() => restorePut());
    assert.deepEqual(await page.evaluate(() => snapshot()), abortBefore);
    await page.evaluate(async () => {
      globalThis.selection = await selectLane();
    });
    assert.deepEqual(
      await page.evaluate(() => disposeRejected(selection, preparedHead)),
      { removedDependents: 1, survivors: [abortSurvivorRow] },
    );
    const abortAfter = await page.evaluate(() => snapshot());
    assert.deepEqual(abortAfter.rows, [abortSurvivorRow]);
    assert.equal(abortBefore.usage.rows - abortAfter.usage.rows, 2);
    assert.equal(
      abortBefore.usage.bytes - abortAfter.usage.bytes,
      sumBytes(abortBefore.rows.slice(0, 2)),
    );

    // Abort before the usage counter: the usage write runs after its transaction
    // has been aborted. The result is storage_failed and nothing is removed.
    await page.evaluate(() => closureFixture());
    const abortFirstBefore = await page.evaluate(() => snapshot());
    await page.evaluate(() => faultPutAbortFirst('usage'));
    assert.deepEqual(
      await page.evaluate(() =>
        settle(disposeRejected(selection, preparedHead)),
      ),
      { error: 'storage_failed' },
    );
    await page.evaluate(() => restorePut());
    assert.deepEqual(await page.evaluate(() => snapshot()), abortFirstBefore);

    // Authorize sweep. The closure discard makes `counted.calls` authorize calls.
    // Throwing at each call in turn rejects with that error, and aborting the signal
    // at each call in turn rejects with owner_not_ready. Each leaves the state intact.
    await page.evaluate(() => closureFixture());
    const counted = await page.evaluate(() =>
      countAuthorizeCalls(selection, preparedHead),
    );
    assert.equal(counted.outcome.value.removedDependents, 2);
    assert.ok(
      counted.calls >= 6,
      'each of the three deletes is bracketed by authorize calls',
    );
    for (const [mode, code] of [
      ['deny', 'invalid_selection'],
      ['abort', 'owner_not_ready'],
    ]) {
      for (let call = 1; call <= counted.calls; call += 1) {
        await page.evaluate(() => closureFixture());
        const before = await page.evaluate(() => snapshot());
        assert.deepEqual(
          await page.evaluate(
            ({ call, mode }) => interruptedDiscard(call, mode),
            { call, mode },
          ),
          { error: code },
          `${mode} at authorize call ${call}`,
        );
        assert.deepEqual(
          await page.evaluate(() => snapshot()),
          before,
          `${mode} at authorize call ${call} changed state`,
        );
      }
    }

    // Bound: 64 rows that total exactly LANE_BYTES are one closure, deleted in one
    // readwrite transaction with exact deltas.
    await page.evaluate(async () => {
      await fillBoundPrefix();
      await store.enqueue(closingInput(0));
      globalThis.preparedHead = await store.prepare(
        boundPrefix[0],
        authorize,
        signal,
      );
      globalThis.selection = await selectLane();
    });
    const boundBefore = await page.evaluate(() => snapshot());
    assert.equal(boundBefore.rows.length, 64);
    assert.equal(sumBytes(boundBefore.rows), LANE_BYTES);
    assert.equal(laneCheckpoint(boundBefore, primaryArtifact).rows, 64);
    assert.equal(
      laneCheckpoint(boundBefore, primaryArtifact).bytes,
      LANE_BYTES,
    );
    assert.deepEqual(
      await page.evaluate(() =>
        countReadwrite(() => disposeRejected(selection, preparedHead)),
      ),
      {
        result: { value: { removedDependents: 63, survivors: [] } },
        readwrite: 1,
      },
    );
    const boundAfter = await page.evaluate(() => snapshot());
    assert.deepEqual(boundAfter.rows, []);
    assert.deepEqual(boundAfter.checkpoints, []);
    assert.equal(boundBefore.usage.rows - boundAfter.usage.rows, 64);
    assert.equal(boundBefore.usage.bytes - boundAfter.usage.bytes, LANE_BYTES);

    // Bound +1: a 65th row, and a 64th row that takes the lane one byte past
    // LANE_BYTES, are refused by enqueue with capacity_exceeded. Rows and usage stay.
    await page.evaluate(() => fillSmallLane(64));
    const rowCapBefore = await page.evaluate(() => snapshot());
    assert.equal(rowCapBefore.rows.length, 64);
    assert.deepEqual(
      await page.evaluate(() =>
        settle(
          store.enqueue({
            ...input('0', { after: smallRows.at(-1).sequence }),
            key: keyFor(64),
          }),
        ),
      ),
      { error: 'capacity_exceeded' },
    );
    assert.deepEqual(await page.evaluate(() => snapshot()), rowCapBefore);
    await page.evaluate(() => fillBoundPrefix());
    const byteCapBefore = await page.evaluate(() => snapshot());
    assert.equal(byteCapBefore.rows.length, 63);
    assert.deepEqual(
      await page.evaluate(() => settle(store.enqueue(closingInput(1)))),
      { error: 'capacity_exceeded' },
    );
    assert.deepEqual(await page.evaluate(() => snapshot()), byteCapBefore);

    // A lane above the cap, written through IndexedDB: 64 stored rows plus a raw 65th
    // intent. The counters stay as the store wrote them, so the global usage check
    // (intent count against the checkpoints) refuses the store before any delete.
    // ADR 0049 rule 7 requires the strict decoder to refuse a lane above the caps
    // without partial deletion but does not name the code; storage_corrupt is
    // asserted here and is flagged as unspecified by the ADR.
    await page.evaluate(async () => {
      await fillBoundPrefix();
      globalThis.closingRow = await store.enqueue(closingInput(0));
      globalThis.preparedHead = await store.prepare(
        boundPrefix[0],
        authorize,
        signal,
      );
      globalThis.selection = await selectLane();
      await rawEdit([
        {
          store: 'intents',
          put: {
            ...closingRow,
            sequence: closingRow.sequence + 1,
            key: keyFor(64),
            base: { after: closingRow.sequence },
          },
        },
      ]);
    });
    const aboveCap = await page.evaluate(() => snapshot());
    assert.equal(aboveCap.rows.length, 65);
    assert.deepEqual(
      await page.evaluate(() =>
        settle(disposeRejected(selection, preparedHead)),
      ),
      { error: 'storage_corrupt' },
    );
    assert.deepEqual(await page.evaluate(() => snapshot()), aboveCap);

    // Corruption: a usage counter below the closure, or a checkpoint below the lane,
    // makes the discard refuse with storage_corrupt and delete nothing.
    for (const tamper of [
      { store: 'usage', key: 'global', field: 'rows', value: 2 },
      { store: 'usage', key: 'global', field: 'bytes', value: 0 },
      { store: 'checkpoints', lane: true, field: 'rows', value: 2 },
    ]) {
      await page.evaluate(() => closureFixture());
      await page.evaluate((edit) => rawEdit([edit]), tamper);
      const tampered = await page.evaluate(() => snapshot());
      assert.deepEqual(
        await page.evaluate(() =>
          settle(disposeRejected(selection, preparedHead)),
        ),
        { error: 'storage_corrupt' },
        JSON.stringify(tamper),
      );
      assert.deepEqual(
        await page.evaluate(() => snapshot()),
        tampered,
        JSON.stringify(tamper),
      );
    }

    // LocalStore.discard and discardSnapshot keep refusing a prepared head; only the
    // scheduler's rejected-head operation removes one.
    await page.evaluate(() => closureFixture());
    const preparedClosure = await page.evaluate(() => snapshot());
    assert.deepEqual(
      await page.evaluate(() =>
        settle(store.discard(selection.rows, authorize, signal)),
      ),
      { error: 'invalid_selection' },
    );
    assert.deepEqual(
      await page.evaluate(() =>
        settle(store.discardSnapshot(selection, authorize, signal)),
      ),
      { error: 'invalid_selection' },
    );
    assert.deepEqual(await page.evaluate(() => snapshot()), preparedClosure);

    // Rejections before any delete: an unprepared head, a dispatched copy that
    // differs from the stored head, and a dependent admitted after the selection.
    await page.evaluate(async () => {
      await resetDatabase();
      await enqueueChainPair();
      globalThis.selection = await selectLane();
    });
    const unpreparedBefore = await page.evaluate(() => snapshot());
    await assert.rejects(
      page.evaluate(() => disposeRejected(selection, chainHead)),
      /invalid_selection/,
    );
    assert.deepEqual(await page.evaluate(() => snapshot()), unpreparedBefore);

    await page.evaluate(async () => {
      await resetDatabase();
      await enqueueChainPair();
      globalThis.preparedHead = await store.prepare(
        chainHead,
        authorize,
        signal,
      );
      globalThis.selection = await selectLane();
    });
    const mismatchBefore = await page.evaluate(() => snapshot());
    await assert.rejects(
      page.evaluate(() =>
        disposeRejected(selection, {
          ...preparedHead,
          request: { ...preparedHead.request, body: '{"changed":true}' },
        }),
      ),
      /invalid_selection/,
    );
    assert.deepEqual(await page.evaluate(() => snapshot()), mismatchBefore);

    await page.evaluate(async () => {
      await resetDatabase();
      await enqueueChainPair();
      globalThis.preparedHead = await store.prepare(
        chainHead,
        authorize,
        signal,
      );
      globalThis.selection = await selectLane();
      await store.enqueue(input('4', { after: chainChild.sequence }));
    });
    const staleBefore = await page.evaluate(() => snapshot());
    assert.equal(staleBefore.rows.length, 3);
    await assert.rejects(
      page.evaluate(() => disposeRejected(selection, preparedHead)),
      /invalid_selection/,
    );
    assert.deepEqual(await page.evaluate(() => snapshot()), staleBefore);
  } finally {
    await context.close();
    server.closeAllConnections();
    await new Promise((resolve, reject) =>
      server.close((error) => (error ? reject(error) : resolve())),
    );
  }
  console.log(
    'outbox store: native preparation, ACK/checkpoint, trim, abort/quota/signal/authorize interruptions, lane caps, corruption and rejected-head discard passed',
  );
}
