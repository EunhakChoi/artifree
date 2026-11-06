import assert from 'node:assert/strict';

/** Internal fixed-owner cleanup; no public sender or recovery is enabled. */
export async function checkSchedulerDisposal(harness, selected) {
  if (harness.createArtifact !== undefined) return;
  const modes = [
    'disposal-capacity',
    'disposal-empty',
    'disposal-abort',
    'disposal-stale',
    'disposal-anchors',
    'disposal-close-abort',
    'disposal-auth-abort',
    'disposal-close-commit',
    'disposal-auth-commit',
    'disposal-budget-discard',
    'disposal-budget-empty',
    'disposal-phase-prepared',
    'disposal-phase-ack',
    'disposal-tokens',
    'retirement-close-commit',
    'retirement-auth-commit',
    'retirement-abort',
    'retirement-admission',
    'retirement-wake',
    'retirement-stale-row',
    'retirement-usage-fault',
    'cleanup-online',
    'cleanup-503',
    'cleanup-401',
    'cleanup-offline-revoked',
    'cleanup-lock-lost',
    'cleanup-host-error',
    'disposal-ineligible-healthy',
    'disposal-ineligible-blocked',
    'disposal-ineligible-retry',
    'disposal-epoch',
    'retirement-epoch',
    'admission-close-abort',
    'admission-close-commit',
    'admission-auth-abort',
    'admission-auth-commit',
    'disposal-authority-check',
  ];
  for (const mode of modes.filter(
    (value) =>
      selected === undefined ||
      selected === 'disposal-all' ||
      selected === value,
  )) {
    harness.configure('normal');
    harness.configureBinding('normal');
    const page = await harness.setup();
    await page.evaluate(() => {
      globalThis.localGate = new deliveryApi.LocalOperationGate();
      globalThis.cleanupIdentity = {};
      globalThis.cleanupBindingReads = 0;
      globalThis.cleanupObservedStatuses = [];
      globalThis.cleanupUnhandled = 0;
      addEventListener('unhandledrejection', (event) => {
        event.preventDefault();
        cleanupUnhandled += 1;
      });
      globalThis.cleanupStorageSnapshot = async () => {
        const opening = indexedDB.open('artifree-data-outbox');
        const db = await new Promise((resolve, reject) => {
          opening.onsuccess = () => resolve(opening.result);
          opening.onerror = () => reject(opening.error);
        });
        try {
          return await new Promise((resolve, reject) => {
            const tx = db.transaction([
              'intents',
              'receipts',
              'checkpoints',
              'usage',
            ]);
            const rows = tx.objectStore('intents').getAll();
            const receipts = tx.objectStore('receipts').getAll();
            const checkpoints = tx.objectStore('checkpoints').getAll();
            const usage = tx.objectStore('usage').get('global');
            tx.oncomplete = () =>
              resolve({
                rows: rows.result,
                receipts: receipts.result,
                checkpoints: checkpoints.result,
                usage: usage.result,
              });
            tx.onabort = () => reject(tx.error);
          });
        } finally {
          db.close();
        }
      };
      const proof = Object.freeze({
        identity: cleanupIdentity,
        signal: stop.signal,
        check() {
          authorize();
          globalThis.onCleanupProofCheck?.();
          if (!locallyAuthenticated)
            throw new deliveryApi.OutboxError('owner_not_ready');
        },
      });
      const enqueue = enqueueLikeHead;
      globalThis.enqueueLikeHead = (changes) =>
        localGate.run(async () => {
          proof.check();
          globalThis.onAdmissionStart?.(changes);
          return enqueue(changes, {
            check: () => proof.check(),
            signal: localGate.signal,
          });
        });
      installScheduler({
        gate: localGate,
        async authorize(mode, signal) {
          if (globalThis.cleanupAuthorityFailure !== undefined)
            throw cleanupAuthorityFailure;
          proof.check();
          if (mode === 'online') {
            cleanupBindingReads += 1;
            let current;
            try {
              current = await deliveryApi.readBinding(
                location.origin,
                deliveryContext.accessToken,
                signal,
                (status) => {
                  cleanupObservedStatuses.push(status);
                  if (status === 401)
                    deliveryContext.invalidateAuthentication();
                },
              );
            } catch (error) {
              if (error instanceof deliveryApi.ArtifreeApiError)
                globalThis.cleanupRawReadDiagnostic = {
                  code: error.code,
                  status: error.status,
                  retryAfterSeconds: error.retryAfterSeconds,
                };
              throw error;
            }
            if (JSON.stringify(current) !== JSON.stringify(auth)) {
              currentAuthority = Symbol();
              stop.abort();
              throw new deliveryApi.OutboxError('owner_not_ready');
            }
          }
          proof.check();
          return proof;
        },
      });
      globalThis.cleanupLanes = [[...lane]];
    });
    if (!['disposal-capacity', 'disposal-empty'].includes(mode)) {
      await checkDisposalFault(harness, page, mode);
      assert.equal(await page.evaluate(() => cleanupUnhandled), 0);
      await harness.close(page);
      console.log(`outbox disposal: ${mode} passed`);
      continue;
    }
    await page.evaluate(async () => {
      for (let index = 1; index < 256; index += 1) {
        const key = [
          ...lane.slice(0, 4),
          'art_' + index.toString(16).padStart(32, '0'),
        ];
        await enqueueLikeHead({
          key: crypto.randomUUID().replaceAll('-', ''),
          lane: key,
          artifactId: key[4],
          base: { version: 0 },
        });
        cleanupLanes.push(key);
      }
      faultPut('intents', 'prepared');
      cleanupLanes.forEach((key) => scheduler.wake(key));
    });
    await page.waitForFunction(
      () => events.filter((event) => event.kind === 'paused').length === 256,
    );
    await page.evaluate(() => {
      restorePut();
    });
    assert.equal(harness.requests().length, 0);
    if (mode === 'disposal-capacity') {
      await page.evaluate(async () => {
        globalThis.cleanupChoice = await scheduler.selectUnsent(
          cleanupLanes[0],
          'offline',
        );
        const discard = store.discardSnapshot.bind(store);
        globalThis.cleanupOperations = 0;
        globalThis.cleanupCommitted = false;
        globalThis.cleanupPublication = new Promise((resolve) => {
          globalThis.releaseCleanupPublication = resolve;
        });
        store.discardSnapshot = async (...args) => {
          cleanupOperations += 1;
          const result = await discard(...args);
          cleanupCommitted = true;
          await cleanupPublication;
          return result;
        };
        globalThis.cleanupPending = scheduler.discardUnsent(cleanupChoice);
      });
      await page.waitForFunction(() => cleanupCommitted);
      assert.equal(
        await page.evaluate(() =>
          scheduler.lanes.has(JSON.stringify(cleanupLanes[0])),
        ),
        true,
        'committed cleanup still holds its lane until retirement publication',
      );
      assert.equal(
        await page.evaluate(() => {
          const duplicate = scheduler.discardUnsent(cleanupChoice);
          duplicate.catch(() => undefined);
          return duplicate === cleanupPending;
        }),
        true,
      );
      assert.deepEqual(
        await page.evaluate(() => {
          const calls = [
            'retryPreparation',
            'retryCleanup',
            'resumeDelivery',
            'retireEmptyPreparationGate',
          ].map((method) => scheduler[method](cleanupChoice));
          globalThis.cleanupWrongKinds = Promise.all(
            calls.map((call) =>
              call.then(
                () => 'unexpected',
                (error) => error.code,
              ),
            ),
          );
          return calls.map((call) => call === cleanupPending);
        }),
        Array(4).fill(false),
        'wrong-kind rejection must not wait on the original publication',
      );
      assert.deepEqual(
        await page.evaluate(() => cleanupWrongKinds),
        Array(4).fill('invalid_selection'),
      );
      await page.evaluate(() => {
        globalThis.newCleanupLane = [
          ...lane.slice(0, 4),
          'art_' + '9'.repeat(32),
        ];
        globalThis.admissionEntered = false;
        globalThis.onAdmissionStart = () => {
          admissionEntered = true;
        };
        globalThis.cleanupAdmission = enqueueLikeHead({
          key: crypto.randomUUID().replaceAll('-', ''),
          lane: newCleanupLane,
          artifactId: newCleanupLane[4],
          base: { version: 0 },
        });
      });
      assert.equal(
        await page.evaluate(() => admissionEntered),
        false,
        'admission remains behind committed cleanup publication',
      );
      await page.evaluate(() => releaseCleanupPublication());
      assert.deepEqual(await page.evaluate(() => cleanupPending), {
        kind: 'discarded_unsent',
      });
      assert.equal(
        await page.evaluate(() =>
          scheduler.lanes.has(JSON.stringify(cleanupLanes[0])),
        ),
        false,
      );
      await page.evaluate(async () => {
        await cleanupAdmission;
        scheduler.wake(newCleanupLane);
      });
      await page.waitForFunction(() =>
        events.some(
          (event) =>
            event.kind === 'attempt' &&
            event.result.kind === 'server_acknowledged',
        ),
      );
      assert.equal(await page.evaluate(() => cleanupOperations), 1);
      assert.equal(harness.requests().length, 1);
      assert.equal(
        (await page.evaluate(() => storageSnapshot())).rows.length,
        255,
      );
      assert.equal(
        await page.evaluate(() =>
          scheduler.discardUnsent(cleanupChoice).then(
            () => 'unexpected',
            (error) => error.code,
          ),
        ),
        'invalid_selection',
      );
      await page.evaluate(async () => {
        for (const key of cleanupLanes.slice(1)) {
          const choice = await scheduler.selectUnsent(key, 'offline');
          await scheduler.discardUnsent(choice);
        }
      });
      assert.deepEqual((await page.evaluate(() => storageSnapshot())).rows, []);
    } else {
      await page.evaluate(async () => {
        for (const key of cleanupLanes)
          await localGate.run(async () =>
            store.discard(await store.lane(key), authorize, stop.signal),
          );
        globalThis.newCleanupLane = [
          ...lane.slice(0, 4),
          'art_' + '9'.repeat(32),
        ];
        await enqueueLikeHead({
          key: crypto.randomUUID().replaceAll('-', ''),
          lane: newCleanupLane,
          artifactId: newCleanupLane[4],
          base: { version: 0 },
        });
        scheduler.wake(newCleanupLane);
      });
      await page.waitForFunction(() =>
        events.some((event) => event.kind === 'reconciled'),
      );
      assert.equal(
        harness.requests().length,
        0,
        'low-level deletion does not retire retained preparation gates',
      );
      assert.deepEqual(
        await page.evaluate(() =>
          events.findLast((event) => event.kind === 'reconciled'),
        ),
        {
          kind: 'reconciled',
          queued: 0,
          pending: true,
        },
      );
      await page.evaluate(async () => {
        globalThis.emptyChoice = await scheduler.selectEmptyPreparationGate(
          cleanupLanes[0],
          'offline',
        );
        const native = store.checkEmptyCleanup.bind(store);
        globalThis.emptyInNative = false;
        globalThis.emptyCommitted = false;
        globalThis.emptyDuringTx = [];
        globalThis.emptyWrongKinds = [];
        globalThis.emptyWrongIdentity = [];
        globalThis.emptyPublication = new Promise((resolve) => {
          globalThis.releaseEmptyPublication = resolve;
        });
        globalThis.onCleanupProofCheck = () => {
          if (!emptyInNative) return;
          const duplicate = scheduler.retireEmptyPreparationGate(emptyChoice);
          duplicate.catch(() => undefined);
          emptyDuringTx.push(duplicate === emptyPending);
          const wrong = scheduler.discardUnsent(emptyChoice);
          emptyWrongIdentity.push(wrong === emptyPending);
          emptyWrongKinds.push(
            wrong.then(
              () => 'unexpected',
              (error) => error.code,
            ),
          );
        };
        store.checkEmptyCleanup = async (...args) => {
          emptyInNative = true;
          try {
            await native(...args);
          } finally {
            emptyInNative = false;
          }
          emptyCommitted = true;
          await emptyPublication;
        };
        globalThis.emptyPending =
          scheduler.retireEmptyPreparationGate(emptyChoice);
        const duplicate = scheduler.retireEmptyPreparationGate(emptyChoice);
        duplicate.catch(() => undefined);
        globalThis.sameEmptyPromise = duplicate === emptyPending;
      });
      assert.equal(await page.evaluate(() => sameEmptyPromise), true);
      await page.waitForFunction(() => emptyCommitted);
      assert.equal(
        await page.evaluate(
          () => emptyDuringTx.length > 0 && emptyDuringTx.every(Boolean),
        ),
        true,
      );
      assert.equal(
        await page.evaluate(() => emptyWrongIdentity.some(Boolean)),
        false,
      );
      assert.equal(
        await page.evaluate(async () =>
          (await Promise.all(emptyWrongKinds)).every(
            (code) => code === 'invalid_selection',
          ),
        ),
        true,
      );
      assert.equal(
        await page.evaluate(() =>
          scheduler.lanes.has(JSON.stringify(cleanupLanes[0])),
        ),
        true,
      );
      assert.equal(
        await page.evaluate(() => {
          const duplicate = scheduler.retireEmptyPreparationGate(emptyChoice);
          duplicate.catch(() => undefined);
          return duplicate === emptyPending;
        }),
        true,
      );
      await page.evaluate(() => releaseEmptyPublication());
      assert.deepEqual(await page.evaluate(() => emptyPending), {
        kind: 'retired_empty_preparation_gate',
      });
      assert.equal(
        await page.evaluate(() =>
          scheduler.lanes.has(JSON.stringify(cleanupLanes[0])),
        ),
        false,
      );
      await page.waitForFunction(() =>
        events.some(
          (event) =>
            event.kind === 'attempt' &&
            event.result.kind === 'server_acknowledged',
        ),
      );
      assert.equal(harness.requests().length, 1);
      assert.equal(
        await page.evaluate(() =>
          scheduler.retireEmptyPreparationGate(emptyChoice).then(
            () => 'unexpected',
            (error) => error.code,
          ),
        ),
        'invalid_selection',
      );
      assert.deepEqual((await page.evaluate(() => storageSnapshot())).rows, []);
    }
    assert.equal(await page.evaluate(() => cleanupBindingReads), 0);
    assert.equal(await page.evaluate(() => revalidationCalls), 0);
    assert.equal(await page.evaluate(() => scheduler.epoch), 0);
    assert.equal(await page.evaluate(() => scheduler.aggregateFailures), 0);
    assert.equal(await page.evaluate(() => cleanupUnhandled), 0);
    await harness.close(page);
    console.log(`outbox disposal: ${mode} passed`);
  }
}

