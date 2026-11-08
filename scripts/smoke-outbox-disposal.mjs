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
    'rejected-wrong-kind',
    'rejected-pending-identity',
    'rejected-offline-authority',
    'rejected-abort',
    'rejected-no-resurrection',
    'rejected-serialization',
    'rejected-stale-selection',
    'rejected-token-forgery',
    'rejected-replaced-pause',
    'rejected-post-commit-close',
    'rejected-close-releases-proof',
    'rejected-post-commit-wake',
    'rejected-accounting',
    'rejected-survivors',
    'rejected-online-read',
    'rejected-selection-guards',
    'rejected-foreign-owner',
    'rejected-foreign-actor',
    'rejected-foreign-binding',
    'rejected-offline-logout',
    'rejected-offline-lock-loss',
    'rejected-offline-generation',
    'rejected-offline-profile',
    'rejected-offline-invalidated',
    'rejected-offline-check-throws',
    'rejected-offline-closed',
    'rejected-offline-aborted',
    'rejected-post-commit-auth',
    'rejected-online-401',
    'unresolved-exclusivity',
    'unresolved-ineligible',
    'unresolved-eligible-heads',
    'unresolved-wrong-kind',
    'unresolved-token-forgery',
    'unresolved-stale-selection',
    'unresolved-replaced-pause',
    'unresolved-selection-guards',
    'unresolved-foreign-owner',
    'unresolved-foreign-actor',
    'unresolved-foreign-binding',
    'unresolved-accounting',
    'unresolved-online-read',
    'unresolved-survivors',
    'unresolved-abort',
    'unresolved-bound',
    'unresolved-offline-authority',
    'unresolved-offline-logout',
    'unresolved-offline-lock-loss',
    'unresolved-offline-generation',
    'unresolved-offline-profile',
    'unresolved-offline-invalidated',
    'unresolved-offline-check-throws',
    'unresolved-offline-closed',
    'unresolved-offline-aborted',
    'unresolved-offline-detached',
    'unresolved-offline-mid-transaction',
    'unresolved-online-401',
    'unresolved-online-select-401',
    'unresolved-online-binding',
    'unresolved-online-select-binding',
    'unresolved-resume-read-failed',
    'unresolved-post-commit-close',
    'unresolved-post-commit-auth',
    'unresolved-post-commit-wake',
    'unresolved-post-commit-retired',
    'unresolved-pending-identity',
    'unresolved-serialization',
    'unresolved-no-resurrection',
    'unresolved-late-rejection',
    'unresolved-late-response',
    'unresolved-kernel-identity',
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
      globalThis.cleanupAuthorizeModes = [];
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
          cleanupAuthorizeModes.push(mode);
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
      if (mode.startsWith('rejected-'))
        await checkRejectedDisposal(harness, page, mode);
      else if (mode.startsWith('unresolved-'))
        await checkUnresolvedDisposal(harness, page, mode);
      else await checkDisposalFault(harness, page, mode);
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

/**
 * Rejected-head fixtures for the internal discardRejected path. Each mode first
 * blocks a lane with one definitive 409 (data_conflict), so the lane holds a
 * rejection proof, then exercises one part of the rejected acceptance matrix.
 */
