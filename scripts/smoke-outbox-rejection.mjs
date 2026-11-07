import assert from 'node:assert/strict';

const originalBody =
  '{"base_data_version":0,"path":"marker","value":{"original":1}}';

/** Real-server rejection/replay contrasts; no public sender or cleanup is added. */
export async function checkRejectionResume(harness, selected) {
  const cases = [
    ['unforwarded', () => checkUnforwarded(harness)],
    ['committed', () => checkCommitted(harness)],
    ['fresh-conflict', () => checkFreshConflict(harness)],
    ['preparation-first-conflict', () => checkFreshConflict(harness, true)],
    [
      'preparation-proof-mismatch',
      () => checkPreparationProofMismatch(harness),
    ],
    ['preparation-unknown-rejection', () => checkUnforwarded(harness, true)],
    [
      'preparation-recreated-rejection',
      () => checkRecreatedPreparation(harness),
    ],
    [
      'preparation-budget-conflict',
      () => checkRecreatedPreparation(harness, false),
    ],
    ['capture-abort', () => checkPauseCapture(harness, 'abort')],
    ['capture-authority', () => checkPauseCapture(harness, 'authority')],
  ];
  if (selected !== undefined && !cases.some(([name]) => name === selected))
    throw new Error('Unknown rejection sequence');
  for (const [name, check] of cases) {
    if (selected === undefined || selected === name) await check();
  }
}