async function checkDisposalFault(harness, page, mode) {
  await page.evaluate(() => {
    faultPut('intents', 'prepared');
    scheduler.wake(lane);
  });
  await page.waitForFunction(() =>
    events.some((event) => event.kind === 'paused'),
  );
  await page.evaluate(() => {
    restorePut();
    globalThis.originalCleanupLane = scheduler.lanes.get(JSON.stringify(lane));
    globalThis.originalPreparationChoice = events.find(
      (event) => event.kind === 'paused',
    ).pause.selection;
  });
  if (mode === 'disposal-authority-check') {
    const before = await page.evaluate(() => cleanupStorageSnapshot());
    await page.evaluate(async () => {
      globalThis.faultChoice = await scheduler.selectUnsent(lane, 'offline');
      globalThis.authorityDeleteSuccesses = 0;
      const deletion = IDBObjectStore.prototype.delete;
      globalThis.restoreAuthorityDelete = () => {
        IDBObjectStore.prototype.delete = deletion;
      };
      IDBObjectStore.prototype.delete = function (...args) {
        const pending = deletion.apply(this, args);
        if (this.name === 'intents')
          pending.addEventListener(
            'success',
            () => {
              authorityDeleteSuccesses += 1;
              // Deliberately delay host abort notification to isolate the native
              // transaction's mandatory current-authority check from its signal.
              currentAuthority = Symbol();
            },
            { once: true },
          );
        return pending;
      };
      globalThis.faultOutcome = scheduler.discardUnsent(faultChoice).then(
        () => 'committed',
        (error) => error.code,
      );
    });
    assert.equal(
      await page.evaluate(() => faultOutcome),
      'owner_not_ready',
      'transaction-local authority loss aborts before commit',
    );
    await page.evaluate(() => restoreAuthorityDelete());
    assert.equal(await page.evaluate(() => authorityDeleteSuccesses), 1);
    assert.deepEqual(
      await page.evaluate(() => cleanupStorageSnapshot()),
      before,
    );
    assert.equal(
      await page.evaluate(
        () => scheduler.lanes.get(JSON.stringify(lane)) === originalCleanupLane,
      ),
      true,
    );
    assert.equal(harness.requests().length, 0);
    return;
  } else if (mode.startsWith('admission-')) {
    await page.evaluate(async (mode) => {
      globalThis.admissionBefore = await cleanupStorageSnapshot();
      globalThis.admissionAborts = 0;
      globalThis.admissionCompletes = 0;
      globalThis.admissionAbortAttempts = 0;
      globalThis.admissionAbortAccepted = 0;
      globalThis.admissionAbortTooLate = 0;
      const transact = IDBDatabase.prototype.transaction;
      const abort = IDBTransaction.prototype.abort;
      const add = IDBObjectStore.prototype.add;
      let owned;
      let closed = false;
      const close = () => {
        if (closed) return;
        closed = true;
        globalThis.queuedAdmissionOutcome = enqueueLikeHead({
          key: crypto.randomUUID().replaceAll('-', ''),
          lane: [...lane.slice(0, 4), 'art_' + 'f'.repeat(32)],
          artifactId: 'art_' + 'f'.repeat(32),
          base: { version: 0 },
        }).then(
          () => 'unexpected',
          (error) => error.code,
        );
        if (mode.includes('-auth-')) deliveryContext.invalidateAuthentication();
        else scheduler.close();
      };
      IDBDatabase.prototype.transaction = function (...args) {
        const tx = transact.apply(this, args);
        if (args[1] === 'readwrite') {
          owned = tx;
          tx.addEventListener('abort', () => {
            admissionAborts += 1;
          });
          tx.addEventListener('complete', () => {
            admissionCompletes += 1;
            if (mode.endsWith('-commit')) close();
          });
        }
        return tx;
      };
      IDBTransaction.prototype.abort = function (...args) {
        if (this === owned) admissionAbortAttempts += 1;
        try {
          const value = abort.apply(this, args);
          if (this === owned) admissionAbortAccepted += 1;
          return value;
        } catch (error) {
          if (this === owned) admissionAbortTooLate += 1;
          throw error;
        }
      };
      IDBObjectStore.prototype.add = function (...args) {
        const pending = add.apply(this, args);
        if (this.name === 'intents' && mode.endsWith('-abort'))
          pending.addEventListener('success', close, { once: true });
        return pending;
      };
      globalThis.restoreAdmissionBoundary = () => {
        IDBDatabase.prototype.transaction = transact;
        IDBTransaction.prototype.abort = abort;
        IDBObjectStore.prototype.add = add;
      };
      globalThis.admissionOutcome = enqueueLikeHead({
        key: crypto.randomUUID().replaceAll('-', ''),
        lane: [...lane.slice(0, 4), 'art_' + 'e'.repeat(32)],
        artifactId: 'art_' + 'e'.repeat(32),
        base: { version: 0 },
      }).then(
        (row) => ({ row }),
        (error) => ({ code: error.code }),
      );
    }, mode);
    const outcome = await page.evaluate(() => admissionOutcome);
    await page.evaluate(() => restoreAdmissionBoundary());
    const before = await page.evaluate(() => admissionBefore);
    const after = await page.evaluate(() => cleanupStorageSnapshot());
    assert.equal(await page.evaluate(() => localGate.signal.aborted), true);
    assert.equal(
      await page.evaluate(() => queuedAdmissionOutcome),
      'owner_not_ready',
    );
    assert.equal(await page.evaluate(() => admissionAbortAttempts > 0), true);
    if (mode.endsWith('-commit')) {
      assert.notEqual(
        outcome.row,
        undefined,
        'too-late abort cannot relabel committed durable admission',
      );
      assert.equal(await page.evaluate(() => admissionCompletes), 1);
      assert.equal(await page.evaluate(() => admissionAborts), 0);
      assert.equal(await page.evaluate(() => admissionAbortTooLate), 1);
      assert.deepEqual(after.rows, [...before.rows, outcome.row]);
      assert.equal(after.usage.rows, before.usage.rows + 1);
      assert.equal(after.usage.bytes, before.usage.bytes + outcome.row.bytes);
    } else {
      assert.deepEqual(outcome, { code: 'owner_not_ready' });
      assert.equal(await page.evaluate(() => admissionAbortAccepted > 0), true);
      assert.equal(await page.evaluate(() => admissionAborts), 1);
      assert.equal(await page.evaluate(() => admissionCompletes), 0);
      assert.deepEqual(after, before);
    }
    assert.equal(harness.requests().length, 0);
    assert.equal(await page.evaluate(() => cleanupBindingReads), 0);
    assert.equal(await page.evaluate(() => scheduler.epoch), 0);
    assert.equal(await page.evaluate(() => scheduler.aggregateFailures), 0);
    return;
  } else if (mode === 'disposal-epoch' || mode === 'retirement-epoch') {
    await page.evaluate(async () => {
      globalThis.epochOther = [...lane.slice(0, 4), 'art_' + 'c'.repeat(32)];
      await enqueueLikeHead({
        key: crypto.randomUUID().replaceAll('-', ''),
        lane: epochOther,
        artifactId: epochOther[4],
        base: { version: 0 },
      });
      faultPut('intents', 'acknowledged');
      scheduler.wake(epochOther);
    });
    await page.waitForFunction(
      () => events.filter((event) => event.kind === 'paused').length === 2,
    );
    await page.evaluate(async (mode) => {
      restorePut();
      if (mode === 'retirement-epoch') {
        await localGate.run(async () =>
          store.discard(await store.lane(lane), authorize, stop.signal),
        );
        globalThis.epochChoice = await scheduler.selectEmptyPreparationGate(
          lane,
          'offline',
        );
      } else
        globalThis.epochChoice = await scheduler.selectUnsent(lane, 'offline');
      const otherChoice = events.findLast(
        (event) => event.kind === 'paused' && event.lane[4] === epochOther[4],
      ).pause.selection;
      await scheduler.resumeDelivery(otherChoice);
    }, mode);
    await page.waitForFunction(() =>
      events.some(
        (event) =>
          event.kind === 'attempt' &&
          event.result.kind === 'server_acknowledged',
      ),
    );
    const before = await page.evaluate(() => cleanupStorageSnapshot());
    assert.equal(await page.evaluate(() => scheduler.epoch), 1);
    assert.equal(
      await page.evaluate(
        (mode) =>
          scheduler[
            mode === 'retirement-epoch'
              ? 'retireEmptyPreparationGate'
              : 'discardUnsent'
          ](epochChoice).then(
            () => 'unexpected',
            (error) => error.code,
          ),
        mode,
      ),
      'invalid_selection',
      'local cleanup choices cannot cross an explicitly renewed delivery epoch',
    );
    assert.deepEqual(
      await page.evaluate(() => cleanupStorageSnapshot()),
      before,
    );
    assert.equal(
      await page.evaluate(
        () => scheduler.lanes.get(JSON.stringify(lane)) === originalCleanupLane,
      ),
      true,
    );
    await page.evaluate(async (mode) => {
      if (mode === 'retirement-epoch') {
        const fresh = await scheduler.selectEmptyPreparationGate(
          lane,
          'offline',
        );
        await scheduler.retireEmptyPreparationGate(fresh);
      } else {
        const fresh = await scheduler.selectUnsent(lane, 'offline');
        await scheduler.discardUnsent(fresh);
      }
    }, mode);
    assert.equal(harness.requests().length, 2);
    assert.equal(await page.evaluate(() => revalidationCalls), 1);
    assert.equal(await page.evaluate(() => cleanupBindingReads), 0);
    assert.equal(await page.evaluate(() => scheduler.epoch), 1);
    assert.equal(await page.evaluate(() => scheduler.aggregateFailures), 0);
    return;
  } else if (
    mode === 'retirement-stale-row' ||
    mode === 'retirement-usage-fault'
  ) {
    await page.evaluate(async (mode) => {
      await localGate.run(async () =>
        store.discard(await store.lane(lane), authorize, stop.signal),
      );
      globalThis.faultChoice = await scheduler.selectEmptyPreparationGate(
        lane,
        'offline',
      );
      globalThis.usageBeforeFault = (await cleanupStorageSnapshot()).usage;
      if (mode === 'retirement-stale-row')
        await enqueueLikeHead({
          key: crypto.randomUUID().replaceAll('-', ''),
          base: { version: 0 },
        });
      else {
        const opening = indexedDB.open('artifree-data-outbox');
        const db = await new Promise((resolve, reject) => {
          opening.onsuccess = () => resolve(opening.result);
          opening.onerror = () => reject(opening.error);
        });
        try {
          await new Promise((resolve, reject) => {
            const tx = db.transaction('usage', 'readwrite');
            tx.objectStore('usage').put({ ...usageBeforeFault, rows: 257 });
            tx.oncomplete = resolve;
            tx.onabort = () => reject(tx.error);
          });
        } finally {
          db.close();
        }
      }
    }, mode);
    const before = await page.evaluate(() => cleanupStorageSnapshot());
    if (mode === 'retirement-stale-row') {
      assert.equal(before.rows.length, 1);
      assert.deepEqual(
        before.checkpoints,
        [
          {
            format: 2,
            lane: before.rows[0].lane,
            owner: before.rows[0].owner,
            generation: before.rows[0].authorizedGeneration,
            predecessor: null,
            rows: 1,
            bytes: before.rows[0].bytes,
          },
        ],
        'a valid retained checkpoint belongs to the newly admitted intent',
      );
      assert.equal(before.usage.rows, 1);
      assert.equal(before.usage.bytes, before.rows[0].bytes);
    }
    assert.equal(
      await page.evaluate(() =>
        scheduler.retireEmptyPreparationGate(faultChoice).then(
          () => 'unexpected',
          (error) => error.code,
        ),
      ),
      mode === 'retirement-stale-row' ? 'invalid_selection' : 'storage_corrupt',
      'valid retained work rejects the empty selection; corrupt accounting stays a storage error',
    );
    assert.deepEqual(
      await page.evaluate(() => cleanupStorageSnapshot()),
      before,
    );
    assert.equal(
      await page.evaluate(
        () => scheduler.lanes.get(JSON.stringify(lane)) === originalCleanupLane,
      ),
      true,
    );
    if (mode === 'retirement-stale-row') {
      await page.evaluate(async () => {
        const deliberateDiscard = await scheduler.selectUnsent(lane, 'offline');
        globalThis.freshDifferentResult =
          await scheduler.discardUnsent(deliberateDiscard);
      });
      assert.deepEqual(await page.evaluate(() => freshDifferentResult), {
        kind: 'discarded_unsent',
      });
    }
  } else if (mode.startsWith('disposal-ineligible-')) {
    if (!mode.endsWith('-healthy'))
      harness.configure('status', mode.endsWith('-blocked') ? 409 : 503);
    await page.evaluate(async (mode) => {
      globalThis.ineligibleLane = [
        ...lane.slice(0, 4),
        'art_' + 'd'.repeat(32),
      ];
      await enqueueLikeHead({
        key: crypto.randomUUID().replaceAll('-', ''),
        lane: ineligibleLane,
        artifactId: ineligibleLane[4],
        base: { version: 0 },
      });
      if (!mode.endsWith('-healthy')) scheduler.wake(ineligibleLane);
    }, mode);
    if (!mode.endsWith('-healthy'))
      await page.waitForFunction(() =>
        events.some(
          (event) =>
            event.lane?.[4] === ineligibleLane[4] &&
            (event.kind === 'paused' || event.kind === 'delivery_retrying'),
        ),
      );
    const before = await page.evaluate(() => cleanupStorageSnapshot());
    const epoch = await page.evaluate(() => ({
      epoch: scheduler.epoch,
      aggregate: scheduler.aggregateFailures,
      slots: scheduler.lanes.size,
    }));
    for (const method of ['selectUnsent', 'selectEmptyPreparationGate'])
      assert.equal(
        await page.evaluate(
          (method) =>
            scheduler[method](ineligibleLane, 'offline').then(
              () => 'unexpected',
              (error) => error.code,
            ),
          method,
        ),
        'invalid_selection',
        'the narrow slice cannot mint cleanup authority for an unsupported head',
      );
    assert.deepEqual(
      await page.evaluate(() => cleanupStorageSnapshot()),
      before,
    );
    assert.deepEqual(
      await page.evaluate(() => ({
        epoch: scheduler.epoch,
        aggregate: scheduler.aggregateFailures,
        slots: scheduler.lanes.size,
      })),
      epoch,
    );
    assert.equal(harness.requests().length, mode.endsWith('-healthy') ? 0 : 1);
    assert.equal(await page.evaluate(() => cleanupBindingReads), 0);
    return;
  } else if (mode.startsWith('cleanup-')) {
    const online =
      mode === 'cleanup-online' ||
      mode === 'cleanup-503' ||
      mode === 'cleanup-401';
    await page.evaluate(async (online) => {
      globalThis.authorityChoice = await scheduler.selectUnsent(
        lane,
        online ? 'online' : 'offline',
      );
    }, online);
    const before = await page.evaluate(() => cleanupStorageSnapshot());
    if (mode === 'cleanup-503' || mode === 'cleanup-401')
      harness.configureBinding(mode === 'cleanup-401' ? '401stalled' : '503');
    await page.evaluate(async (mode) => {
      if (mode === 'cleanup-offline-revoked')
        deliveryContext.invalidateAuthentication();
      if (mode === 'cleanup-lock-lost') {
        releaseLock();
        await lockLife;
      }
      if (mode === 'cleanup-host-error')
        globalThis.cleanupAuthorityFailure = new Error(
          'private-cleanup-authority-message',
          { cause: new Error('private-cleanup-authority-cause') },
        );
      globalThis.authorityOutcome = scheduler
        .discardUnsent(authorityChoice)
        .then(
          (value) => ({ kind: value.kind }),
          (error) => ({
            code: error.code,
            status: error.status,
            retryAfterSeconds: error.retryAfterSeconds,
            message: error.message,
            body: error.body,
            cause: error.cause,
          }),
        );
    }, mode);
    if (mode === 'cleanup-401') {
      await page.waitForFunction(() => cleanupObservedStatuses.includes(401));
      assert.equal(
        await page.evaluate(() => locallyAuthenticated),
        false,
        '401 headers invalidate offline cleanup before body completion',
      );
    }
    const result = await page.evaluate(() => authorityOutcome);
    if (mode === 'cleanup-online') {
      assert.deepEqual(result, { kind: 'discarded_unsent' });
      assert.deepEqual(
        (await page.evaluate(() => cleanupStorageSnapshot())).rows,
        [],
      );
      assert.equal(await page.evaluate(() => cleanupBindingReads), 2);
    } else {
      if (mode === 'cleanup-503' || mode === 'cleanup-401') {
        const observed = await page.evaluate(() => cleanupRawReadDiagnostic);
        assert.equal(
          result.code,
          observed.code,
          'retain the actual bounded read diagnostic after headers/body failure',
        );
        assert.equal(result.status, mode === 'cleanup-401' ? 401 : 503);
        assert.equal(result.retryAfterSeconds, 3600);
        if (mode === 'cleanup-503') assert.equal(result.code, 'unavailable');
      } else assert.equal(result.code, 'owner_not_ready');
      assert.equal(result.body, undefined);
      assert.equal(result.cause, undefined);
      assert.equal((result.message ?? '').includes('private'), false);
      assert.deepEqual(
        await page.evaluate(() => cleanupStorageSnapshot()),
        before,
      );
      assert.equal(
        await page.evaluate(() =>
          scheduler.discardUnsent(authorityChoice).then(
            () => 'unexpected',
            (error) => error.code,
          ),
        ),
        'invalid_selection',
      );
      if (mode === 'cleanup-503') {
        assert.equal(harness.bindingReadCount(), 1);
        assert.equal(await page.evaluate(() => cleanupBindingReads), 2);
        await page.evaluate(async () => {
          const explicitOffline = await scheduler.selectUnsent(lane, 'offline');
          globalThis.explicitOfflineResult =
            await scheduler.discardUnsent(explicitOffline);
        });
        assert.deepEqual(await page.evaluate(() => explicitOfflineResult), {
          kind: 'discarded_unsent',
        });
        assert.equal(
          harness.bindingReadCount(),
          1,
          'explicit offline makes no fallback binding read',
        );
      } else if (mode !== 'cleanup-host-error') {
        assert.equal(
          await page.evaluate(() =>
            scheduler.selectUnsent(lane, 'offline').then(
              () => 'unexpected',
              (error) => error.code,
            ),
          ),
          'owner_not_ready',
        );
      }
    }
    assert.equal(harness.requests().length, 0);
    assert.equal(await page.evaluate(() => revalidationCalls), 0);
    assert.equal(await page.evaluate(() => scheduler.epoch), 0);
    assert.equal(await page.evaluate(() => scheduler.aggregateFailures), 0);
    harness.configureBinding('normal');
    return;
  } else if (mode === 'retirement-admission' || mode === 'retirement-wake') {
    await page.evaluate(async () => {
      await localGate.run(async () =>
        store.discard(await store.lane(lane), authorize, stop.signal),
      );
      globalThis.publicationChoice = await scheduler.selectEmptyPreparationGate(
        lane,
        'offline',
      );
      const native = store.checkEmptyCleanup.bind(store);
      globalThis.probeCommitted = false;
      globalThis.probePublication = new Promise((resolve) => {
        globalThis.releaseProbePublication = resolve;
      });
      store.checkEmptyCleanup = async (...args) => {
        await native(...args);
        probeCommitted = true;
        await probePublication;
      };
      globalThis.publicationPending =
        scheduler.retireEmptyPreparationGate(publicationChoice);
      globalThis.publicationOutcome = publicationPending.then(
        (value) => value.kind,
        (error) => error.code,
      );
    });
    await page.waitForFunction(() => probeCommitted);
    await page.evaluate((mode) => {
      globalThis.publicationAdmissionEntered = false;
      globalThis.onAdmissionStart = () => {
        publicationAdmissionEntered = true;
      };
      globalThis.publicationAdmission = enqueueLikeHead({
        key: crypto.randomUUID().replaceAll('-', ''),
        base: { version: 0 },
        text: '{"new-incarnation":true}',
      });
      if (mode === 'retirement-wake') scheduler.wake(lane);
    }, mode);
    assert.equal(await page.evaluate(() => publicationAdmissionEntered), false);
    await page.evaluate(() => releaseProbePublication());
    assert.equal(
      await page.evaluate(() => publicationOutcome),
      mode === 'retirement-wake'
        ? 'invalid_selection'
        : 'retired_empty_preparation_gate',
    );
    await page.evaluate(async () => {
      await publicationAdmission;
      scheduler.wake(lane);
      globalThis.afterPublicationLane = scheduler.lanes.get(
        JSON.stringify(lane),
      );
    });
    if (mode === 'retirement-wake') {
      assert.equal(
        await page.evaluate(() => afterPublicationLane === originalCleanupLane),
        true,
      );
      assert.equal(harness.requests().length, 0);
      assert.equal(
        await page.evaluate(() =>
          scheduler.retryPreparation(originalPreparationChoice).then(
            () => 'unexpected',
            (error) => error.code,
          ),
        ),
        'invalid_selection',
      );
      await page.evaluate(() =>
        scheduler.retryPreparation(
          events.findLast((event) => event.kind === 'paused').pause.selection,
        ),
      );
    } else {
      assert.equal(
        await page.evaluate(
          () =>
            afterPublicationLane !== undefined &&
            afterPublicationLane !== originalCleanupLane,
        ),
        true,
      );
      assert.equal(
        await page.evaluate(() =>
          scheduler.retryPreparation(originalPreparationChoice).then(
            () => 'unexpected',
            (error) => error.code,
          ),
        ),
        'invalid_selection',
      );
    }
    await page.waitForFunction(() =>
      events.some(
        (event) =>
          event.kind === 'attempt' &&
          event.result.kind === 'server_acknowledged',
      ),
    );
    assert.equal(harness.requests().length, 1);
    assert.deepEqual(
      (await page.evaluate(() => cleanupStorageSnapshot())).rows,
      [],
    );
    assert.equal(await page.evaluate(() => cleanupBindingReads), 0);
    assert.equal(await page.evaluate(() => revalidationCalls), 0);
    return;
  } else if (mode === 'retirement-abort') {
    await page.evaluate(async () => {
      await localGate.run(async () =>
        store.discard(await store.lane(lane), authorize, stop.signal),
      );
      globalThis.faultChoice = await scheduler.selectEmptyPreparationGate(
        lane,
        'offline',
      );
      globalThis.emptyBeforeAbort = await cleanupStorageSnapshot();
      globalThis.emptyProbeAborts = 0;
      globalThis.emptyProbeIdentity = [];
      const transact = IDBDatabase.prototype.transaction;
      globalThis.restoreEmptyAbort = () => {
        IDBDatabase.prototype.transaction = transact;
      };
      IDBDatabase.prototype.transaction = function (...args) {
        const tx = transact.apply(this, args);
        if (args[1] === 'readwrite') {
          tx.addEventListener('abort', () => {
            emptyProbeAborts += 1;
            const duplicate = scheduler.retireEmptyPreparationGate(faultChoice);
            duplicate.catch(() => undefined);
            emptyProbeIdentity.push(duplicate === faultPending);
          });
          tx.abort();
        }
        return tx;
      };
      globalThis.faultPending =
        scheduler.retireEmptyPreparationGate(faultChoice);
      globalThis.faultOutcome = faultPending.then(
        () => 'unexpected',
        (error) => error.code,
      );
    });
    assert.equal(await page.evaluate(() => faultOutcome), 'storage_failed');
    await page.evaluate(() => restoreEmptyAbort());
    assert.equal(await page.evaluate(() => emptyProbeAborts), 1);
    assert.deepEqual(await page.evaluate(() => emptyProbeIdentity), [true]);
    assert.deepEqual(
      await page.evaluate(() => cleanupStorageSnapshot()),
      await page.evaluate(() => emptyBeforeAbort),
    );
    assert.equal(
      await page.evaluate(
        () => scheduler.lanes.get(JSON.stringify(lane)) === originalCleanupLane,
      ),
      true,
    );
    assert.equal(
      await page.evaluate(() =>
        scheduler.retireEmptyPreparationGate(faultChoice).then(
          () => 'unexpected',
          (error) => error.code,
        ),
      ),
      'invalid_selection',
    );
    await page.evaluate(async () => {
      const fresh = await scheduler.selectEmptyPreparationGate(lane, 'offline');
      globalThis.recoveredCleanup =
        await scheduler.retireEmptyPreparationGate(fresh);
    });
    assert.deepEqual(await page.evaluate(() => recoveredCleanup), {
      kind: 'retired_empty_preparation_gate',
    });
  } else if (mode === 'disposal-tokens') {
    await page.evaluate(async () => {
      globalThis.oldUnsentChoice = await scheduler.selectUnsent(
        lane,
        'offline',
      );
      await localGate.run(async () =>
        store.discard(await store.lane(lane), authorize, stop.signal),
      );
      globalThis.faultChoice = await scheduler.selectEmptyPreparationGate(
        lane,
        'offline',
      );
      globalThis.choiceReads = 0;
      const originalCapture = store.captureCleanup.bind(store);
      const originalLane = store.lane.bind(store);
      store.captureCleanup = (...args) => {
        choiceReads += 1;
        return originalCapture(...args);
      };
      store.lane = (...args) => {
        choiceReads += 1;
        return originalLane(...args);
      };
      globalThis.invalidCleanupCalls = [];
      globalThis.choiceGetterCalls = 0;
      const getterChoice = Object.defineProperty(
        {},
        Object.getOwnPropertySymbols(faultChoice)[0],
        {
          get() {
            choiceGetterCalls += 1;
            throw new Error('private-choice-getter');
          },
        },
      );
      const descriptorFault = new Proxy(
        {},
        {
          getOwnPropertyDescriptor() {
            throw new Error('private-choice-descriptor');
          },
        },
      );
      const revoked = Proxy.revocable({}, {});
      revoked.revoke();
      for (const method of ['discardUnsent', 'retireEmptyPreparationGate']) {
        for (const value of [
          undefined,
          null,
          1,
          'choice',
          {},
          { ...faultChoice },
          Object.create(faultChoice),
          getterChoice,
          descriptorFault,
          revoked.proxy,
        ]) {
          try {
            const result = scheduler[method](value);
            invalidCleanupCalls.push({
              synchronous: false,
              code: await result.then(
                () => 'unexpected',
                (error) => error.code,
              ),
            });
          } catch (error) {
            invalidCleanupCalls.push({ synchronous: true, code: error.code });
          }
        }
      }
    });
    assert.deepEqual(
      await page.evaluate(() => invalidCleanupCalls),
      Array(20).fill({ synchronous: false, code: 'invalid_selection' }),
    );
    assert.equal(await page.evaluate(() => choiceGetterCalls), 0);
    assert.equal(await page.evaluate(() => choiceReads), 0);
    assert.equal(
      await page.evaluate(() =>
        scheduler.discardUnsent(oldUnsentChoice).then(
          () => 'unexpected',
          (error) => error.code,
        ),
      ),
      'invalid_selection',
    );
    await page.evaluate(async () => {
      await scheduler.retireEmptyPreparationGate(faultChoice);
      await enqueueLikeHead({
        key: crypto.randomUUID().replaceAll('-', ''),
        base: { version: 0 },
      });
      faultPut('intents', 'prepared');
      scheduler.wake(lane);
    });
    await page.waitForFunction(
      () => events.filter((event) => event.kind === 'paused').length === 2,
    );
    await page.evaluate(() => {
      restorePut();
      choiceReads = 0;
    });
    assert.equal(
      await page.evaluate(() =>
        scheduler.retryPreparation(originalPreparationChoice).then(
          () => 'unexpected',
          (error) => error.code,
        ),
      ),
      'invalid_selection',
    );
    assert.equal(
      await page.evaluate(() =>
        scheduler.retireEmptyPreparationGate(faultChoice).then(
          () => 'unexpected',
          (error) => error.code,
        ),
      ),
      'invalid_selection',
    );
    assert.equal(
      await page.evaluate(() => choiceReads),
      0,
      'old incarnation choices never inspect newly admitted work',
    );
    assert.equal(
      await page.evaluate(
        () => scheduler.lanes.get(JSON.stringify(lane)) !== originalCleanupLane,
      ),
      true,
    );
    assert.equal(
      (await page.evaluate(() => cleanupStorageSnapshot())).rows[0].state,
      'unprepared',
    );
  } else if (mode.startsWith('disposal-phase-')) {
    await page.evaluate(async (mode) => {
      globalThis.faultChoice = await scheduler.selectUnsent(lane, 'offline');
      await localGate.run(async () =>
        store.prepare((await store.lane(lane))[0], authorize, stop.signal),
      );
      if (mode.endsWith('-ack')) {
        faultPut('usage');
        globalThis.phaseAttempt = await localGate.run(() => kernel.run(lane));
        restorePut();
      }
    }, mode);
    const before = await page.evaluate(() => cleanupStorageSnapshot());
    assert.equal(
      before.rows[0].state,
      mode.endsWith('-ack') ? 'acknowledged' : 'prepared',
    );
    assert.equal(
      await page.evaluate(() =>
        scheduler.discardUnsent(faultChoice).then(
          () => 'unexpected',
          (error) => error.code,
        ),
      ),
      'invalid_selection',
    );
    assert.equal(
      await page.evaluate(() =>
        scheduler.selectUnsent(lane, 'offline').then(
          () => 'unexpected',
          (error) => error.code,
        ),
      ),
      'invalid_selection',
    );
    assert.deepEqual(
      await page.evaluate(() => cleanupStorageSnapshot()),
      before,
    );
    assert.equal(harness.requests().length, mode.endsWith('-ack') ? 1 : 0);
    assert.equal(await page.evaluate(() => cleanupBindingReads), 0);
    assert.equal(await page.evaluate(() => revalidationCalls), 0);
    return;
  } else if (mode.startsWith('disposal-budget-')) {
    harness.configure('invalid_json');
    await page.evaluate(async () => {
      globalThis.budgetLane = [...lane.slice(0, 4), 'art_' + 'b'.repeat(32)];
      await enqueueLikeHead({
        key: crypto.randomUUID().replaceAll('-', ''),
        lane: budgetLane,
        artifactId: budgetLane[4],
        base: { version: 0 },
      });
      scheduler.wake(budgetLane);
    });
    await page.waitForFunction(
      () =>
        events.some((event) => event.kind === 'delivery_retrying') &&
        timers.some((timer) => timer.active),
    );
    if (mode.endsWith('-empty'))
      await page.evaluate(async () => {
        await localGate.run(async () =>
          store.discard(await store.lane(lane), authorize, stop.signal),
        );
      });
    const before = await page.evaluate(() => cleanupStorageSnapshot());
    const budget = await page.evaluate(() => ({
      epoch: scheduler.epoch,
      aggregate: scheduler.aggregateFailures,
      failures: scheduler.lanes.get(JSON.stringify(budgetLane)).failures,
      timers: timers
        .filter((timer) => timer.active)
        .map((timer) => timer.milliseconds),
    }));
    assert.equal(budget.aggregate, 1);
    await page.evaluate(async (mode) => {
      if (mode.endsWith('-empty')) {
        const choice = await scheduler.selectEmptyPreparationGate(
          lane,
          'offline',
        );
        globalThis.budgetCleanupResult =
          await scheduler.retireEmptyPreparationGate(choice);
      } else {
        const choice = await scheduler.selectUnsent(lane, 'offline');
        globalThis.budgetCleanupResult = await scheduler.discardUnsent(choice);
      }
    }, mode);
    assert.deepEqual(
      await page.evaluate(() => ({
        epoch: scheduler.epoch,
        aggregate: scheduler.aggregateFailures,
        failures: scheduler.lanes.get(JSON.stringify(budgetLane)).failures,
        timers: timers
          .filter((timer) => timer.active)
          .map((timer) => timer.milliseconds),
      })),
      budget,
      'explicit local cleanup preserves the epoch, both budgets and unrelated backoff',
    );
    const after = await page.evaluate(() => cleanupStorageSnapshot());
    assert.deepEqual(
      after.rows,
      before.rows.filter((row) => row.state === 'prepared'),
    );
    if (mode.endsWith('-empty')) assert.deepEqual(after, before);
    assert.equal(after.usage.rows, 1);
    assert.equal(after.usage.bytes, after.rows[0].bytes);
    assert.equal(after.usage.next, before.usage.next);
    assert.equal(harness.requests().length, 1);
    await page.evaluate(() => {
      const timer = timers.find(
        (timer) => timer.active && timer.milliseconds < 15000,
      );
      timer.active = false;
      timer.callback();
    });
    await page.waitForFunction(
      () =>
        events.filter((event) => event.kind === 'delivery_retrying').length ===
        2,
    );
    assert.equal(
      await page.evaluate(
        () =>
          events.findLast((event) => event.kind === 'delivery_retrying')
            .aggregateRemaining,
      ),
      30,
    );
    assert.equal(harness.requests().length, 2);
    assert.equal(
      harness.requests()[0].headers['idempotency-key'],
      harness.requests()[1].headers['idempotency-key'],
    );
    assert.equal(await page.evaluate(() => cleanupBindingReads), 0);
    assert.equal(await page.evaluate(() => revalidationCalls), 0);
    return;
  } else if (
    mode.startsWith('disposal-close-') ||
    mode.startsWith('disposal-auth-') ||
    mode.startsWith('retirement-')
  ) {
    await page.evaluate(async (mode) => {
      if (mode.startsWith('retirement-'))
        await localGate.run(async () =>
          store.discard(await store.lane(lane), authorize, stop.signal),
        );
      globalThis.boundaryOther = [...lane.slice(0, 4), 'art_' + 'e'.repeat(32)];
      await enqueueLikeHead({
        key: crypto.randomUUID().replaceAll('-', ''),
        lane: boundaryOther,
        artifactId: boundaryOther[4],
        base: { version: 0 },
      });
      globalThis.boundaryBefore = await cleanupStorageSnapshot();
      globalThis.boundaryChoice = mode.startsWith('retirement-')
        ? await scheduler.selectEmptyPreparationGate(lane, 'offline')
        : await scheduler.selectUnsent(lane, 'offline');
      globalThis.boundaryAbortAttempts = 0;
      globalThis.boundaryAbortAccepted = 0;
      globalThis.boundaryAbortTooLate = 0;
      globalThis.boundaryAborted = 0;
      globalThis.boundaryCompleted = 0;
      globalThis.boundaryClosed = false;
      const transact = IDBDatabase.prototype.transaction;
      const abort = IDBTransaction.prototype.abort;
      const deletion = IDBObjectStore.prototype.delete;
      let owned;
      const close = () => {
        if (boundaryClosed) return;
        boundaryClosed = true;
        globalThis.boundaryAdmission = enqueueLikeHead({
          key: crypto.randomUUID().replaceAll('-', ''),
          lane: [...lane.slice(0, 4), 'art_' + 'f'.repeat(32)],
          artifactId: 'art_' + 'f'.repeat(32),
          base: { version: 0 },
        }).then(
          () => 'unexpected',
          (error) => error.code,
        );
        scheduler.wake(boundaryOther);
        if (mode.includes('-auth-')) deliveryContext.invalidateAuthentication();
        else scheduler.close();
      };
      IDBTransaction.prototype.abort = function (...args) {
        if (this === owned) boundaryAbortAttempts += 1;
        try {
          const result = abort.apply(this, args);
          if (this === owned) boundaryAbortAccepted += 1;
          return result;
        } catch (error) {
          if (this === owned) boundaryAbortTooLate += 1;
          throw error;
        }
      };
      IDBDatabase.prototype.transaction = function (...args) {
        const tx = transact.apply(this, args);
        if (args[1] === 'readwrite') {
          owned = tx;
          tx.addEventListener('abort', () => {
            boundaryAborted += 1;
          });
          // This owned observer runs before the library's terminal callback:
          // physical commit is complete, but the initiating action is pending.
          tx.addEventListener('complete', () => {
            boundaryCompleted += 1;
            if (mode.endsWith('-commit')) close();
          });
        }
        return tx;
      };
      IDBObjectStore.prototype.delete = function (...args) {
        const pending = deletion.apply(this, args);
        if (this.name === 'intents' && mode.endsWith('-abort'))
          pending.addEventListener('success', close, { once: true });
        return pending;
      };
      globalThis.restoreBoundary = () => {
        IDBDatabase.prototype.transaction = transact;
        IDBTransaction.prototype.abort = abort;
        IDBObjectStore.prototype.delete = deletion;
      };
      const action = mode.startsWith('retirement-')
        ? scheduler.retireEmptyPreparationGate(boundaryChoice)
        : scheduler.discardUnsent(boundaryChoice);
      globalThis.boundaryOutcome = action.then(
        (result) => result.kind,
        (error) => error.code,
      );
    }, mode);
    assert.equal(
      await page.evaluate(() => boundaryOutcome),
      mode.endsWith('-commit') && !mode.startsWith('retirement-')
        ? 'discarded_unsent'
        : 'owner_not_ready',
    );
    await page.evaluate(() => restoreBoundary());
    assert.equal(await page.evaluate(() => boundaryClosed), true);
    assert.equal(
      await page.evaluate(() => boundaryAdmission),
      'owner_not_ready',
      'queued admission is fenced without relabeling the active cleanup result',
    );
    assert.equal(
      await page.evaluate(() => boundaryAbortAttempts > 0),
      true,
      'close/auth must synchronously attempt native abort',
    );
    const after = await page.evaluate(() => cleanupStorageSnapshot());
    const before = await page.evaluate(() => boundaryBefore);
    if (mode.endsWith('-commit')) {
      assert.equal(await page.evaluate(() => boundaryAbortTooLate), 1);
      assert.equal(await page.evaluate(() => boundaryCompleted), 1);
      assert.equal(await page.evaluate(() => boundaryAborted), 0);
      if (mode.startsWith('retirement-')) assert.deepEqual(after, before);
      else assert.deepEqual(after.rows, before.rows.slice(1));
      assert.equal(after.usage.rows, 1);
      assert.equal(after.usage.bytes, after.rows[0].bytes);
    } else {
      assert.equal(await page.evaluate(() => boundaryAbortAccepted > 0), true);
      assert.equal(await page.evaluate(() => boundaryAborted), 1);
      assert.equal(await page.evaluate(() => boundaryCompleted), 0);
      assert.deepEqual(after, before);
    }
    assert.equal(
      await page.evaluate(
        () => scheduler.lanes.get(JSON.stringify(lane)) === originalCleanupLane,
      ),
      true,
      'invalidated context cannot publish metadata retirement',
    );
    assert.equal(
      await page.evaluate(() => {
        try {
          scheduler.wake(boundaryOther);
          return 'unexpected';
        } catch (error) {
          return error.code;
        }
      }),
      'owner_not_ready',
    );
  } else if (mode === 'disposal-abort') {
    const retained = await page.evaluate(() => cleanupStorageSnapshot());
    await page.evaluate(async () => {
      globalThis.faultChoice = await scheduler.selectUnsent(lane, 'offline');
      globalThis.deleteSuccesses = 0;
      globalThis.sameDuringDelete = [];
      globalThis.wrongDuringDelete = [];
      const original = IDBObjectStore.prototype.delete;
      globalThis.restoreCleanupDelete = () => {
        IDBObjectStore.prototype.delete = original;
      };
      IDBObjectStore.prototype.delete = function (...args) {
        const result = original.apply(this, args);
        if (this.name === 'intents')
          result.addEventListener('success', () => {
            deleteSuccesses += 1;
            const duplicate = scheduler.discardUnsent(faultChoice);
            duplicate.catch(() => undefined);
            sameDuringDelete.push(duplicate === faultPending);
            wrongDuringDelete.push(
              scheduler.retireEmptyPreparationGate(faultChoice).then(
                () => 'unexpected',
                (error) => error.code,
              ),
            );
          });
        return result;
      };
      faultPut('usage');
      globalThis.faultPending = scheduler.discardUnsent(faultChoice);
      globalThis.faultOutcome = faultPending.then(
        () => 'unexpected',
        (error) => error.code,
      );
    });
    assert.equal(await page.evaluate(() => faultOutcome), 'storage_failed');
    await page.evaluate(() => {
      restorePut();
      restoreCleanupDelete();
    });
    assert.equal(
      await page.evaluate(() => deleteSuccesses),
      1,
      'native delete request success precedes the aborted transaction',
    );
    assert.deepEqual(await page.evaluate(() => sameDuringDelete), [true]);
    assert.deepEqual(
      await page.evaluate(() => Promise.all(wrongDuringDelete)),
      ['invalid_selection'],
    );
    assert.deepEqual(
      await page.evaluate(() => cleanupStorageSnapshot()),
      retained,
    );
    assert.equal(
      await page.evaluate(
        () => scheduler.lanes.get(JSON.stringify(lane)) === originalCleanupLane,
      ),
      true,
    );
    assert.equal(
      await page.evaluate(() =>
        scheduler.discardUnsent(faultChoice).then(
          () => 'unexpected',
          (error) => error.code,
        ),
      ),
      'invalid_selection',
    );
    await page.evaluate(async () => {
      const fresh = await scheduler.selectUnsent(lane, 'offline');
      globalThis.recoveredCleanup = await scheduler.discardUnsent(fresh);
    });
    assert.deepEqual(await page.evaluate(() => recoveredCleanup), {
      kind: 'discarded_unsent',
    });
    assert.deepEqual(
      (await page.evaluate(() => cleanupStorageSnapshot())).rows,
      [],
    );
  } else if (mode === 'disposal-stale') {
    await page.evaluate(async () => {
      globalThis.faultChoice = await scheduler.selectUnsent(lane, 'offline');
      const dependent = await enqueueLikeHead({
        key: crypto.randomUUID().replaceAll('-', ''),
        base: { after: head.sequence },
      });
      globalThis.independent = await enqueueLikeHead({
        key: crypto.randomUUID().replaceAll('-', ''),
        base: { version: 0 },
        text: '{"independent":1}',
      });
      await enqueueLikeHead({
        key: crypto.randomUUID().replaceAll('-', ''),
        base: { after: independent.sequence },
        text: '{"independent":2}',
      });
      globalThis.headDependent = dependent.sequence;
    });
    const retained = await page.evaluate(() => cleanupStorageSnapshot());
    assert.equal(
      await page.evaluate(() =>
        scheduler.discardUnsent(faultChoice).then(
          () => 'unexpected',
          (error) => error.code,
        ),
      ),
      'invalid_selection',
    );
    assert.deepEqual(
      await page.evaluate(() => cleanupStorageSnapshot()),
      retained,
    );
    await page.evaluate(async () => {
      const fresh = await scheduler.selectUnsent(lane, 'offline');
      globalThis.recoveredCleanup = await scheduler.discardUnsent(fresh);
    });
    assert.deepEqual(await page.evaluate(() => recoveredCleanup), {
      kind: 'discarded_unsent',
    });
    const remaining = await page.evaluate(() => cleanupStorageSnapshot());
    assert.deepEqual(
      remaining.rows,
      retained.rows.slice(2),
      'only the original head and its dependent closure are deleted',
    );
    assert.equal(remaining.usage.rows, 2);
    assert.equal(
      remaining.usage.bytes,
      remaining.rows.reduce((sum, row) => sum + row.bytes, 0),
    );
    assert.equal(
      await page.evaluate(
        () => scheduler.lanes.get(JSON.stringify(lane)) === originalCleanupLane,
      ),
      true,
    );
    assert.equal(
      harness.requests().length,
      0,
      'independent survivors remain paused',
    );
  } else {
    await page.evaluate(async () => {
      await localGate.run(async () =>
        store.discard(await store.lane(lane), authorize, stop.signal),
      );
    });
    for (const anchorStore of ['receipts', 'checkpoints']) {
      // Current checkpoints require rows > 0 and a matching origin ledger;
      // this malformed/orphan record is corruption, never a valid empty anchor.
      // retirement-stale-row separately checks a valid checkpoint + live row.
      const expected =
        anchorStore === 'checkpoints' ? 'storage_corrupt' : 'invalid_selection';
      await page.evaluate(async (anchorStore) => {
        globalThis.anchorChoice = await scheduler.selectEmptyPreparationGate(
          lane,
          'offline',
        );
        const opening = indexedDB.open('artifree-data-outbox');
        const db = await new Promise((resolve, reject) => {
          opening.onsuccess = () => resolve(opening.result);
          opening.onerror = () => reject(opening.error);
        });
        try {
          await new Promise((resolve, reject) => {
            const tx = db.transaction(anchorStore, 'readwrite');
            tx.objectStore(anchorStore).put({
              lane: [...lane],
              owner,
              sentinel: 'owned-test-anchor',
            });
            tx.oncomplete = resolve;
            tx.onabort = () => reject(tx.error);
          });
        } finally {
          db.close();
        }
      }, anchorStore);
      const retained = await page.evaluate(() => cleanupStorageSnapshot());
      assert.equal(
        await page.evaluate(() =>
          scheduler.retireEmptyPreparationGate(anchorChoice).then(
            () => 'unexpected',
            (error) => error.code,
          ),
        ),
        expected,
        `${anchorStore} anchor cannot authorize empty-gate retirement`,
      );
      assert.deepEqual(
        await page.evaluate(() => cleanupStorageSnapshot()),
        retained,
      );
      assert.equal(
        await page.evaluate(
          () =>
            scheduler.lanes.get(JSON.stringify(lane)) === originalCleanupLane,
        ),
        true,
      );
      assert.equal(
        await page.evaluate(() =>
          scheduler.selectEmptyPreparationGate(lane, 'offline').then(
            () => 'unexpected',
            (error) => error.code,
          ),
        ),
        expected,
      );
      await page.evaluate(async (anchorStore) => {
        const opening = indexedDB.open('artifree-data-outbox');
        const db = await new Promise((resolve, reject) => {
          opening.onsuccess = () => resolve(opening.result);
          opening.onerror = () => reject(opening.error);
        });
        try {
          await new Promise((resolve, reject) => {
            const tx = db.transaction(anchorStore, 'readwrite');
            tx.objectStore(anchorStore).delete(lane);
            tx.oncomplete = resolve;
            tx.onabort = () => reject(tx.error);
          });
        } finally {
          db.close();
        }
      }, anchorStore);
    }
  }
  assert.equal(harness.requests().length, 0);
  assert.equal(await page.evaluate(() => cleanupBindingReads), 0);
  assert.equal(await page.evaluate(() => scheduler.epoch), 0);
  assert.equal(await page.evaluate(() => scheduler.aggregateFailures), 0);
}