async function checkRejectedDisposal(harness, page, mode) {
  harness.configure('status', 409);
  await page.evaluate(async () => {
    globalThis.rejectedKey = [...lane.slice(0, 4), 'art_' + 'e'.repeat(32)];
    globalThis.rejectedHead = {
      key: crypto.randomUUID().replaceAll('-', ''),
      lane: rejectedKey,
      artifactId: rejectedKey[4],
      base: { version: 0 },
    };
    globalThis.rejectionCode = (promise) =>
      promise.then(
        () => 'unexpected',
        (error) => error.code,
      );
    globalThis.rejectedLaneState = () => {
      const current = scheduler.lanes.get(JSON.stringify(rejectedKey));
      return {
        present: current !== undefined,
        identical:
          current !== undefined &&
          current === rejectedOriginal.lane &&
          current.pause === rejectedOriginal.pause &&
          current.rejection === rejectedOriginal.proof,
        pause: current?.pause?.kind ?? null,
        proof: current?.rejection !== undefined,
        failures: current?.failures ?? null,
        aggregate: scheduler.aggregateFailures,
        epoch: scheduler.epoch,
        fenced: scheduler.fence.signal.aborted,
      };
    };
    globalThis.rejectedHeadRow = await enqueueLikeHead(rejectedHead);
    scheduler.wake(rejectedKey);
  });
  await page.waitForFunction(() =>
    events.some(
      (event) =>
        event.kind === 'paused' &&
        event.lane?.[4] === rejectedKey[4] &&
        event.pause.kind === 'blocked',
    ),
  );
  await page.evaluate(() => {
    const current = scheduler.lanes.get(JSON.stringify(rejectedKey));
    globalThis.rejectedOriginal = {
      lane: current,
      pause: current.pause,
      proof: current.rejection,
    };
  });
  assert.deepEqual(
    await page.evaluate(() => ({
      reason: events.findLast(
        (event) =>
          event.kind === 'paused' && event.lane?.[4] === rejectedKey[4],
      ).pause.failure.reason,
      proof: rejectedOriginal.proof !== undefined,
    })),
    { reason: 'data_conflict', proof: true },
  );
  const requests = harness.requests().length;
  assert.equal(requests, 1);
  const observe = () =>
    page.evaluate(async () => ({
      storage: await cleanupStorageSnapshot(),
      lane: rejectedLaneState(),
      bindingReads: cleanupBindingReads,
      events: events.length,
    }));
  // ADR 0049 rule 2: a fence ends the proof and nothing else about the lane.
  // The proof must be present exactly while the scheduler is open.
  async function expectUnchangedExceptFence(before) {
    const after = await observe();
    const { fenced } = after.lane;
    assert.deepEqual(after, {
      ...before,
      lane: { ...before.lane, fenced, proof: !fenced, identical: !fenced },
    });
  }
  const before = await observe();
  assert.equal(before.lane.identical, true);
  switch (mode) {
    case 'rejected-wrong-kind': {
      // Wrong-kind, unbranded, copied and derived tokens are refused before any
      // consumption, so the genuine token still works afterwards.
      await page.evaluate(async () => {
        globalThis.rejectedChoice = await scheduler.selectRejected(
          rejectedKey,
          'offline',
        );
      });
      const refusals = await page.evaluate(async () => ({
        unsent: await rejectionCode(scheduler.discardUnsent(rejectedChoice)),
        unbranded: await rejectionCode(scheduler.discardRejected({})),
        copied: await rejectionCode(
          scheduler.discardRejected({ ...rejectedChoice }),
        ),
        derived: await rejectionCode(
          scheduler.discardRejected(Object.create(rejectedChoice)),
        ),
      }));
      assert.deepEqual(refusals, {
        unsent: 'invalid_selection',
        unbranded: 'invalid_selection',
        copied: 'invalid_selection',
        derived: 'invalid_selection',
      });
      await expectUnchangedExceptFence(before);
      assert.deepEqual(
        await page.evaluate(() => scheduler.discardRejected(rejectedChoice)),
        { kind: 'discarded_rejected', removedDependents: 0 },
      );
      const after = await observe();
      assert.equal(after.storage.rows.length, before.storage.rows.length - 1);
      assert.equal(after.storage.usage.rows, before.storage.usage.rows - 1);
      assert.equal(harness.requests().length, requests);
      break;
    }
    case 'rejected-pending-identity': {
      const outcome = await page.evaluate(async () => {
        const choice = await scheduler.selectRejected(rejectedKey, 'offline');
        const first = scheduler.discardRejected(choice);
        const second = scheduler.discardRejected(choice);
        const crossKind = await rejectionCode(scheduler.discardUnsent(choice));
        const result = await first;
        const consumed = await rejectionCode(scheduler.discardRejected(choice));
        return { shared: first === second, crossKind, result, consumed };
      });
      assert.deepEqual(outcome, {
        shared: true,
        crossKind: 'invalid_selection',
        result: { kind: 'discarded_rejected', removedDependents: 0 },
        consumed: 'invalid_selection',
      });
      const after = await observe();
      // One operation: a second run would remove the head twice.
      assert.equal(after.storage.rows.length, before.storage.rows.length - 1);
      assert.equal(after.storage.usage.rows, before.storage.usage.rows - 1);
      assert.equal(after.bindingReads, 0);
      assert.equal(harness.requests().length, requests);
      break;
    }
    case 'rejected-offline-authority': {
      const local = await page.evaluate(async () => {
        globalThis.rejectedChoice = await scheduler.selectRejected(
          rejectedKey,
          'offline',
        );
        globalThis.cleanupAuthorityFailure = new deliveryApi.OutboxError(
          'owner_not_ready',
        );
        const code = await rejectionCode(
          scheduler.discardRejected(rejectedChoice),
        );
        globalThis.cleanupAuthorityFailure = undefined;
        return code;
      });
      assert.equal(local, 'owner_not_ready');
      await expectUnchangedExceptFence(before);
      const midTransaction = await page.evaluate(async () => {
        globalThis.rejectedChoice = await scheduler.selectRejected(
          rejectedKey,
          'offline',
        );
        const deletion = IDBObjectStore.prototype.delete;
        globalThis.rejectedDeletes = 0;
        globalThis.restoreRejectedDelete = () => {
          IDBObjectStore.prototype.delete = deletion;
        };
        IDBObjectStore.prototype.delete = function (...args) {
          const pending = deletion.apply(this, args);
          if (this.name === 'intents')
            pending.addEventListener(
              'success',
              () => {
                rejectedDeletes += 1;
                // Authority changes after the intent delete, so the transaction's
                // own check before commit must refuse the closure.
                currentAuthority = Symbol();
              },
              { once: true },
            );
          return pending;
        };
        const code = await rejectionCode(
          scheduler.discardRejected(rejectedChoice),
        );
        restoreRejectedDelete();
        return { code, deletes: rejectedDeletes };
      });
      assert.deepEqual(midTransaction, { code: 'owner_not_ready', deletes: 1 });
      await expectUnchangedExceptFence(before);
      assert.equal(harness.requests().length, requests);
      break;
    }
    case 'rejected-abort': {
      const aborted = await page.evaluate(async () => {
        globalThis.rejectedChoice = await scheduler.selectRejected(
          rejectedKey,
          'offline',
        );
        // The usage write is the last request before commit, so this aborts
        // after the closure's intent and checkpoint writes have succeeded.
        faultPut('usage');
        const code = await rejectionCode(
          scheduler.discardRejected(rejectedChoice),
        );
        restorePut();
        return code;
      });
      assert.equal(aborted, 'storage_failed');
      await expectUnchangedExceptFence(before);
      assert.equal(harness.requests().length, requests);
      // Any attempt consumes the token, so the failed selection cannot be retried.
      assert.equal(
        await page.evaluate(() =>
          rejectionCode(scheduler.discardRejected(rejectedChoice)),
        ),
        'invalid_selection',
      );
      await expectUnchangedExceptFence(before);
      const committed = await page.evaluate(async () => {
        globalThis.rejectedChoice = await scheduler.selectRejected(
          rejectedKey,
          'offline',
        );
        return scheduler.discardRejected(rejectedChoice);
      });
      assert.deepEqual(committed, {
        kind: 'discarded_rejected',
        removedDependents: 0,
      });
      const after = await observe();
      const removed = before.storage.rows.filter(
        (row) => !after.storage.rows.some((kept) => kept.key === row.key),
      );
      // Accounting once: the aborted attempt left counters untouched, and the
      // committed attempt subtracts the removed row's recorded bytes exactly once.
      assert.equal(removed.length, 1);
      assert.equal(after.storage.usage.rows, before.storage.usage.rows - 1);
      assert.equal(
        after.storage.usage.bytes,
        before.storage.usage.bytes - removed[0].bytes,
      );
      assert.equal(
        after.storage.checkpoints.length,
        before.storage.checkpoints.length - 1,
      );
      break;
    }
    case 'rejected-no-resurrection': {
      const replay = await page.evaluate(async () => {
        globalThis.rejectedChoice = await scheduler.selectRejected(
          rejectedKey,
          'offline',
        );
        const first = await scheduler.discardRejected(rejectedChoice);
        return {
          first,
          retired: !scheduler.lanes.has(JSON.stringify(rejectedKey)),
          replay: await rejectionCode(
            scheduler.discardRejected(rejectedChoice),
          ),
        };
      });
      assert.deepEqual(replay, {
        first: { kind: 'discarded_rejected', removedDependents: 0 },
        retired: true,
        replay: 'invalid_selection',
      });
      // A new row under the same lane key is a new incarnation. It must not
      // inherit the old proof, pause or failure count. The held request keeps
      // the lane in flight, so the state is observed before any response.
      harness.configure('headers_hold');
      const arrival = harness.waitRequest();
      await page.evaluate(async () => {
        await enqueueLikeHead({
          key: crypto.randomUUID().replaceAll('-', ''),
          lane: rejectedKey,
          artifactId: rejectedKey[4],
          base: { version: 0 },
        });
        scheduler.wake(rejectedKey);
      });
      await arrival;
      assert.deepEqual(
        await page.evaluate(() => {
          const { present, pause, proof, failures } = rejectedLaneState();
          return { present, pause, proof, failures };
        }),
        { present: true, pause: null, proof: false, failures: 0 },
      );
      assert.equal(harness.requests().length, 1);
      break;
    }
    case 'rejected-serialization': {
      const outcome = await page.evaluate(async () => {
        globalThis.rejectedChoice = await scheduler.selectRejected(
          rejectedKey,
          'offline',
        );
        const admittedKey = crypto.randomUUID().replaceAll('-', '');
        let admission;
        // The hook runs inside the action's authority check, so the admission
        // waits for the gate. If it joined the closure, the snapshot would change
        // and the discard would refuse with invalid_selection.
        globalThis.onCleanupProofCheck = () => {
          if (admission === undefined)
            admission = enqueueLikeHead({
              key: admittedKey,
              lane: rejectedKey,
              artifactId: rejectedKey[4],
              base: { version: 0 },
            });
        };
        // The gated admission starts only after the committed discard has retired
        // the blocked lane, so it cannot see the old lane.
        let lanePresentAtAdmission;
        globalThis.onAdmissionStart = () => {
          lanePresentAtAdmission = scheduler.lanes.has(
            JSON.stringify(rejectedKey),
          );
        };
        const result = await scheduler.discardRejected(rejectedChoice);
        globalThis.onCleanupProofCheck = undefined;
        globalThis.onAdmissionStart = undefined;
        await admission;
        return { result, admittedKey, lanePresentAtAdmission };
      });
      assert.deepEqual(outcome.result, {
        kind: 'discarded_rejected',
        removedDependents: 0,
      });
      assert.equal(outcome.lanePresentAtAdmission, false);
      const after = await observe();
      assert.equal(
        after.storage.rows.some((row) => row.key === outcome.admittedKey),
        true,
      );
      assert.equal(after.storage.rows.length, before.storage.rows.length);
      assert.equal(harness.requests().length, requests);
      break;
    }
    case 'rejected-stale-selection': {
      // A wake between selection and entry changes the wake marker.
      const woken = await page.evaluate(async () => {
        globalThis.rejectedChoice = await scheduler.selectRejected(
          rejectedKey,
          'offline',
        );
        scheduler.wake(rejectedKey);
        return rejectionCode(scheduler.discardRejected(rejectedChoice));
      });
      assert.equal(woken, 'invalid_selection');
      await expectUnchangedExceptFence(before);
      // A wake inside the transaction, after the intent delete, is refused by
      // the transaction's own check before commit.
      const wokenInside = await page.evaluate(async () => {
        globalThis.rejectedChoice = await scheduler.selectRejected(
          rejectedKey,
          'offline',
        );
        const deletion = IDBObjectStore.prototype.delete;
        let fired = false;
        globalThis.restoreRejectedDelete = () => {
          IDBObjectStore.prototype.delete = deletion;
        };
        IDBObjectStore.prototype.delete = function (...args) {
          const pending = deletion.apply(this, args);
          if (this.name === 'intents' && !fired)
            pending.addEventListener(
              'success',
              () => {
                fired = true;
                scheduler.wake(rejectedKey);
              },
              { once: true },
            );
          return pending;
        };
        const code = await rejectionCode(
          scheduler.discardRejected(rejectedChoice),
        );
        restoreRejectedDelete();
        return code;
      });
      assert.equal(wokenInside, 'invalid_selection');
      await expectUnchangedExceptFence(before);
      // A dependent enqueued after selection changes the closure signature.
      await page.evaluate(async () => {
        globalThis.rejectedChoice = await scheduler.selectRejected(
          rejectedKey,
          'offline',
        );
        await enqueueLikeHead({
          key: crypto.randomUUID().replaceAll('-', ''),
          lane: rejectedKey,
          artifactId: rejectedKey[4],
          base: { version: 0 },
        });
      });
      const withDependent = await observe();
      assert.equal(
        withDependent.storage.rows.length,
        before.storage.rows.length + 1,
      );
      assert.equal(
        await page.evaluate(() =>
          rejectionCode(scheduler.discardRejected(rejectedChoice)),
        ),
        'invalid_selection',
      );
      assert.deepEqual(await observe(), withDependent);
      assert.equal(harness.requests().length, requests);
      break;
    }
    case 'rejected-token-forgery': {
      // Forged brands and tokens of other kinds are refused before consumption,
      // so the genuine token still discards afterwards.
      await page.evaluate(async () => {
        globalThis.rejectedChoice = await scheduler.selectRejected(
          rejectedKey,
          'offline',
        );
      });
      assert.deepEqual(
        await page.evaluate(async () => ({
          forgedBrand: await rejectionCode(
            scheduler.discardRejected(Object.freeze({ [Symbol()]: true })),
          ),
          retryPreparation: await rejectionCode(
            scheduler.retryPreparation(rejectedChoice),
          ),
          resumeDelivery: await rejectionCode(
            scheduler.resumeDelivery(rejectedChoice),
          ),
          retryCleanup: await rejectionCode(
            scheduler.retryCleanup(rejectedChoice),
          ),
          retireEmptyGate: await rejectionCode(
            scheduler.retireEmptyPreparationGate(rejectedChoice),
          ),
        })),
        {
          forgedBrand: 'invalid_selection',
          retryPreparation: 'invalid_selection',
          resumeDelivery: 'invalid_selection',
          retryCleanup: 'invalid_selection',
          retireEmptyGate: 'invalid_selection',
        },
      );
      await expectUnchangedExceptFence(before);
      assert.equal(harness.requests().length, requests);
      assert.deepEqual(
        await page.evaluate(() => scheduler.discardRejected(rejectedChoice)),
        { kind: 'discarded_rejected', removedDependents: 0 },
      );
      assert.equal(harness.requests().length, requests);
      break;
    }
    case 'rejected-replaced-pause': {
      // An equal-content copy is not the pause the proof was made against.
      const refused = await page.evaluate(async () => {
        globalThis.rejectedChoice = await scheduler.selectRejected(
          rejectedKey,
          'offline',
        );
        const current = scheduler.lanes.get(JSON.stringify(rejectedKey));
        current.pause = { ...current.pause };
        const code = await rejectionCode(
          scheduler.discardRejected(rejectedChoice),
        );
        current.pause = rejectedOriginal.pause;
        return code;
      });
      assert.equal(refused, 'invalid_selection');
      await expectUnchangedExceptFence(before);
      assert.equal(harness.requests().length, requests);
      assert.deepEqual(
        await page.evaluate(async () => {
          globalThis.rejectedChoice = await scheduler.selectRejected(
            rejectedKey,
            'offline',
          );
          return scheduler.discardRejected(rejectedChoice);
        }),
        { kind: 'discarded_rejected', removedDependents: 0 },
      );
      break;
    }
    case 'rejected-post-commit-close': {
      // Closing right after the commit decides the result: the committed removal
      // stays, publication is suppressed and the lane is not retired.
      const outcome = await page.evaluate(async () => {
        globalThis.rejectedChoice = await scheduler.selectRejected(
          rejectedKey,
          'offline',
        );
        const transaction = IDBDatabase.prototype.transaction;
        globalThis.restoreRejectedTransaction = () => {
          IDBDatabase.prototype.transaction = transaction;
        };
        IDBDatabase.prototype.transaction = function (...args) {
          const tx = Reflect.apply(transaction, this, args);
          if (args[1] === 'readwrite' && [args[0]].flat().includes('intents'))
            tx.addEventListener('complete', () => scheduler.close(), {
              once: true,
            });
          return tx;
        };
        const result = await scheduler.discardRejected(rejectedChoice).then(
          (value) => value,
          (error) => ({ rejected: error.code }),
        );
        restoreRejectedTransaction();
        return {
          result,
          retained: scheduler.lanes.has(JSON.stringify(rejectedKey)),
        };
      });
      assert.deepEqual(outcome, {
        result: { kind: 'discarded_rejected', removedDependents: 0 },
        retained: true,
      });
      const after = await observe();
      assert.equal(after.storage.rows.length, before.storage.rows.length - 1);
      assert.equal(after.storage.usage.rows, before.storage.usage.rows - 1);
      assert.equal(after.events, before.events);
      assert.equal(harness.requests().length, requests);
      break;
    }
    case 'rejected-close-releases-proof': {
      // Close ends the proof, the lane's selection pointer and the selection's
      // token entry. The lane and its blocked pause stay, and the retired
      // selection still fails closed.
      const closed = await page.evaluate(async () => {
        globalThis.rejectedChoice = await scheduler.selectRejected(
          rejectedKey,
          'offline',
        );
        scheduler.close();
        const current = scheduler.lanes.get(JSON.stringify(rejectedKey));
        return {
          sameLane: current === rejectedOriginal.lane,
          samePause: current?.pause === rejectedOriginal.pause,
          pause: current?.pause?.kind ?? null,
          proof: current?.rejection !== undefined,
          selection: current?.rejectedSelection !== undefined,
          tokenEntry: scheduler.rejections.tokens.has(rejectedChoice),
        };
      });
      assert.deepEqual(closed, {
        sameLane: true,
        samePause: true,
        pause: 'blocked',
        proof: false,
        selection: false,
        tokenEntry: false,
      });
      assert.equal(
        await page.evaluate(() =>
          rejectionCode(scheduler.discardRejected(rejectedChoice)),
        ),
        'owner_not_ready',
      );
      const after = await observe();
      assert.deepEqual(after.storage, before.storage);
      assert.equal(after.events, before.events);
      assert.equal(harness.requests().length, requests);
      break;
    }
    case 'rejected-post-commit-auth': {
      // Invalidating authentication right after the commit decides the result the
      // same way closing does: the removal stays, nothing publishes, and the lane
      // is not retired.
      const outcome = await page.evaluate(async () => {
        globalThis.rejectedChoice = await scheduler.selectRejected(
          rejectedKey,
          'offline',
        );
        const transaction = IDBDatabase.prototype.transaction;
        globalThis.restoreRejectedTransaction = () => {
          IDBDatabase.prototype.transaction = transaction;
        };
        IDBDatabase.prototype.transaction = function (...args) {
          const tx = Reflect.apply(transaction, this, args);
          if (args[1] === 'readwrite' && [args[0]].flat().includes('intents'))
            tx.addEventListener(
              'complete',
              () => deliveryContext.invalidateAuthentication(),
              { once: true },
            );
          return tx;
        };
        const result = await scheduler.discardRejected(rejectedChoice).then(
          (value) => value,
          (error) => ({ rejected: error.code }),
        );
        restoreRejectedTransaction();
        return {
          result,
          retained: scheduler.lanes.has(JSON.stringify(rejectedKey)),
        };
      });
      assert.deepEqual(outcome, {
        result: { kind: 'discarded_rejected', removedDependents: 0 },
        retained: true,
      });
      const after = await observe();
      assert.equal(after.storage.rows.length, before.storage.rows.length - 1);
      assert.equal(after.storage.usage.rows, before.storage.usage.rows - 1);
      assert.equal(after.events, before.events);
      assert.equal(harness.requests().length, requests);
      break;
    }
    case 'rejected-post-commit-wake': {
      // A wake after the commit does not keep the removed lane alive.
      const outcome = await page.evaluate(async () => {
        globalThis.rejectedChoice = await scheduler.selectRejected(
          rejectedKey,
          'offline',
        );
        const transaction = IDBDatabase.prototype.transaction;
        globalThis.restoreRejectedTransaction = () => {
          IDBDatabase.prototype.transaction = transaction;
        };
        IDBDatabase.prototype.transaction = function (...args) {
          const tx = Reflect.apply(transaction, this, args);
          if (args[1] === 'readwrite' && [args[0]].flat().includes('intents'))
            tx.addEventListener('complete', () => scheduler.wake(rejectedKey), {
              once: true,
            });
          return tx;
        };
        const result = await scheduler.discardRejected(rejectedChoice).then(
          (value) => value,
          (error) => ({ rejected: error.code }),
        );
        restoreRejectedTransaction();
        return {
          result,
          retained: scheduler.lanes.has(JSON.stringify(rejectedKey)),
        };
      });
      assert.deepEqual(outcome, {
        result: { kind: 'discarded_rejected', removedDependents: 0 },
        retained: false,
      });
      const after = await observe();
      assert.equal(after.storage.rows.length, before.storage.rows.length - 1);
      assert.equal(harness.requests().length, requests);
      break;
    }
    case 'rejected-accounting': {
      // A committed zero-survivor disposal moves only the removed row's usage.
      await page.evaluate(async () => {
        globalThis.rejectedChoice = await scheduler.selectRejected(
          rejectedKey,
          'offline',
        );
      });
      const counters = () =>
        page.evaluate(() => ({
          aggregate: scheduler.aggregateFailures,
          epoch: scheduler.epoch,
        }));
      const countersBefore = await counters();
      assert.deepEqual(
        await page.evaluate(() => scheduler.discardRejected(rejectedChoice)),
        { kind: 'discarded_rejected', removedDependents: 0 },
      );
      assert.deepEqual(await counters(), countersBefore);
      const after = await observe();
      const removed = before.storage.rows.filter(
        (row) => !after.storage.rows.some((kept) => kept.key === row.key),
      );
      assert.equal(removed.length, 1);
      assert.equal(after.storage.usage.rows, before.storage.usage.rows - 1);
      assert.equal(
        after.storage.usage.bytes,
        before.storage.usage.bytes - removed[0].bytes,
      );
      assert.equal(harness.requests().length, requests);
      break;
    }
    case 'rejected-survivors': {
      // Survivors stay held until the resume token minted at publication sends
      // them with their original key and base.
      const survivorKey = await page.evaluate(async () => {
        // Counts page-side fetches when they start, before the network can deliver
        // them to the harness.
        globalThis.fetchLog = [];
        const originalFetch = globalThis.fetch;
        globalThis.fetch = (input, init) => {
          fetchLog.push({ method: init?.method ?? 'GET', url: String(input) });
          return originalFetch(input, init);
        };
        const newKey = () => crypto.randomUUID().replaceAll('-', '');
        // A dependent must name the current lane tail, so it is enqueued before
        // the independent survivor becomes the tail.
        await enqueueLikeHead({
          key: newKey(),
          lane: rejectedKey,
          artifactId: rejectedKey[4],
          base: { after: rejectedHeadRow.sequence },
          path: 'dependent',
          text: '{"d":1}',
        });
        const survivor = await enqueueLikeHead({
          key: newKey(),
          lane: rejectedKey,
          artifactId: rejectedKey[4],
          base: { version: 0 },
          path: 'survivor',
          text: '{"s":1}',
        });
        globalThis.rejectedChoice = await scheduler.selectRejected(
          rejectedKey,
          'offline',
        );
        return survivor.key;
      });
      const baseline = await observe();
      assert.equal(
        baseline.storage.rows.length,
        before.storage.rows.length + 2,
      );
      assert.deepEqual(
        await page.evaluate(() =>
          scheduler.discardRejected(rejectedChoice).then(
            (value) => value,
            (error) => ({ rejected: error.code }),
          ),
        ),
        { kind: 'discarded_rejected', removedDependents: 1 },
      );
      const committed = await observe();
      assert.equal(
        committed.storage.rows.length,
        baseline.storage.rows.length - 2,
      );
      assert.equal(
        committed.storage.rows.some((row) => row.key === survivorKey),
        true,
      );
      assert.deepEqual(
        await page.evaluate(() => {
          const current = scheduler.lanes.get(JSON.stringify(rejectedKey));
          const published = events.filter(
            (event) =>
              event.kind === 'paused' &&
              event.pause.reason === 'survivors_after_disposal',
          );
          return {
            kind: current.pause?.kind ?? null,
            reason: current.pause?.reason ?? null,
            scope: current.pause?.scope ?? null,
            publications: published.length,
            proof: current.rejection === undefined,
          };
        }),
        {
          kind: 'retry_paused',
          reason: 'survivors_after_disposal',
          scope: 'lane',
          publications: 1,
          proof: true,
        },
      );
      // Wake and a second selection send nothing before the resume token is used.
      // The macrotask yield lets a dispatch that the wake starts reach the counter.
      assert.equal(
        await page.evaluate(async () => {
          scheduler.wake(rejectedKey);
          await new Promise((resolve) => setTimeout(resolve, 0));
          return fetchLog.length;
        }),
        0,
      );
      assert.equal(
        await page.evaluate(() =>
          rejectionCode(scheduler.selectRejected(rejectedKey, 'offline')),
        ),
        'invalid_selection',
      );
      assert.equal(harness.requests().length, requests);
      // The resume token is a DeliveryResume, which discardRejected refuses by
      // brand. The unprepared-lane selections refuse a retry-paused lane.
      const publishedState = await observe();
      assert.deepEqual(
        await page.evaluate(async () => {
          const token = scheduler.lanes.get(JSON.stringify(rejectedKey)).pause
            .selection;
          return {
            discardRejected: await rejectionCode(
              scheduler.discardRejected(token),
            ),
            selectUnsent: await rejectionCode(
              scheduler.selectUnsent(rejectedKey, 'offline'),
            ),
            selectEmptyGate: await rejectionCode(
              scheduler.selectEmptyPreparationGate(rejectedKey, 'offline'),
            ),
          };
        }),
        {
          discardRejected: 'invalid_selection',
          selectUnsent: 'invalid_selection',
          selectEmptyGate: 'invalid_selection',
        },
      );
      assert.deepEqual(await observe(), publishedState);
      harness.configure('normal');
      const arrival = harness.waitRequest();
      assert.equal(
        await page.evaluate(() => {
          const token = scheduler.lanes.get(JSON.stringify(rejectedKey)).pause
            .selection;
          return scheduler.resumeDelivery(token).then(
            () => 'resumed',
            (error) => error.code,
          );
        }),
        'resumed',
      );
      // The resume's dispatch is asynchronous; configure() starts a new request
      // log, so the survivor is the only request after it.
      await arrival;
      // The dispatch carries the survivor's own row key as its idempotency key.
      const sent = harness.requests().map((request) => ({
        method: request.method,
        idempotencyKey: request.headers['idempotency-key'],
      }));
      assert.deepEqual(sent, [{ method: 'PUT', idempotencyKey: survivorKey }]);
      // The resume may first read the binding; only the artifact PUT is counted.
      assert.deepEqual(
        await page.evaluate(() =>
          fetchLog
            .filter((entry) => entry.method === 'PUT')
            .map((entry) => entry.url.endsWith('/data')),
        ),
        [true],
      );
      await page.waitForFunction(() =>
        events.some(
          (event) =>
            event.kind === 'attempt' &&
            event.result.kind === 'server_acknowledged',
        ),
      );
      const resumed = await observe();
      assert.equal(
        resumed.storage.rows.some((row) => row.key === survivorKey),
        false,
      );
      break;
    }
    case 'rejected-online-read': {
      // A failed online revalidation fails closed: no offline fallback, no token,
      // no storage change, and one binding read per revalidating call.
      harness.configureBinding('503');
      assert.equal(
        await page.evaluate(() =>
          rejectionCode(scheduler.selectRejected(rejectedKey, 'online')),
        ),
        'unavailable',
      );
      const failed = await observe();
      assert.equal(failed.bindingReads, before.bindingReads + 1);
      assert.deepEqual(
        { ...failed, bindingReads: 0 },
        { ...before, bindingReads: 0 },
      );
      assert.deepEqual(await page.evaluate(() => cleanupAuthorizeModes), [
        'online',
      ]);
      harness.configureBinding('normal');
      assert.deepEqual(
        await page.evaluate(async () => {
          globalThis.rejectedChoice = await scheduler.selectRejected(
            rejectedKey,
            'online',
          );
          return scheduler.discardRejected(rejectedChoice);
        }),
        { kind: 'discarded_rejected', removedDependents: 0 },
      );
      const committed = await observe();
      assert.equal(committed.bindingReads, before.bindingReads + 3);
      assert.deepEqual(await page.evaluate(() => cleanupAuthorizeModes), [
        'online',
        'online',
        'online',
      ]);
      assert.equal(harness.requests().length, requests);
      break;
    }
    case 'rejected-online-401': {
      // A 401 from the online revalidation invalidates authentication and closes
      // the scheduler. Nothing is deleted and no offline authority is tried.
      // Invalidation without an abort leaves the close to the scheduler itself.
      await page.evaluate(() => {
        globalThis.authInvalidationAborts = false;
      });
      harness.configureBinding('401stalled');
      await page.evaluate(() => {
        globalThis.onlineFailure = scheduler
          .selectRejected(rejectedKey, 'online')
          .then(
            () => 'unexpected',
            (error) => ({
              code: error.code,
              status: error.status,
              retryAfterSeconds: error.retryAfterSeconds,
            }),
          );
      });
      await page.waitForFunction(() => cleanupObservedStatuses.includes(401));
      assert.equal(await page.evaluate(() => locallyAuthenticated), false);
      const failure = await page.evaluate(() => onlineFailure);
      assert.deepEqual(failure, {
        code: await page.evaluate(() => cleanupRawReadDiagnostic.code),
        status: 401,
        retryAfterSeconds: 3600,
      });
      // The close happens after the body completes, so it is observed here.
      assert.equal(
        await page.evaluate(() => scheduler.fence.signal.aborted),
        true,
      );
      assert.deepEqual(await page.evaluate(() => cleanupAuthorizeModes), [
        'online',
      ]);
      const after = await observe();
      assert.equal(after.bindingReads, before.bindingReads + 1);
      // The 401 fenced the scheduler (asserted above), so the proof has ended.
      assert.deepEqual(
        { ...after, bindingReads: 0 },
        {
          ...before,
          bindingReads: 0,
          lane: {
            ...before.lane,
            fenced: true,
            proof: false,
            identical: false,
          },
        },
      );
      assert.equal(harness.requests().length, requests);
      assert.equal(
        await page.evaluate(() =>
          rejectionCode(scheduler.selectRejected(rejectedKey, 'offline')),
        ),
        'owner_not_ready',
      );
      // Later checks in this process read the binding normally again.
      harness.configureBinding('normal');
      break;
    }
    case 'rejected-selection-guards': {
      // Unsettled lanes (a pending timer, a queued turn or the active turn) are
      // refused before any head is selected.
      const refused = await page.evaluate(async () => {
        const current = scheduler.lanes.get(JSON.stringify(rejectedKey));
        const select = (key) =>
          rejectionCode(scheduler.selectRejected(key, 'offline'));
        const outcomes = {};
        current.cancelTimer = () => {};
        outcomes.timer = await select(rejectedKey);
        delete current.cancelTimer;
        current.queued = true;
        outcomes.queued = await select(rejectedKey);
        current.queued = false;
        const active = scheduler.activeLane;
        scheduler.activeLane = current;
        outcomes.active = await select(rejectedKey);
        scheduler.activeLane = active;
        return outcomes;
      });
      assert.deepEqual(refused, {
        timer: 'invalid_selection',
        queued: 'invalid_selection',
        active: 'invalid_selection',
      });
      await expectUnchangedExceptFence(before);
      assert.equal(harness.requests().length, requests);
      assert.deepEqual(
        await page.evaluate(async () => {
          globalThis.rejectedChoice = await scheduler.selectRejected(
            rejectedKey,
            'offline',
          );
          return scheduler.discardRejected(rejectedChoice);
        }),
        { kind: 'discarded_rejected', removedDependents: 0 },
      );
      break;
    }
    case 'rejected-foreign-owner':
    case 'rejected-foreign-actor':
    case 'rejected-foreign-binding': {
      // A key naming another owner, actor or storage epoch is refused. The
      // scheduler fails closed on its first failed check, so each one runs on
      // its own page.
      const index = {
        'rejected-foreign-owner': 0,
        'rejected-foreign-actor': 2,
        'rejected-foreign-binding': 3,
      }[mode];
      const refused = await page.evaluate(
        ({ index }) => {
          const replacement = {
            0: 'other-owner',
            2: 'other-actor',
            3: rejectedKey[3] + 1,
          };
          const key = [...rejectedKey];
          key[index] = replacement[index];
          return rejectionCode(scheduler.selectRejected(key, 'offline'));
        },
        { index },
      );
      assert.equal(refused, 'owner_not_ready');
      await expectUnchangedExceptFence(before);
      assert.equal(harness.requests().length, requests);
      break;
    }
    case 'rejected-offline-closed': {
      // Close retires the held selection. Its token entry and the lane's proof
      // leave, the blocked pause stays, and the retired selection still fails
      // closed with nothing stored, emitted or requested.
      const closed = await page.evaluate(async () => {
        globalThis.rejectedChoice = await scheduler.selectRejected(
          rejectedKey,
          'offline',
        );
        scheduler.close();
        const current = scheduler.lanes.get(JSON.stringify(rejectedKey));
        return {
          pause: current?.pause?.kind ?? null,
          proof: current?.rejection !== undefined,
          selection: current?.rejectedSelection !== undefined,
          tokenEntry: scheduler.rejections.tokens.has(rejectedChoice),
        };
      });
      assert.deepEqual(closed, {
        pause: 'blocked',
        proof: false,
        selection: false,
        tokenEntry: false,
      });
      assert.equal(
        await page.evaluate(() =>
          rejectionCode(scheduler.discardRejected(rejectedChoice)),
        ),
        'owner_not_ready',
      );
      await expectUnchangedExceptFence(before);
      assert.equal(harness.requests().length, requests);
      break;
    }
    case 'rejected-offline-logout':
    case 'rejected-offline-lock-loss':
    case 'rejected-offline-generation':
    case 'rejected-offline-profile':
    case 'rejected-offline-invalidated':
    case 'rejected-offline-aborted':
    case 'rejected-offline-check-throws': {
      // One lost authority fact, an aborted authority signal or a throwing check,
      // injected between selection and the transaction. The scheduler fails
      // closed on its first failed check, so each fact runs on its own page.
      const fact = mode.slice('rejected-offline-'.length);
      const refused = await page.evaluate(async (fact) => {
        const facts = {
          logout: () => {
            locallyAuthenticated = false;
          },
          'lock-loss': () => {
            lockHeld = false;
          },
          generation: () => {
            currentAuthority = Symbol();
          },
          profile: () => {
            sessionStorage.setItem(
              'artifree-data-outbox-owner-v1',
              'replaced-owner',
            );
          },
          invalidated: () => {
            authInvalidationAborts = false;
            deliveryContext.invalidateAuthentication();
          },
          aborted: () => {
            stop.abort();
          },
          'check-throws': () => {
            authorityException = makeAuthorityException();
          },
        };
        globalThis.rejectedChoice = await scheduler.selectRejected(
          rejectedKey,
          'offline',
        );
        facts[fact]();
        return rejectionCode(scheduler.discardRejected(rejectedChoice));
      }, fact);
      assert.equal(refused, 'owner_not_ready');
      await expectUnchangedExceptFence(before);
      assert.equal(harness.requests().length, requests);
      break;
    }
    default:
      throw new Error(`unknown rejected disposal fixture: ${mode}`);
  }
}

