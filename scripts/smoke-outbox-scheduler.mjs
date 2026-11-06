import assert from 'node:assert/strict';

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
            'resumed',
            'failed staged renewal must preserve the original epoch and selection',
          );
          await page.waitForFunction(() =>
            events.some(
              (event) =>
                event.kind === 'attempt' &&
                event.result.kind === 'server_acknowledged',
            ),
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