/** Small real-Rust authority to native-IDB cleanup slice; public delivery stays off. */
export async function checkSchedulerDisposalLive(harness) {
  for (const empty of [false, true]) {
    const artifact = await harness.createArtifact();
    const page = await harness.setup({ artifact });
    const readsBefore = harness.bindingReadCount();
    await page.evaluate(() => {
      globalThis.liveCleanupReads = 0;
      globalThis.liveCleanupReadActors = [];
      globalThis.liveCleanupUnhandled = 0;
      addEventListener('unhandledrejection', (event) => {
        event.preventDefault();
        liveCleanupUnhandled += 1;
      });
      globalThis.liveCleanupGate = new deliveryApi.LocalOperationGate();
      const proof = Object.freeze({
        identity: {},
        signal: stop.signal,
        check() {
          authorize();
          if (!locallyAuthenticated)
            throw new deliveryApi.OutboxError('owner_not_ready');
        },
      });
      installScheduler({
        gate: liveCleanupGate,
        async authorize(mode, signal) {
          proof.check();
          if (mode !== 'online')
            throw new Error('Expected explicit online proof');
          liveCleanupReads += 1;
          const current = await deliveryApi.readBinding(
            location.origin,
            deliveryContext.accessToken,
            signal,
            (status) => {
              if (status === 401) deliveryContext.invalidateAuthentication();
            },
          );
          liveCleanupReadActors.push(current.actor_id);
          if (JSON.stringify(current) !== JSON.stringify(auth)) {
            currentAuthority = Symbol();
            stop.abort();
            throw new deliveryApi.OutboxError('owner_not_ready');
          }
          proof.check();
          return proof;
        },
      });
      faultPut('intents', 'prepared');
      scheduler.wake(lane);
    });
    await page.waitForFunction(() =>
      events.some(
        (event) =>
          event.kind === 'paused' && event.pause.kind === 'preparation_paused',
      ),
    );
    const result = await page.evaluate(async (empty) => {
      restorePut();
      const snapshot = async () => {
        const opening = indexedDB.open('artifree-data-outbox');
        const db = await new Promise((resolve, reject) => {
          opening.onsuccess = () => resolve(opening.result);
          opening.onerror = () => reject(opening.error);
        });
        try {
          return await new Promise((resolve, reject) => {
            const tx = db.transaction([
              'intents',
              'receipts',
              'checkpoints',
              'usage',
            ]);
            const rows = tx.objectStore('intents').getAll();
            const receipts = tx.objectStore('receipts').getAll();
            const checkpoints = tx.objectStore('checkpoints').getAll();
            const usage = tx.objectStore('usage').get('global');
            tx.oncomplete = () =>
              resolve({
                rows: rows.result,
                receipts: receipts.result,
                checkpoints: checkpoints.result,
                usage: usage.result,
              });
            tx.onabort = () => reject(tx.error);
          });
        } finally {
          db.close();
        }
      };
      const initialStorage = await snapshot();
      if (empty)
        await liveCleanupGate.run(async () =>
          store.discard(await store.lane(lane), authorize, stop.signal),
        );
      const beforeStorage = await snapshot();
      const oldLane = scheduler.lanes.get(JSON.stringify(lane));
      const originalGateWasPaused = oldLane.pause.kind === 'preparation_paused';
      const epoch = scheduler.epoch;
      const failures = scheduler.aggregateFailures;
      const selection = empty
        ? await scheduler.selectEmptyPreparationGate(lane, 'online')
        : await scheduler.selectUnsent(lane, 'online');
      const terminal = { complete: 0, abort: 0 };
      const transaction = IDBDatabase.prototype.transaction;
      IDBDatabase.prototype.transaction = function (...args) {
        const tx = Reflect.apply(transaction, this, args);
        if (args[1] === 'readwrite') {
          tx.addEventListener('complete', () => {
            terminal.complete += 1;
          });
          tx.addEventListener('abort', () => {
            terminal.abort += 1;
          });
        }
        return tx;
      };
      const action = empty
        ? scheduler.retireEmptyPreparationGate(selection)
        : scheduler.discardUnsent(selection);
      const duplicate = empty
        ? scheduler.retireEmptyPreparationGate(selection)
        : scheduler.discardUnsent(selection);
      const samePending = action === duplicate;
      let outcome;
      try {
        outcome = await action;
      } finally {
        IDBDatabase.prototype.transaction = transaction;
      }
      return {
        initialStorage,
        beforeStorage,
        afterStorage: await snapshot(),
        terminal,
        outcome,
        samePending,
        reads: liveCleanupReads,
        actor: auth.actor_id,
        readActors: liveCleanupReadActors,
        rows: await store.lane(lane),
        snapshot: await store.captureCleanup(
          lane,
          () => authorize(),
          stop.signal,
        ),
        originalGateWasPaused,
        retired: !scheduler.lanes.has(JSON.stringify(lane)),
        epochUnchanged: scheduler.epoch === epoch,
        budgetUnchanged: scheduler.aggregateFailures === failures,
        unhandled: liveCleanupUnhandled,
      };
    }, empty);
    assert.equal(result.initialStorage.rows.length, 1);
    assert.equal(result.initialStorage.rows[0].state, 'unprepared');
    assert.equal(result.initialStorage.usage.rows, 1);
    assert.equal(
      result.initialStorage.usage.bytes,
      result.initialStorage.rows[0].bytes,
    );
    assert.deepEqual(result.initialStorage.receipts, []);
    assert.equal(result.initialStorage.checkpoints.length, 1);
    const expectedEmptyStorage = {
      rows: [],
      receipts: [],
      checkpoints: [],
      usage: { ...result.initialStorage.usage, rows: 0, bytes: 0 },
    };
    assert.deepEqual(
      result.beforeStorage,
      empty ? expectedEmptyStorage : result.initialStorage,
    );
    assert.deepEqual(result.afterStorage, expectedEmptyStorage);
    assert.deepEqual(
      result.terminal,
      { complete: 1, abort: 0 },
      'the typed action resolves after one native readwrite terminal complete',
    );
    assert.equal(
      result.actor,
      harness.expectedActorId,
      'initial binding matches the independently created user',
    );
    assert.deepEqual(
      result.readActors,
      [harness.expectedActorId, harness.expectedActorId],
      'both cleanup binding reads match the independently created user',
    );
    assert.equal(
      result.outcome.kind,
      empty ? 'retired_empty_preparation_gate' : 'discarded_unsent',
    );
    assert.equal(result.samePending, true);
    assert.equal(
      result.reads,
      2,
      'selection and action each read the live binding once',
    );
    assert.equal(
      harness.bindingReadCount() - readsBefore,
      2,
      'both proof reads reached the real Rust server',
    );
    assert.deepEqual(result.rows, []);
    assert.deepEqual(result.snapshot, { kind: 'empty' });
    assert.equal(result.originalGateWasPaused, true);
    assert.equal(result.retired, true);
    assert.equal(result.epochUnchanged, true);
    assert.equal(result.budgetUnchanged, true);
    assert.equal(result.unhandled, 0);
    assert.deepEqual(
      harness.requests(),
      [],
      'explicit local cleanup sends no mutation',
    );
    await harness.close(page);
  }
  console.log('outbox disposal: disposal-live passed');
}