const dataConflictBody = {
  error: {
    code: 'data_conflict',
    message: 'private-response-marker',
    current_data_version: 5,
  },
};
const notFoundBody = {
  error: { code: 'not_found', message: 'private-not-found-marker' },
};
const unclassifiedBody = {
  error: {
    code: 'unrecognized_marker',
    message: 'private-unclassified-marker',
  },
};
const eligibleBlocked = {
  kind: 'blocked',
  failure: { reason: 'access_unavailable', status: 404 },
};

/** Waits for a paused event of the lane tagged `tag`, then records its pause. */
async function unresolvedWaitPause(page, tag, kind, detail = null) {
  await page.waitForFunction(
    ({ artifactId, kind, detail }) =>
      events.some(
        (event) =>
          event.kind === 'paused' &&
          event.lane?.[4] === artifactId &&
          event.pause.kind === kind &&
          (detail === null ||
            (event.pause.reason ?? event.pause.failure?.reason) === detail),
      ),
    { artifactId: 'art_' + tag.repeat(32), kind, detail },
  );
  await page.evaluate((tag) => unresolvedMark(tag), tag);
}

/**
 * Exhausts a blocked lane into a lane-scope retry pause: its eighth retryable
 * failure (a 503) publishes the pause that carries a resume token.
 */
async function unresolvedRetryPause(harness, page, tag) {
  await page.evaluate((tag) => {
    const current = unresolvedLane(tag);
    delete current.pause;
    current.failures = 7;
  }, tag);
  harness.configure('status', 503);
  await page.evaluate((tag) => scheduler.wake(unresolvedKey(tag)), tag);
  await unresolvedWaitPause(page, tag, 'retry_paused', 'server_error');
}

/** Abandons the page's `staleChoice`, which must be refused with no observable change. */
async function assertAbandonRefused(harness, page, label) {
  const before = await page.evaluate(() => unresolvedObserve());
  const requests = harness.requests().length;
  assert.equal(
    await page.evaluate(() =>
      unresolvedCode(scheduler.abandonUnresolved(staleChoice)),
    ),
    'invalid_selection',
    label,
  );
  assert.deepEqual(
    await page.evaluate(() => unresolvedObserve()),
    before,
    label,
  );
  assert.equal(harness.requests().length, requests, label);
}

// Stored rows and checkpoints name a fixture lane by its artifact, which every
// unresolved case builds as 'art_' followed by the tag repeated 32 times.
function laneRowsOf(rows, tag) {
  return rows.filter((row) => row.artifactId === 'art_' + tag.repeat(32));
}

function checkpointOf(ledger, tag) {
  return ledger.storage.checkpoints.find(
    (checkpoint) => checkpoint.lane[4] === 'art_' + tag.repeat(32),
  );
}

/** The ledger without the fields that binding reads and requests advance. */
function withoutReads(ledger) {
  return {
    storage: ledger.storage,
    epoch: ledger.epoch,
    aggregate: ledger.aggregate,
    events: ledger.events,
    lanes: ledger.lanes,
  };
}

/**
 * ADR 0049 unresolved abandonment (R08c-3b). Each lane starts from a prepared
 * head and no rejection proof unless a case says otherwise. Refusals are checked
 * against storage, pause identity, events, epoch and HTTP mutations.
 */
