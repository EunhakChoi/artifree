import assert from 'node:assert/strict';
import { checkSchedulerDisposal } from './smoke-outbox-disposal.mjs';

/** Native storage/Fetch scheduler checks; no public handle enables delivery. */
export async function checkOutboxScheduler(harness) {
  const { setup, close, configure, requests, createArtifact, waitRequest } =
    harness;
  const { setMode } = harness;
  async function fresh(mode = 'normal') {
    // The mutation fault must not destroy the real proxy's binding GET too.
    configure('normal');
    const page = await setup(
      createArtifact ? { artifact: await createArtifact() } : {},
    );
    await page.evaluate(() => installScheduler());
    configure(mode);
    return page;
  }
  async function waitPause(page, kind = 'retry_paused') {
    await page.waitForFunction(
      (kind) =>
        events.some(
          (event) => event.kind === 'paused' && event.pause.kind === kind,
        ),
      kind,
    );
    await page.evaluate((kind) => {
      globalThis.selection = events.findLast(
        (event) => event.kind === 'paused' && event.pause.kind === kind,
      ).pause.selection;
    }, kind);
  }
  async function waitAttempts(page, count) {
    await page.waitForFunction(
      (count) =>
        events.filter((event) => event.kind === 'attempt').length >= count,
      count,
    );
  }
  async function fireRetries(page) {
    await page.evaluate(() => {
      for (const timer of timers) {
        if (timer.active && timer.milliseconds < 15_000) {
          timer.active = false;
          timer.callback();
        }
      }
    });
  }

  const selected = process.env.ARTIFREE_SCHEDULER_CASE;
  await checkSchedulerDisposal(harness, selected);
  if (
    createArtifact === undefined &&
    (selected === undefined || selected === 'renew-read-once')
  ) {
    for (const mode of [
      'selected-initial',
      'other',
      'selected-final',
      'late-authority',
    ].filter(
      (value) =>
        process.env.ARTIFREE_RENEW_MODE === undefined ||
        value === process.env.ARTIFREE_RENEW_MODE,
    )) {
      const page = await fresh();
      await page.evaluate(async () => {
        globalThis.otherLane = [...lane.slice(0, 4), 'art_' + 'c'.repeat(32)];
        await enqueueLikeHead({
          key: '2'.repeat(32),
          lane: otherLane,
          artifactId: otherLane[4],
          base: { version: 0 },
        });
        faultPut('intents', 'acknowledged');
        scheduler.wake(lane);
        scheduler.wake(otherLane);
      });
      await page.waitForFunction(
        () => events.filter((event) => event.kind === 'paused').length === 2,
      );
      const retained = await page.evaluate(() => storageSnapshot());
      await page.evaluate((mode) => {
        restorePut();
        globalThis.firstSelection = events.find(
          (event) => event.kind === 'paused' && event.lane[4] === lane[4],
        ).pause.selection;
        globalThis.otherSelection = events.find(
          (event) => event.kind === 'paused' && event.lane[4] === otherLane[4],
        ).pause.selection;
        // Internal diagnostic only: no public state/debug API is introduced.
        // Closing makes further probes unable to observe a partial budget reset.
        globalThis.renewState = () => ({
          epoch: scheduler.epoch,
          aggregateFailures: scheduler.aggregateFailures,
          lanes: [...scheduler.lanes.values()].map((entry) => ({
            key: [...entry.key],
            failures: entry.failures,
            pause: entry.pause.kind,
            reason: entry.pause.reason,
            scope: entry.pause.scope,
          })),
        });
        globalThis.renewBefore = renewState();
        globalThis.renewUnhandled = 0;
        addEventListener('unhandledrejection', (event) => {
          event.preventDefault();
          renewUnhandled += 1;
        });
        globalThis.renewCaptureStarted = false;
        globalThis.renewCaptureGate = new Promise((resolve) => {
          globalThis.releaseRenewCapture = resolve;
        });
        globalThis.renewNativeLane = store.lane.bind(store);
        let selectedReads = 0;
        store.lane = (key) => {
          const own = key[4] === lane[4];
          if (own) selectedReads += 1;
          const fault =
            mode === 'selected-initial'
              ? own && selectedReads === 1
              : mode === 'selected-final'
                ? own && selectedReads === 2
                : !own;
          if (!fault) return renewNativeLane(key);
          renewCaptureStarted = true;
          return renewCaptureGate.then(() => {
            const transact = IDBDatabase.prototype.transaction;
            IDBDatabase.prototype.transaction = function (...args) {
              const tx = transact.apply(this, args);
              if (args[1] === 'readonly' && mode !== 'late-authority')
                tx.abort();
              return tx;
            };
            try {
              const pending = renewNativeLane(key);
              if (mode === 'late-authority')
                return pending.then((rows) => {
                  currentAuthority = Symbol();
                  stop.abort();
                  return rows;
                });
              return pending;
            } finally {
              IDBDatabase.prototype.transaction = transact;
            }
          });
        };
        globalThis.renewPending = scheduler.resumeDelivery(firstSelection);
        globalThis.renewOutcome = renewPending.then(
          () => 'unexpected',
          (error) => error.code,
        );
      }, mode);
      await page.waitForFunction(() => renewCaptureStarted);
      assert.equal(await page.evaluate(() => revalidationCalls), 1);
      assert.equal(
        await page.evaluate(
          () => scheduler.resumeDelivery(firstSelection) === renewPending,
        ),
        true,
      );
      assert.deepEqual(
        await page.evaluate(() =>
          Promise.all(
            ['retryPreparation', 'retryCleanup'].map((method) =>
              scheduler[method](firstSelection).then(
                () => 'unexpected',
                (error) => error.code,
              ),
            ),
          ),
        ),
        ['invalid_selection', 'invalid_selection'],
      );
      await page.evaluate(() => releaseRenewCapture());
      assert.equal(
        await page.evaluate(() => renewOutcome),
        mode === 'late-authority' ? 'owner_not_ready' : 'storage_failed',
      );
      assert.deepEqual(
        await page.evaluate(() => renewState()),
        await page.evaluate(() => renewBefore),
      );
      assert.deepEqual(await page.evaluate(() => storageSnapshot()), retained);
      await page.evaluate(() => {
        store.lane = renewNativeLane;
      });
      assert.deepEqual(
        await page.evaluate(async () => ({
          code: await scheduler.resumeDelivery(firstSelection).then(
            () => 'resumed',
            (error) => error.code,
          ),
          reads: revalidationCalls,
        })),
        { code: 'invalid_selection', reads: 1 },
        'one consumed delivery choice permits only one binding read, even when staged IDB renewal fails',
      );
      assert.equal(
        await page.evaluate(() =>
          scheduler.resumeDelivery(otherSelection).then(
            () => 'unexpected',
            (error) => error.code,
          ),
        ),
        'owner_not_ready',
      );
      assert.equal(
        await page.evaluate(() => {
          try {
            scheduler.wake(otherLane);
            return 'unexpected';
          } catch (error) {
            return error.code;
          }
        }),
        'owner_not_ready',
      );
      assert.equal(requests().length, 2);
      assert.equal(await page.evaluate(() => revalidationCalls), 1);
      assert.equal(await page.evaluate(() => renewUnhandled), 0);
      assert.equal(
        await page.evaluate(
          () => timers.filter((timer) => timer.active).length,
        ),
        0,
      );
      await close(page);
    }
  }
  if (
    createArtifact === undefined &&
    (selected === undefined || selected === 'local-action-snapshot')
  ) {
    for (const kind of ['preparation', 'cleanup'].filter(
      (value) =>
        process.env.ARTIFREE_LOCAL_KIND === undefined ||
        value === process.env.ARTIFREE_LOCAL_KIND,
    )) {
      for (const boundary of ['second-read', 'before-transaction']) {
        const page = await fresh();
        await page.evaluate((kind) => {
          faultPut(
            kind === 'preparation' ? 'intents' : 'usage',
            kind === 'preparation' ? 'prepared' : undefined,
          );
          scheduler.wake(lane);
        }, kind);
        await waitPause(page, `${kind}_paused`);
        const result = await page.evaluate(
          async ({ kind, boundary }) => {
            restorePut();
            globalThis.localOldSelection = selection;
            globalThis.localMethod =
              kind === 'preparation' ? 'retryPreparation' : 'retryCleanup';
            const nativeLane = store.lane.bind(store);
            const storeMethod = kind === 'preparation' ? 'prepare' : 'trim';
            const nativeMutation = store[storeMethod].bind(store);
            let reads = 0;
            let admitted = false;
            const admit = async () => {
              if (admitted) return;
              admitted = true;
              await enqueueLikeHead({
                key: crypto.randomUUID().replaceAll('-', ''),
                base: { after: head.sequence },
                text: '{"concurrent":true}',
              });
              globalThis.localBeforeMutation = await storageSnapshot();
            };
            store.lane = async (key) => {
              reads += 1;
              if (boundary === 'second-read' && reads === 2) await admit();
              return nativeLane(key);
            };
            store[storeMethod] = async (...args) => {
              if (boundary === 'before-transaction') await admit();
              return nativeMutation(...args);
            };
            try {
              await scheduler[localMethod](localOldSelection);
              return 'unexpected';
            } catch (error) {
              return error.code;
            } finally {
              store.lane = nativeLane;
              store[storeMethod] = nativeMutation;
            }
          },
          { kind, boundary },
        );
        assert.equal(
          result,
          'invalid_selection',
          `${kind} selection must reject concurrent enqueue at ${boundary}`,
        );
        assert.deepEqual(
          await page.evaluate(() => storageSnapshot()),
          await page.evaluate(() => localBeforeMutation),
          'selection failure changes no accepted row, checkpoint or quota counter',
        );
        assert.equal(requests().length, kind === 'preparation' ? 0 : 1);
        assert.equal(await page.evaluate(() => revalidationCalls), 0);
        assert.equal(
          await page.evaluate(() => {
            const pause = events.findLast((event) => event.kind === 'paused');
            globalThis.localFreshSelection = pause.pause.selection;
            return localFreshSelection !== localOldSelection;
          }),
          true,
          'a failed mutation retires the old choice and publishes a fresh gate',
        );
        assert.equal(
          await page.evaluate(() =>
            scheduler[localMethod](localOldSelection).then(
              () => 'unexpected',
              (error) => error.code,
            ),
          ),
          'invalid_selection',
        );
        await page.evaluate(() => scheduler[localMethod](localFreshSelection));
        await page.waitForFunction(
          (count) =>
            events.filter(
              (event) =>
                event.kind === 'attempt' &&
                event.result.kind === 'server_acknowledged',
            ).length === count,
          kind === 'preparation' ? 2 : 1,
        );
        assert.equal(requests().length, 2);
        assert.notEqual(
          requests()[0].headers['idempotency-key'],
          requests()[1].headers['idempotency-key'],
          'fresh local retry preserves the durable ACK and only sends the new successor',
        );
        assert.deepEqual(
          (await page.evaluate(() => storageSnapshot())).rows,
          [],
        );
        await page.evaluate(() => scheduler.close());
        await close(page);
      }
    }
  }

  if (
    createArtifact === undefined &&
    (selected === undefined || selected === 'local-action-capture-failure')
  ) {
    for (const kind of ['preparation', 'cleanup'].filter(
      (value) =>
        process.env.ARTIFREE_LOCAL_KIND === undefined ||
        value === process.env.ARTIFREE_LOCAL_KIND,
    )) {
      const page = await fresh();
      await page.evaluate((kind) => {
        faultPut(
          kind === 'preparation' ? 'intents' : 'usage',
          kind === 'preparation' ? 'prepared' : undefined,
        );
        scheduler.wake(lane);
      }, kind);
      await waitPause(page, `${kind}_paused`);
      const retained = await page.evaluate(() => storageSnapshot());
      await page.evaluate((kind) => {
        globalThis.localOldSelection = selection;
        globalThis.localMethod =
          kind === 'preparation' ? 'retryPreparation' : 'retryCleanup';
        globalThis.localAttempts = 0;
        globalThis.localCaptureStarted = false;
        globalThis.localUnhandled = 0;
        addEventListener('unhandledrejection', (event) => {
          event.preventDefault();
          localUnhandled += 1;
        });
        const storeMethod = kind === 'preparation' ? 'prepare' : 'trim';
        const mutation = store[storeMethod].bind(store);
        store[storeMethod] = (...args) => {
          localAttempts += 1;
          return mutation(...args);
        };
        globalThis.localNativeLane = store.lane.bind(store);
        let reads = 0;
        globalThis.localCaptureGate = new Promise((resolve) => {
          globalThis.releaseLocalCapture = resolve;
        });
        store.lane = (key) => {
          reads += 1;
          if (reads !== 3) return localNativeLane(key);
          localCaptureStarted = true;
          return localCaptureGate.then(() => {
            const transact = IDBDatabase.prototype.transaction;
            IDBDatabase.prototype.transaction = function (...args) {
              const tx = transact.apply(this, args);
              if (args[1] === 'readonly') tx.abort();
              return tx;
            };
            try {
              return localNativeLane(key);
            } finally {
              IDBDatabase.prototype.transaction = transact;
            }
          });
        };
        globalThis.localPending = scheduler[localMethod](localOldSelection);
        globalThis.localOutcome = localPending.then(
          () => 'unexpected',
          (error) => error.code,
        );
      }, kind);
      await page.waitForFunction(() => localCaptureStarted);
      assert.equal(
        await page.evaluate(
          () => scheduler[localMethod](localOldSelection) === localPending,
        ),
        true,
      );
      assert.deepEqual(
        await page.evaluate(() =>
          Promise.all(
            ['retryPreparation', 'retryCleanup', 'resumeDelivery']
              .filter((method) => method !== localMethod)
              .map((method) =>
                scheduler[method](localOldSelection).then(
                  () => 'unexpected',
                  (error) => error.code,
                ),
              ),
          ),
        ),
        ['invalid_selection', 'invalid_selection'],
      );
      await page.evaluate(() => releaseLocalCapture());
      assert.equal(await page.evaluate(() => localOutcome), 'storage_failed');
      await page.evaluate(() => {
        store.lane = localNativeLane;
      });
      assert.equal(
        await page.evaluate(() =>
          scheduler[localMethod](localOldSelection).then(
            () => 'unexpected',
            (error) => error.code,
          ),
        ),
        'invalid_selection',
        'a spent local choice cannot retry after replacement capture fails',
      );
      assert.equal(await page.evaluate(() => localAttempts), 1);
      assert.equal(
        await page.evaluate(() => {
          try {
            scheduler.wake([...lane.slice(0, 4), 'art_' + 'f'.repeat(32)]);
            return 'unexpected';
          } catch (error) {
            return error.code;
          }
        }),
        'owner_not_ready',
        'failed replacement capture fences independent lanes too',
      );
      assert.deepEqual(await page.evaluate(() => storageSnapshot()), retained);
      assert.equal(requests().length, kind === 'preparation' ? 0 : 1);
      assert.equal(await page.evaluate(() => revalidationCalls), 0);
      assert.equal(await page.evaluate(() => localUnhandled), 0);
      assert.equal(
        await page.evaluate(
          () => timers.filter((timer) => timer.active).length,
        ),
        0,
      );
      await close(page);
    }
  }
  if (
    createArtifact === undefined &&
    (selected === undefined || selected === 'capacity-retained-gates')
  ) {
    const page = await fresh();
    await page.evaluate(async () => {
      globalThis.retainedGates = [[...lane]];
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
        retainedGates.push(key);
      }
      faultPut('intents', 'prepared');
      retainedGates.forEach((key) => scheduler.wake(key));
    });
    await page.waitForFunction(
      () => events.filter((event) => event.kind === 'paused').length === 256,
    );
    await page.evaluate(async () => {
      restorePut();
      for (const key of retainedGates)
        await store.discard(await store.lane(key), authorize, stop.signal);
      globalThis.emptyAfterDisposal = await storageSnapshot();
      const key = [...lane.slice(0, 4), 'art_' + '9'.repeat(32)];
      await enqueueLikeHead({
        key: crypto.randomUUID().replaceAll('-', ''),
        lane: key,
        artifactId: key[4],
        base: { version: 0 },
      });
      globalThis.retainedBeforeScan = await storageSnapshot();
      scheduler.wake(key);
    });
    await page.waitForFunction(() =>
      events.some((event) => event.kind === 'reconciled'),
    );
    assert.equal(
      (await page.evaluate(() => emptyAfterDisposal)).rows.length,
      0,
      'explicit low-level discard can remove all durable rows without retiring gates',
    );
    assert.deepEqual(
      await page.evaluate(() => storageSnapshot()),
      await page.evaluate(() => retainedBeforeScan),
    );
    assert.deepEqual(
      await page.evaluate(() =>
        events.findLast((event) => event.kind === 'reconciled'),
      ),
      { kind: 'reconciled', queued: 0, pending: true },
      'retained gates require future explicit disposal integration; no automatic retirement',
    );
    assert.equal(requests().length, 0);
    assert.equal(await page.evaluate(() => revalidationCalls), 0);
    assert.equal(
      await page.evaluate(() => timers.filter((timer) => timer.active).length),
      0,
      'capacity waiting must not create an automatic retry loop',
    );
    await close(page);
  }
  if (selected === undefined || selected === 'capacity-wake-race') {
    const modes =
      createArtifact === undefined
        ? [
            'plain',
            'phantoms',
            'during-scan',
            'same-lane',
            'empty-pause',
            'fairness',
            'scan-failure',
            'scan-abort',
          ]
        : ['plain'];
    for (const mode of process.env.ARTIFREE_CAPACITY_MODE === undefined
      ? modes
      : modes.filter((value) => value === process.env.ARTIFREE_CAPACITY_MODE)) {
      configure('normal');
      const afterArtifact = createArtifact
        ? await createArtifact()
        : 'art_' + '9'.repeat(32);
      const page = await fresh();
      const prepCount = mode === 'fairness' ? 254 : 255;
      await page.evaluate(async (prepCount) => {
        globalThis.retainedPrepLanes = [];
        if (prepCount === 254)
          await enqueueLikeHead({
            key: crypto.randomUUID().replaceAll('-', ''),
            text: '{"successor":true}',
            base: { after: head.sequence },
          });
        for (let index = 1; index <= prepCount; index += 1) {
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
          retainedPrepLanes.push(key);
        }
        faultPut('intents', 'prepared');
        retainedPrepLanes.forEach((key) => scheduler.wake(key));
      }, prepCount);
      await page.waitForFunction(
        (count) =>
          events.filter(
            (event) =>
              event.kind === 'paused' &&
              event.pause.kind === 'preparation_paused',
          ).length === count,
        prepCount,
      );
      await page.evaluate(
        async ({ mode, afterArtifact }) => {
          restorePut();
          if (mode === 'empty-pause')
            await store.discard(
              await store.lane(retainedPrepLanes[0]),
              authorize,
              stop.signal,
            );
          globalThis.afterAckLane =
            mode === 'same-lane'
              ? [...lane]
              : [...lane.slice(0, 4), afterArtifact];
          globalThis.scanReads = 0;
          globalThis.capacityTrace = [];
          globalThis.publishedScans = 0;
          globalThis.scanAborts = 0;
          globalThis.snapshotHeld = false;
          globalThis.nativeRecover = store.recover.bind(store);
          globalThis.snapshotGate = new Promise((resolve) => {
            globalThis.releaseSnapshot = resolve;
          });
          globalThis.trimGate = new Promise((resolve) => {
            globalThis.releaseTrim = resolve;
          });
          globalThis.scanFaultGate = new Promise((resolve) => {
            globalThis.releaseScanFault = resolve;
          });
          globalThis.scanFaultCompletion = new Promise((resolve) => {
            globalThis.completeScanFault = resolve;
          });
          globalThis.trimHeld = false;
          if (mode === 'fairness') {
            const trim = store.trim.bind(store);
            store.trim = async (...args) => {
              const result = await trim(...args);
              if (!trimHeld && args[0][0].lane[4] === lane[4]) {
                trimHeld = true;
                await trimGate;
              }
              return result;
            };
          }
          store.recover = async (...args) => {
            scanReads += 1;
            const first = scanReads === 1;
            const readId = scanReads;
            if (first && (mode === 'scan-failure' || mode === 'scan-abort')) {
              // Hold before the asynchronous snapshot so a test cannot mistake
              // entry into recover() for the later native abort publication.
              if (mode === 'scan-abort') await scanFaultGate;
              // Capture every row, checkpoint and quota counter before the
              // actual faulted native transaction; a failed scan is read-only.
              try {
                globalThis.beforeScanFault = await storageSnapshot();
              } catch (error) {
                completeScanFault();
                throw error;
              }
              const transact = IDBDatabase.prototype.transaction;
              const abort = IDBTransaction.prototype.abort;
              IDBTransaction.prototype.abort = function (...values) {
                if (this.mode === 'readonly') scanAborts += 1;
                return abort.apply(this, values);
              };
              if (mode === 'scan-failure')
                IDBDatabase.prototype.transaction = function (...values) {
                  const tx = transact.apply(this, values);
                  if (values[1] === 'readonly') tx.abort();
                  return tx;
                };
              try {
                globalThis.scanSignal = args[1];
                const pending = nativeRecover(...args);
                if (mode === 'scan-abort') stop.abort();
                return pending;
              } finally {
                IDBDatabase.prototype.transaction = transact;
                IDBTransaction.prototype.abort = abort;
                completeScanFault();
              }
            }
            return nativeRecover(...args).then(async (rows) => {
              if (mode === 'empty-pause')
                capacityTrace.push('capture-' + readId);
              if (first && (mode === 'during-scan' || mode === 'same-lane')) {
                snapshotHeld = true;
                await snapshotGate;
              }
              return rows;
            });
          };
          let triggered = false;
          globalThis.onSchedulerEvent = (event) => {
            if (mode === 'empty-pause') {
              if (event.kind === 'reconciled') {
                publishedScans += 1;
                capacityTrace.push('publish-' + publishedScans);
              }
              if (
                event.kind === 'attempt' &&
                event.result.kind === 'server_acknowledged'
              ) {
                if (event.lane[4] === afterAckLane[4])
                  capacityTrace.push('ack-target');
                if (event.lane[4] === globalThis.pauseProbeLane?.[4])
                  capacityTrace.push('probe-ack');
              }
            }
            if (
              triggered ||
              event.kind !== 'attempt' ||
              event.result.kind !== 'server_acknowledged' ||
              event.lane[4] !== lane[4]
            )
              return;
            triggered = true;
            if (mode === 'fairness') return;
            if (mode === 'during-scan' || mode === 'same-lane') {
              scheduler.wake([...lane.slice(0, 4), 'art_' + '8'.repeat(32)]);
              return;
            }
            globalThis.capacityWake = (async () => {
              try {
                if (mode === 'empty-pause')
                  scheduler.wake([
                    ...lane.slice(0, 4),
                    'art_' + '8'.repeat(32),
                  ]);
                if (mode === 'phantoms')
                  for (let index = 1000; index < 2024; index += 1)
                    scheduler.wake([
                      ...lane.slice(0, 4),
                      'art_' + index.toString(16).padStart(32, '0'),
                    ]);
                await enqueueLikeHead({
                  key: crypto.randomUUID().replaceAll('-', ''),
                  lane: afterAckLane,
                  artifactId: afterAckLane[4],
                  base: { version: 0 },
                });
                globalThis.afterAckAdmitted = true;
                scheduler.wake(afterAckLane);
                return 'accepted';
              } catch (error) {
                return error.code;
              }
            })();
          };
          scheduler.wake(lane);
        },
        { mode, afterArtifact },
      );
      if (mode === 'during-scan' || mode === 'same-lane') {
        await page.waitForFunction(() => snapshotHeld);
        await page.evaluate(async (mode) => {
          await enqueueLikeHead({
            key: crypto.randomUUID().replaceAll('-', ''),
            lane: afterAckLane,
            artifactId: afterAckLane[4],
            text: '{"after":true}',
            base: { version: mode === 'same-lane' ? 1 : 0 },
          });
          globalThis.afterAckAdmitted = true;
          scheduler.wake(afterAckLane);
          scheduler.wake(lane);
          globalThis.capacityWake = Promise.resolve('accepted');
          releaseSnapshot();
        }, mode);
      }
      if (mode === 'fairness') {
        await page.waitForFunction(() => trimHeld);
        await page.evaluate(async () => {
          scheduler.wake([...lane.slice(0, 4), 'art_' + '8'.repeat(32)]);
          await enqueueLikeHead({
            key: crypto.randomUUID().replaceAll('-', ''),
            lane: afterAckLane,
            artifactId: afterAckLane[4],
            base: { version: 0 },
          });
          globalThis.afterAckAdmitted = true;
          scheduler.wake(afterAckLane);
          scheduler.wake(lane);
          globalThis.capacityWake = Promise.resolve('accepted');
          releaseTrim();
        });
      }
      await page.waitForFunction(() => globalThis.capacityWake !== undefined);
      const capacityResult = await page.evaluate(() => capacityWake);
      assert.equal(await page.evaluate(() => afterAckAdmitted), true);
      assert.equal(
        capacityResult,
        'accepted',
        'durable admission after trim must retain its wake before idle metadata retires',
      );
      if (mode === 'scan-failure') {
        await page.waitForFunction(() =>
          events.some(
            (event) => event.kind === 'stopped' || event.kind === 'reconciled',
          ),
        );
      } else if (mode === 'scan-abort') {
        await page.waitForFunction(() => scanReads > 0);
        assert.equal(
          await page.evaluate(async () => {
            releaseScanFault();
            await scanFaultCompletion;
            return globalThis.scanSignal?.aborted;
          }),
          true,
          'observe the actual native abort publication, not entry into recover',
        );
      }
      if (mode === 'scan-failure' || mode === 'scan-abort') {
        assert.deepEqual(
          await page.evaluate(() => storageSnapshot()),
          await page.evaluate(() => beforeScanFault),
          'failed/aborted reconciliation preserves all rows, checkpoints and quota accounting',
        );
        assert.equal(
          await page.evaluate(() => scanAborts),
          1,
          'the native readonly scan transaction must be aborted',
        );
        assert.equal(
          await page.evaluate(() => {
            try {
              scheduler.wake(afterAckLane);
              return 'unexpected';
            } catch (error) {
              return error.code;
            }
          }),
          'owner_not_ready',
        );
        assert.equal(
          (await page.evaluate(() => store.lane(afterAckLane)))[0].state,
          'unprepared',
        );
        assert.equal(requests().length, 1);
        assert.equal(
          await page.evaluate(
            () => events.filter((event) => event.kind === 'reconciled').length,
          ),
          0,
        );
      } else {
        if (mode === 'phantoms') {
          await page.waitForFunction(() =>
            events.some(
              (event) =>
                event.kind === 'reconciled' ||
                (event.kind === 'attempt' &&
                  event.result.kind === 'idle' &&
                  event.lane[4] === lane[4]),
            ),
          );
          assert.equal(
            await page.evaluate(
              () => events.find((event) => event.kind === 'reconciled')?.queued,
            ),
            1,
            'phantom hints cannot displace the one durable lane',
          );
        }
        if (mode === 'during-scan') {
          await page.waitForFunction(() =>
            events.some((event) => event.kind === 'reconciled'),
          );
          assert.equal(
            await page.evaluate(
              () => events.find((event) => event.kind === 'reconciled').pending,
            ),
            true,
            'a wake after the snapshot requires another scan',
          );
        }
        if (mode === 'same-lane') {
          await page.waitForFunction(() =>
            events.some((event) => event.kind === 'reconciled'),
          );
          await page.evaluate(() => {
            globalThis.probeLane = retainedPrepLanes[0];
            const choice = events.find(
              (event) =>
                event.kind === 'paused' && event.lane[4] === probeLane[4],
            ).pause.selection;
            return scheduler.retryPreparation(choice);
          });
          await page.waitForFunction(() =>
            events.some(
              (event) =>
                event.kind === 'attempt' &&
                event.result.kind === 'server_acknowledged' &&
                event.lane[4] === probeLane[4],
            ),
          );
          assert.equal(
            requests().length,
            3,
            'a same-lane wake after the snapshot must run before the later explicit probe',
          );
          assert.equal(
            requests()[1].url.includes(await page.evaluate(() => lane[4])),
            true,
          );
        } else {
          await page.waitForFunction(() =>
            events.some(
              (event) =>
                event.kind === 'attempt' &&
                event.result.kind === 'server_acknowledged' &&
                event.lane[4] === afterAckLane[4],
            ),
          );
          if (mode === 'fairness')
            await page.waitForFunction(
              () =>
                events.filter(
                  (event) =>
                    event.kind === 'attempt' &&
                    event.result.kind === 'server_acknowledged',
                ).length === 3,
            );
          assert.equal(requests().length, mode === 'fairness' ? 3 : 2);
          if (mode === 'fairness')
            assert.deepEqual(
              requests().map((request) => request.url.split('/')[3]),
              await page.evaluate(() => [lane[4], afterAckLane[4], lane[4]]),
            );
        }
        assert.equal(await page.evaluate(() => scanReads >= 1), true);
        if (mode === 'phantoms')
          assert.equal(
            await page.evaluate(() => scanReads <= 2),
            true,
            '1024 phantom keys coalesce into at most an initial and follow-up scan',
          );
        if (mode === 'empty-pause') {
          await page.evaluate(async () => {
            const key = retainedPrepLanes[0];
            await enqueueLikeHead({
              key: crypto.randomUUID().replaceAll('-', ''),
              lane: key,
              artifactId: key[4],
              base: { version: 0 },
            });
            capacityTrace.push('admitted');
            scheduler.wake(key);
            globalThis.pauseProbeLane = retainedPrepLanes[1];
            const choice = events.find(
              (event) =>
                event.kind === 'paused' && event.lane[4] === pauseProbeLane[4],
            ).pause.selection;
            return scheduler.retryPreparation(choice);
          });
          await page.waitForFunction(() =>
            events.some(
              (event) =>
                event.kind === 'attempt' &&
                event.result.kind === 'server_acknowledged' &&
                event.lane[4] === pauseProbeLane[4],
            ),
          );
          const trace = await page.evaluate(() => capacityTrace);
          assert.deepEqual(trace.slice(0, 3), [
            'capture-1',
            'publish-1',
            'ack-target',
          ]);
          assert.equal(trace.at(-1), 'probe-ack');
          assert.equal(trace.indexOf('admitted') > 2, true);
          for (
            let read = 1;
            read <= (await page.evaluate(() => publishedScans));
            read += 1
          ) {
            assert.equal(
              trace.indexOf('capture-' + read) <
                trace.indexOf('publish-' + read),
              true,
            );
            assert.equal(
              trace.indexOf('publish-' + read) < trace.indexOf('admitted'),
              true,
            );
          }
          console.log('capacity empty-pause observed order:', trace.join(','));
          assert.equal(
            requests().length,
            3,
            'an empty preparation pause must survive reconciliation and a later admission',
          );
          const pausedArtifact = await page.evaluate(
            () => retainedPrepLanes[0][4],
          );
          assert.equal(
            requests().some((request) => request.url.includes(pausedArtifact)),
            false,
          );
        }
      }
      assert.equal(
        await page.evaluate(
          () =>
            events.filter(
              (event) =>
                event.kind === 'paused' &&
                event.pause.kind === 'preparation_paused',
            ).length,
        ),
        prepCount,
      );
      assert.equal(await page.evaluate(() => revalidationCalls), 0);
      await close(page);
    }
  }

  if (
    createArtifact === undefined &&
    (selected === undefined || selected === 'capacity-reconcile-budget')
  ) {
    for (const failures of [31, 32]) {
      const page = await fresh('invalid_json');
      await page.evaluate(async () => {
        for (const character of ['b', 'c', 'd', 'e']) {
          const key = [...lane.slice(0, 4), 'art_' + character.repeat(32)];
          await enqueueLikeHead({
            key: crypto.randomUUID().replaceAll('-', ''),
            lane: key,
            artifactId: key[4],
            base: { version: 0 },
          });
          scheduler.wake(key);
        }
        scheduler.wake(lane);
      });
      for (let count = 5; count <= 30; count += 5) {
        await page.waitForFunction(
          (count) =>
            events.filter((event) => event.kind === 'delivery_retrying')
              .length === count,
          count,
        );
        if (count < 30) await fireRetries(page);
      }
      for (let count = 31; count <= failures; count += 1) {
        await page.evaluate(() => {
          const timer = timers.find(
            (timer) => timer.active && timer.milliseconds < 15000,
          );
          if (timer === undefined) throw new Error('missing eligible retry');
          timer.active = false;
          timer.callback();
        });
        await page.waitForFunction(
          (count) =>
            events.filter(
              (event) =>
                event.kind === 'attempt' &&
                event.result.kind === 'delivery_failed',
            ).length === count,
          count,
        );
        await page.evaluate(() => storageSnapshot());
      }
      await page.evaluate(async () => {
        globalThis.budgetLane = [...lane.slice(0, 4), 'art_' + '9'.repeat(32)];
        await enqueueLikeHead({
          key: crypto.randomUUID().replaceAll('-', ''),
          lane: budgetLane,
          artifactId: budgetLane[4],
          base: { version: 0 },
        });
        for (let index = 1000; index < 1251; index += 1)
          scheduler.wake([
            ...lane.slice(0, 4),
            'art_' + index.toString(16).padStart(32, '0'),
          ]);
        scheduler.wake(budgetLane);
      });
      await page.waitForFunction(() =>
        events.some(
          (event) =>
            event.lane?.[4] === budgetLane[4] &&
            (event.kind === 'paused' || event.kind === 'delivery_retrying'),
        ),
      );
      assert.equal(
        requests().length,
        32,
        'reconciliation must neither consume nor replenish mutation attempts',
      );
      assert.equal(
        await page.evaluate(
          () =>
            events.findLast(
              (event) =>
                event.kind === 'paused' && event.lane[4] === budgetLane[4],
            )?.pause.reason,
        ),
        failures === 31 ? 'invalid_response' : 'aggregate_exhausted',
      );
      assert.equal(await page.evaluate(() => revalidationCalls), 0);
      assert.equal(
        await page.evaluate(
          () => timers.filter((timer) => timer.active).length,
        ),
        0,
      );
      await close(page);
    }
  }

  if (
    createArtifact === undefined &&
    (selected === undefined || selected === 'cleanup-empty-budget')
  ) {
    const page = await fresh();
    await page.evaluate(async () => {
      globalThis.healthyCleanupLanes = [lane];
      for (let index = 1; index <= 250; index += 1) {
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
        healthyCleanupLanes.push(key);
      }
      faultPut('usage');
      healthyCleanupLanes.forEach((key) => scheduler.wake(key));
    });
    await page.waitForFunction(
      () =>
        events.filter(
          (event) =>
            event.kind === 'paused' && event.pause.kind === 'cleanup_paused',
        ).length === 251,
    );
    await page.evaluate(() => {
      globalThis.cleanupChoices = events
        .filter(
          (event) =>
            event.kind === 'paused' && event.pause.kind === 'cleanup_paused',
        )
        .map((event) => event.pause.selection);
      restorePut();
    });
    setMode('invalid_json');
    await page.evaluate(async () => {
      for (const character of ['b', 'c', 'd', 'e', 'f']) {
        const key = [...lane.slice(0, 4), 'art_' + character.repeat(32)];
        await enqueueLikeHead({
          key: crypto.randomUUID().replaceAll('-', ''),
          lane: key,
          artifactId: key[4],
          base: { version: 0 },
        });
        scheduler.wake(key);
      }
    });
    for (let round = 1; round <= 6; round += 1) {
      await page.waitForFunction(
        (count) =>
          events.filter((event) => event.kind === 'delivery_retrying').length >=
          count,
        round * 5,
      );
      await fireRetries(page);
    }
    await page.waitForFunction(
      () =>
        events.filter(
          (event) =>
            event.kind === 'attempt' && event.result.kind === 'delivery_failed',
        ).length === 32,
    );
    await page.evaluate(async () => {
      for (const choice of cleanupChoices) await scheduler.retryCleanup(choice);
    });
    assert.equal(requests().length, 283);
    const accepted = await page.evaluate(async () => {
      globalThis.freshLane = [...lane.slice(0, 4), 'art_' + '9'.repeat(32)];
      globalThis.pausePublicationOrder = [];
      globalThis.captureLane = store.lane.bind(store);
      let started;
      globalThis.freshCaptureStarted = new Promise((resolve) => {
        started = resolve;
      });
      globalThis.freshPausePublished = new Promise((resolve) => {
        globalThis.onSchedulerEvent = (event) => {
          if (event.kind === 'paused' && event.lane[4] === freshLane[4]) {
            pausePublicationOrder.push('pause-published');
            resolve(event.pause);
          }
        };
      });
      store.lane = (key) => {
        const pending = captureLane(key);
        if (key[4] === freshLane[4]) started();
        return pending.then((rows) => {
          if (key[4] === freshLane[4])
            pausePublicationOrder.push('capture-complete');
          return rows;
        });
      };
      await enqueueLikeHead({
        key: crypto.randomUUID().replaceAll('-', ''),
        lane: freshLane,
        artifactId: freshLane[4],
        base: { version: 0 },
      });
      try {
        scheduler.wake(freshLane);
        return 'accepted';
      } catch (error) {
        return error.code;
      }
    });
    assert.equal(
      await page.evaluate(
        () => events.filter((event) => event.kind === 'stopped').length,
      ),
      0,
    );
    assert.equal(
      accepted,
      'accepted',
      'trimmed healthy empty lanes cannot consume retained lane capacity',
    );
    // Start the real capture before queuing the overlapping readwrite action.
    // Parallel readonly snapshots cannot establish this publication ordering.
    await page.evaluate(() => freshCaptureStarted);
    await page.evaluate(async () => {
      globalThis.failedCaptureLane = [
        ...lane.slice(0, 4),
        'art_' + '8'.repeat(32),
      ];
      await enqueueLikeHead({
        key: crypto.randomUUID().replaceAll('-', ''),
        lane: failedCaptureLane,
        artifactId: failedCaptureLane[4],
        base: { version: 0 },
      });
      pausePublicationOrder.push('write-complete');
      store.lane = captureLane;
    });
    assert.deepEqual(await page.evaluate(() => pausePublicationOrder), [
      'capture-complete',
      'pause-published',
      'write-complete',
    ]);
    assert.equal(
      (await page.evaluate(() => freshPausePublished)).reason,
      'aggregate_exhausted',
      'actual pending work remains budget paused',
    );
    assert.equal(
      (await page.evaluate(() => store.lane(freshLane)))[0].state,
      'unprepared',
    );
    assert.equal(requests().length, 283);
    console.log(
      'outbox scheduler: capture-complete, pause-published, write-complete observed',
    );
    await page.evaluate(async () => {
      globalThis.onSchedulerEvent = undefined;
      globalThis.beforeFailedCapture = await storageSnapshot();
      globalThis.readLane = store.lane.bind(store);
      store.lane = (key) =>
        key[4] === failedCaptureLane[4]
          ? Promise.reject(new deliveryApi.OutboxError('storage_failed'))
          : readLane(key);
      scheduler.wake(failedCaptureLane);
    });
    await page.evaluate(() => storageSnapshot());
    assert.equal(
      await page.evaluate(() => {
        try {
          scheduler.wake(freshLane);
          return 'unexpected';
        } catch (error) {
          return error.code;
        }
      }),
      'owner_not_ready',
      'failed capture cannot be treated as empty work',
    );
    await page.evaluate(() => {
      store.lane = readLane;
    });
    assert.deepEqual(
      await page.evaluate(() => storageSnapshot()),
      await page.evaluate(() => beforeFailedCapture),
    );
    assert.equal(requests().length, 283);
    await close(page);
  }

  if (selected === undefined || selected === 'cleanup-progress') {
    const page = await fresh();
    await page.evaluate(async () => {
      await enqueueLikeHead({
        key: crypto.randomUUID().replaceAll('-', ''),
        text: '{"next":true}',
        base: { after: head.sequence },
      });
      const trim = store.trim.bind(store);
      globalThis.trimAttempts = 0;
      store.trim = (...args) => {
        trimAttempts += 1;
        return trim(...args);
      };
      faultPut('usage');
      scheduler.wake(lane);
    });
    await page.waitForFunction(() =>
      events.some(
        (event) =>
          event.kind === 'attempt' &&
          event.result.kind === 'acknowledged_cleanup_pending',
      ),
    );
    await page.evaluate(() => storageSnapshot());
    assert.equal(
      await page.evaluate(
        () =>
          events.filter(
            (event) =>
              event.kind === 'paused' && event.pause.kind === 'cleanup_paused',
          ).length,
      ),
      1,
      'durable ACK cleanup failure must expose a deliberate cleanup continuation',
    );
    await waitPause(page, 'cleanup_paused');
    const retained = await page.evaluate(() => storageSnapshot());
    assert.equal(retained.rows[0].state, 'acknowledged');
    assert.equal(requests().length, 1);
    await page.evaluate(() => {
      scheduler.wake(lane);
      scheduler.wake(lane);
    });
    assert.equal(await page.evaluate(() => trimAttempts), 1);
    await page.evaluate(() => {
      globalThis.retiredCleanup = selection;
      scheduler.close();
      installScheduler();
      scheduler.wake(lane);
    });
    await page.waitForFunction(() => trimAttempts === 2);
    await waitPause(page, 'cleanup_paused');
    assert.equal(
      requests().length,
      1,
      'retained durable ACK restart only trims',
    );
    for (let attempt = 3; attempt <= 4; attempt += 1) {
      await page.evaluate(() => scheduler.retryCleanup(selection));
      await waitPause(page, 'cleanup_paused');
      assert.equal(await page.evaluate(() => trimAttempts), attempt);
      assert.equal(
        requests().length,
        1,
        'durable ACK must never be retransmitted',
      );
      assert.deepEqual(await page.evaluate(() => storageSnapshot()), retained);
      assert.equal(
        await page.evaluate(
          () => timers.filter((timer) => timer.active).length,
        ),
        0,
      );
    }
    assert.equal(
      await page.evaluate(() =>
        scheduler.resumeDelivery(selection).then(
          () => 'unexpected',
          (error) => error.code,
        ),
      ),
      'invalid_selection',
    );
    assert.equal(
      await page.evaluate(() =>
        scheduler.retryPreparation(selection).then(
          () => 'unexpected',
          (error) => error.code,
        ),
      ),
      'invalid_selection',
    );
    await page.evaluate(() => {
      restorePut();
      return scheduler.retryCleanup(selection);
    });
    await page.waitForFunction(() =>
      events.some(
        (event) =>
          event.kind === 'attempt' &&
          event.result.kind === 'server_acknowledged',
      ),
    );
    assert.equal(requests().length, 2);
    assert.equal(JSON.parse(requests()[1].body).base_data_version, 1);
    assert.notEqual(
      requests()[0].headers['idempotency-key'],
      requests()[1].headers['idempotency-key'],
    );
    assert.deepEqual(await page.evaluate(() => snapshot()), []);
    assert.equal(await page.evaluate(() => revalidationCalls), 0);
    await close(page);
  }

  if (
    createArtifact === undefined &&
    (selected === undefined || selected === 'pending-retired')
  ) {
    const page = await fresh();
    await page.evaluate(async () => {
      globalThis.otherLane = [...lane.slice(0, 4), 'art_' + 'c'.repeat(32)];
      await enqueueLikeHead({
        key: '2'.repeat(32),
        lane: otherLane,
        artifactId: otherLane[4],
        base: { version: 0 },
      });
      faultPut('intents', 'acknowledged');
      scheduler.wake(lane);
      scheduler.wake(otherLane);
    });
    await page.waitForFunction(
      () => events.filter((event) => event.kind === 'paused').length === 2,
    );
    await page.evaluate(() => {
      globalThis.oldSelection = events[1].pause.selection;
      globalThis.pendingObservations = [];
      globalThis.pendingUnhandled = 0;
      addEventListener('unhandledrejection', (event) => {
        event.preventDefault();
        pendingUnhandled += 1;
      });
      globalThis.onSchedulerEvent = (event) => {
        if (event.kind !== 'paused') return;
        const duplicate = scheduler.resumeDelivery(oldSelection);
        duplicate.catch(() => undefined);
        pendingObservations.push(duplicate === resuming);
        globalThis.crossKind = scheduler.retryPreparation(oldSelection).then(
          () => 'unexpected',
          (error) => error.code,
        );
      };
      restorePut();
      globalThis.resuming = scheduler.resumeDelivery(oldSelection);
    });
    await page.evaluate(() => resuming);
    assert.deepEqual(
      await page.evaluate(() => pendingObservations),
      [true],
      'same-kind duplicate must share the exact pending promise after token retirement',
    );
    assert.equal(await page.evaluate(() => crossKind), 'invalid_selection');
    assert.equal(await page.evaluate(() => pendingUnhandled), 0);
    assert.equal(await page.evaluate(() => revalidationCalls), 1);
    assert.equal(
      await page.evaluate(() =>
        scheduler.resumeDelivery(oldSelection).then(
          () => 'unexpected',
          (error) => error.code,
        ),
      ),
      'invalid_selection',
    );
    await close(page);
  }

  if (
    createArtifact === undefined &&
    (selected === undefined || selected === 'pending-local')
  ) {
    for (const kind of ['preparation', 'cleanup']) {
      const page = await fresh();
      await page.evaluate((kind) => {
        faultPut(
          kind === 'preparation' ? 'intents' : 'usage',
          kind === 'preparation' ? 'prepared' : undefined,
        );
        scheduler.wake(lane);
      }, kind);
      await waitPause(page, `${kind}_paused`);
      await page.evaluate((kind) => {
        globalThis.oldSelection = selection;
        globalThis.retryMethod =
          kind === 'preparation' ? 'retryPreparation' : 'retryCleanup';
        globalThis.pendingObservations = [];
        globalThis.crossKinds = [];
        globalThis.pendingUnhandled = 0;
        addEventListener('unhandledrejection', (event) => {
          event.preventDefault();
          pendingUnhandled += 1;
        });
        globalThis.onSchedulerEvent = (event) => {
          if (event.kind !== 'paused') return;
          const duplicate = scheduler[retryMethod](oldSelection);
          duplicate.catch(() => undefined);
          pendingObservations.push(duplicate === retrying);
          crossKinds = ['retryPreparation', 'retryCleanup', 'resumeDelivery']
            .filter((method) => method !== retryMethod)
            .map((method) =>
              scheduler[method](oldSelection).then(
                () => 'unexpected',
                (error) => error.code,
              ),
            );
        };
        globalThis.retrying = scheduler[retryMethod](oldSelection);
      }, kind);
      await page.evaluate(() => retrying);
      assert.deepEqual(await page.evaluate(() => pendingObservations), [true]);
      assert.deepEqual(await page.evaluate(() => Promise.all(crossKinds)), [
        'invalid_selection',
        'invalid_selection',
      ]);
      assert.equal(await page.evaluate(() => pendingUnhandled), 0);
      assert.equal(await page.evaluate(() => revalidationCalls), 0);
      assert.equal(requests().length, kind === 'preparation' ? 0 : 1);
      await page.evaluate(() => {
        selection = events.findLast((event) => event.kind === 'paused').pause
          .selection;
        scheduler.close();
      });
      assert.equal(
        await page.evaluate(() =>
          scheduler[retryMethod](selection).then(
            () => 'unexpected',
            (error) => error.code,
          ),
        ),
        'owner_not_ready',
      );
      await close(page);
    }
  }

  if (
    createArtifact === undefined &&
    (selected === undefined || selected === 'aggregate-diagnostic')
  ) {
    for (const cause of [
      'invalid_response',
      'rate_limited',
      'server_error',
      'ack_storage_failed',
    ]) {
      configure('normal', cause === 'rate_limited' ? 429 : 503);
      const page = await fresh('invalid_json');
      await page.evaluate(async () => {
        globalThis.testLanes = [lane];
        for (const character of ['b', 'c', 'd', 'e']) {
          const key = [...lane.slice(0, 4), 'art_' + character.repeat(32)];
          await enqueueLikeHead({
            key: character.repeat(32),
            lane: key,
            artifactId: key[4],
            base: { version: 0 },
          });
          testLanes.push(key);
        }
        testLanes.forEach((key) => scheduler.wake(key));
      });
      for (let count = 5; count <= 30; count += 5) {
        await page.waitForFunction(
          (count) =>
            events.filter((event) => event.kind === 'delivery_retrying')
              .length === count,
          count,
        );
        if (count < 30) await fireRetries(page);
      }
      const fireOne = () =>
        page.evaluate(() => {
          const timer = timers.find(
            (timer) => timer.active && timer.milliseconds < 15_000,
          );
          if (timer === undefined) throw new Error('missing eligible retry');
          timer.active = false;
          timer.callback();
        });
      await fireOne();
      await page.waitForFunction(
        () =>
          events.filter((event) => event.kind === 'delivery_retrying')
            .length === 31,
      );
      if (cause === 'ack_storage_failed') {
        setMode('normal');
        await page.evaluate(() => faultPut('intents', 'acknowledged'));
      } else if (cause !== 'invalid_response') setMode('status');
      await fireOne();
      await page.waitForFunction(
        () => events.filter((event) => event.kind === 'paused').length === 5,
      );
      const diagnosis = await page.evaluate(() => {
        const last = events.findLast((event) => event.kind === 'attempt');
        const pauses = events.filter((event) => event.kind === 'paused');
        const matching = pauses.find(
          (event) => JSON.stringify(event.lane) === JSON.stringify(last.lane),
        );
        const others = pauses.filter((event) => event !== matching);
        globalThis.aggregateSelection = others[0].pause.selection;
        return {
          actual: matching.pause.reason,
          scopes: others.map((event) => event.pause.scope),
          otherReasons: others.map((event) => event.pause.reason),
        };
      });
      assert.equal(
        diagnosis.actual,
        cause,
        'the 32nd dispatched failure keeps its classification',
      );
      assert.deepEqual(
        diagnosis.otherReasons,
        Array(4).fill('aggregate_exhausted'),
        'other lanes have a budget gate, not a fabricated network failure',
      );
      assert.deepEqual(diagnosis.scopes, Array(4).fill('coordinator'));
      assert.equal(requests().length, 32);
      await page.evaluate(() =>
        testLanes.forEach((key) => scheduler.wake(key)),
      );
      await page.evaluate(() => storageSnapshot());
      assert.equal(
        requests().length,
        32,
        'wake cannot replenish exhausted delivery budget',
      );
      assert.equal(await page.evaluate(() => revalidationCalls), 0);
      if (cause === 'invalid_response') {
        await page.evaluate(async () => {
          failRevalidation = true;
          globalThis.spentAggregateSelection = aggregateSelection;
          await scheduler
            .resumeDelivery(aggregateSelection)
            .catch(() => undefined);
          aggregateSelection = events.findLast(
            (event) => event.kind === 'paused',
          ).pause.selection;
          testLanes.forEach((key) => scheduler.wake(key));
        });
        await page.evaluate(async () => {
          globalThis.afterReadLane = [
            ...lane.slice(0, 4),
            'art_' + 'f'.repeat(32),
          ];
          await enqueueLikeHead({
            key: 'f'.repeat(32),
            lane: afterReadLane,
            artifactId: afterReadLane[4],
            base: { version: 0 },
          });
          scheduler.wake(afterReadLane);
        });
        await page.waitForFunction(
          () =>
            events.some(
              (event) =>
                event.kind === 'paused' && event.lane[4] === afterReadLane[4],
            ) || events.filter((event) => event.kind === 'attempt').length > 32,
        );
        assert.equal(
          requests().length,
          32,
          'failed bound read cannot refill budget for a newly admitted lane',
        );
        assert.equal(
          await page.evaluate(() =>
            scheduler.resumeDelivery(spentAggregateSelection).then(
              () => 'unexpected',
              (error) => error.code,
            ),
          ),
          'invalid_selection',
        );
        await page.evaluate(() => {
          failRevalidation = false;
        });
      }
      setMode('invalid_json');
      await page.evaluate(async () => {
        if (globalThis.restorePut !== undefined) restorePut();
        const first = scheduler.resumeDelivery(aggregateSelection);
        const duplicate = scheduler.resumeDelivery(aggregateSelection);
        globalThis.sameAggregatePromise = first === duplicate;
        await first;
      });
      assert.equal(await page.evaluate(() => sameAggregatePromise), true);
      await page.waitForFunction(
        () =>
          events.filter((event) => event.kind === 'delivery_retrying').length >
          31,
      );
      assert.equal(
        await page.evaluate(
          () =>
            events.filter((event) => event.kind === 'delivery_retrying')[31]
              .aggregateRemaining,
        ),
        31,
        'explicit resume begins exactly one new bounded epoch',
      );
      assert.equal(
        await page.evaluate(() => revalidationCalls),
        cause === 'invalid_response' ? 2 : 1,
      );
      assert.equal(
        await page.evaluate(() =>
          scheduler.resumeDelivery(aggregateSelection).then(
            () => 'unexpected',
            (error) => error.code,
          ),
        ),
        'invalid_selection',
      );
      await close(page);
    }
  }

  if (
    createArtifact === undefined &&
    (selected === undefined || selected === 'binding-observer-cleanup')
  ) {
    harness.configureBinding('normal');
    const page = await fresh();
    await page.evaluate(() => {
      faultPut('intents', 'acknowledged');
      scheduler.wake(lane);
    });
    await waitPause(page);
    const retained = await page.evaluate(() => storageSnapshot());
    harness.configureBinding('401stalled');
    const failure = await page.evaluate(async () => {
      restorePut();
      authenticationInvalidationException = makeAuthorityException();
      globalThis.bindingObserverUnhandled = 0;
      addEventListener('unhandledrejection', (event) => {
        event.preventDefault();
        bindingObserverUnhandled += 1;
      });
      const originalFetch = globalThis.fetch;
      const nativeFetch = originalFetch.bind(globalThis);
      globalThis.fetch = (input, init) => {
        if (String(input).endsWith('/v1/data-outbox/binding'))
          globalThis.observerFetchSignal = init.signal;
        return nativeFetch(input, init);
      };
      try {
        await scheduler.resumeDelivery(selection);
        return { code: 'unexpected' };
      } catch (error) {
        return {
          code: error.code,
          message: error.message,
          body: error.body,
          cause: error.cause,
        };
      } finally {
        globalThis.fetch = originalFetch;
      }
    });
    assert.deepEqual(failure, {
      code: 'owner_not_ready',
      message: 'owner_not_ready',
      body: undefined,
      cause: undefined,
    });
    assert.equal(
      await page.evaluate(() => stop.signal.aborted),
      false,
      'the throwing invalidator did not abort host authority',
    );
    assert.equal(
      await page.evaluate(() => observerFetchSignal.aborted),
      true,
      'a status observer exception must cancel its unread native Fetch body',
    );
    await harness.waitBindingClosed();
    assert.equal(harness.bindingReadCount(), 1);
    assert.equal(requests().length, 1);
    assert.deepEqual(await page.evaluate(() => storageSnapshot()), retained);
    assert.equal(await page.evaluate(() => bindingObserverUnhandled), 0);
    assert.equal(
      await page.evaluate(() => {
        try {
          scheduler.wake(lane);
          return 'unexpected';
        } catch (error) {
          return error.code;
        }
      }),
      'owner_not_ready',
    );
    harness.configureBinding('normal');
    await close(page);
  }

  if (
    createArtifact === undefined &&
    (selected === undefined ||
      selected === 'revalidation-mismatch' ||
      selected === 'revalidation-transient')
  ) {
    const transientCases = [
      'network',
      'redirect',
      '403',
      '404',
      '429',
      '503',
      'override_transient',
      '503binding',
    ];
    const terminalCases = [
      'held_mismatch',
      '401',
      '401malformed',
      '401stalled',
      'override_generation',
      'override_auth',
      'override_invalid',
      'override_throw',
      'override_bad_code',
      'override_bad_status',
      'override_bad_retry',
      'override_null',
      'override_binding',
      '409binding',
    ];
    for (const test of selected === 'revalidation-transient'
      ? transientCases
      : selected === 'revalidation-mismatch'
        ? terminalCases
        : [...terminalCases, ...transientCases]) {
      harness.configureBinding('normal');
      const page = await fresh();
      await page.evaluate(() => {
        faultPut('intents', 'acknowledged');
        scheduler.wake(lane);
      });
      await waitPause(page);
      const retained = await page.evaluate(() => storageSnapshot());
      harness.configureBinding(test.startsWith('override_') ? 'normal' : test);
      const arrived =
        test === 'held_mismatch' ? harness.waitBinding() : undefined;
      await page.evaluate((test) => {
        restorePut();
        globalThis.oldReadSelection = selection;
        globalThis.readPromiseIdentity = [];
        globalThis.readWrongKinds = [];
        globalThis.readUnhandled = 0;
        addEventListener('unhandledrejection', (event) => {
          event.preventDefault();
          readUnhandled += 1;
        });
        if (test === 'override_generation')
          globalThis.revalidationOverride = {
            kind: 'authority_lost',
            reason: 'actor_generation_changed',
          };
        if (test === 'override_auth')
          globalThis.revalidationOverride = {
            kind: 'authority_lost',
            reason: 'authentication_invalid',
          };
        if (test === 'override_invalid')
          globalThis.revalidationOverride = {
            kind: 'authorized',
            failure: { status: 401 },
          };
        if (test === 'override_binding')
          globalThis.revalidationOverride = {
            kind: 'read_failed',
            failure: { code: 'outbox_binding_changed', status: 409 },
          };
        if (test === 'override_transient')
          globalThis.revalidationOverride = {
            kind: 'read_failed',
            failure: {
              code: 'rate_limited',
              status: 429,
              retryAfterSeconds: 999999,
              body: 'private body',
              cause: 'private cause',
            },
          };
        if (test === 'override_bad_code')
          globalThis.revalidationOverride = {
            kind: 'read_failed',
            failure: { code: 'private-unknown', status: 503 },
          };
        if (test === 'override_bad_status')
          globalThis.revalidationOverride = {
            kind: 'read_failed',
            failure: { code: 'network_error', status: 999 },
          };
        if (test === 'override_bad_retry')
          globalThis.revalidationOverride = {
            kind: 'read_failed',
            failure: {
              code: 'rate_limited',
              status: 429,
              retryAfterSeconds: NaN,
            },
          };
        if (test === 'override_null') globalThis.revalidationOverride = null;
        if (test === 'override_throw')
          globalThis.revalidationOverride = Object.defineProperty({}, 'kind', {
            get() {
              throw makeAuthorityException();
            },
          });
        globalThis.onSchedulerEvent = (event) => {
          if (event.kind !== 'paused' || event.pause.revalidation === undefined)
            return;
          const duplicate = scheduler.resumeDelivery(oldReadSelection);
          duplicate.catch(() => undefined);
          readPromiseIdentity.push(duplicate === readAction);
          readWrongKinds = ['retryPreparation', 'retryCleanup'].map((method) =>
            scheduler[method](oldReadSelection).then(
              () => 'unexpected',
              (error) => error.code,
            ),
          );
        };
        globalThis.readAction = scheduler.resumeDelivery(selection);
        globalThis.revalidating = readAction.then(
          () => ({ code: 'unexpected' }),
          (error) => ({
            code: error.code,
            status: error.status,
            message: error.message,
            body: error.body,
            cause: error.cause,
          }),
        );
      }, test);
      if (arrived !== undefined) {
        await arrived;
        await page.evaluate(async () => {
          globalThis.otherLane = [...lane.slice(0, 4), 'art_' + 'c'.repeat(32)];
          await enqueueLikeHead({
            key: 'c'.repeat(32),
            lane: otherLane,
            artifactId: otherLane[4],
            base: { version: 0 },
          });
          scheduler.wake(otherLane);
        });
        harness.releaseBinding();
      }
      if (test.startsWith('401')) {
        await page.waitForFunction(() => observedBindingStatuses.includes(401));
        assert.equal(
          await page.evaluate(() => locallyAuthenticated),
          false,
          '401 headers revoke local auth before any body completion',
        );
      }
      const result = await page.evaluate(() => revalidating);
      assert.equal(result.body, undefined);
      assert.equal(result.cause, undefined);
      assert(
        !(result.message ?? '').includes('private') &&
          !(result.message ?? '').includes('http'),
      );
      await page.evaluate(() => storageSnapshot());
      assert.equal(await page.evaluate(() => revalidationCalls), 1);
      if (terminalCases.includes(test)) {
        assert.equal(
          test.startsWith('401') ||
            test === '409binding' ||
            test === 'override_binding'
            ? result.status
            : result.code,
          test.startsWith('401')
            ? 401
            : test === '409binding' || test === 'override_binding'
              ? 409
              : 'owner_not_ready',
        );
        assert.equal(
          requests().length,
          1,
          'terminal revalidation cannot dispatch another mutation',
        );
        assert.equal(
          await page.evaluate(() => {
            try {
              scheduler.wake(lane);
              return 'unexpected';
            } catch (error) {
              return error.code;
            }
          }),
          'owner_not_ready',
        );
        assert.equal(
          await page.evaluate(
            () => timers.filter((timer) => timer.active).length,
          ),
          0,
        );
        assert.equal(
          await page.evaluate(() =>
            scheduler.resumeDelivery(oldReadSelection).then(
              () => 'unexpected',
              (error) => error.code,
            ),
          ),
          'invalid_selection',
        );
        assert.equal(await page.evaluate(() => revalidationCalls), 1);
        if (test.startsWith('401') || test === 'override_auth')
          assert.equal(await page.evaluate(() => locallyAuthenticated), false);
      } else {
        const expectedCode =
          test === 'network' || test === 'redirect'
            ? 'network_error'
            : test === '503binding'
              ? 'outbox_binding_changed'
              : test === '403'
                ? 'forbidden'
                : test === '404'
                  ? 'not_found'
                  : test === '429' || test === 'override_transient'
                    ? 'rate_limited'
                    : 'unavailable';
        assert.equal(result.code, expectedCode);
        assert.equal(
          result.status,
          test === 'network' || test === 'redirect'
            ? 0
            : test === 'override_transient'
              ? 429
              : Number.parseInt(test, 10),
        );
        assert.deepEqual(
          await page.evaluate(() => storageSnapshot()),
          retained,
        );
        assert.equal(
          await page.evaluate(() =>
            scheduler.resumeDelivery(oldReadSelection).then(
              () => 'unexpected',
              (error) => error.code,
            ),
          ),
          'invalid_selection',
        );
        assert.equal(
          await page.evaluate(() => revalidationCalls),
          1,
          'spent selection cannot issue another bound read',
        );
        if (test !== 'network' && !test.startsWith('override_'))
          assert.equal(harness.bindingReadCount(), 1);
        assert.deepEqual(await page.evaluate(() => readPromiseIdentity), [
          true,
        ]);
        assert.deepEqual(
          await page.evaluate(() => Promise.all(readWrongKinds)),
          ['invalid_selection', 'invalid_selection'],
        );
        assert.equal(await page.evaluate(() => readUnhandled), 0);
        await waitPause(page);
        assert.equal(
          await page.evaluate(
            () =>
              events.findLast((event) => event.kind === 'paused').pause
                .revalidation.retryAfterSeconds,
          ),
          test === 'network' || test === 'redirect' ? undefined : 3600,
        );
        await page.evaluate(() => {
          onSchedulerEvent = undefined;
          scheduler.wake(lane);
        });
        assert.equal(requests().length, 1);
        await page.evaluate(() =>
          scheduler.resumeDelivery(selection).catch(() => undefined),
        );
        assert.equal(
          await page.evaluate(() => revalidationCalls),
          2,
          'fresh explicit action permits one further read',
        );
        assert.equal(requests().length, 1);
      }
      harness.configureBinding('normal');
      await close(page);
    }
  }

  if (createArtifact === undefined) {
    if (
      selected === undefined ||
      selected === 'binding-observer' ||
      selected === 'binding-observer-throw'
    ) {
      for (const behavior of selected === 'binding-observer-throw'
        ? ['throw']
        : ['mutate', 'throw', 'close']) {
        const page = await fresh('binding_changed');
        const callbackErrors = [];
        page.on('pageerror', (error) => callbackErrors.push(error.message));
        await page.evaluate(async (behavior) => {
          globalThis.otherLane = [...lane.slice(0, 4), 'art_' + 'c'.repeat(32)];
          await enqueueLikeHead({
            key: '2'.repeat(32),
            lane: otherLane,
            artifactId: otherLane[4],
            base: { version: 0 },
          });
          globalThis.callbackWakes = [];
          globalThis.onSchedulerEvent = (event) => {
            if (event.kind !== 'attempt' && event.kind !== 'paused') return;
            if (behavior === 'close') scheduler.close();
            try {
              scheduler.wake(otherLane);
              callbackWakes.push('unexpected');
            } catch (error) {
              callbackWakes.push(error.code);
            }
            if (behavior === 'throw')
              throw new Error('private observer exception');
            if (event.kind === 'paused' && event.pause.kind === 'blocked')
              event.pause.failure.reason = 'network_error';
          };
          scheduler.wake(lane);
          scheduler.wake(otherLane);
        }, behavior);
        await page.waitForFunction(() => callbackWakes.length > 0);
        await page.evaluate(() => storageSnapshot());
        assert.equal(
          await page.evaluate(() => {
            try {
              scheduler.wake(otherLane);
              return 'unexpected';
            } catch (error) {
              return error.code;
            }
          }),
          'owner_not_ready',
          'observer mutation or throwing cannot revive binding authority',
        );
        const wakes = await page.evaluate(() => callbackWakes);
        assert(
          wakes.every((code) => code === 'owner_not_ready'),
          'binding loss must fence before any external observer can reenter wake',
        );
        assert.equal(requests().length, 1);
        if (behavior === 'close')
          assert.equal(
            wakes.length,
            1,
            'explicit close in the first refusal callback must cancel later publication',
          );
        assert.equal(
          (await page.evaluate(() => store.lane(otherLane)))[0].state,
          'unprepared',
        );
        assert.equal(
          await page.evaluate(
            () => timers.filter((timer) => timer.active).length,
          ),
          0,
        );
        assert.deepEqual(callbackErrors, []);
        await close(page);
      }
    }
    if (selected === undefined || selected === 'fairness') {
      const page = await fresh('body_hold');
      const arrival = waitRequest();
      await page.evaluate(async () => {
        globalThis.otherLane = [...lane.slice(0, 4), 'art_' + 'c'.repeat(32)];
        await enqueueLikeHead({
          key: '2'.repeat(32),
          lane: otherLane,
          artifactId: otherLane[4],
          base: { version: 0 },
        });
        await enqueueLikeHead({
          key: '3'.repeat(32),
          base: { after: head.sequence },
        });
        scheduler.wake(lane);
      });
      await arrival;
      await page.evaluate(() => {
        scheduler.wake(lane);
        scheduler.wake(otherLane);
      });
      setMode('normal');
      harness.releaseHeld();
      await page.waitForFunction(
        () =>
          events.filter(
            (event) =>
              event.kind === 'attempt' &&
              event.result.kind === 'server_acknowledged',
          ).length === 3,
      );
      assert.deepEqual(
        requests().map((request) => request.url.split('/')[3]),
        [
          'art_' + 'a'.repeat(32),
          'art_' + 'c'.repeat(32),
          'art_' + 'a'.repeat(32),
        ],
        'an active lane wake must not put its successor ahead of a waiting lane',
      );
      await close(page);
    }
    if (selected === undefined || selected === 'binding-fence') {
      const page = await fresh('binding_changed');
      await page.evaluate(async () => {
        globalThis.otherLane = [...lane.slice(0, 4), 'art_' + 'c'.repeat(32)];
        await enqueueLikeHead({
          key: '2'.repeat(32),
          lane: otherLane,
          artifactId: otherLane[4],
          base: { version: 0 },
        });
        scheduler.wake(lane);
        scheduler.wake(otherLane);
      });
      await page.waitForFunction(() =>
        events.some(
          (event) =>
            event.kind === 'paused' &&
            event.pause.kind === 'blocked' &&
            event.pause.failure.reason === 'binding_changed',
        ),
      );
      assert.equal(
        await page.evaluate(() => {
          try {
            scheduler.wake(otherLane);
            return 'unexpected';
          } catch (error) {
            return error.code;
          }
        }),
        'owner_not_ready',
        'binding loss must fence the entire captured authority',
      );
      assert.equal(requests().length, 1);
      assert.equal(
        (await page.evaluate(() => store.lane(otherLane)))[0].state,
        'unprepared',
      );
      await close(page);
    }
    if (selected === undefined || selected === 'queued-backoff') {
      const page = await fresh('headers_hold_status');
      const arrival = waitRequest();
      await page.evaluate(() => {
        globalThis.retryLaneReads = 0;
        const original = store.lane.bind(store);
        store.lane = (key) => {
          if (events.some((event) => event.kind === 'delivery_retrying'))
            retryLaneReads += 1;
          return original(key);
        };
        globalThis.onSchedulerEvent = (event) => {
          if (
            event.kind === 'attempt' &&
            event.result.kind === 'delivery_failed'
          )
            scheduler.wake(lane);
        };
        scheduler.wake(lane);
      });
      await arrival;
      await page.evaluate(() => {
        for (let index = 0; index < 10; index += 1) scheduler.wake(lane);
      });
      harness.releaseHeld();
      await page.waitForFunction(() =>
        events.some((event) => event.kind === 'delivery_retrying'),
      );
      await page.evaluate(() => storageSnapshot());
      assert.equal(
        await page.evaluate(() => retryLaneReads),
        0,
        'a wake queued during HTTP cannot bypass the later Retry-After gate',
      );
      assert.equal(requests().length, 1);
      assert.deepEqual(
        await page.evaluate(() =>
          timers
            .filter((timer) => timer.active)
            .map((timer) => timer.milliseconds),
        ),
        [3_600_000],
      );
      await page.evaluate(() => {
        scheduler.close();
        for (const timer of timers) timer.callback();
      });
      assert.equal(requests().length, 1);
      assert.equal(
        await page.evaluate(
          () => timers.filter((timer) => timer.active).length,
        ),
        0,
      );
      await close(page);
    }
    for (const regression of ['renew-failure', 'cross-kind']) {
      if (selected === undefined || selected === regression) {
        const page = await fresh();
        await page.evaluate(async () => {
          globalThis.otherLane = [...lane.slice(0, 4), 'art_' + 'c'.repeat(32)];
          await enqueueLikeHead({
            key: '2'.repeat(32),
            lane: otherLane,
            artifactId: otherLane[4],
            base: { version: 0 },
          });
          faultPut('intents', 'acknowledged');
          scheduler.wake(lane);
          scheduler.wake(otherLane);
        });
        await page.waitForFunction(
          () =>
            events.filter(
              (event) =>
                event.kind === 'paused' || event.kind === 'delivery_retrying',
            ).length === 2,
        );
        assert.equal(
          await page.evaluate(
            () => events.filter((event) => event.kind === 'paused').length,
          ),
          2,
          'both ACK aborts must pause immediately without automatic retry',
        );
        await page.evaluate(() => {
          globalThis.firstSelection = events[1].pause.selection;
          globalThis.otherSelection = events[3].pause.selection;
          restorePut();
        });
        if (regression === 'renew-failure') {
          await page.evaluate(() => {
            const original = store.lane.bind(store);
            globalThis.restoreLane = () => {
              store.lane = original;
            };
            store.lane = (key) => {
              if (key[4] === otherLane[4])
                return Promise.reject(
                  new deliveryApi.OutboxError('storage_failed'),
                );
              return original(key);
            };
          });
          assert.equal(
            await page.evaluate(() =>
              scheduler.resumeDelivery(firstSelection).then(
                () => 'unexpected',
                (error) => error.code,
              ),
            ),
            'storage_failed',
          );
          assert.equal(requests().length, 2);
          assert.equal(
            await page.evaluate(
              () => events.filter((event) => event.kind === 'paused').length,
            ),
            2,
          );
          await page.evaluate(() => restoreLane());
          assert.equal(
            await page.evaluate(() =>
              scheduler.resumeDelivery(firstSelection).then(
                () => 'resumed',
                (error) => error.code,
              ),
            ),
            'invalid_selection',
            'failed staged renewal preserves the epoch, but consumes the one-read selection',
          );
          assert.equal(await page.evaluate(() => revalidationCalls), 1);
          assert.equal(requests().length, 2);
          assert.equal(
            await page.evaluate(() =>
              scheduler.resumeDelivery(otherSelection).then(
                () => 'unexpected',
                (error) => error.code,
              ),
            ),
            'owner_not_ready',
          );
        } else {
          assert.equal(
            await page.evaluate(() => {
              globalThis.resuming = scheduler.resumeDelivery(firstSelection);
              const wrong = scheduler.retryPreparation(firstSelection);
              globalThis.wrongKindResult = wrong.then(
                () => 'unexpected',
                (error) => error.code,
              );
              return wrong === resuming;
            }),
            false,
            'pending coalescing cannot substitute another action kind',
          );
          assert.equal(
            await page.evaluate(() => wrongKindResult),
            'invalid_selection',
          );
          await page.evaluate(() => resuming);
        }
        await close(page);
      }
    }
    if (selected === undefined || selected === 'preparation-read') {
      const page = await fresh();
      await page.evaluate(() => {
        faultPut('intents', 'prepared');
        scheduler.wake(lane);
      });
      await waitPause(page, 'preparation_paused');
      await page.evaluate(() => {
        restorePut();
        const original = store.lane.bind(store);
        let reads = 0;
        globalThis.restoreLane = () => {
          store.lane = original;
        };
        store.lane = (key) => {
          reads += 1;
          if (reads === 2)
            return Promise.reject(
              new deliveryApi.OutboxError('storage_failed'),
            );
          return original(key);
        };
      });
      assert.equal(
        await page.evaluate(() =>
          scheduler.retryPreparation(selection).then(
            () => 'unexpected',
            (error) => error.code,
          ),
        ),
        'storage_failed',
      );
      assert.equal(requests().length, 0);
      await page.evaluate(() => restoreLane());
      assert.equal(
        await page.evaluate(() =>
          scheduler.retryPreparation(selection).then(
            () => 'prepared',
            (error) => error.code,
          ),
        ),
        'prepared',
        'failed pre-preparation read must preserve the preparation gate and selection',
      );
      await page.waitForFunction(() =>
        events.some(
          (event) =>
            event.kind === 'attempt' &&
            event.result.kind === 'server_acknowledged',
        ),
      );
      await close(page);
    }
    if (selected === undefined || selected === 'lifetime') {
      const page = await fresh();
      const result = await page.evaluate(async () => {
        try {
          for (let index = 0; index < 257; index += 1) {
            const current =
              index === 0
                ? lane
                : [
                    ...lane.slice(0, 4),
                    'art_' + index.toString(16).padStart(32, '0'),
                  ];
            if (index !== 0)
              await enqueueLikeHead({
                key: crypto.randomUUID().replaceAll('-', ''),
                lane: current,
                artifactId: current[4],
                base: { version: 0 },
              });
            await new Promise((resolve) => {
              globalThis.onSchedulerEvent = (event) => {
                if (event.kind === 'attempt' && event.result.kind === 'idle')
                  resolve();
              };
              scheduler.wake(current);
            });
          }
          return 'drained';
        } catch (error) {
          return error.code ?? error.name;
        }
      });
      assert.equal(
        result,
        'drained',
        'healthy idle lanes must not consume lifetime capacity',
      );
      assert.equal(requests().length, 257);
      await close(page);
    }
    if (selected === undefined || selected === 'epoch') {
      const page = await fresh();
      await page.evaluate(async () => {
        globalThis.otherLane = [...lane.slice(0, 4), 'art_' + 'c'.repeat(32)];
        await enqueueLikeHead({
          key: '2'.repeat(32),
          lane: otherLane,
          artifactId: otherLane[4],
          base: { version: 0 },
        });
        faultPut('intents', 'acknowledged');
        scheduler.wake(lane);
        scheduler.wake(otherLane);
      });
      await page.waitForFunction(
        () =>
          events.filter(
            (event) =>
              event.kind === 'paused' || event.kind === 'delivery_retrying',
          ).length === 2,
      );
      assert.equal(
        await page.evaluate(
          () => events.filter((event) => event.kind === 'paused').length,
        ),
        2,
        'both ACK aborts must pause immediately without automatic retry',
      );
      await page.evaluate(async () => {
        globalThis.firstSelection = events[1].pause.selection;
        globalThis.oldOtherSelection = events[3].pause.selection;
        restorePut();
        await scheduler.resumeDelivery(firstSelection);
      });
      await page.waitForFunction(() =>
        events.some(
          (event) =>
            event.kind === 'attempt' &&
            event.result.kind === 'server_acknowledged',
        ),
      );
      assert.equal(
        await page.evaluate(() =>
          scheduler.resumeDelivery(oldOtherSelection).then(
            () => 'unexpected',
            (error) => error.code,
          ),
        ),
        'invalid_selection',
        'a different lane cannot renew the new epoch with an old selection',
      );
      await page.evaluate(async () => {
        const current = events.findLast(
          (event) => event.kind === 'paused' && event.lane[4] === otherLane[4],
        ).pause.selection;
        if (current === oldOtherSelection)
          throw new Error('missing fresh epoch selection');
        await scheduler.resumeDelivery(current);
      });
      await page.waitForFunction(
        () =>
          events.filter(
            (event) =>
              event.kind === 'attempt' &&
              event.result.kind === 'server_acknowledged',
          ).length === 2,
      );
      await close(page);
    }
    if (selected === undefined || selected === 'token') {
      const page = await fresh();
      const result = await page.evaluate(async () => {
        const result = { synchronous: [], rejected: [], unhandled: 0 };
        addEventListener('unhandledrejection', (event) => {
          event.preventDefault();
          result.unhandled += 1;
        });
        for (const token of [null, undefined, false, 1, 'token']) {
          for (const method of ['resumeDelivery', 'retryPreparation']) {
            try {
              await scheduler[method](token).catch((error) =>
                result.rejected.push(error.code),
              );
            } catch (error) {
              result.synchronous.push(error.name);
            }
          }
        }
        // Cross a task boundary through native storage, allowing the browser to
        // report genuinely unhandled rejection events before observing the count.
        await snapshot();
        return result;
      });
      assert.deepEqual(result, {
        synchronous: [],
        rejected: Array(10).fill('invalid_selection'),
        unhandled: 0,
      });
      await close(page);
    }
    if (selected !== undefined && selected !== 'main') return;
  }

  // Construction is inert. A deliberate wake drains healthy dependent work past
  // eight successes without consuming a failure budget or rebasing its payload.
  let page = await fresh();
  await page.evaluate(async () => {
    let previous = head;
    for (let index = 1; index < 10; index += 1) {
      previous = await enqueueLikeHead({
        key: crypto.randomUUID().replaceAll('-', ''),
        text: JSON.stringify({ original: index + 1 }),
        base: { after: previous.sequence },
      });
    }
  });
  assert.equal(requests().length, 0);
  await page.evaluate(() => {
    for (let index = 0; index < 10; index += 1) scheduler.wake(lane);
  });
  await waitAttempts(page, 11);
  assert.equal(requests().length, 10);
  assert.deepEqual(
    requests().map((request) => JSON.parse(request.body).base_data_version),
    Array.from({ length: 10 }, (_, index) => index),
  );
  assert.deepEqual(await page.evaluate(() => snapshot()), []);
  assert.equal(
    await page.evaluate(() => events.some((event) => event.kind === 'paused')),
    false,
  );
  await close(page);

  // A lost response retries the exact frozen bytes/key; successful replay is a
  // historical receipt, never an implicit read-latest or replacement operation.
  page = await fresh('lost_response');
  await page.evaluate(() => scheduler.wake(lane));
  await page.waitForFunction(() =>
    events.some((event) => event.kind === 'delivery_retrying'),
  );
  const first = requests()[0];
  configure('normal');
  await fireRetries(page);
  await page.waitForFunction(() =>
    events.some(
      (event) =>
        event.kind === 'attempt' && event.result.kind === 'server_acknowledged',
    ),
  );
  assert.equal(requests().length, 1);
  assert.equal(requests()[0].body, first.body);
  assert.equal(
    requests()[0].headers['idempotency-key'],
    first.headers['idempotency-key'],
  );
  if (createArtifact !== undefined)
    assert.equal(
      await page.evaluate(
        () =>
          events.find(
            (event) =>
              event.kind === 'attempt' &&
              event.result.kind === 'server_acknowledged',
          ).result.receipt.replayed,
      ),
      true,
    );
  await close(page);

  if (createArtifact !== undefined) {
    console.log(
      'outbox scheduler: real Rust dependent drain and same-key replay passed',
    );
    return;
  }

  page = await fresh();
  await page.evaluate(() => {
    const original = store.lane.bind(store);
    let admitted = false;
    store.lane = async (key) => {
      const rows = await original(key);
      if (rows.length === 0 && !admitted) {
        admitted = true;
        await enqueueLikeHead({ key: '2'.repeat(32), base: { version: 1 } });
        scheduler.wake(lane);
      }
      return rows;
    };
    scheduler.wake(lane);
  });
  await page.waitForFunction(
    () =>
      events.filter(
        (event) =>
          event.kind === 'attempt' &&
          event.result.kind === 'server_acknowledged',
      ).length === 2,
  );
  assert.equal(
    requests().length,
    2,
    'an active idle read must not lose a concurrent durable enqueue wake',
  );
  await close(page);

  // All lane mutations serialize. A wake during an active request cannot start
  // a second lane; completion puts the first lane behind the waiting lane.
  page = await fresh('headers_hold');
  const firstArrival = waitRequest();
  await page.evaluate(async () => {
    globalThis.otherLane = [...lane.slice(0, 4), 'art_' + 'c'.repeat(32)];
    await enqueueLikeHead({
      key: '2'.repeat(32),
      lane: otherLane,
      artifactId: otherLane[4],
      base: { version: 0 },
    });
    await enqueueLikeHead({
      key: '3'.repeat(32),
      base: { after: head.sequence },
    });
    scheduler.wake(lane);
    scheduler.wake(otherLane);
  });
  await firstArrival;
  assert.equal(requests().length, 1);
  configure('normal');
  await page.evaluate(() => fire(15_000));
  await page.waitForFunction(() =>
    events.some((event) => event.kind === 'delivery_retrying'),
  );
  await page.waitForFunction(() =>
    events.some(
      (event) =>
        event.kind === 'attempt' &&
        event.lane[4] === otherLane[4] &&
        event.result.kind === 'server_acknowledged',
    ),
  );
  await fireRetries(page);
  await page.waitForFunction(
    () =>
      events.filter(
        (event) =>
          event.kind === 'attempt' &&
          event.result.kind === 'server_acknowledged',
      ).length === 3,
  );
  assert.deepEqual(
    requests().map((request) => request.url.split('/')[3]),
    ['art_' + 'c'.repeat(32), 'art_' + 'a'.repeat(32), 'art_' + 'a'.repeat(32)],
  );
  await close(page);

  page = await fresh('invalid_json');
  await page.evaluate(() => scheduler.wake(lane));
  for (let attempt = 1; attempt < 8; attempt += 1) {
    await page.waitForFunction(
      (count) =>
        events.filter((event) => event.kind === 'delivery_retrying').length >=
        count,
      attempt,
    );
    await page.evaluate(() => {
      const timer = timers.find((timer) => timer.active);
      timer.active = false;
      timer.callback();
    });
  }
  await page.waitForFunction(
    () =>
      events.some((event) => event.kind === 'paused') ||
      events.filter((event) => event.kind === 'delivery_retrying').length === 8,
  );
  assert.equal(
    await page.evaluate(
      () => events.filter((event) => event.kind === 'delivery_retrying').length,
    ),
    7,
    'eighth lane failure must pause without scheduling another timer',
  );
  await waitPause(page);
  assert.equal(requests().length, 8);
  assert.equal(
    new Set(requests().map((request) => request.headers['idempotency-key']))
      .size,
    1,
  );
  assert.equal(new Set(requests().map((request) => request.body)).size, 1);
  assert.deepEqual(
    await page.evaluate(() =>
      events
        .filter((event) => event.kind === 'delivery_retrying')
        .map((event) => event.delayMilliseconds),
    ),
    [250, 500, 1000, 2000, 4000, 8000, 15000],
  );
  assert.equal(
    await page.evaluate(() => timers.filter((timer) => timer.active).length),
    0,
  );
  await page.evaluate(() => {
    scheduler.wake(lane);
    globalThis.oldSelection = selection;
    globalThis.failRevalidation = true;
  });
  assert.equal(
    await page.evaluate(() =>
      scheduler.resumeDelivery(selection).then(
        () => 'unexpected',
        (error) => error.code,
      ),
    ),
    'network_error',
  );
  assert.equal(requests().length, 8);
  await waitPause(page);
  configure('normal');
  await page.evaluate(async () => {
    failRevalidation = false;
    const first = scheduler.resumeDelivery(selection);
    const second = scheduler.resumeDelivery(selection);
    if (first !== second) throw new Error('pending resume did not coalesce');
    await first;
  });
  await page.waitForFunction(() =>
    events.some(
      (event) =>
        event.kind === 'attempt' && event.result.kind === 'server_acknowledged',
    ),
  );
  assert.equal(requests().length, 1);
  assert.equal(await page.evaluate(() => revalidationCalls), 2);
  assert.equal(
    await page.evaluate(() =>
      scheduler.resumeDelivery(oldSelection).then(
        () => 'unexpected',
        (error) => error.code,
      ),
    ),
    'invalid_selection',
  );
  await close(page);

  // A preparation abort is local-only, one action permits one retry. Cross-kind
  // tokens cannot turn a preparation retry into an epoch reset or HTTP attempt.
  page = await fresh();
  await page.evaluate(() => {
    faultPut('intents', 'prepared');
    scheduler.wake(lane);
  });
  await waitPause(page, 'preparation_paused');
  assert.equal(requests().length, 0);
  assert.equal(
    await page.evaluate(() =>
      scheduler.resumeDelivery(selection).then(
        () => 'unexpected',
        (error) => error.code,
      ),
    ),
    'invalid_selection',
  );
  await page.evaluate(() => scheduler.retryPreparation(selection));
  await page.waitForFunction(
    () => events.filter((event) => event.kind === 'paused').length === 2,
  );
  await waitPause(page, 'preparation_paused');
  assert.equal(requests().length, 0);
  await page.evaluate(() => {
    restorePut();
    return scheduler.retryPreparation(selection);
  });
  await page.waitForFunction(() =>
    events.some(
      (event) =>
        event.kind === 'attempt' && event.result.kind === 'server_acknowledged',
    ),
  );
  assert.equal(requests().length, 1);
  assert.equal(await page.evaluate(() => revalidationCalls), 0);
  await close(page);

  // An observed receipt with failed persistence pauses immediately. Its old
  // receipt cannot be reused locally; an explicit resume makes a fresh replay.
  page = await fresh();
  await page.evaluate(() => {
    faultPut('intents', 'acknowledged');
    scheduler.wake(lane);
  });
  await page.waitForFunction(() =>
    events.some(
      (event) => event.kind === 'paused' || event.kind === 'delivery_retrying',
    ),
  );
  assert.equal(
    await page.evaluate(() =>
      events.some((event) => event.kind === 'delivery_retrying'),
    ),
    false,
    'ACK abort must immediately pause even when HTTP budget remains',
  );
  await waitPause(page);
  assert.equal(requests().length, 1);
  const ackRequest = requests()[0];
  assert.equal(
    await page.evaluate(
      () => events.findLast((event) => event.kind === 'paused').pause.reason,
    ),
    'ack_storage_failed',
  );
  assert.equal(
    await page.evaluate(() => timers.filter((timer) => timer.active).length),
    0,
  );
  await page.evaluate(() => scheduler.wake(lane));
  assert.equal(requests().length, 1);
  await page.evaluate(async () => {
    restorePut();
    await enqueueLikeHead({
      key: '4'.repeat(32),
      base: { after: head.sequence },
    });
  });
  assert.equal(
    await page.evaluate(() =>
      scheduler.resumeDelivery(selection).then(
        () => 'unexpected',
        (error) => error.code,
      ),
    ),
    'invalid_selection',
  );
  assert.equal(
    requests().length,
    1,
    'stale snapshot refresh grants no delivery or budget',
  );
  await waitPause(page);
  await page.evaluate(() => scheduler.resumeDelivery(selection));
  await page.waitForFunction(
    () =>
      events.filter(
        (event) =>
          event.kind === 'attempt' &&
          event.result.kind === 'server_acknowledged',
      ).length === 2,
  );
  assert.equal(requests().length, 3);
  assert.equal(requests()[1].body, ackRequest.body);
  assert.equal(
    requests()[1].headers['idempotency-key'],
    ackRequest.headers['idempotency-key'],
  );
  await close(page);

  // Aggregate budget is shared by lanes and is never replenished by new wakes.
  page = await fresh('invalid_json');
  await page.evaluate(() => {
    faultPut('intents', 'prepared');
    scheduler.wake(lane);
  });
  await waitPause(page, 'preparation_paused');
  await page.evaluate(() => {
    globalThis.preparationSelection = selection;
    restorePut();
  });
  await page.evaluate(async () => {
    globalThis.allLanes = [];
    for (const character of ['b', 'c', 'd', 'e', 'f']) {
      const key = [...lane.slice(0, 4), 'art_' + character.repeat(32)];
      await enqueueLikeHead({
        key: character.repeat(32),
        lane: key,
        artifactId: key[4],
        base: { version: 0 },
      });
      allLanes.push(key);
    }
    allLanes.forEach((key) => scheduler.wake(key));
  });
  for (let round = 1; round <= 6; round += 1) {
    await page.waitForFunction(
      (count) =>
        events.filter((event) => event.kind === 'delivery_retrying').length >=
        count,
      round * 5,
    );
    if (round === 1) {
      setMode('normal');
      await page.evaluate(async () => {
        const healthy = [...lane.slice(0, 4), 'art_' + '1'.repeat(32)];
        globalThis.healthy = healthy;
        const healthyHead = await enqueueLikeHead({
          key: crypto.randomUUID().replaceAll('-', ''),
          lane: healthy,
          artifactId: healthy[4],
          base: { version: 0 },
        });
        await enqueueLikeHead({
          key: crypto.randomUUID().replaceAll('-', ''),
          lane: healthy,
          artifactId: healthy[4],
          text: '{"next":true}',
          base: { after: healthyHead.sequence },
        });
        faultPut('usage');
        scheduler.wake(healthy);
      });
      await page.waitForFunction(() =>
        events.some(
          (event) =>
            event.kind === 'attempt' &&
            event.result.kind === 'acknowledged_cleanup_pending',
        ),
      );
      await waitPause(page, 'cleanup_paused');
      await page.evaluate(() => {
        globalThis.cleanupBudgetSelection = selection;
        restorePut();
      });
      setMode('invalid_json');
    }
    await fireRetries(page);
  }
  await page.waitForFunction(
    () =>
      events.filter(
        (event) =>
          event.kind === 'attempt' && event.result.kind === 'delivery_failed',
      ).length >= 32,
  );
  // A native storage turn lets the last failure decision finish without sleeps.
  await page.evaluate(() => snapshot());
  assert.equal(
    requests().length,
    33,
    'aggregate exhaustion stops every mutation lane',
  );
  assert.equal(
    await page.evaluate(() => timers.filter((timer) => timer.active).length),
    0,
  );
  await page.waitForFunction(
    () => events.filter((event) => event.kind === 'paused').length === 7,
  );
  assert.equal(requests().length, 33);
  assert.equal(
    await page.evaluate(() => timers.filter((timer) => timer.active).length),
    0,
  );
  await page.evaluate(() => allLanes.forEach((key) => scheduler.wake(key)));
  assert.equal(requests().length, 33);
  await page.evaluate(() => scheduler.retryCleanup(cleanupBudgetSelection));
  await page.waitForFunction(
    () =>
      events.filter((event) => event.kind === 'paused').length === 8 ||
      events.filter(
        (event) =>
          event.kind === 'attempt' && event.result.kind === 'delivery_failed',
      ).length > 32,
  );
  assert.equal(
    requests().length,
    33,
    'cleanup retry cannot refill exhausted HTTP budget',
  );
  assert.equal(
    (await page.evaluate(() => store.lane(healthy)))[0].state,
    'unprepared',
  );
  await page.evaluate(() => scheduler.retryPreparation(preparationSelection));
  await page.waitForFunction(
    () => events.filter((event) => event.kind === 'paused').length === 9,
  );
  assert.equal(
    (await page.evaluate(() => snapshot()))[0].state,
    'prepared',
    'preparation retry must perform one local transaction even with exhausted HTTP budget',
  );
  assert.equal(
    requests().length,
    33,
    'preparation retry must not refill HTTP budget',
  );
  assert.equal(await page.evaluate(() => revalidationCalls), 0);
  const foreignWake = await page.evaluate(async () => {
    globalThis.foreignReads = 0;
    const original = store.lane.bind(store);
    store.lane = (key) => {
      if (key[0] !== owner) foreignReads += 1;
      return original(key);
    };
    const foreign = [...lane];
    foreign[0] = '0'.repeat(32);
    try {
      scheduler.wake(foreign);
      return 'unexpected';
    } catch (error) {
      return error.code;
    }
  });
  await page.evaluate(() => storageSnapshot());
  assert.deepEqual(
    { result: foreignWake, reads: await page.evaluate(() => foreignReads) },
    { result: 'owner_not_ready', reads: 0 },
    'even exhausted-budget pauses must reject a foreign owner before storage',
  );
  assert.equal(requests().length, 33);
  await close(page);

  // An observer receives copies: mutation of the observation cannot counterfeit
  // a durable ACK, reset retry counters, or release a successor.
  page = await fresh('invalid_json');
  await page.evaluate(() => {
    globalThis.onSchedulerEvent = (event) => {
      if (event.kind === 'attempt') event.result.kind = 'server_acknowledged';
    };
    scheduler.wake(lane);
  });
  await page.waitForFunction(() =>
    events.some((event) => event.kind === 'attempt'),
  );
  assert.equal(requests().length, 1);
  assert.equal((await page.evaluate(() => snapshot()))[0].state, 'prepared');
  assert.equal(
    await page.evaluate(
      () => events.filter((event) => event.kind === 'delivery_retrying').length,
    ),
    1,
    'observer mutations cannot forge durable ACK progress',
  );
  await close(page);

  page = await fresh();
  configure('status', 429);
  await page.evaluate(() => scheduler.wake(lane));
  await page.waitForFunction(() =>
    events.some((event) => event.kind === 'delivery_retrying'),
  );
  assert.equal(
    await page.evaluate(
      () =>
        events.find((event) => event.kind === 'delivery_retrying')
          .delayMilliseconds,
    ),
    3_600_000,
  );
  await close(page);

  // Closing cancels timers immediately, including manually invoked stale clock
  // callbacks. The host signal revokes the same native transport/IDB authority.
  page = await fresh('invalid_json');
  await page.evaluate(() => scheduler.wake(lane));
  await page.waitForFunction(() =>
    events.some((event) => event.kind === 'delivery_retrying'),
  );
  await page.evaluate(() => {
    scheduler.close();
    for (const timer of timers) timer.callback();
  });
  assert.equal(requests().length, 1);
  assert.equal(
    await page.evaluate(() => timers.filter((timer) => timer.active).length),
    0,
  );
  assert.equal(
    await page.evaluate(() => {
      try {
        scheduler.wake(lane);
        return 'unexpected';
      } catch (error) {
        return error.code;
      }
    }),
    'owner_not_ready',
  );
  await close(page);
  configure('normal');
  console.log(
    'outbox scheduler: native serialization, 8/32 budgets, typed pauses and close fencing passed',
  );
}