async function setup(harness) {
  harness.configure('normal');
  const artifact = await harness.createArtifact();
  const page = await harness.setup({ artifact });
  await page.evaluate(() => {
    globalThis.rejectionUnhandled = 0;
    addEventListener('unhandledrejection', (event) => {
      event.preventDefault();
      rejectionUnhandled += 1;
    });
    globalThis.rejectionSnapshot = async () => {
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
    installScheduler();
  });
  assert.equal(
    await page.evaluate(() => auth.actor_id),
    harness.expectedActorId,
  );
  const key = await page.evaluate(() => head.key);
  return {
    page,
    artifact,
    expected: { method: 'PUT', body: originalBody, key },
  };
}

async function startUnknown(page, localPreparation = false) {
  if (!localPreparation) await page.evaluate(() => scheduler.wake(lane));
  // The published retry observation and installed timer follow the completed
  // preparation transaction and HTTP failure. No readonly scan is used as a
  // substitute for scheduler publication.
  await page.waitForFunction(
    () =>
      events.some((event) => event.kind === 'delivery_retrying') &&
      timers.some((timer) => timer.active && timer.milliseconds === 250),
  );
  const observation = await page.evaluate(() => {
    const result = events.find(
      (event) =>
        event.kind === 'attempt' && event.result.kind === 'delivery_failed',
    ).result;
    return {
      result: {
        kind: result.kind,
        failure: result.failure,
        outcome: result.outcome,
        priorDelivery: result.priorDelivery,
      },
      epoch: scheduler.epoch,
      failures: scheduler.aggregateFailures,
    };
  });
  assert.deepEqual(observation.result, {
    kind: 'delivery_failed',
    failure: { reason: 'network_error', status: 0 },
    outcome: 'unknown',
    priorDelivery: 'none',
  });
  assert.equal(observation.epoch, 0);
  assert.equal(observation.failures, 1);
  const snapshot = await page.evaluate(() => rejectionSnapshot());
  assert.equal(snapshot.rows.length, 1);
  assert.equal(snapshot.rows[0].state, 'prepared');
  assert.deepEqual(snapshot.rows[0].request, {
    method: 'PUT',
    body: originalBody,
    base: 0,
  });
  return snapshot;
}

function requestIdentity(request) {
  return {
    method: request.method,
    body: request.body,
    key: request.headers['idempotency-key'],
  };
}

async function checkUnforwarded(harness, localPreparation = false) {
  const { page, artifact, expected } = await setup(harness);
  harness.configure('drop_before_forward');
  if (localPreparation) await retryAfterPreparationAbort(harness, page);
  const prepared = await startUnknown(page, localPreparation);
  assert.deepEqual(
    harness.requests().map((request) => requestIdentity(request)),
    [expected],
  );
  assert.equal(
    harness.forwardedMutations(),
    0,
    'the first request did not reach Rust',
  );
  const before = await harness.canonical(artifact);
  assert.equal(before.data_version, 0);
  const independent = await harness.independentWrite(artifact, 0, {
    canonical: 1,
  });
  assert.equal(independent.data_version, 1);
  harness.setMode('normal');
  await page.evaluate(() => fire(250));
  await page.waitForFunction(() =>
    events.some(
      (event) =>
        event.kind === 'paused' && event.pause.kind !== 'preparation_paused',
    ),
  );
  const state = await page.evaluate(() => {
    const paused = events.findLast((event) => event.kind === 'paused').pause;
    globalThis.rejectionSelection = paused.selection;
    return {
      pause: { kind: paused.kind, reason: paused.reason, scope: paused.scope },
      result: events.filter((event) => event.kind === 'attempt').at(-1).result,
      epoch: scheduler.epoch,
      failures: scheduler.aggregateFailures,
      reads: revalidationCalls,
      retryTimers: timers.filter(
        (timer) => timer.active && timer.milliseconds < 15_000,
      ).length,
    };
  });
  assert.deepEqual(state.result.failure, {
    reason: 'data_conflict',
    status: 409,
    currentDataVersion: 1,
  });
  assert.deepEqual(
    harness.requests().map((request) => requestIdentity(request)),
    [expected, expected],
  );
  assert.equal(
    harness.forwardedMutations(),
    1,
    'the later rejection came from Rust',
  );
  const afterRejection = await page.evaluate(() => rejectionSnapshot());
  const replies = harness.responses();
  assert.equal(replies.length, 1);
  assert.equal(replies[0].status, 409);
  assert.ok(Buffer.byteLength(replies[0].body) <= 65_536);
  const rejection = JSON.parse(replies[0].body);
  assert.equal(rejection?.error?.code, 'data_conflict');
  assert.equal(rejection?.error?.current_data_version, 1);
  console.log(
    'outbox rejection evidence: ' +
      JSON.stringify({
        sequence: 'unforwarded',
        before: prepared,
        after: afterRejection,
        canonicalBefore: before,
        independentReceipt: independent,
        response: {
          status: replies[0].status,
          code: rejection.error.code,
          currentDataVersion: rejection.error.current_data_version,
        },
        pause: state.pause,
        epoch: state.epoch,
        failures: state.failures,
      }),
  );
  assert.deepEqual(
    afterRejection,
    prepared,
    'later rejection leaves the full prepared state and accounting intact',
  );
  assert.equal(state.epoch, 0);
  assert.equal(state.failures, 2);
  assert.equal(state.reads, 0);
  assert.equal(state.retryTimers, 0);
  // Expected fail-first boundary: retained uncertainty must remain actionable
  // with exact-key resume, rather than becoming a definitive blocked choice.
  assert.equal(
    state.pause.kind,
    'retry_paused',
    'an unknown head retains its explicit delivery-resume path after a later rejection',
  );
  assert.equal(state.pause.reason, 'unresolved_after_rejection');
  assert.equal(state.pause.scope, 'lane');
  assert.equal(state.result.initialState, 'prepared');
  assert.equal(state.result.priorDelivery, 'unknown');
  const pendingKinds = await page.evaluate(() => {
    globalThis.rejectionResume = scheduler.resumeDelivery(rejectionSelection);
    const duplicate = scheduler.resumeDelivery(rejectionSelection);
    const wrong = [
      scheduler.retryPreparation(rejectionSelection),
      scheduler.retryCleanup(rejectionSelection),
      scheduler.discardUnsent(rejectionSelection),
      scheduler.retireEmptyPreparationGate(rejectionSelection),
    ];
    // Install handlers before returning so a deliberately broken kind guard
    // still produces a precise assertion, never an unhandled rejection.
    globalThis.rejectionResumeSettlement = Promise.allSettled([
      rejectionResume,
      duplicate,
    ]);
    globalThis.rejectionWrongKinds = Promise.all(
      wrong.map((call) =>
        call.then(
          () => 'unexpected',
          (error) => error.code,
        ),
      ),
    );
    return {
      same: rejectionResume === duplicate,
      wrongBorrowed: wrong.map((call) => call === rejectionResume),
    };
  });
  assert.deepEqual(pendingKinds, {
    same: true,
    wrongBorrowed: [false, false, false, false],
  });
  const resumed = await page.evaluate(async () => ({
    settlements: (await rejectionResumeSettlement).map(
      (result) => result.status,
    ),
    wrong: await rejectionWrongKinds,
    reads: revalidationCalls,
  }));
  assert.deepEqual(resumed, {
    settlements: ['fulfilled', 'fulfilled'],
    wrong: Array(4).fill('invalid_selection'),
    reads: 1,
  });
  await page.waitForFunction(
    () =>
      events.filter(
        (event) =>
          event.kind === 'paused' && event.pause.kind !== 'preparation_paused',
      ).length === 2,
  );
  assert.deepEqual(
    harness.requests().map((request) => requestIdentity(request)),
    [expected, expected, expected],
  );
  assert.equal(harness.forwardedMutations(), 2);
  assert.deepEqual(await page.evaluate(() => rejectionSnapshot()), prepared);
  assert.deepEqual(
    await page.evaluate(() => ({
      epoch: scheduler.epoch,
      failures: scheduler.aggregateFailures,
      reason: events.findLast((event) => event.kind === 'paused').pause.reason,
      unhandled: rejectionUnhandled,
    })),
    {
      epoch: 1,
      failures: 1,
      reason: 'unresolved_after_rejection',
      unhandled: 0,
    },
  );
  assert.equal(
    await page.evaluate(() =>
      scheduler.resumeDelivery(rejectionSelection).then(
        () => 'unexpected',
        (error) => error.code,
      ),
    ),
    'invalid_selection',
  );
  assert.equal(await page.evaluate(() => revalidationCalls), 1);
  const canonical = await harness.canonical(artifact);
  assert.equal(canonical.data_version, 1);
  assert.deepEqual(canonical.value, { canonical: 1 });
  await harness.close(page);
  console.log(
    'outbox rejection: ' +
      (localPreparation ? 'preparation-unknown-rejection' : 'unforwarded') +
      ' passed',
  );
}

async function checkCommitted(harness) {
  const { page, artifact, expected } = await setup(harness);
  harness.configure('lost_response');
  const prepared = await startUnknown(page);
  assert.equal(
    harness.forwardedMutations(),
    1,
    'the original write reached Rust',
  );
  const committed = await harness.canonical(artifact);
  assert.equal(committed.data_version, 1);
  assert.deepEqual(committed.value, { marker: { original: 1 } });
  const independent = await harness.independentWrite(artifact, 1, {
    canonical: 2,
  });
  assert.equal(independent.data_version, 2);
  harness.setMode('normal');
  await page.evaluate(() => fire(250));
  await page.waitForFunction(
    () => events.filter((event) => event.kind === 'attempt').length >= 2,
  );
  const observation = await page.evaluate(() => {
    const attempts = events.filter((event) => event.kind === 'attempt');
    return {
      result: attempts[1].result,
      order: attempts.map((event) => event.result.kind),
    };
  });
  const replies = harness.responses();
  const receipts = replies.map((reply) => {
    assert.ok(Buffer.byteLength(reply.body) <= 65_536);
    return JSON.parse(reply.body);
  });
  const after = await page.evaluate(() => rejectionSnapshot());
  const canonical = await harness.canonical(artifact);
  console.log(
    'outbox rejection evidence: ' +
      JSON.stringify({
        sequence: 'committed',
        before: prepared,
        after,
        canonicalAfterCommit: committed,
        canonicalAfterReplay: canonical,
        attemptOrder: observation.order,
        responses: receipts.map((receipt, index) => ({
          status: replies[index].status,
          dataVersion: receipt.data_version,
          replayed: receipt.replayed,
        })),
      }),
  );
  const result = observation.result;
  assert.equal(
    result.kind,
    'server_acknowledged',
    'a real same-key replay receipt follows the durable ACK path',
  );
  assert.deepEqual(observation.order.slice(0, 2), [
    'delivery_failed',
    'server_acknowledged',
  ]);
  assert.equal(result.receipt.artifact_id, artifact);
  assert.equal(result.receipt.path, 'marker');
  assert.equal(result.receipt.data_version, 1);
  assert.equal(result.receipt.replayed, true);
  assert.equal(result.ackDurable, true);
  assert.deepEqual(
    harness.requests().map((request) => requestIdentity(request)),
    [expected, expected],
  );
  assert.equal(harness.forwardedMutations(), 2);
  assert.deepEqual(
    replies.map((reply) => reply.status),
    [200, 200],
  );
  assert.deepEqual(
    receipts.map((receipt) => receipt.data_version),
    [1, 1],
  );
  assert.deepEqual(
    receipts.map((receipt) => receipt.replayed),
    [false, true],
  );
  assert.deepEqual(after.rows, []);
  assert.equal(after.usage.rows, 0);
  assert.equal(after.usage.bytes, 0);
  assert.equal(canonical.data_version, 2);
  assert.deepEqual(canonical.value, { canonical: 2 });
  assert.deepEqual(
    await page.evaluate(() => ({
      epoch: scheduler.epoch,
      failures: scheduler.aggregateFailures,
      paused: events.filter((event) => event.kind === 'paused').length,
      unhandled: rejectionUnhandled,
    })),
    { epoch: 0, failures: 1, paused: 0, unhandled: 0 },
  );
  await harness.close(page);
  console.log('outbox rejection: committed passed');
}

async function retryAfterPreparationAbort(
  harness,
  page,
  blockedBudget = false,
) {
  await page.evaluate(() => {
    faultPut('intents', 'prepared');
    scheduler.wake(lane);
  });
  await page.waitForFunction(() =>
    events.some(
      (event) =>
        event.kind === 'paused' && event.pause.kind === 'preparation_paused',
    ),
  );
  assert.equal(harness.requests().length, 0, 'preparation abort sends no HTTP');
  const beforeRetry = await page.evaluate(() => rejectionSnapshot());
  assert.equal(beforeRetry.rows.length, 1);
  assert.equal(beforeRetry.rows[0].state, 'unprepared');
  assert.deepEqual(
    await page.evaluate(() => ({
      failures: scheduler.aggregateFailures,
      epoch: scheduler.epoch,
      reads: revalidationCalls,
    })),
    { failures: 0, epoch: 0, reads: 0 },
  );
  await page.evaluate(async (blockedBudget) => {
    restorePut();
    if (blockedBudget) scheduler.aggregateFailures = 32;
    const choice = events.findLast((event) => event.kind === 'paused').pause
      .selection;
    const first = scheduler.retryPreparation(choice);
    const duplicate = scheduler.retryPreparation(choice);
    globalThis.rejectionPreparationSame = first === duplicate;
    const wrong = scheduler.resumeDelivery(choice).then(
      () => 'unexpected',
      (error) => error.code,
    );
    await first;
    globalThis.rejectionPreparationWrong = await wrong;
  }, blockedBudget);
  assert.deepEqual(
    await page.evaluate(() => ({
      same: rejectionPreparationSame,
      wrong: rejectionPreparationWrong,
    })),
    { same: true, wrong: 'invalid_selection' },
  );
}

async function checkPreparationProofMismatch(harness) {
  const { page, artifact, expected } = await setup(harness);
  await harness.independentWrite(artifact, 0, { canonical: 1 });
  harness.configure('normal');
  await page.evaluate(async () => {
    globalThis.rejectionPrepared = await store.prepare(
      head,
      authorize,
      stop.signal,
    );
  });
  assert.equal(harness.requests().length, 0);
  const before = await page.evaluate(() => rejectionSnapshot());
  const result = await page.evaluate(() =>
    kernel.run(lane, {
      ...rejectionPrepared,
      request: {
        ...rejectionPrepared.request,
        body: rejectionPrepared.request.body + ' ',
      },
    }),
  );
  assert.equal(result.kind, 'delivery_failed');
  assert.equal(result.initialState, 'prepared');
  assert.equal(
    result.priorDelivery,
    'unknown',
    'a different body cannot prove this exact prepared request was never sent',
  );
  assert.equal(result.failure.reason, 'data_conflict');
  assert.deepEqual(await page.evaluate(() => rejectionSnapshot()), before);
  assert.deepEqual(
    harness.requests().map((request) => requestIdentity(request)),
    [expected],
  );
  assert.equal(harness.responses()[0].status, 409);
  assert.equal(await page.evaluate(() => rejectionUnhandled), 0);
  await harness.close(page);
  console.log('outbox rejection: preparation-proof-mismatch passed');
}

async function checkRecreatedPreparation(harness, recreate = true) {
  const { page, artifact, expected } = await setup(harness);
  await harness.independentWrite(artifact, 0, { canonical: 1 });
  harness.configure('normal');
  // Inject only the existing exhausted gate so a successful local preparation
  // can be observed before any dispatch. This does not prove failure counting.
  await retryAfterPreparationAbort(harness, page, true);
  await page.waitForFunction(() =>
    events.some(
      (event) => event.kind === 'paused' && event.pause.kind === 'retry_paused',
    ),
  );
  assert.equal(harness.requests().length, 0);
  const prepared = await page.evaluate(() => rejectionSnapshot());
  assert.equal(prepared.rows[0].state, 'prepared');
  assert.deepEqual(
    await page.evaluate(() => ({
      failures: scheduler.aggregateFailures,
      epoch: scheduler.epoch,
      reads: revalidationCalls,
    })),
    { failures: 32, epoch: 0, reads: 0 },
  );
  if (recreate) {
    await page.evaluate(() => {
      scheduler.close();
      installScheduler();
      scheduler.wake(lane);
    });
  } else {
    await page.evaluate(() => {
      const choice = events.findLast((event) => event.kind === 'paused').pause
        .selection;
      return scheduler.resumeDelivery(choice);
    });
  }
  await page.waitForFunction(() =>
    events.some(
      (event) =>
        event.kind === 'attempt' && event.result.kind === 'delivery_failed',
    ),
  );
  await page.evaluate(() => scheduler.activeTask);
  const observed = await page.evaluate(() => ({
    pause: events.findLast((event) => event.kind === 'paused').pause,
    result: events.findLast((event) => event.kind === 'attempt').result,
    failures: scheduler.aggregateFailures,
    epoch: scheduler.epoch,
    reads: revalidationCalls,
    unhandled: rejectionUnhandled,
  }));
  assert.equal(observed.result.initialState, 'prepared');
  assert.equal(observed.result.priorDelivery, recreate ? 'unknown' : 'none');
  assert.equal(observed.result.failure.reason, 'data_conflict');
  assert.equal(observed.pause.kind, recreate ? 'retry_paused' : 'blocked');
  if (recreate)
    assert.equal(observed.pause.reason, 'unresolved_after_rejection');
  assert.equal(observed.failures, 1);
  assert.equal(observed.epoch, recreate ? 0 : 1);
  assert.equal(observed.reads, recreate ? 0 : 1);
  assert.equal(observed.unhandled, 0);
  assert.deepEqual(await page.evaluate(() => rejectionSnapshot()), prepared);
  assert.deepEqual(
    harness.requests().map((request) => requestIdentity(request)),
    [expected],
  );
  assert.equal(harness.responses()[0].status, 409);
  await harness.close(page);
  console.log(
    'outbox rejection: ' +
      (recreate
        ? 'preparation-recreated-rejection'
        : 'preparation-budget-conflict') +
      ' passed',
  );
}

async function checkFreshConflict(harness, retryPreparation = false) {
  const { page, artifact, expected } = await setup(harness);
  const independent = await harness.independentWrite(artifact, 0, {
    canonical: 1,
  });
  assert.equal(independent.data_version, 1);
  harness.configure('normal');
  if (retryPreparation) {
    await retryAfterPreparationAbort(harness, page);
  } else await page.evaluate(() => scheduler.wake(lane));
  await page.waitForFunction(
    () =>
      events.some(
        (event) =>
          event.kind === 'attempt' && event.result.kind === 'delivery_failed',
      ) &&
      events.some(
        (event) =>
          event.kind === 'paused' && event.pause.kind !== 'preparation_paused',
      ),
  );
  const observed = await page.evaluate(() => ({
    pause: events.findLast((event) => event.kind === 'paused').pause,
    attempt: events.find(
      (event) =>
        event.kind === 'attempt' && event.result.kind === 'delivery_failed',
    ).result,
    epoch: scheduler.epoch,
    failures: scheduler.aggregateFailures,
    retryTimers: timers.filter(
      (timer) => timer.active && timer.milliseconds < 15_000,
    ).length,
    unhandled: rejectionUnhandled,
  }));
  assert.equal(
    observed.attempt.initialState,
    retryPreparation ? 'prepared' : 'unprepared',
  );
  assert.equal(observed.attempt.priorDelivery, 'none');
  assert.deepEqual(observed.attempt.failure, {
    reason: 'data_conflict',
    status: 409,
    currentDataVersion: 1,
  });
  const replies = harness.responses();
  assert.equal(replies.length, 1);
  assert.equal(replies[0].status, 409);
  assert.ok(Buffer.byteLength(replies[0].body) <= 65_536);
  const rejection = JSON.parse(replies[0].body);
  assert.equal(rejection?.error?.code, 'data_conflict');
  assert.equal(rejection?.error?.current_data_version, 1);
  console.log(
    'outbox rejection preparation evidence: ' +
      JSON.stringify({
        retryPreparation,
        response: {
          status: replies[0].status,
          code: rejection.error.code,
          currentDataVersion: rejection.error.current_data_version,
        },
        observed,
        snapshot: await page.evaluate(() => rejectionSnapshot()),
      }),
  );
  assert.equal(
    observed.pause.kind,
    'blocked',
    'a fresh first-attempt rejection keeps its existing blocked behavior',
  );
  assert.equal(observed.pause.failure.reason, 'data_conflict');
  assert.equal(observed.epoch, 0);
  assert.equal(observed.failures, 1);
  assert.equal(observed.retryTimers, 0);
  assert.equal(observed.unhandled, 0);
  assert.deepEqual(
    harness.requests().map((request) => requestIdentity(request)),
    [expected],
  );
  const stored = await page.evaluate(() => rejectionSnapshot());
  assert.equal(stored.rows.length, 1);
  assert.equal(stored.rows[0].state, 'prepared');
  assert.deepEqual(stored.rows[0].request, {
    method: 'PUT',
    body: originalBody,
    base: 0,
  });
  await harness.close(page);
  console.log(
    'outbox rejection: ' +
      (retryPreparation ? 'preparation-first-conflict' : 'fresh-conflict') +
      ' passed',
  );
}

async function checkPauseCapture(harness, fault) {
  const { page, artifact, expected } = await setup(harness);
  harness.configure('drop_before_forward');
  await startUnknown(page);
  assert.equal(harness.forwardedMutations(), 0);
  const other = await harness.createArtifact();
  await page.evaluate(async (other) => {
    globalThis.rejectionOtherLane = [...lane.slice(0, 4), other];
    await enqueueLikeHead({
      lane: rejectionOtherLane,
      artifactId: other,
      key: crypto.randomUUID().replaceAll('-', ''),
      base: { version: 0 },
      text: '{"independent":1}',
    });
  }, other);
  const before = await page.evaluate(() => rejectionSnapshot());
  await harness.independentWrite(artifact, 0, { canonical: 1 });
  await page.evaluate((fault) => {
    globalThis.captureFault = { success: 0, complete: 0, abort: 0 };
    const getAll = IDBIndex.prototype.getAll;
    globalThis.restoreRejectionCapture = () => {
      IDBIndex.prototype.getAll = getAll;
    };
    globalThis.onSchedulerEvent = (event) => {
      if (
        event.kind !== 'attempt' ||
        event.result.kind !== 'delivery_failed' ||
        event.result.failure.reason !== 'data_conflict'
      )
        return;
      IDBIndex.prototype.getAll = function (...args) {
        const request = Reflect.apply(getAll, this, args);
        if (
          this.name === 'laneSequence' &&
          this.objectStore.name === 'intents'
        ) {
          const tx = this.objectStore.transaction;
          tx.addEventListener(
            'complete',
            () => {
              captureFault.complete += 1;
            },
            { once: true },
          );
          tx.addEventListener(
            'abort',
            () => {
              captureFault.abort += 1;
            },
            { once: true },
          );
          request.addEventListener(
            'success',
            () => {
              captureFault.success += 1;
              if (fault === 'abort') tx.abort();
              else deliveryContext.invalidateAuthentication();
            },
            { once: true },
          );
          IDBIndex.prototype.getAll = getAll;
        }
        return request;
      };
      scheduler.wake(rejectionOtherLane);
    };
  }, fault);
  harness.setMode('normal');
  await page.evaluate(() => fire(250));
  await page.waitForFunction(
    () =>
      captureFault.success === 1 &&
      captureFault.complete + captureFault.abort === 1,
  );
  const observed = await page.evaluate(async () => {
    // Wait for the operation that publishes/fences scheduler state, rather
    // than assuming an IDB terminal event has already run its microtasks.
    await scheduler.activeTask?.catch(() => undefined);
    restoreRejectionCapture();
    let wake;
    try {
      scheduler.wake(rejectionOtherLane);
      wake = 'unexpected';
    } catch (error) {
      wake = error.code;
    }
    return {
      fault: captureFault,
      wake,
      epoch: scheduler.epoch,
      failures: scheduler.aggregateFailures,
      pauses: events.filter((event) => event.kind === 'paused').length,
      unhandled: rejectionUnhandled,
    };
  });
  assert.deepEqual(
    observed.fault,
    fault === 'abort'
      ? { success: 1, complete: 0, abort: 1 }
      : { success: 1, complete: 1, abort: 0 },
  );
  assert.equal(
    observed.wake,
    'owner_not_ready',
    'failed pause capture or late authority loss fences queued work',
  );
  assert.equal(observed.epoch, 0);
  assert.equal(observed.failures, 2);
  assert.equal(observed.pauses, 0);
  assert.equal(observed.unhandled, 0);
  assert.deepEqual(await page.evaluate(() => rejectionSnapshot()), before);
  assert.deepEqual(
    harness.requests().map((request) => requestIdentity(request)),
    [expected, expected],
  );
  assert.equal(harness.forwardedMutations(), 1);
  await harness.close(page);
  console.log('outbox rejection: capture-' + fault + ' passed');
}