async function checkUnresolvedDisposal(harness, page, mode) {
  await page.evaluate(() => {
    globalThis.unresolvedPauses = new Map();
    globalThis.unresolvedKey = (tag) => [
      ...lane.slice(0, 4),
      'art_' + tag.repeat(32),
    ];
    globalThis.unresolvedLane = (tag) =>
      scheduler.lanes.get(JSON.stringify(unresolvedKey(tag)));
    globalThis.unresolvedCode = (promise) =>
      promise.then(
        () => 'unexpected',
        (error) => error.code,
      );
    globalThis.unresolvedSelect = (tag) =>
      scheduler.selectUnresolved(unresolvedKey(tag), 'offline').then(
        () => 'accepted',
        (error) => error.code,
      );
    globalThis.unresolvedEnqueue = (tag) =>
      enqueueLikeHead({
        key: crypto.randomUUID().replaceAll('-', ''),
        lane: unresolvedKey(tag),
        artifactId: unresolvedKey(tag)[4],
        base: { version: 0 },
      });
    globalThis.unresolvedStart = async (tag) => {
      await unresolvedEnqueue(tag);
      scheduler.wake(unresolvedKey(tag));
    };
    globalThis.unresolvedMark = (tag) => {
      unresolvedPauses.set(tag, unresolvedLane(tag).pause);
    };
    globalThis.unresolvedPaused = (tag) =>
      unresolvedLane(tag)?.pause === unresolvedPauses.get(tag);
    globalThis.unresolvedRows = async (tag) =>
      (await cleanupStorageSnapshot()).rows.filter(
        (row) => row.artifactId === unresolvedKey(tag)[4],
      );
    globalThis.unresolvedObserve = async () => ({
      storage: await cleanupStorageSnapshot(),
      epoch: scheduler.epoch,
      events: events.length,
    });
    // Each helper restores what it replaced, even when the refusal is not thrown.
    globalThis.unresolvedWithPause = async (tag, pause) => {
      const current = unresolvedLane(tag);
      const saved = current.pause;
      current.pause = pause;
      try {
        return await unresolvedSelect(tag);
      } finally {
        current.pause = saved;
      }
    };
    globalThis.unresolvedWithoutPause = async (tag) => {
      const current = unresolvedLane(tag);
      const saved = current.pause;
      delete current.pause;
      try {
        return await unresolvedSelect(tag);
      } finally {
        current.pause = saved;
      }
    };
    globalThis.unresolvedWithFlag = async (tag, field, value) => {
      const current = unresolvedLane(tag);
      const saved = current[field];
      current[field] = value;
      try {
        return await unresolvedSelect(tag);
      } finally {
        current[field] = saved;
      }
    };
    globalThis.unresolvedAsActive = async (tag) => {
      const active = scheduler.activeLane;
      scheduler.activeLane = unresolvedLane(tag);
      try {
        return await unresolvedSelect(tag);
      } finally {
        scheduler.activeLane = active;
      }
    };
    // Storage-level change between selection and action, by row index in the lane.
    // The store's own acknowledgement of the head, as a delivery would write it.
    globalThis.unresolvedAcknowledge = async (tag) => {
      const [head] = await store.lane(unresolvedKey(tag));
      await localGate.run(async () =>
        store.acknowledge(
          head,
          {
            artifact_id: head.artifactId,
            path: head.path,
            data_version: head.request.base + 1,
            changed: true,
            replayed: false,
          },
          authorize,
          stop.signal,
        ),
      );
    };
    // A wake that lands inside the transaction, after the first intent delete.
    globalThis.unresolvedWakeInside = (tag) => {
      const deletion = IDBObjectStore.prototype.delete;
      let fired = false;
      globalThis.restoreUnresolvedDelete = () => {
        IDBObjectStore.prototype.delete = deletion;
      };
      IDBObjectStore.prototype.delete = function (...args) {
        const pending = deletion.apply(this, args);
        if (this.name === 'intents' && !fired)
          pending.addEventListener(
            'success',
            () => {
              fired = true;
              scheduler.wake(unresolvedKey(tag));
            },
            { once: true },
          );
        return pending;
      };
    };
    globalThis.unresolvedDependent = (tag, after, changes = {}) =>
      enqueueLikeHead({
        key: crypto.randomUUID().replaceAll('-', ''),
        lane: unresolvedKey(tag),
        artifactId: unresolvedKey(tag)[4],
        base: { after },
        ...changes,
      });
    // A real closure member: its `after` names the stored head. The store accepts
    // an `after` link only from the lane tail, so callers use a lane whose head is
    // its only row.
    globalThis.unresolvedDependentOfHead = async (tag) => {
      const [head] = await store.lane(unresolvedKey(tag));
      return unresolvedDependent(tag, head.sequence);
    };
    // Counts every page-side fetch, so a network-free path is observable.
    globalThis.unresolvedFetches = 0;
    const unresolvedFetch = globalThis.fetch;
    globalThis.fetch = (input, init) => {
      unresolvedFetches += 1;
      return unresolvedFetch(input, init);
    };
    globalThis.unresolvedGate = (tag) => {
      const current = unresolvedLane(tag);
      return {
        pause: current?.pause?.kind ?? null,
        failures: current?.failures ?? null,
        rejection: current?.rejection !== undefined,
      };
    };
    globalThis.unresolvedLedger = async () => ({
      storage: await cleanupStorageSnapshot(),
      epoch: scheduler.epoch,
      aggregate: scheduler.aggregateFailures,
      events: events.length,
      lanes: { c: unresolvedGate('c'), d: unresolvedGate('d') },
      fetches: unresolvedFetches,
      bindingReads: cleanupBindingReads,
      authorizeModes: [...cleanupAuthorizeModes],
    });
    // Records the mode of every IndexedDB transaction that one operation opens.
    globalThis.unresolvedTransactions = async (operation) => {
      const open = IDBDatabase.prototype.transaction;
      const modes = [];
      IDBDatabase.prototype.transaction = function (names, mode, ...rest) {
        modes.push(mode ?? 'readonly');
        return Reflect.apply(open, this, [names, mode, ...rest]);
      };
      try {
        return { result: await operation(), modes };
      } finally {
        IDBDatabase.prototype.transaction = open;
      }
    };
  });
  try {
    switch (mode) {
      case 'unresolved-exclusivity': {
        // The proof-bearing 409 lane from the 3a fixture.
        harness.setErrorBody(dataConflictBody);
        harness.configure('status', 409);
        await page.evaluate((tag) => unresolvedStart(tag), 'e');
        await unresolvedWaitPause(page, 'e', 'blocked', 'data_conflict');
        assert.equal(
          await page.evaluate(
            () => unresolvedLane('e').rejection !== undefined,
          ),
          true,
        );
        await page.evaluate(() => {
          globalThis.proofOfE = unresolvedLane('e').rejection;
        });
        // The 409 pause is ineligible by kind. Substituting an eligible pause
        // isolates the proof as the only reason the selection is refused.
        const beforeProof = await page.evaluate(() => unresolvedObserve());
        assert.equal(
          await page.evaluate(
            (pause) => unresolvedWithPause('e', pause),
            eligibleBlocked,
          ),
          'invalid_selection',
          'Exclusivity: both directions reject',
        );
        assert.deepEqual(
          await page.evaluate(() => unresolvedObserve()),
          beforeProof,
        );
        // A proof attached after selection makes the committed action refuse.
        harness.setErrorBody(notFoundBody);
        harness.configure('status', 404);
        await page.evaluate((tag) => unresolvedStart(tag), 'f');
        await unresolvedWaitPause(page, 'f', 'blocked', 'access_unavailable');
        await page.evaluate(async () => {
          globalThis.exclusiveChoice = await scheduler.selectUnresolved(
            unresolvedKey('f'),
            'offline',
          );
          unresolvedLane('f').rejection = proofOfE;
        });
        const beforeAbandon = await page.evaluate(() => unresolvedObserve());
        assert.equal(
          await page.evaluate(() =>
            unresolvedCode(scheduler.abandonUnresolved(exclusiveChoice)),
          ),
          'invalid_selection',
          'Exclusivity: abandonment refuses a proof-bearing lane',
        );
        assert.deepEqual(
          await page.evaluate(() => unresolvedObserve()),
          beforeAbandon,
        );
        // Without the proof, the same lane selects and commits.
        assert.deepEqual(
          await page.evaluate(async () => {
            delete unresolvedLane('f').rejection;
            globalThis.exclusiveChoice = await scheduler.selectUnresolved(
              unresolvedKey('f'),
              'offline',
            );
            return scheduler.abandonUnresolved(exclusiveChoice);
          }),
          { kind: 'abandoned_unknown', removedDependents: 0 },
        );
        // The other direction: an unresolved lane refuses the rejected kind.
        await page.evaluate((tag) => unresolvedStart(tag), '0');
        await unresolvedWaitPause(page, '0', 'blocked', 'access_unavailable');
        await page.evaluate(async () => {
          globalThis.wrongKindChoice = await scheduler.selectUnresolved(
            unresolvedKey('0'),
            'offline',
          );
        });
        const beforeRejected = await page.evaluate(() => unresolvedObserve());
        assert.deepEqual(
          await page.evaluate(async () => ({
            selectRejected: await unresolvedCode(
              scheduler.selectRejected(unresolvedKey('0'), 'offline'),
            ),
            discardRejected: await unresolvedCode(
              scheduler.discardRejected(wrongKindChoice),
            ),
          })),
          {
            selectRejected: 'invalid_selection',
            discardRejected: 'invalid_selection',
          },
        );
        assert.deepEqual(
          await page.evaluate(() => unresolvedObserve()),
          beforeRejected,
        );
        assert.deepEqual(
          await page.evaluate(() =>
            scheduler.abandonUnresolved(wrongKindChoice),
          ),
          { kind: 'abandoned_unknown', removedDependents: 0 },
        );
        assert.equal(harness.requests().length, 2);
        break;
      }
      case 'unresolved-ineligible': {
        harness.setErrorBody(notFoundBody);
        // Acknowledged head: the receipt is durable but the usage write fails,
        // so the scheduler holds cleanup_paused over an acknowledged head.
        harness.configure('normal');
        // The head is prepared before the usage fault is armed, as in the phase
        // fixtures, so only the post-receipt usage write fails.
        await page.evaluate(async (tag) => {
          await unresolvedEnqueue(tag);
          await localGate.run(async () =>
            store.prepare(
              (await store.lane(unresolvedKey(tag)))[0],
              authorize,
              stop.signal,
            ),
          );
          faultPut('usage');
          scheduler.wake(unresolvedKey(tag));
        }, 'c');
        await unresolvedWaitPause(page, 'c', 'cleanup_paused');
        await page.evaluate(() => restorePut());
        assert.equal(
          (await page.evaluate(() => unresolvedRows('c')))[0].state,
          'acknowledged',
        );
        assert.equal(
          await page.evaluate(
            (pause) => unresolvedWithPause('c', pause),
            eligibleBlocked,
          ),
          'invalid_selection',
          'acknowledged head',
        );
        // Unprepared head: the preparation write fails before any request.
        harness.configure('status', 404);
        await page.evaluate(() => faultPut('intents', 'prepared'));
        await page.evaluate((tag) => unresolvedStart(tag), 'b');
        await unresolvedWaitPause(page, 'b', 'preparation_paused');
        await page.evaluate(() => restorePut());
        assert.equal(
          (await page.evaluate(() => unresolvedRows('b')))[0].state,
          'unprepared',
        );
        assert.equal(
          await page.evaluate(
            (pause) => unresolvedWithPause('b', pause),
            eligibleBlocked,
          ),
          'invalid_selection',
          'unprepared head',
        );
        // Baseline: a prepared head under a 404 access refusal. Each factor
        // below is substituted alone, and the baseline must select again after.
        await page.evaluate((tag) => unresolvedStart(tag), 'a');
        await unresolvedWaitPause(page, 'a', 'blocked', 'access_unavailable');
        assert.equal(
          (await page.evaluate(() => unresolvedRows('a')))[0].state,
          'prepared',
        );
        const before = await page.evaluate(() => unresolvedObserve());
        const substitutions = {
          cleanup_paused: {
            kind: 'cleanup_paused',
            selection: {},
            ackDurable: true,
          },
          survivors_after_disposal: {
            kind: 'retry_paused',
            reason: 'survivors_after_disposal',
            scope: 'lane',
            selection: {},
          },
          preparation_paused: { kind: 'preparation_paused', selection: {} },
          'blocked 401': {
            kind: 'blocked',
            failure: { reason: 'access_unavailable', status: 401 },
          },
          'non-access blocked pause': {
            kind: 'blocked',
            failure: { reason: 'binding_changed', status: 409 },
          },
        };
        for (const [name, pause] of Object.entries(substitutions)) {
          assert.equal(
            await page.evaluate(
              (pause) => unresolvedWithPause('a', pause),
              pause,
            ),
            'invalid_selection',
            name,
          );
          assert.deepEqual(
            await page.evaluate(() => unresolvedObserve()),
            before,
            name,
          );
          assert.equal(
            await page.evaluate(() => unresolvedPaused('a')),
            true,
            name,
          );
          assert.equal(
            await page.evaluate(() => unresolvedSelect('a')),
            'accepted',
            name,
          );
        }
        assert.equal(
          await page.evaluate(() => unresolvedWithoutPause('a')),
          'invalid_selection',
          'healthy lane with no pause',
        );
        assert.equal(
          await page.evaluate(() =>
            unresolvedWithFlag('a', 'cancelTimer', () => {}),
          ),
          'invalid_selection',
          'timer-pending lane',
        );
        assert.equal(
          await page.evaluate(() => unresolvedAsActive('a')),
          'invalid_selection',
          'active lane',
        );
        assert.deepEqual(
          await page.evaluate(() => unresolvedObserve()),
          before,
        );
        assert.equal(harness.requests().length, 1);
        break;
      }
      case 'unresolved-eligible-heads': {
        harness.setErrorBody(notFoundBody);
        harness.configure('status', 403);
        await page.evaluate((tag) => unresolvedStart(tag), 'a');
        await unresolvedWaitPause(page, 'a', 'blocked', 'access_unavailable');
        assert.equal(
          await page.evaluate(() => unresolvedLane('a').pause.failure.status),
          403,
        );
        assert.equal(
          await page.evaluate(() => unresolvedSelect('a')),
          'accepted',
          'blocked 403',
        );
        harness.configure('status', 404);
        await page.evaluate((tag) => unresolvedStart(tag), 'b');
        await unresolvedWaitPause(page, 'b', 'blocked', 'access_unavailable');
        assert.equal(
          await page.evaluate(() => unresolvedLane('b').pause.failure.status),
          404,
        );
        const beforeRead = await page.evaluate(() => unresolvedObserve());
        assert.equal(
          await page.evaluate(() => unresolvedSelect('b')),
          'accepted',
          'blocked 404',
        );
        // Selection reads storage only: no row, checkpoint, usage or epoch change.
        assert.deepEqual(
          await page.evaluate(() => unresolvedObserve()),
          beforeRead,
        );
        harness.setErrorBody(unclassifiedBody);
        harness.configure('status', 400);
        await page.evaluate((tag) => unresolvedStart(tag), 'c');
        await unresolvedWaitPause(
          page,
          'c',
          'retry_paused',
          'unclassified_error_response',
        );
        assert.equal(
          await page.evaluate(() => unresolvedLane('c').pause.scope),
          'lane',
        );
        assert.equal(
          await page.evaluate(() => unresolvedSelect('c')),
          'accepted',
          'unclassified_error_response (lane)',
        );
        // The ACK write fails after a 200 receipt, so the head stays prepared:
        // the observed receipt is never promoted to an ACK.
        harness.setErrorBody(notFoundBody);
        harness.configure('normal');
        await page.evaluate(() => faultPut('intents', 'acknowledged'));
        await page.evaluate((tag) => unresolvedStart(tag), 'd');
        await unresolvedWaitPause(
          page,
          'd',
          'retry_paused',
          'ack_storage_failed',
        );
        await page.evaluate(() => restorePut());
        assert.equal(
          (await page.evaluate(() => unresolvedRows('d')))[0].state,
          'prepared',
        );
        assert.equal(
          await page.evaluate(() => unresolvedSelect('d')),
          'accepted',
          'ack_storage_failed',
        );
        // Coordinator-scope exhaustion of a healthy lane that holds a prepared head.
        harness.configure('status', 404);
        await page.evaluate((tag) => unresolvedStart(tag), 'e');
        await unresolvedWaitPause(page, 'e', 'blocked', 'access_unavailable');
        await page.evaluate(() => {
          delete unresolvedLane('e').pause;
          globalThis.savedAggregate = scheduler.aggregateFailures;
          scheduler.aggregateFailures = 32;
          scheduler.wake(unresolvedKey('e'));
        });
        await unresolvedWaitPause(
          page,
          'e',
          'retry_paused',
          'aggregate_exhausted',
        );
        await page.evaluate(() => {
          scheduler.aggregateFailures = savedAggregate;
        });
        assert.equal(
          await page.evaluate(() => unresolvedLane('e').pause.scope),
          'coordinator',
        );
        assert.equal(
          await page.evaluate(() => unresolvedSelect('e')),
          'accepted',
          'aggregate_exhausted with a prepared head',
        );
        // Lane-scope exhaustion: the eighth retryable failure pauses the lane.
        await page.evaluate((tag) => unresolvedStart(tag), 'f');
        await unresolvedWaitPause(page, 'f', 'blocked', 'access_unavailable');
        await page.evaluate(() => {
          const current = unresolvedLane('f');
          delete current.pause;
          current.failures = 7;
        });
        harness.configure('status', 503);
        await page.evaluate(() => scheduler.wake(unresolvedKey('f')));
        await unresolvedWaitPause(page, 'f', 'retry_paused', 'server_error');
        assert.equal(
          await page.evaluate(() => unresolvedLane('f').pause.scope),
          'lane',
        );
        assert.equal(
          await page.evaluate(() => unresolvedSelect('f')),
          'accepted',
          'lane-scope retryable exhaustion',
        );
        break;
      }
      case 'unresolved-wrong-kind': {
        harness.setErrorBody(dataConflictBody);
        harness.configure('status', 409);
        await page.evaluate((tag) => unresolvedStart(tag), 'e');
        await unresolvedWaitPause(page, 'e', 'blocked', 'data_conflict');
        await page.evaluate(async () => {
          globalThis.rejectedChoice = await scheduler.selectRejected(
            unresolvedKey('e'),
            'offline',
          );
        });
        harness.setErrorBody(notFoundBody);
        harness.configure('status', 404);
        await page.evaluate((tag) => unresolvedStart(tag), 'f');
        await unresolvedWaitPause(page, 'f', 'blocked', 'access_unavailable');
        await page.evaluate(async () => {
          globalThis.unresolvedChoice = await scheduler.selectUnresolved(
            unresolvedKey('f'),
            'offline',
          );
        });
        const before = await page.evaluate(() => unresolvedObserve());
        assert.deepEqual(
          await page.evaluate(async () => ({
            abandonRejected: await unresolvedCode(
              scheduler.abandonUnresolved(rejectedChoice),
            ),
            discardUnresolved: await unresolvedCode(
              scheduler.discardRejected(unresolvedChoice),
            ),
          })),
          {
            abandonRejected: 'invalid_selection',
            discardUnresolved: 'invalid_selection',
          },
        );
        assert.deepEqual(
          await page.evaluate(() => unresolvedObserve()),
          before,
        );
        assert.equal(harness.requests().length, 1);
        // Refused by brand before consumption, so each token still acts once.
        assert.deepEqual(
          await page.evaluate(() => scheduler.discardRejected(rejectedChoice)),
          { kind: 'discarded_rejected', removedDependents: 0 },
        );
        assert.deepEqual(
          await page.evaluate(() =>
            scheduler.abandonUnresolved(unresolvedChoice),
          ),
          { kind: 'abandoned_unknown', removedDependents: 0 },
        );
        break;
      }
      case 'unresolved-token-forgery': {
        harness.setErrorBody(notFoundBody);
        harness.configure('status', 404);
        await page.evaluate((tag) => unresolvedStart(tag), 'f');
        await unresolvedWaitPause(page, 'f', 'blocked', 'access_unavailable');
        await page.evaluate(async () => {
          globalThis.forgeryChoice = await scheduler.selectUnresolved(
            unresolvedKey('f'),
            'offline',
          );
        });
        const before = await page.evaluate(() => unresolvedObserve());
        assert.deepEqual(
          await page.evaluate(async () => ({
            unbranded: await unresolvedCode(scheduler.abandonUnresolved({})),
            spreadCopy: await unresolvedCode(
              scheduler.abandonUnresolved({ ...forgeryChoice }),
            ),
            prototypeDerived: await unresolvedCode(
              scheduler.abandonUnresolved(Object.create(forgeryChoice)),
            ),
            freshSymbol: await unresolvedCode(
              scheduler.abandonUnresolved(Object.freeze({ [Symbol()]: true })),
            ),
          })),
          {
            unbranded: 'invalid_selection',
            spreadCopy: 'invalid_selection',
            prototypeDerived: 'invalid_selection',
            freshSymbol: 'invalid_selection',
          },
        );
        assert.deepEqual(
          await page.evaluate(() => unresolvedObserve()),
          before,
        );
        // A token from another scheduler instance over the same store is unknown here.
        assert.equal(
          await page.evaluate(() =>
            unresolvedCode(
              new deliveryApi.DeliveryScheduler(store, {
                context: deliveryContext,
                random: () => 0,
                observe() {},
                revalidate: async () => ({ kind: 'authorized' }),
              }).abandonUnresolved(forgeryChoice),
            ),
          ),
          'invalid_selection',
          'another scheduler token',
        );
        assert.deepEqual(
          await page.evaluate(() => unresolvedObserve()),
          before,
        );
        // The genuine token still commits once, and is then consumed.
        assert.deepEqual(
          await page.evaluate(() => scheduler.abandonUnresolved(forgeryChoice)),
          { kind: 'abandoned_unknown', removedDependents: 0 },
        );
        const committed = await page.evaluate(() => unresolvedObserve());
        assert.equal(
          committed.storage.rows.length,
          before.storage.rows.length - 1,
        );
        assert.equal(
          await page.evaluate(() =>
            unresolvedCode(scheduler.abandonUnresolved(forgeryChoice)),
          ),
          'invalid_selection',
          'consumed token',
        );
        assert.deepEqual(
          await page.evaluate(() => unresolvedObserve()),
          committed,
        );
        assert.equal(harness.requests().length, 1);
        break;
      }
      case 'unresolved-stale-selection': {
        harness.setErrorBody(notFoundBody);
        harness.configure('status', 404);
        // A wake between selection and entry retires the token.
        await page.evaluate((tag) => unresolvedStart(tag), 'a');
        await unresolvedWaitPause(page, 'a', 'blocked', 'access_unavailable');
        await page.evaluate(async () => {
          globalThis.staleChoice = await scheduler.selectUnresolved(
            unresolvedKey('a'),
            'offline',
          );
          scheduler.wake(unresolvedKey('a'));
        });
        // A surviving token would be refused by the wake-marker check with the
        // same code. The retirement is checked on the registry and the lane.
        assert.equal(
          await page.evaluate(() =>
            scheduler.unresolved.tokens.has(staleChoice),
          ),
          false,
          'a wake retires the selection from the token map',
        );
        assert.equal(
          await page.evaluate(() => unresolvedLane('a').unresolvedSelection),
          undefined,
          'a wake clears the lane selection',
        );
        await assertAbandonRefused(harness, page, 'wake before entry');
        // A wake inside the transaction is seen by the transaction's own check.
        await page.evaluate((tag) => unresolvedStart(tag), 'b');
        await unresolvedWaitPause(page, 'b', 'blocked', 'access_unavailable');
        await page.evaluate(async () => {
          globalThis.staleChoice = await scheduler.selectUnresolved(
            unresolvedKey('b'),
            'offline',
          );
          unresolvedWakeInside('b');
        });
        const beforeInside = await page.evaluate(() => unresolvedObserve());
        assert.equal(
          await page.evaluate(async () => {
            const code = await unresolvedCode(
              scheduler.abandonUnresolved(staleChoice),
            );
            restoreUnresolvedDelete();
            return code;
          }),
          'invalid_selection',
          'wake inside the transaction',
        );
        assert.deepEqual(
          await page.evaluate(() => unresolvedObserve()),
          beforeInside,
        );
        // A dependent enqueued after selection changes the closure.
        await page.evaluate((tag) => unresolvedStart(tag), 'c');
        await unresolvedWaitPause(page, 'c', 'blocked', 'access_unavailable');
        await page.evaluate(async () => {
          globalThis.staleChoice = await scheduler.selectUnresolved(
            unresolvedKey('c'),
            'offline',
          );
          await unresolvedDependentOfHead('c');
        });
        await assertAbandonRefused(
          harness,
          page,
          'dependent enqueued after selection',
        );
        // A numeric-base row is a survivor, outside the closure. Written after
        // selection, it still changes the lane the selection captured.
        await page.evaluate((tag) => unresolvedStart(tag), 'd');
        await unresolvedWaitPause(page, 'd', 'blocked', 'access_unavailable');
        await page.evaluate(async () => {
          globalThis.staleChoice = await scheduler.selectUnresolved(
            unresolvedKey('d'),
            'offline',
          );
          await unresolvedEnqueue('d');
        });
        await assertAbandonRefused(
          harness,
          page,
          'survivor enqueued after selection',
        );
        // The store's own ACK of the head after selection changes the closure. A
        // trim removes only acknowledged rows, so it cannot interleave with a
        // blocked prepared head unless the ACK happens first.
        await page.evaluate((tag) => unresolvedStart(tag), 'd');
        await unresolvedWaitPause(page, 'd', 'blocked', 'access_unavailable');
        await page.evaluate(async () => {
          globalThis.staleChoice = await scheduler.selectUnresolved(
            unresolvedKey('d'),
            'offline',
          );
        });
        await page.evaluate(() => unresolvedAcknowledge('d'));
        await assertAbandonRefused(
          harness,
          page,
          'ACK written after selection',
        );
        // Another lane's delivery resume renews the epoch after selection.
        harness.setErrorBody(unclassifiedBody);
        harness.configure('status', 400);
        await page.evaluate((tag) => unresolvedStart(tag), '8');
        await unresolvedWaitPause(
          page,
          '8',
          'retry_paused',
          'unclassified_error_response',
        );
        harness.setErrorBody(notFoundBody);
        harness.configure('status', 404);
        await page.evaluate((tag) => unresolvedStart(tag), '7');
        await unresolvedWaitPause(page, '7', 'blocked', 'access_unavailable');
        await page.evaluate(async () => {
          globalThis.staleChoice = await scheduler.selectUnresolved(
            unresolvedKey('7'),
            'offline',
          );
          globalThis.epochBeforeRenewal = scheduler.epoch;
          await scheduler.resumeDelivery(unresolvedLane('8').pause.selection);
        });
        await page.waitForFunction(
          () =>
            scheduler.epoch > epochBeforeRenewal &&
            unresolvedLane('8')?.pause?.kind === 'blocked',
        );
        await assertAbandonRefused(
          harness,
          page,
          'epoch renewed after selection',
        );
        // A reselection on the same lane retires the older token.
        await page.evaluate((tag) => unresolvedStart(tag), '9');
        await unresolvedWaitPause(page, '9', 'blocked', 'access_unavailable');
        await page.evaluate(async () => {
          globalThis.olderChoice = await scheduler.selectUnresolved(
            unresolvedKey('9'),
            'offline',
          );
          globalThis.staleChoice = await scheduler.selectUnresolved(
            unresolvedKey('9'),
            'offline',
          );
        });
        const beforeOlder = await page.evaluate(() => unresolvedObserve());
        assert.equal(
          await page.evaluate(() =>
            unresolvedCode(scheduler.abandonUnresolved(olderChoice)),
          ),
          'invalid_selection',
          'stale-selection row: old token is retired by reselection',
        );
        assert.deepEqual(
          await page.evaluate(() => unresolvedObserve()),
          beforeOlder,
        );
        assert.deepEqual(
          await page.evaluate(() => scheduler.abandonUnresolved(staleChoice)),
          { kind: 'abandoned_unknown', removedDependents: 0 },
        );
        break;
      }
      case 'unresolved-replaced-pause': {
        harness.setErrorBody(notFoundBody);
        harness.configure('status', 404);
        await page.evaluate((tag) => unresolvedStart(tag), 'a');
        await unresolvedWaitPause(page, 'a', 'blocked', 'access_unavailable');
        const before = await page.evaluate(() => unresolvedObserve());
        const requests = harness.requests().length;
        // An equal-content copy is not the pause the selection was made against.
        await page.evaluate(async () => {
          globalThis.staleChoice = await scheduler.selectUnresolved(
            unresolvedKey('a'),
            'offline',
          );
          unresolvedLane('a').pause = { ...unresolvedLane('a').pause };
        });
        assert.equal(
          await page.evaluate(() =>
            unresolvedCode(scheduler.abandonUnresolved(staleChoice)),
          ),
          'invalid_selection',
          'refreshed or replaced pause rejects',
        );
        await page.evaluate((tag) => {
          unresolvedLane(tag).pause = unresolvedPauses.get(tag);
        }, 'a');
        assert.equal(await page.evaluate(() => unresolvedPaused('a')), true);
        assert.deepEqual(
          await page.evaluate(() => unresolvedObserve()),
          before,
        );
        // A replacement with different content is refused the same way.
        await page.evaluate(async () => {
          globalThis.staleChoice = await scheduler.selectUnresolved(
            unresolvedKey('a'),
            'offline',
          );
          unresolvedLane('a').pause = {
            kind: 'blocked',
            failure: { reason: 'access_unavailable', status: 403 },
          };
        });
        assert.equal(
          await page.evaluate(() =>
            unresolvedCode(scheduler.abandonUnresolved(staleChoice)),
          ),
          'invalid_selection',
          'replaced pause with different content rejects',
        );
        await page.evaluate((tag) => {
          unresolvedLane(tag).pause = unresolvedPauses.get(tag);
        }, 'a');
        assert.deepEqual(
          await page.evaluate(() => unresolvedObserve()),
          before,
        );
        // The restored lane selects and commits, with no request made.
        assert.deepEqual(
          await page.evaluate(async () => {
            globalThis.staleChoice = await scheduler.selectUnresolved(
              unresolvedKey('a'),
              'offline',
            );
            return scheduler.abandonUnresolved(staleChoice);
          }),
          { kind: 'abandoned_unknown', removedDependents: 0 },
        );
        assert.equal(harness.requests().length, requests);
        break;
      }
      case 'unresolved-selection-guards': {
        harness.setErrorBody(notFoundBody);
        harness.configure('status', 404);
        await page.evaluate((tag) => unresolvedStart(tag), 'a');
        await unresolvedWaitPause(page, 'a', 'blocked', 'access_unavailable');
        assert.equal(
          await page.evaluate(() => unresolvedWithFlag('a', 'queued', true)),
          'invalid_selection',
          'queued lane',
        );
        assert.equal(
          await page.evaluate(() => unresolvedAsActive('a')),
          'invalid_selection',
          'active lane',
        );
        // Control: a head with an unprepared dependent is accepted. Acknowledged
        // or prepared dependents are not stored states, so they are not forged.
        await page.evaluate((tag) => unresolvedStart(tag), 'b');
        await unresolvedWaitPause(page, 'b', 'blocked', 'access_unavailable');
        await page.evaluate((tag) => unresolvedDependentOfHead(tag), 'b');
        assert.equal(
          await page.evaluate(() => unresolvedSelect('b')),
          'accepted',
          'control: unprepared dependent',
        );
        break;
      }
      case 'unresolved-foreign-owner':
      case 'unresolved-foreign-actor':
      case 'unresolved-foreign-binding': {
        // A key naming another owner, actor or storage epoch is refused. The
        // scheduler fails closed on its first failed check, so each runs alone.
        const index = {
          'unresolved-foreign-owner': 0,
          'unresolved-foreign-actor': 2,
          'unresolved-foreign-binding': 3,
        }[mode];
        harness.setErrorBody(notFoundBody);
        harness.configure('status', 404);
        await page.evaluate((tag) => unresolvedStart(tag), 'a');
        await unresolvedWaitPause(page, 'a', 'blocked', 'access_unavailable');
        const before = await page.evaluate(() => unresolvedObserve());
        const requests = harness.requests().length;
        assert.equal(
          await page.evaluate((index) => {
            const replacement = {
              0: 'other-owner',
              2: 'other-actor',
              3: unresolvedKey('a')[3] + 1,
            };
            const key = [...unresolvedKey('a')];
            key[index] = replacement[index];
            return unresolvedCode(scheduler.selectUnresolved(key, 'offline'));
          }, index),
          'owner_not_ready',
        );
        assert.deepEqual(
          await page.evaluate(() => unresolvedObserve()),
          before,
        );
        assert.equal(harness.requests().length, requests);
        break;
      }
      case 'unresolved-accounting': {
        // Rules 7-9 and 11: one readwrite transaction removes the head and its
        // closure, counters move once, and a full removal leaves no tombstone.
        // The other lane keeps its pause, its counters and its selection token.
        harness.setErrorBody(notFoundBody);
        harness.configure('status', 404);
        await page.evaluate(async () => {
          const head = await unresolvedEnqueue('c');
          const first = await unresolvedDependent('c', head.sequence);
          await unresolvedDependent('c', first.sequence);
          await unresolvedEnqueue('d');
          scheduler.wake(unresolvedKey('c'));
          scheduler.wake(unresolvedKey('d'));
        });
        await unresolvedWaitPause(page, 'c', 'blocked', 'access_unavailable');
        await unresolvedWaitPause(page, 'd', 'blocked', 'access_unavailable');
        const requests = harness.requests().length;
        const start = await page.evaluate(() => unresolvedLedger());
        await page.evaluate(async () => {
          globalThis.otherChoice = await scheduler.selectUnresolved(
            unresolvedKey('d'),
            'offline',
          );
          globalThis.choice = await scheduler.selectUnresolved(
            unresolvedKey('c'),
            'offline',
          );
        });
        const before = await page.evaluate(() => unresolvedLedger());
        assert.equal(
          before.fetches,
          start.fetches,
          'offline selection makes no network request',
        );
        const outcome = await page.evaluate(() =>
          unresolvedTransactions(() => scheduler.abandonUnresolved(choice)),
        );
        assert.deepEqual(outcome.result, {
          kind: 'abandoned_unknown',
          removedDependents: 2,
        });
        assert.deepEqual(outcome.modes, ['readwrite']);
        const after = await page.evaluate(() => unresolvedLedger());
        const removed = laneRowsOf(before.storage.rows, 'c');
        assert.equal(removed.length, 3);
        assert.deepEqual(laneRowsOf(after.storage.rows, 'c'), []);
        assert.equal(after.storage.usage.rows, before.storage.usage.rows - 3);
        assert.equal(
          after.storage.usage.bytes,
          before.storage.usage.bytes - bytesOf(removed),
        );
        assert.equal(checkpointOf(before, 'c').bytes, bytesOf(removed));
        assert.equal(checkpointOf(after, 'c'), undefined);
        assert.deepEqual(
          { aggregate: after.aggregate, epoch: after.epoch },
          { aggregate: before.aggregate, epoch: before.epoch },
          'Accounting once: before and after counters are equal',
        );
        assert.deepEqual(
          laneRowsOf(after.storage.rows, 'd'),
          laneRowsOf(before.storage.rows, 'd'),
        );
        assert.deepEqual(checkpointOf(after, 'd'), checkpointOf(before, 'd'));
        assert.deepEqual(after.lanes.d, before.lanes.d);
        assert.equal(await page.evaluate(() => unresolvedPaused('d')), true);
        assert.equal(
          after.fetches,
          before.fetches,
          'offline abandonment makes no network request',
        );
        assert.deepEqual(after.authorizeModes, [
          'offline',
          'offline',
          'offline',
        ]);
        assert.equal(harness.requests().length, requests);
        assert.equal(
          await page.evaluate(() =>
            scheduler.lanes.has(JSON.stringify(unresolvedKey('c'))),
          ),
          false,
          'full removal leaves no tombstone',
        );
        assert.deepEqual(
          await page.evaluate(() => {
            scheduler.wake(unresolvedKey('c'));
            const incarnation = scheduler.lanes.get(
              JSON.stringify(unresolvedKey('c')),
            );
            return {
              present: incarnation !== undefined,
              failures: incarnation?.failures ?? null,
              pause: incarnation?.pause ?? null,
            };
          }),
          { present: true, failures: 0, pause: null },
          'a later wake creates an incarnation with no failures and no pause',
        );
        assert.deepEqual(
          await page.evaluate(() => scheduler.abandonUnresolved(otherChoice)),
          { kind: 'abandoned_unknown', removedDependents: 0 },
          'the other lane keeps its selection token',
        );
        break;
      }
      case 'unresolved-online-read': {
        // Rule 6: online mode makes one binding read per selection and one per
        // action. A failed read fails closed, deletes nothing and never falls back
        // to offline. Offline mode makes no network request at all.
        harness.setErrorBody(notFoundBody);
        harness.configure('status', 404);
        await page.evaluate((tag) => unresolvedStart(tag), 'c');
        await unresolvedWaitPause(page, 'c', 'blocked', 'access_unavailable');
        const start = await page.evaluate(() => unresolvedLedger());
        harness.configureBinding('503');
        assert.equal(
          await page.evaluate(() =>
            unresolvedCode(
              scheduler.selectUnresolved(unresolvedKey('c'), 'online'),
            ),
          ),
          'unavailable',
          'failed read deletes nothing and makes no offline transaction',
        );
        const refusedSelection = await page.evaluate(() => unresolvedLedger());
        assert.equal(refusedSelection.bindingReads, start.bindingReads + 1);
        assert.deepEqual(
          withoutReads(refusedSelection),
          withoutReads(start),
          'failed read deletes nothing and makes no offline transaction',
        );
        assert.deepEqual(refusedSelection.authorizeModes, ['online']);
        harness.configureBinding('normal');
        await page.evaluate(async () => {
          globalThis.choice = await scheduler.selectUnresolved(
            unresolvedKey('c'),
            'online',
          );
        });
        const selected = await page.evaluate(() => unresolvedLedger());
        assert.equal(selected.bindingReads, refusedSelection.bindingReads + 1);
        harness.configureBinding('503');
        assert.equal(
          await page.evaluate(() =>
            unresolvedCode(scheduler.abandonUnresolved(choice)),
          ),
          'unavailable',
          'failed read deletes nothing and makes no offline transaction',
        );
        const refusedAction = await page.evaluate(() => unresolvedLedger());
        assert.equal(refusedAction.bindingReads, selected.bindingReads + 1);
        assert.deepEqual(
          withoutReads(refusedAction),
          withoutReads(selected),
          'failed read deletes nothing and makes no offline transaction',
        );
        assert.deepEqual(refusedAction.authorizeModes, [
          'online',
          'online',
          'online',
        ]);
        harness.configureBinding('normal');
        assert.deepEqual(
          await page.evaluate(async () => {
            globalThis.choice = await scheduler.selectUnresolved(
              unresolvedKey('c'),
              'online',
            );
            return scheduler.abandonUnresolved(choice);
          }),
          { kind: 'abandoned_unknown', removedDependents: 0 },
        );
        const committed = await page.evaluate(() => unresolvedLedger());
        assert.equal(committed.bindingReads, refusedAction.bindingReads + 2);
        assert.deepEqual(committed.authorizeModes, [
          'online',
          'online',
          'online',
          'online',
          'online',
        ]);
        assert.deepEqual(laneRowsOf(committed.storage.rows, 'c'), []);
        await page.evaluate((tag) => unresolvedStart(tag), 'd');
        await unresolvedWaitPause(page, 'd', 'blocked', 'access_unavailable');
        const offlineStart = await page.evaluate(() => unresolvedLedger());
        assert.deepEqual(
          await page.evaluate(async () => {
            globalThis.offlineChoice = await scheduler.selectUnresolved(
              unresolvedKey('d'),
              'offline',
            );
            return scheduler.abandonUnresolved(offlineChoice);
          }),
          { kind: 'abandoned_unknown', removedDependents: 0 },
        );
        const offline = await page.evaluate(() => unresolvedLedger());
        assert.equal(
          offline.fetches,
          offlineStart.fetches,
          'offline mode makes zero network requests',
        );
        assert.equal(offline.bindingReads, offlineStart.bindingReads);
        assert.deepEqual(offline.authorizeModes, [
          ...committed.authorizeModes,
          'offline',
          'offline',
        ]);
        assert.deepEqual(laneRowsOf(offline.storage.rows, 'd'), []);
        break;
      }
      case 'unresolved-survivors': {
        // Rules 9-11: survivors leave as one lane-scope resume choice. Their keys,
        // bases and failure count are unchanged, nothing is sent until that fresh
        // token is used once, and the retired token is refused.
        harness.setErrorBody(notFoundBody);
        harness.configure('status', 404);
        await page.evaluate(async () => {
          const head = await unresolvedEnqueue('c');
          const first = await unresolvedDependent('c', head.sequence);
          await unresolvedDependent('c', first.sequence);
          globalThis.survivorRow = await unresolvedEnqueue('c');
          scheduler.wake(unresolvedKey('c'));
        });
        await unresolvedWaitPause(page, 'c', 'blocked', 'access_unavailable');
        // Lane-scope retry exhaustion: the eighth retryable failure pauses the lane.
        await page.evaluate(() => {
          const current = unresolvedLane('c');
          delete current.pause;
          current.failures = 7;
        });
        harness.configure('status', 503);
        await page.evaluate(() => scheduler.wake(unresolvedKey('c')));
        await unresolvedWaitPause(page, 'c', 'retry_paused', 'server_error');
        const survivorKey = await page.evaluate(() => survivorRow.key);
        await page.evaluate(() => {
          globalThis.retiredResume = unresolvedLane('c').pause.selection;
        });
        const requests = harness.requests().length;
        await page.evaluate(async () => {
          globalThis.choice = await scheduler.selectUnresolved(
            unresolvedKey('c'),
            'offline',
          );
        });
        const before = await page.evaluate(() => unresolvedLedger());
        // A local preparation sets the one-use proof, and the pump clears it only
        // when it starts a run. Aggregate exhaustion pauses the lane before that
        // run, so the pause keeps the proof. This sets the field directly for the
        // stored head instead of replaying that route (this head was dispatched
        // earlier). The commit below deletes the head.
        await page.evaluate(async () => {
          const [head] = await store.lane(unresolvedKey('c'));
          unresolvedLane('c').preparedWithoutDispatch = head;
        });
        const outcome = await page.evaluate(() =>
          unresolvedTransactions(() => scheduler.abandonUnresolved(choice)),
        );
        assert.deepEqual(outcome.result, {
          kind: 'abandoned_unknown',
          removedDependents: 2,
        });
        assert.deepEqual(
          outcome.modes,
          ['readwrite'],
          'abandonment opens one transaction, with no second survivor read',
        );
        const after = await page.evaluate(() => unresolvedLedger());
        const closure = laneRowsOf(before.storage.rows, 'c');
        const removed = closure.slice(0, 3);
        const survivor = closure.at(-1);
        assert.equal(survivor.key, survivorKey);
        assert.deepEqual(laneRowsOf(after.storage.rows, 'c'), [survivor]);
        assert.equal(after.storage.usage.rows, before.storage.usage.rows - 3);
        assert.equal(
          after.storage.usage.bytes,
          before.storage.usage.bytes - bytesOf(removed),
        );
        assert.equal(checkpointOf(after, 'c').rows, 1);
        assert.equal(checkpointOf(after, 'c').bytes, survivor.bytes);
        assert.equal(
          after.lanes.c.failures,
          8,
          'survivors keep their keys, bases and failure count',
        );
        assert.deepEqual(
          await page.evaluate(() => ({
            kind: unresolvedLane('c').pause?.kind ?? null,
            reason: unresolvedLane('c').pause?.reason ?? null,
            scope: unresolvedLane('c').pause?.scope ?? null,
            publications: events.filter(
              (event) =>
                event.kind === 'paused' &&
                event.pause.reason === 'survivors_after_disposal',
            ).length,
            fresh: unresolvedLane('c').pause?.selection !== retiredResume,
          })),
          {
            kind: 'retry_paused',
            reason: 'survivors_after_disposal',
            scope: 'lane',
            publications: 1,
            fresh: true,
          },
          'one lane-scope survivor pause with a fresh resume token',
        );
        // The commit deleted the head that the one-use proof names, so the lane
        // that the survivors keep must not carry that proof into a later run.
        assert.equal(
          await page.evaluate(
            () => unresolvedLane('c').preparedWithoutDispatch === undefined,
          ),
          true,
          'the surviving lane drops the one-use proof of the deleted head',
        );
        assert.equal(
          await page.evaluate(() =>
            unresolvedCode(scheduler.resumeDelivery(retiredResume)),
          ),
          'invalid_selection',
          'the retired resume token is refused',
        );
        await page.evaluate(async () => {
          // A macrotask yield lets a dispatch that the wake starts reach the counter.
          scheduler.wake(unresolvedKey('c'));
          await new Promise((resolve) => setTimeout(resolve, 0));
        });
        const waiting = await page.evaluate(() => unresolvedLedger());
        assert.equal(
          harness.requests().length,
          requests,
          'zero survivor HTTP before resume',
        );
        assert.equal(
          waiting.fetches,
          after.fetches,
          'zero survivor HTTP before resume',
        );
        harness.configure('normal');
        const arrival = harness.waitRequest();
        const eventsBefore = await page.evaluate(() => events.length);
        assert.equal(
          await page.evaluate(() =>
            scheduler.resumeDelivery(unresolvedLane('c').pause.selection).then(
              () => 'resumed',
              (error) => error.code,
            ),
          ),
          'resumed',
          'survivor resume succeeds once without a refresh rejection',
        );
        await arrival;
        assert.deepEqual(
          harness.requests().map((request) => ({
            method: request.method,
            idempotencyKey: request.headers['idempotency-key'],
          })),
          [{ method: 'PUT', idempotencyKey: survivorKey }],
          'survivor resume succeeds once without a refresh rejection',
        );
        // The acknowledged lane queues once more; with no rows left that pass is
        // the idle attempt that retires it, so waiting for it fixes the sequence.
        await page.waitForFunction(() =>
          events.some(
            (event) => event.kind === 'attempt' && event.result.kind === 'idle',
          ),
        );
        assert.deepEqual(
          await page.evaluate(
            (from) => ({
              attempts: events
                .slice(from)
                .filter((event) => event.kind === 'attempt')
                .map((event) => event.result.kind),
              pauses: events
                .slice(from)
                .filter((event) => event.kind === 'paused').length,
            }),
            eventsBefore,
          ),
          { attempts: ['server_acknowledged', 'idle'], pauses: 0 },
          'survivor resume succeeds once without a refresh rejection',
        );
        const finished = await page.evaluate(() => unresolvedLedger());
        assert.deepEqual(laneRowsOf(finished.storage.rows, 'c'), []);
        break;
      }
      case 'unresolved-abort': {
        // A failed commit keeps the gate and its resume token, sends nothing and
        // leaves rows, checkpoint and counters intact. It consumes only its own
        // choice, so a fresh selection commits once.
        harness.setErrorBody(notFoundBody);
        harness.configure('status', 404);
        await page.evaluate(async () => {
          const head = await unresolvedEnqueue('c');
          await unresolvedDependent('c', head.sequence);
          scheduler.wake(unresolvedKey('c'));
        });
        await unresolvedWaitPause(page, 'c', 'blocked', 'access_unavailable');
        // Lane-scope retry exhaustion: the eighth retryable failure pauses the lane.
        await page.evaluate(() => {
          const current = unresolvedLane('c');
          delete current.pause;
          current.failures = 7;
        });
        harness.configure('status', 503);
        await page.evaluate(() => scheduler.wake(unresolvedKey('c')));
        await unresolvedWaitPause(page, 'c', 'retry_paused', 'server_error');
        await page.evaluate(() => {
          globalThis.gatePause = unresolvedLane('c').pause;
          globalThis.gateToken = gatePause.selection;
        });
        const requests = harness.requests().length;
        await page.evaluate(async () => {
          globalThis.choice = await scheduler.selectUnresolved(
            unresolvedKey('c'),
            'offline',
          );
        });
        const before = await page.evaluate(() => unresolvedLedger());
        const failed = await page.evaluate(async () => {
          faultPut('usage');
          const code = await unresolvedCode(
            scheduler.abandonUnresolved(choice),
          );
          restorePut();
          return {
            code,
            gate: {
              pause: unresolvedLane('c')?.pause === gatePause,
              token: unresolvedLane('c')?.pause?.selection === gateToken,
              // The resume token is usable only while the delivery registry holds it.
              registered: scheduler.deliveries.tokens.has(gateToken),
            },
          };
        });
        assert.deepEqual(
          failed.gate,
          { pause: true, token: true, registered: true },
          'Abort before counters: the gate and resume token remain',
        );
        assert.equal(failed.code, 'storage_failed');
        const aborted = await page.evaluate(() => unresolvedLedger());
        assert.deepEqual(
          withoutReads(aborted),
          withoutReads(before),
          'Abort before counters: rows, checkpoint and counters are intact',
        );
        assert.equal(aborted.fetches, before.fetches);
        assert.equal(harness.requests().length, requests);
        assert.equal(
          await page.evaluate(() =>
            unresolvedCode(scheduler.abandonUnresolved(choice)),
          ),
          'invalid_selection',
          'the failed choice is consumed',
        );
        const committed = await page.evaluate(async () => {
          globalThis.choice = await scheduler.selectUnresolved(
            unresolvedKey('c'),
            'offline',
          );
          return unresolvedTransactions(() =>
            scheduler.abandonUnresolved(choice),
          );
        });
        assert.deepEqual(committed.result, {
          kind: 'abandoned_unknown',
          removedDependents: 1,
        });
        assert.deepEqual(committed.modes, ['readwrite']);
        const after = await page.evaluate(() => unresolvedLedger());
        const removed = laneRowsOf(before.storage.rows, 'c');
        assert.equal(removed.length, 2);
        assert.deepEqual(laneRowsOf(after.storage.rows, 'c'), []);
        assert.equal(after.storage.usage.rows, before.storage.usage.rows - 2);
        assert.equal(
          after.storage.usage.bytes,
          before.storage.usage.bytes - bytesOf(removed),
        );
        assert.equal(checkpointOf(after, 'c'), undefined);
        assert.deepEqual(
          { aggregate: after.aggregate, epoch: after.epoch },
          { aggregate: before.aggregate, epoch: before.epoch },
          'Accounting once: before and after counters are equal',
        );
        assert.equal(harness.requests().length, requests);
        break;
      }
      case 'unresolved-bound': {
        // The lane cap is 64 rows and 2 MiB. The closure fills the cap exactly,
        // and one readwrite transaction removes all 64 rows with exact deltas.
        harness.setErrorBody(notFoundBody);
        harness.configure('status', 404);
        const closureBytes = await page.evaluate(async () => {
          const quoted = (length) => JSON.stringify('x'.repeat(length - 2));
          const fill = 28000;
          const head = await unresolvedEnqueue('c');
          let tail = head;
          let total = head.bytes;
          for (let index = 0; index < 62; index += 1) {
            tail = await unresolvedDependent('c', tail.sequence, {
              text: quoted(fill),
            });
            total += tail.bytes;
          }
          // Stored bytes are the text plus a fixed overhead per row; the last row
          // takes exactly the lane budget that is left.
          const overhead = tail.bytes - tail.text.length;
          const last = await unresolvedDependent('c', tail.sequence, {
            text: quoted(2 * 1024 * 1024 - total - overhead),
          });
          scheduler.wake(unresolvedKey('c'));
          return total + last.bytes;
        });
        assert.equal(closureBytes, 2 * 1024 * 1024);
        await unresolvedWaitPause(page, 'c', 'blocked', 'access_unavailable');
        const before = await page.evaluate(() => unresolvedLedger());
        const closure = laneRowsOf(before.storage.rows, 'c');
        assert.equal(closure.length, 64);
        assert.equal(bytesOf(closure), 2 * 1024 * 1024);
        await page.evaluate(async () => {
          globalThis.choice = await scheduler.selectUnresolved(
            unresolvedKey('c'),
            'offline',
          );
        });
        const outcome = await page.evaluate(() =>
          unresolvedTransactions(() => scheduler.abandonUnresolved(choice)),
        );
        assert.deepEqual(outcome.result, {
          kind: 'abandoned_unknown',
          removedDependents: 63,
        });
        assert.deepEqual(outcome.modes, ['readwrite']);
        const after = await page.evaluate(() => unresolvedLedger());
        assert.deepEqual(laneRowsOf(after.storage.rows, 'c'), []);
        assert.equal(after.storage.usage.rows, before.storage.usage.rows - 64);
        assert.equal(
          after.storage.usage.bytes,
          before.storage.usage.bytes - 2 * 1024 * 1024,
        );
        assert.equal(checkpointOf(after, 'c'), undefined);
        break;
      }
      case 'unresolved-offline-authority': {
        // Rule 15's authority, refused by the host before the action and by a change
        // that lands after the last pre-transaction check. A failed check closes the
        // scheduler, so each fact that reaches one has a page of its own, and the
        // in-transaction change runs last.
        await unresolvedBlockedLane(harness, page, 'a');
        await assertOfflineRefused(
          harness,
          page,
          'a',
          'host-refusal',
          'host refusal before the action keeps rows',
        );
        // The change lands as the readwrite intents transaction opens, after the
        // last pre-transaction check, so only the transaction's own check can refuse.
        await page.evaluate(async () => {
          unresolvedArrangers.restore();
          globalThis.unresolvedChoice = await scheduler.selectUnresolved(
            unresolvedKey('a'),
            'offline',
          );
        });
        const beforeOpen = await page.evaluate(() => unresolvedObserve());
        const requestsOpen = harness.requests().length;
        const opened = await page.evaluate(async () => {
          const open = IDBDatabase.prototype.transaction;
          IDBDatabase.prototype.transaction = function (...args) {
            if (args[1] === 'readwrite' && [args[0]].flat().includes('intents'))
              currentAuthority = Symbol();
            return Reflect.apply(open, this, args);
          };
          try {
            return await unresolvedCode(
              scheduler.abandonUnresolved(unresolvedChoice),
            );
          } finally {
            IDBDatabase.prototype.transaction = open;
          }
        });
        assert.equal(
          opened,
          'owner_not_ready',
          'Offline authority: change between selection and transaction rejects',
        );
        assert.deepEqual(
          await page.evaluate(() => unresolvedObserve()),
          beforeOpen,
          'Offline authority: change between selection and transaction rejects',
        );
        assert.equal(harness.requests().length, requestsOpen);
        break;
      }
      case 'unresolved-offline-logout':
      case 'unresolved-offline-lock-loss':
      case 'unresolved-offline-generation':
      case 'unresolved-offline-profile':
      case 'unresolved-offline-invalidated':
      case 'unresolved-offline-check-throws':
      case 'unresolved-offline-aborted':
      case 'unresolved-offline-detached': {
        // One lost authority fact between selection and the action, on its own page.
        await unresolvedBlockedLane(harness, page, 'a');
        await assertOfflineRefused(
          harness,
          page,
          'a',
          mode.slice('unresolved-offline-'.length),
          `${mode} keeps rows`,
        );
        break;
      }
      case 'unresolved-offline-closed': {
        // Closing after selection retires the token and the lane's selection. The
        // blocked pause stays, and the abandonment fails closed with nothing changed.
        await unresolvedBlockedLane(harness, page, 'a');
        await page.evaluate(async () => {
          globalThis.unresolvedChoice = await scheduler.selectUnresolved(
            unresolvedKey('a'),
            'offline',
          );
        });
        const before = await page.evaluate(() => unresolvedObserve());
        const requests = harness.requests().length;
        assert.deepEqual(
          await page.evaluate(() => {
            scheduler.close();
            return {
              paused: unresolvedPaused('a'),
              selection: unresolvedLane('a').unresolvedSelection !== undefined,
              token: scheduler.unresolved.tokens.has(unresolvedChoice),
            };
          }),
          { paused: true, selection: false, token: false },
        );
        assert.equal(
          await page.evaluate(() =>
            unresolvedCode(scheduler.abandonUnresolved(unresolvedChoice)),
          ),
          'owner_not_ready',
        );
        assert.deepEqual(
          await page.evaluate(() => unresolvedObserve()),
          before,
        );
        assert.equal(harness.requests().length, requests);
        break;
      }
      case 'unresolved-offline-mid-transaction': {
        // Authentication loss after the first intents delete succeeds: the next
        // in-transaction check fails through the aborted fence signal, so the
        // transaction aborts and the first delete is rolled back.
        await unresolvedBlockedLane(harness, page, 'a');
        await page.evaluate(async () => {
          globalThis.unresolvedChoice = await scheduler.selectUnresolved(
            unresolvedKey('a'),
            'offline',
          );
        });
        const before = await page.evaluate(() => unresolvedObserve());
        const requests = harness.requests().length;
        const outcome = await page.evaluate(async () => {
          const deletion = IDBObjectStore.prototype.delete;
          let deletes = 0;
          globalThis.restoreUnresolvedDelete = () => {
            IDBObjectStore.prototype.delete = deletion;
          };
          IDBObjectStore.prototype.delete = function (...args) {
            const pending = deletion.apply(this, args);
            if (this.name === 'intents')
              pending.addEventListener(
                'success',
                () => {
                  deletes += 1;
                  deliveryContext.invalidateAuthentication();
                },
                { once: true },
              );
            return pending;
          };
          const code = await unresolvedCode(
            scheduler.abandonUnresolved(unresolvedChoice),
          );
          restoreUnresolvedDelete();
          return { code, deletes };
        });
        assert.deepEqual(outcome, { code: 'owner_not_ready', deletes: 1 });
        assert.deepEqual(
          await page.evaluate(() => unresolvedObserve()),
          before,
        );
        assert.equal(harness.requests().length, requests);
        break;
      }
      case 'unresolved-online-401':
      case 'unresolved-online-select-401':
      case 'unresolved-online-binding':
      case 'unresolved-online-select-binding': {
        // An online binding fault during the selection or the action closes the
        // scheduler, deletes nothing and keeps the lane's pause. A 401 also
        // invalidates authentication; a 409 outbox_binding_changed does not.
        const atSelection = mode.includes('-select-');
        const unauthorized = mode.endsWith('-401');
        await unresolvedBlockedLane(harness, page, 'a');
        await page.evaluate(() => {
          authInvalidationAborts = false;
        });
        if (!atSelection)
          await page.evaluate(async () => {
            globalThis.unresolvedChoice = await scheduler.selectUnresolved(
              unresolvedKey('a'),
              'online',
            );
          });
        const before = await page.evaluate(() => unresolvedObserve());
        const requests = harness.requests().length;
        harness.configureBinding(unauthorized ? '401stalled' : '409binding');
        await page.evaluate((atSelection) => {
          globalThis.onlineOutcome = (
            atSelection
              ? scheduler.selectUnresolved(unresolvedKey('a'), 'online')
              : scheduler.abandonUnresolved(unresolvedChoice)
          ).then(
            () => 'unexpected',
            (error) => ({
              code: error.code,
              status: error.status,
              retryAfterSeconds: error.retryAfterSeconds,
            }),
          );
        }, atSelection);
        if (unauthorized) {
          await page.waitForFunction(() =>
            cleanupObservedStatuses.includes(401),
          );
          assert.equal(
            await page.evaluate(() => locallyAuthenticated),
            false,
            'a 401 invalidates authentication',
          );
        }
        const failure = await page.evaluate(() => onlineOutcome);
        const raw = await page.evaluate(() => cleanupRawReadDiagnostic);
        assert.equal(failure.status, unauthorized ? 401 : 409);
        assert.equal(failure.code, raw.code);
        if (!unauthorized) assert.equal(raw.code, 'outbox_binding_changed');
        assert.equal(
          await page.evaluate(() => scheduler.fence.signal.aborted),
          true,
          'the online fault closes the scheduler',
        );
        assert.deepEqual(
          await page.evaluate(() => cleanupAuthorizeModes),
          atSelection ? ['online'] : ['online', 'online'],
        );
        assert.deepEqual(
          await page.evaluate(() => unresolvedObserve()),
          before,
          'the online fault deletes nothing',
        );
        assert.equal(await page.evaluate(() => unresolvedPaused('a')), true);
        assert.equal(harness.requests().length, requests);
        harness.configureBinding('normal');
        break;
      }
      case 'unresolved-resume-read-failed': {
        // A binding read that fails during a resume replaces the retry pause
        // with a token-refreshed copy. That replacement retires the lane's
        // disposal selection (ADR 0049 rules 5 and 16).
        harness.configure('status', 404);
        await page.evaluate((tag) => unresolvedStart(tag), 'a');
        await unresolvedWaitPause(page, 'a', 'blocked', 'access_unavailable');
        await unresolvedRetryPause(harness, page, 'a');
        await page.evaluate(async () => {
          globalThis.staleChoice = await scheduler.selectUnresolved(
            unresolvedKey('a'),
            'offline',
          );
        });
        assert.equal(
          await page.evaluate(() =>
            scheduler.unresolved.tokens.has(staleChoice),
          ),
          true,
          'the selection is current before the resume',
        );
        harness.configureBinding('503');
        const failureStatus = await page.evaluate(() =>
          scheduler.resumeDelivery(unresolvedLane('a').pause.selection).then(
            () => 'unexpected',
            (error) => error.status,
          ),
        );
        harness.configureBinding('normal');
        assert.equal(failureStatus, 503, 'the binding read failed with 503');
        assert.equal(
          await page.evaluate(() => unresolvedLane('a').pause.kind),
          'retry_paused',
        );
        assert.equal(
          await page.evaluate(() => unresolvedLane('a').unresolvedSelection),
          undefined,
          'the replacement pause retires the lane selection',
        );
        assert.equal(
          await page.evaluate(() =>
            scheduler.unresolved.tokens.has(staleChoice),
          ),
          false,
          'a retired selection leaves the token map',
        );
        assert.equal(
          await page.evaluate(() =>
            unresolvedCode(scheduler.abandonUnresolved(staleChoice)),
          ),
          'invalid_selection',
          'a retired selection is refused',
        );
        assert.equal(
          await page.evaluate(() => unresolvedSelect('a')),
          'accepted',
          'the lane selects again after the failed resume',
        );
        break;
      }
      case 'unresolved-post-commit-close': {
        // Closing right after the commit decides the result: the committed removal
        // stays, nothing publishes or reconciles, and the fenced lane keeps its metadata.
        await unresolvedBlockedLane(harness, page, 'c');
        await page.evaluate(async () => {
          globalThis.unresolvedChoice = await scheduler.selectUnresolved(
            unresolvedKey('c'),
            'offline',
          );
        });
        const before = await page.evaluate(() => unresolvedObserve());
        const requests = harness.requests().length;
        const outcome = await page.evaluate(async () => {
          unresolvedCountReconciliation();
          unresolvedOnCommit(() => scheduler.close());
          const result = await scheduler
            .abandonUnresolved(unresolvedChoice)
            .then(
              (value) => value,
              (error) => ({ rejected: error.code }),
            );
          restoreUnresolvedTransaction();
          return {
            result,
            retained: scheduler.lanes.has(JSON.stringify(unresolvedKey('c'))),
            paused: unresolvedPaused('c'),
            reconciliations: reconciliationRequests,
          };
        });
        assert.deepEqual(outcome.result, {
          kind: 'abandoned_unknown',
          removedDependents: 0,
        });
        assert.equal(
          outcome.retained,
          true,
          'fenced scheduler keeps the lane metadata, no publication',
        );
        assert.equal(
          outcome.paused,
          true,
          'fenced scheduler keeps the lane metadata, no publication',
        );
        assert.equal(
          outcome.reconciliations,
          0,
          'no reconciliation after close',
        );
        const after = await page.evaluate(() => unresolvedObserve());
        assert.deepEqual(laneRowsOf(after.storage.rows, 'c'), []);
        assert.equal(after.storage.usage.rows, before.storage.usage.rows - 1);
        assert.equal(
          after.events,
          before.events,
          'fenced scheduler keeps the lane metadata, no publication',
        );
        assert.equal(harness.requests().length, requests);
        break;
      }
      case 'unresolved-post-commit-auth': {
        // Invalidating authentication right after the commit decides the result the
        // same way closing does: the removal stays, nothing publishes or reconciles.
        await unresolvedBlockedLane(harness, page, 'c');
        await page.evaluate(async () => {
          globalThis.unresolvedChoice = await scheduler.selectUnresolved(
            unresolvedKey('c'),
            'offline',
          );
        });
        const before = await page.evaluate(() => unresolvedObserve());
        const requests = harness.requests().length;
        const outcome = await page.evaluate(async () => {
          authInvalidationAborts = true;
          unresolvedCountReconciliation();
          unresolvedOnCommit(() => deliveryContext.invalidateAuthentication());
          const result = await scheduler
            .abandonUnresolved(unresolvedChoice)
            .then(
              (value) => value,
              (error) => ({ rejected: error.code }),
            );
          restoreUnresolvedTransaction();
          return {
            result,
            retained: scheduler.lanes.has(JSON.stringify(unresolvedKey('c'))),
            paused: unresolvedPaused('c'),
            fenced: scheduler.fence.signal.aborted,
            authenticated: locallyAuthenticated,
            reconciliations: reconciliationRequests,
          };
        });
        assert.deepEqual(outcome.result, {
          kind: 'abandoned_unknown',
          removedDependents: 0,
        });
        assert.equal(outcome.authenticated, false);
        assert.equal(outcome.fenced, true);
        assert.equal(
          outcome.retained,
          true,
          'fenced scheduler keeps the lane metadata, no publication',
        );
        assert.equal(outcome.paused, true);
        assert.equal(
          outcome.reconciliations,
          0,
          'no reconciliation after auth loss',
        );
        const after = await page.evaluate(() => unresolvedObserve());
        assert.deepEqual(laneRowsOf(after.storage.rows, 'c'), []);
        assert.equal(after.storage.usage.rows, before.storage.usage.rows - 1);
        assert.equal(after.events, before.events);
        assert.equal(harness.requests().length, requests);
        break;
      }
      case 'unresolved-post-commit-wake': {
        // Lane a holds the setup's marker row, which a reconciliation would deliver.
        // Parking it first leaves the reconciliation below nothing to send.
        await unresolvedBlockedLane(harness, page, 'a');
        // A wake between two transaction requests is refused with nothing deleted.
        await unresolvedBlockedLane(harness, page, 'b');
        await page.evaluate(async () => {
          globalThis.unresolvedChoice = await scheduler.selectUnresolved(
            unresolvedKey('b'),
            'offline',
          );
          unresolvedWakeInside('b');
        });
        const beforeInside = await page.evaluate(() => unresolvedObserve());
        assert.equal(
          await page.evaluate(async () => {
            const code = await unresolvedCode(
              scheduler.abandonUnresolved(unresolvedChoice),
            );
            restoreUnresolvedDelete();
            return code;
          }),
          'invalid_selection',
          'wake between two requests rejects with nothing deleted',
        );
        assert.deepEqual(
          await page.evaluate(() => unresolvedObserve()),
          beforeInside,
          'wake between two requests rejects with nothing deleted',
        );
        // A wake after the commit does not suppress retirement, and asks for one
        // reconciliation, which runs without dispatching anything.
        await unresolvedBlockedLane(harness, page, 'c');
        await page.evaluate(async () => {
          globalThis.unresolvedChoice = await scheduler.selectUnresolved(
            unresolvedKey('c'),
            'offline',
          );
        });
        const before = await page.evaluate(() => unresolvedObserve());
        const requests = harness.requests().length;
        const outcome = await page.evaluate(async () => {
          unresolvedCountReconciliation();
          unresolvedOnCommit(() => scheduler.wake(unresolvedKey('c')));
          const result = await scheduler
            .abandonUnresolved(unresolvedChoice)
            .then(
              (value) => value,
              (error) => ({ rejected: error.code }),
            );
          restoreUnresolvedTransaction();
          return {
            result,
            retained: scheduler.lanes.has(JSON.stringify(unresolvedKey('c'))),
            reconciliations: reconciliationRequests,
          };
        });
        assert.deepEqual(outcome.result, {
          kind: 'abandoned_unknown',
          removedDependents: 0,
        });
        assert.equal(
          outcome.retained,
          false,
          'post-commit wake does not suppress retirement',
        );
        assert.equal(
          outcome.reconciliations,
          1,
          'one reconciliation is requested',
        );
        await unresolvedQuiet(page);
        const after = await page.evaluate(() => unresolvedObserve());
        assert.deepEqual(laneRowsOf(after.storage.rows, 'c'), []);
        assert.equal(after.storage.usage.rows, before.storage.usage.rows - 1);
        assert.equal(harness.requests().length, requests);
        break;
      }
      case 'unresolved-post-commit-retired': {
        // The lane leaves the map after the commit's terminal event and before the
        // store call resolves. A wake marks it woken, then a newer incarnation
        // takes its key. The publication check must request no reconciliation and
        // must not remove that newer incarnation (ADR 0049 rule 11). The newer
        // attempt is held, so it stays in the map until the check.
        await unresolvedBlockedLane(harness, page, 'c');
        await page.evaluate(async () => {
          globalThis.unresolvedChoice = await scheduler.selectUnresolved(
            unresolvedKey('c'),
            'offline',
          );
        });
        const before = await page.evaluate(() => unresolvedObserve());
        const requests = harness.requests().length;
        const outcome = await page.evaluate(async () => {
          const id = JSON.stringify(unresolvedKey('c'));
          const stale = scheduler.lanes.get(id);
          let newer;
          unresolvedCountReconciliation();
          scheduler.kernel.run = () => new Promise(() => {});
          unresolvedOnCommit(() => {
            scheduler.wake(unresolvedKey('c'));
            scheduler.retireIdle(stale);
            scheduler.wake(unresolvedKey('c'));
            newer = scheduler.lanes.get(id);
          });
          const result = await scheduler
            .abandonUnresolved(unresolvedChoice)
            .then(
              (value) => value,
              (error) => ({ rejected: error.code }),
            );
          restoreUnresolvedTransaction();
          delete scheduler.kernel.run;
          const kept = scheduler.lanes.get(id);
          return {
            result,
            newerKept: kept !== undefined && kept === newer,
            reconciliations: reconciliationRequests,
          };
        });
        assert.deepEqual(outcome.result, {
          kind: 'abandoned_unknown',
          removedDependents: 0,
        });
        assert.equal(
          outcome.newerKept,
          true,
          'a retired incarnation must not remove the newer one under its key',
        );
        assert.equal(
          outcome.reconciliations,
          0,
          'a retired incarnation requests no reconciliation after commit',
        );
        await unresolvedQuiet(page);
        const after = await page.evaluate(() => unresolvedObserve());
        assert.deepEqual(laneRowsOf(after.storage.rows, 'c'), []);
        assert.equal(after.storage.usage.rows, before.storage.usage.rows - 1);
        assert.equal(
          after.events,
          before.events,
          'a retired incarnation publishes no event after commit',
        );
        assert.equal(harness.requests().length, requests);
        break;
      }
      case 'unresolved-pending-identity': {
        // A blocked lane (404) keeps one prepared head and no proof. Probes ask
        // for the same token inside the transaction and after its commit, while
        // the first operation is still pending.
        harness.configure('status', 404);
        await page.evaluate((tag) => unresolvedStart(tag), '1');
        await unresolvedWaitPause(page, '1', 'blocked', 'access_unavailable');
        const requests = harness.requests().length;
        const committed = await page.evaluate(async (tag) => {
          const choice = await scheduler.selectUnresolved(
            unresolvedKey(tag),
            'offline',
          );
          globalThis.consumedChoice = choice;
          const probes = [];
          let first;
          let armed = true;
          let insideProbed = false;
          const probe = (boundary) =>
            probes.push({
              boundary,
              shared: scheduler.abandonUnresolved(choice) === first,
            });
          const open = IDBDatabase.prototype.transaction;
          const deletion = IDBObjectStore.prototype.delete;
          IDBDatabase.prototype.transaction = function (names, mode, ...rest) {
            const transaction = Reflect.apply(open, this, [
              names,
              mode,
              ...rest,
            ]);
            if (armed && mode === 'readwrite') {
              armed = false;
              transaction.addEventListener(
                'complete',
                () => probe('post-commit'),
                { once: true },
              );
            }
            return transaction;
          };
          IDBObjectStore.prototype.delete = function (...args) {
            const pending = Reflect.apply(deletion, this, args);
            if (this.name === 'intents' && !insideProbed) {
              insideProbed = true;
              pending.addEventListener('success', () => probe('transaction'), {
                once: true,
              });
            }
            return pending;
          };
          try {
            first = scheduler.abandonUnresolved(choice);
            const second = scheduler.abandonUnresolved(choice);
            const crossKind = await unresolvedCode(
              scheduler.discardRejected(choice),
            );
            const result = await first;
            return {
              shared: second === first,
              crossKind,
              result,
              probes,
              consumed: await unresolvedCode(
                scheduler.abandonUnresolved(choice),
              ),
              tokenRegistered: scheduler.unresolved.tokens.has(choice),
            };
          } finally {
            IDBDatabase.prototype.transaction = open;
            IDBObjectStore.prototype.delete = deletion;
          }
        }, '1');
        assert.deepEqual(committed, {
          shared: true,
          crossKind: 'invalid_selection',
          result: { kind: 'abandoned_unknown', removedDependents: 0 },
          probes: [
            { boundary: 'transaction', shared: true },
            { boundary: 'post-commit', shared: true },
          ],
          consumed: 'invalid_selection',
          tokenRegistered: false,
        });
        assert.equal(harness.requests().length, requests);
        // A refused operation also consumes its token at entry. The refusal
        // comes from the authority, which the page makes fail for one call.
        await page.evaluate((tag) => unresolvedStart(tag), '6');
        await unresolvedWaitPause(page, '6', 'blocked', 'access_unavailable');
        const requestsBeforeRefusal = harness.requests().length;
        const refusal = await page.evaluate(async (tag) => {
          const choice = await scheduler.selectUnresolved(
            unresolvedKey(tag),
            'offline',
          );
          globalThis.cleanupAuthorityFailure = new Error(
            'private-authority-refusal',
          );
          try {
            return {
              refused: await unresolvedCode(
                scheduler.abandonUnresolved(choice),
              ),
              tokenRegistered: scheduler.unresolved.tokens.has(choice),
              replay: await unresolvedCode(scheduler.abandonUnresolved(choice)),
            };
          } finally {
            globalThis.cleanupAuthorityFailure = undefined;
          }
        }, '6');
        assert.deepEqual(refusal, {
          refused: 'owner_not_ready',
          tokenRegistered: false,
          replay: 'invalid_selection',
        });
        // Ending the owner closes the scheduler on the next cleanup authority
        // check. A consumed token then reaches the closed check, which answers.
        const closed = await page.evaluate(async () => {
          stop.abort();
          const closing = await unresolvedCode(
            scheduler.selectUnresolved(unresolvedKey('1'), 'offline'),
          );
          return {
            closing,
            closedRetry: await unresolvedCode(
              scheduler.abandonUnresolved(consumedChoice),
            ),
          };
        });
        assert.deepEqual(closed, {
          closing: 'owner_not_ready',
          closedRetry: 'owner_not_ready',
        });
        const after = await page.evaluate(() => unresolvedLedger());
        assert.equal(laneRowsOf(after.storage.rows, '1').length, 0);
        assert.equal(laneRowsOf(after.storage.rows, '6').length, 1);
        assert.equal(harness.requests().length, requestsBeforeRefusal);
        break;
      }
      case 'unresolved-serialization': {
        // A retry-paused lane (503) carries a resume token. An admission and a
        // resume that start inside the action's authority check must wait for
        // the gate, then see only the new incarnation.
        harness.configure('status', 404);
        await page.evaluate((tag) => unresolvedStart(tag), '2');
        await unresolvedWaitPause(page, '2', 'blocked', 'access_unavailable');
        await unresolvedRetryPause(harness, page, '2');
        const requests = harness.requests().length;
        const outcome = await page.evaluate(async (tag) => {
          const resumeToken = unresolvedLane(tag).pause.selection;
          const choice = await scheduler.selectUnresolved(
            unresolvedKey(tag),
            'offline',
          );
          const admittedKey = crypto.randomUUID().replaceAll('-', '');
          let started = false;
          let admission;
          let resume;
          let lanePresentAtAdmission;
          // The hook runs inside the action's authority check, so both calls
          // queue behind the gate and cannot join the closure.
          globalThis.onCleanupProofCheck = () => {
            if (started) return;
            started = true;
            admission = enqueueLikeHead({
              key: admittedKey,
              lane: unresolvedKey(tag),
              artifactId: unresolvedKey(tag)[4],
              base: { version: 0 },
            });
            resume = scheduler.resumeDelivery(resumeToken).then(
              () => 'accepted',
              (error) => error.code,
            );
          };
          globalThis.onAdmissionStart = () => {
            lanePresentAtAdmission = scheduler.lanes.has(
              JSON.stringify(unresolvedKey(tag)),
            );
          };
          try {
            const result = await scheduler.abandonUnresolved(choice);
            await admission;
            return {
              result,
              admittedKey,
              lanePresentAtAdmission,
              resume: await resume,
            };
          } finally {
            globalThis.onCleanupProofCheck = undefined;
            globalThis.onAdmissionStart = undefined;
          }
        }, '2');
        assert.deepEqual(outcome.result, {
          kind: 'abandoned_unknown',
          removedDependents: 0,
        });
        assert.equal(outcome.lanePresentAtAdmission, false);
        assert.equal(outcome.resume, 'invalid_selection');
        const after = await page.evaluate(() => unresolvedLedger());
        assert.equal(laneRowsOf(after.storage.rows, '2').length, 1);
        assert.equal(
          after.storage.rows.some((row) => row.key === outcome.admittedKey),
          true,
        );
        assert.equal(harness.requests().length, requests);
        break;
      }
      case 'unresolved-no-resurrection': {
        // A retry-paused lane (503) carries a resume token. After commit the old
        // selection and token reject, and a stale kernel result changes nothing.
        harness.configure('status', 404);
        await page.evaluate((tag) => unresolvedStart(tag), '3');
        await unresolvedWaitPause(page, '3', 'blocked', 'access_unavailable');
        await unresolvedRetryPause(harness, page, '3');
        const retired = await page.evaluate(async (tag) => {
          const key = unresolvedKey(tag);
          const resumeToken = unresolvedLane(tag).pause.selection;
          const choice = await scheduler.selectUnresolved(key, 'offline');
          const first = await scheduler.abandonUnresolved(choice);
          return {
            first,
            retired: !scheduler.lanes.has(JSON.stringify(key)),
            replay: await unresolvedCode(scheduler.abandonUnresolved(choice)),
            resume: await unresolvedCode(scheduler.resumeDelivery(resumeToken)),
          };
        }, '3');
        assert.deepEqual(retired, {
          first: { kind: 'abandoned_unknown', removedDependents: 0 },
          retired: true,
          replay: 'invalid_selection',
          resume: 'invalid_selection',
        });
        // White-box: the kernel is held, the lane is retired while its attempt is
        // in flight, and the attempt then returns a binding refusal. The pump must
        // drop that result without pausing, fencing or observing.
        await page.evaluate(() => {
          scheduler.kernel.run = () =>
            new Promise((resolve) => {
              globalThis.staleRelease = resolve;
            });
        });
        await page.evaluate((tag) => unresolvedStart(tag), '3');
        await page.waitForFunction(() => globalThis.staleRelease !== undefined);
        const stale = await page.evaluate(async (tag) => {
          const eventsBefore = events.length;
          scheduler.retireIdle(unresolvedLane(tag));
          staleRelease({
            kind: 'delivery_failed',
            failure: { reason: 'binding_changed', status: 409 },
          });
          const observed = await unresolvedObserve();
          delete scheduler.kernel.run;
          return {
            events: observed.events - eventsBefore,
            fenced: scheduler.fence.signal.aborted,
            present: scheduler.lanes.has(JSON.stringify(unresolvedKey(tag))),
          };
        }, '3');
        assert.deepEqual(stale, { events: 0, fenced: false, present: false });
        // A new row under the same key is a new incarnation with no inherited
        // pause or failure count.
        harness.configure('headers_hold');
        const arrival = harness.waitRequest();
        await page.evaluate((tag) => unresolvedStart(tag), '3');
        await arrival;
        assert.deepEqual(
          await page.evaluate(
            (tag) => ({
              present: scheduler.lanes.has(JSON.stringify(unresolvedKey(tag))),
              ...unresolvedGate(tag),
            }),
            '3',
          ),
          { present: true, pause: null, failures: 0, rejection: false },
        );
        // configure() starts a new request log, so only the new incarnation's
        // request is counted.
        assert.equal(harness.requests().length, 1);
        break;
      }
      case 'unresolved-late-rejection': {
        // White-box: an attempt is held while its lane is retired, then rejects
        // with a dispatched local failure. The pump must drop that rejection
        // without counting it, closing the scheduler or observing it (ADR 0049
        // rule 14), and the retired lane must not come back.
        await page.evaluate(() => {
          scheduler.kernel.run = () =>
            new Promise((resolve, reject) => {
              globalThis.staleReject = reject;
            });
        });
        await page.evaluate((tag) => unresolvedStart(tag), '5');
        await page.waitForFunction(() => globalThis.staleReject !== undefined);
        const stale = await page.evaluate(async (tag) => {
          const eventsBefore = events.length;
          const failuresBefore = scheduler.aggregateFailures;
          scheduler.retireIdle(unresolvedLane(tag));
          staleReject(
            new deliveryApi.DeliveryAttemptError(
              new deliveryApi.OutboxError('storage_failed'),
              true,
            ),
          );
          // The pump's task settles only after the stale result is handled.
          await scheduler.activeTask;
          delete scheduler.kernel.run;
          return {
            events: events.length - eventsBefore,
            failures: scheduler.aggregateFailures - failuresBefore,
            fenced: scheduler.fence.signal.aborted,
            present: scheduler.lanes.has(JSON.stringify(unresolvedKey(tag))),
          };
        }, '5');
        assert.deepEqual(stale, {
          events: 0,
          failures: 0,
          fenced: false,
          present: false,
        });
        break;
      }
      case 'unresolved-late-response': {
        // The committed receipt is held past the 60 s attempt deadline. The
        // deadline aborts the attempt's fetch and the body reader stops on that
        // abort, so the held bytes never reach receipt parsing (ADR 0049 rule
        // 14). Releasing them after abandonment must leave the ledger
        // unchanged: no row, ACK, successor or request. This case proves only
        // the abort path. The lane-identity check for a late kernel callback is
        // covered by unresolved-kernel-identity, which withdraws identity while
        // the body is held.
        harness.configure('body_hold');
        const arrival = harness.waitRequest();
        await page.evaluate((tag) => unresolvedStart(tag), '4');
        await arrival;
        const registered = await page.evaluate(() => timers.length);
        await page.evaluate(() => {
          timers
            .find((timer) => timer.active && timer.milliseconds === 60000)
            .callback();
        });
        // The failed attempt schedules a retry timer. The retry is answered with
        // 503, which pauses the lane with a resume token.
        await page.waitForFunction(
          (count) => timers.length > count && timers.at(-1).active,
          registered,
        );
        // Lane-scope exhaustion: seven failures are already recorded, so this
        // retry's 503 is the eighth retryable failure and publishes the
        // resume-token pause.
        await page.evaluate(() => {
          unresolvedLane('4').failures = 7;
        });
        harness.configure('status', 503);
        await page.evaluate(() => timers.at(-1).callback());
        await unresolvedWaitPause(page, '4', 'retry_paused', 'server_error');
        const outcome = await page.evaluate(async (tag) => {
          const choice = await scheduler.selectUnresolved(
            unresolvedKey(tag),
            'offline',
          );
          return scheduler.abandonUnresolved(choice);
        }, '4');
        assert.deepEqual(outcome, {
          kind: 'abandoned_unknown',
          removedDependents: 0,
        });
        const before = await page.evaluate(() => unresolvedLedger());
        const requests = harness.requests().length;
        // Resolves once the held response has closed on the server: after the
        // rest of the body was written, or at once because the client aborted.
        await harness.releaseHeld();
        // Let the page finish what it started. The pump settles first, then a
        // few macrotasks let queued callbacks run before the ledger is read.
        await page.evaluate(async () => {
          await scheduler.activeTask;
          for (let turn = 0; turn < 5; turn += 1) {
            await new Promise((resolve) => setTimeout(resolve, 0));
          }
        });
        const after = await page.evaluate(() => unresolvedLedger());
        assert.equal(laneRowsOf(after.storage.rows, '4').length, 0);
        assert.deepEqual(after.storage.receipts, before.storage.receipts);
        assert.deepEqual(after.storage.checkpoints, before.storage.checkpoints);
        assert.equal(after.events, before.events);
        assert.equal(harness.requests().length, requests);
        break;
      }
      case 'unresolved-kernel-identity': {
        // Without the scheduler, the lane identity is withdrawn after the
        // request is sent and before the held receipt body completes. The
        // checks that run after the response arrives refuse the late callback,
        // so it writes no ACK, trims nothing and leaves the row unchanged.
        harness.configure('body_hold');
        const arrival = harness.waitRequest();
        await page.evaluate(async (tag) => {
          await unresolvedEnqueue(tag);
          globalThis.kernelCurrent = true;
          globalThis.kernelAttempt = kernel
            .run(unresolvedKey(tag), undefined, () => kernelCurrent)
            .then(
              () => 'completed',
              (error) => error.code,
            );
        }, '8');
        await arrival;
        const before = await page.evaluate(() => unresolvedLedger());
        await page.evaluate(() => {
          kernelCurrent = false;
        });
        harness.releaseHeld();
        const outcome = await page.evaluate(() => kernelAttempt);
        const after = await page.evaluate(() => unresolvedLedger());
        assert.equal(outcome, 'invalid_selection');
        assert.deepEqual(
          laneRowsOf(after.storage.rows, '8').map((row) => row.state),
          ['prepared'],
        );
        assert.deepEqual(after.storage.receipts, before.storage.receipts);
        assert.deepEqual(after.storage.checkpoints, before.storage.checkpoints);
        assert.equal(harness.requests().length, 1);
        break;
      }
      default:
        throw new Error(`unknown unresolved disposal mode: ${mode}`);
    }
  } finally {
    harness.setErrorBody(dataConflictBody);
  }
}

/**
 * Starts lane `tag` as a blocked unresolved lane (404 access_unavailable) with a
 * prepared head, and installs the page helpers that the unresolved cases share.
 * Tag `a` is the artifact of the setup's marker row, so a case that expects a
 * lane to empty uses another tag.
 */
async function unresolvedBlockedLane(harness, page, tag) {
  await page.evaluate(() => {
    // Each arranger injects one rule 15 fact or a refusal. None of them is
    // reverted by the page itself, so a case restores what it needs.
    globalThis.unresolvedArrangers = {
      restore() {
        cleanupAuthorityFailure = undefined;
        authorityException = undefined;
        currentAuthority = authority;
      },
      'host-refusal'() {
        cleanupAuthorityFailure = new deliveryApi.OutboxError(
          'owner_not_ready',
        );
      },
      // A detached handle refuses its check with owner_not_ready.
      detached() {
        authorityException = new deliveryApi.OutboxError('owner_not_ready');
      },
      logout() {
        locallyAuthenticated = false;
      },
      'lock-loss'() {
        lockHeld = false;
      },
      generation() {
        currentAuthority = Symbol();
      },
      profile() {
        sessionStorage.setItem(
          'artifree-data-outbox-owner-v1',
          'replaced-owner',
        );
      },
      // A 401 invalidates authentication and aborts the authority signal.
      invalidated() {
        authInvalidationAborts = true;
        deliveryContext.invalidateAuthentication();
      },
      aborted() {
        stop.abort();
      },
      'check-throws'() {
        authorityException = makeAuthorityException();
      },
    };
    // Counts requests for reconciliation without changing what it does.
    globalThis.unresolvedCountReconciliation = () => {
      const original = scheduler.requestReconciliation;
      globalThis.reconciliationRequests = 0;
      scheduler.requestReconciliation = function () {
        reconciliationRequests += 1;
        return original.call(this);
      };
    };
    // Runs `action` when the next intents readwrite transaction completes.
    globalThis.unresolvedOnCommit = (action) => {
      const open = IDBDatabase.prototype.transaction;
      globalThis.restoreUnresolvedTransaction = () => {
        IDBDatabase.prototype.transaction = open;
      };
      IDBDatabase.prototype.transaction = function (...args) {
        const tx = Reflect.apply(open, this, args);
        if (args[1] === 'readwrite' && [args[0]].flat().includes('intents'))
          tx.addEventListener('complete', action, { once: true });
        return tx;
      };
    };
  });
  harness.setErrorBody(notFoundBody);
  harness.configure('status', 404);
  await page.evaluate((tag) => unresolvedStart(tag), tag);
  await unresolvedWaitPause(page, tag, 'blocked', 'access_unavailable');
}

/** Waits until the page's scheduler has no reconciliation queued or running. */
async function unresolvedQuiet(page) {
  await page.waitForFunction(
    () => !scheduler.reconciling && !scheduler.reconciliationQueued,
  );
}

/**
 * Selects lane `tag` offline, applies one named arranger, and checks that the
 * abandonment is refused as owner_not_ready with every row and counter intact.
 */
async function assertOfflineRefused(harness, page, tag, arranger, label) {
  await page.evaluate(async (tag) => {
    unresolvedArrangers.restore();
    globalThis.unresolvedChoice = await scheduler.selectUnresolved(
      unresolvedKey(tag),
      'offline',
    );
  }, tag);
  const before = await page.evaluate(() => unresolvedObserve());
  const requests = harness.requests().length;
  const code = await page.evaluate((arranger) => {
    unresolvedArrangers[arranger]();
    return unresolvedCode(scheduler.abandonUnresolved(unresolvedChoice));
  }, arranger);
  assert.equal(code, 'owner_not_ready', label);
  assert.deepEqual(
    await page.evaluate(() => unresolvedObserve()),
    before,
    label,
  );
  assert.equal(harness.requests().length, requests, label);
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

// The real Rust server rejects the first three heads; owned fixtures answer the last three.
// Each lane starts with one acknowledged write. The rejected head is queued after it while
// that write is still in the lane, so the checkpoint keeps it as the predecessor.
const defaultPredecessor = { path: 'predecessor', text: '{"p":1}', base: 0 };

const rejectedLiveCases = [
  {
    name: 'data_conflict',
    head: { path: 'marker', text: '{"original":1}' },
    competing: { path: '', base: 1, value: { competing: 1 } },
    survivorBase: 2,
    reason: 'data_conflict',
    status: 409,
    code: undefined,
  },
  {
    name: 'data_path_conflict',
    predecessor: { path: 'marker', text: '1', base: 0 },
    head: { path: 'marker/deeper', text: '1' },
    survivorBase: 1,
    reason: 'rejected',
    status: 409,
    code: 'data_path_conflict',
  },
  {
    name: 'invalid_request',
    head: { path: 'a%2Fb', text: '1' },
    survivorBase: 1,
    reason: 'rejected',
    status: 422,
    code: 'invalid_request',
  },
  {
    name: 'data_too_large',
    fixture: {
      status: 422,
      body: {
        error: { code: 'data_too_large', message: 'private-response-marker' },
      },
    },
    head: { path: 'marker', text: '{"original":1}' },
    survivorBase: 1,
    reason: 'rejected',
    status: 422,
    code: 'data_too_large',
  },
  {
    name: 'request_key_reused',
    fixture: {
      status: 422,
      body: {
        error: {
          code: 'request_key_reused',
          message: 'private-response-marker',
        },
      },
    },
    head: { path: 'marker', text: '{"original":1}' },
    survivorBase: 1,
    reason: 'rejected',
    status: 422,
    code: 'request_key_reused',
  },
  {
    name: 'payload_too_large',
    fixture: {
      status: 413,
      body: {
        error: {
          code: 'payload_too_large',
          message: 'private-response-marker',
        },
      },
    },
    head: { path: 'marker', text: '{"original":1}' },
    survivorBase: 1,
    reason: 'rejected',
    status: 413,
    code: 'payload_too_large',
  },
];

export async function checkRejectedDisposalLive(harness) {
  for (const liveCase of rejectedLiveCases)
    await checkRejectedLiveCase(harness, liveCase);
  console.log(
    'outbox disposal: rejected-disposal-live passed; data_too_large, request_key_reused and payload_too_large are owned HTTP fixtures',
  );
}

function bytesOf(rows) {
  return rows.reduce((total, row) => total + row.bytes, 0);
}

function rowAt(rows, sequence) {
  const row = rows.find((candidate) => candidate.sequence === sequence);
  assert.notEqual(row, undefined, `row ${sequence} is stored`);
  return row;
}

async function checkRejectedLiveCase(harness, liveCase) {
  // The foreign artifact shares the lane's binding; the foreign actor has its own binding.
  const foreignArtifact = `art_${'e'.repeat(32)}`;
  const foreignActorId = `u-${'f'.repeat(32)}`;
  // The server caps user labels at 64 characters, and selectActor appends a 36-character id.
  await harness.selectActor(`Rejected ${liveCase.name}`);
  harness.configure('normal');
  const artifact = await harness.createArtifact();
  const predecessorWrite = liveCase.predecessor ?? defaultPredecessor;
  const page = await harness.setup({
    artifact,
    path: predecessorWrite.path,
    text: predecessorWrite.text,
    base: predecessorWrite.base,
  });
  // Every row is queued before the predecessor's single kernel attempt, so the head is chained
  // after an unsent predecessor and stays unsent until the competing write has happened.
  const seeded = await page.evaluate(
    async ({
      headPath,
      headText,
      survivorBase,
      foreignArtifact,
      foreignActorId,
    }) => {
      globalThis.rejectedSnapshot = async () => {
        const opening = indexedDB.open('artifree-data-outbox');
        const db = await new Promise((resolve, reject) => {
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
      globalThis.installRejectedCleanup = () => {
        const gate = new deliveryApi.LocalOperationGate();
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
          gate,
          async authorize(mode, signal) {
            proof.check();
            if (mode !== 'online')
              throw new Error('Expected explicit online proof');
            const current = await deliveryApi.readBinding(
              location.origin,
              deliveryContext.accessToken,
              signal,
              (status) => {
                if (status === 401) deliveryContext.invalidateAuthentication();
              },
            );
            if (JSON.stringify(current) !== JSON.stringify(auth)) {
              currentAuthority = Symbol();
              stop.abort();
              throw new deliveryApi.OutboxError('owner_not_ready');
            }
            proof.check();
            return proof;
          },
        });
      };
      installRejectedCleanup();
      const newKey = () => crypto.randomUUID().replaceAll('-', '');
      const predecessorKey = head.key;
      const predecessorSequence = head.sequence;
      const target = await enqueueLikeHead({
        key: newKey(),
        base: { after: predecessorSequence },
        path: headPath,
        text: headText,
      });
      const dependent = await enqueueLikeHead({
        key: newKey(),
        base: { after: target.sequence },
        path: 'dependent',
        text: '{"d":1}',
      });
      const grandchild = await enqueueLikeHead({
        key: newKey(),
        base: { after: dependent.sequence },
        path: 'grandchild',
        text: '{"d":2}',
      });
      const survivor = await enqueueLikeHead({
        key: newKey(),
        base: { version: survivorBase },
        path: 'survivor',
        text: '{"s":1}',
      });
      const foreign = await enqueueLikeHead({
        key: newKey(),
        lane: [...lane.slice(0, 4), foreignArtifact],
        artifactId: foreignArtifact,
        path: 'foreign',
        text: '{"f":1}',
      });
      const foreignActor = await enqueueLikeHead({
        key: newKey(),
        binding: [lane[0], lane[1], foreignActorId, lane[3]],
        lane: [lane[0], lane[1], foreignActorId, lane[3], foreignArtifact],
        artifactId: foreignArtifact,
        path: 'foreign-actor',
        text: '{"a":1}',
      });
      // This is the only attempt before the head wakes: it sends the predecessor alone.
      const attempt = await kernel.run(lane);
      return {
        predecessorKey,
        predecessorSequence,
        predecessorKind: attempt.kind,
        head: target.sequence,
        headKey: target.key,
        dependent: dependent.sequence,
        grandchild: grandchild.sequence,
        survivor: survivor.sequence,
        survivorKey: survivor.key,
        foreign: foreign.sequence,
        foreignActor: foreignActor.sequence,
      };
    },
    {
      headPath: liveCase.head.path,
      headText: liveCase.head.text,
      survivorBase: liveCase.survivorBase,
      foreignArtifact,
      foreignActorId,
    },
  );
  assert.equal(
    seeded.predecessorKind,
    'server_acknowledged',
    'the predecessor is acknowledged before the rejected head is woken',
  );
  if (liveCase.competing !== undefined)
    await harness.independentWrite(
      artifact,
      liveCase.competing.base,
      liveCase.competing.path,
      liveCase.competing.value,
    );
  if (liveCase.fixture !== undefined) {
    harness.setStatus(liveCase.fixture.status);
    harness.setErrorBody(liveCase.fixture.body);
    harness.setMode('status');
  }
  await page.evaluate(() => scheduler.wake(lane));
  await page.waitForFunction(() =>
    events.some(
      (event) => event.kind === 'paused' && event.pause.kind === 'blocked',
    ),
  );
  const before = await page.evaluate(async () => ({
    storage: await rejectedSnapshot(),
    pause: scheduler.lanes.get(JSON.stringify(lane)).pause,
  }));
  const readsBefore = harness.bindingReadCount();
  const discarded = await page.evaluate(async () => {
    globalThis.rejectedSelection = await scheduler.selectRejected(
      lane,
      'online',
    );
    return scheduler.discardRejected(rejectedSelection);
  });
  assert.equal(
    harness.bindingReadCount() - readsBefore,
    2,
    'selection and action each read the live binding once',
  );
  const after = await page.evaluate(async () => {
    const { kind, reason, scope } = scheduler.lanes.get(
      JSON.stringify(lane),
    ).pause;
    return {
      storage: await rejectedSnapshot(),
      pause: { kind, reason, scope },
    };
  });
  const laneKey = JSON.stringify(rowAt(before.storage.rows, seeded.head).lane);
  const foreignLaneKey = JSON.stringify(
    rowAt(before.storage.rows, seeded.foreign).lane,
  );
  const foreignActorKey = JSON.stringify(
    rowAt(before.storage.rows, seeded.foreignActor).lane,
  );
  const checkpointsOf = (storage, key) =>
    storage.checkpoints.filter(
      (checkpoint) => JSON.stringify(checkpoint.lane) === key,
    );
  assert.equal(before.storage.rows.length, 6);
  assert.equal(rowAt(before.storage.rows, seeded.head).state, 'prepared');
  assert.equal(before.pause.kind, 'blocked');
  assert.equal(before.pause.failure.reason, liveCase.reason);
  assert.equal(before.pause.failure.status, liveCase.status);
  assert.equal(before.pause.failure.code, liveCase.code);
  assert.deepEqual(
    harness.requests().map((request) => request.headers['idempotency-key']),
    [seeded.predecessorKey, seeded.headKey],
    'only the acknowledged predecessor and the rejected head reached the server before disposal',
  );
  assert.equal(before.storage.usage.rows, 6);
  assert.equal(before.storage.usage.bytes, bytesOf(before.storage.rows));
  const [beforeLane] = checkpointsOf(before.storage, laneKey);
  assert.deepEqual(
    beforeLane.predecessor,
    { sequence: seeded.predecessorSequence, dataVersion: 1 },
    'the acknowledged predecessor anchors the lane',
  );
  assert.deepEqual(
    after.storage.rows.map((row) => row.sequence),
    [seeded.survivor, seeded.foreign, seeded.foreignActor],
    'the dependent-of-dependent leaves with the closure',
  );
  assert.deepEqual(discarded, {
    kind: 'discarded_rejected',
    removedDependents: 2,
  });
  assert.deepEqual(
    after.storage.rows,
    [
      rowAt(before.storage.rows, seeded.survivor),
      rowAt(before.storage.rows, seeded.foreign),
      rowAt(before.storage.rows, seeded.foreignActor),
    ],
    'survivors and foreign bindings keep their rows unchanged',
  );
  assert.deepEqual(after.storage.usage, {
    ...before.storage.usage,
    rows: 3,
    bytes: bytesOf(after.storage.rows),
  });
  assert.deepEqual(
    checkpointsOf(after.storage, laneKey),
    [
      {
        ...beforeLane,
        rows: 1,
        bytes: rowAt(after.storage.rows, seeded.survivor).bytes,
      },
    ],
    'the survivor checkpoint counts the survivor and keeps the predecessor',
  );
  assert.deepEqual(
    checkpointsOf(after.storage, foreignLaneKey),
    checkpointsOf(before.storage, foreignLaneKey),
  );
  assert.deepEqual(
    checkpointsOf(after.storage, foreignActorKey),
    checkpointsOf(before.storage, foreignActorKey),
  );
  assert.deepEqual(after.pause, {
    kind: 'retry_paused',
    reason: 'survivors_after_disposal',
    scope: 'lane',
  });
  assert.equal(
    harness.requests().length,
    2,
    'the survivor waits for resumeDelivery',
  );
  harness.setMode('normal');
  await page.evaluate(() =>
    scheduler.resumeDelivery(
      scheduler.lanes.get(JSON.stringify(lane)).pause.selection,
    ),
  );
  // resumeDelivery settles before the dispatched request is issued; wait for the survivor's ACK.
  await page.waitForFunction(
    () =>
      events.filter(
        (event) =>
          event.kind === 'attempt' &&
          event.result.kind === 'server_acknowledged',
      ).length === 1,
  );
  const sent = harness.requests();
  assert.equal(sent.length, 3, 'resume sends only the survivor');
  const survivorBody = JSON.parse(sent[2].body);
  assert.equal(sent[2].headers['idempotency-key'], seeded.survivorKey);
  assert.equal(survivorBody.path, 'survivor');
  assert.equal(survivorBody.base_data_version, liveCase.survivorBase);
  assert.deepEqual(survivorBody.value, { s: 1 });
  const acknowledged = await page.evaluate(
    () =>
      events.findLast(
        (event) =>
          event.kind === 'attempt' &&
          event.result.kind === 'server_acknowledged',
      ).result.receipt,
  );
  assert.equal(
    acknowledged.data_version,
    liveCase.survivorBase + 1,
    'the server acknowledges the survivor at the next data version',
  );
  const final = await page.evaluate(() => rejectedSnapshot());
  assert.deepEqual(final.rows, [
    rowAt(before.storage.rows, seeded.foreign),
    rowAt(before.storage.rows, seeded.foreignActor),
  ]);
  await harness.close(page);
}
