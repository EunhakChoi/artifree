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
    ['rejected-proof-conflict', () => checkFreshConflict(harness, false, true)],
    ['rejected-proof-prepared', () => checkFreshConflict(harness, true, true)],
    [
      'rejected-proof-budget',
      () => checkRecreatedPreparation(harness, false, true),
    ],
    ['rejected-no-downgrade-unknown', () => checkUnknownNoDowngrade(harness)],
    [
      'rejected-no-downgrade-unresolved',
      () => checkRecreatedPreparation(harness, true, true),
    ],
    ['rejected-recreated', () => checkRejectedRecreated(harness)],
    ['rejected-ineligible-pauses', () => checkIneligiblePauses(harness)],
    [
      'rejected-definitive-selections',
      () => checkDefinitiveSelections(harness),
    ],
    ['rejected-codes', () => checkRejectedCodes(harness)],
    [
      'rejected-no-downgrade-lost-conflict',
      () => checkLostResponseConflict(harness),
    ],
    ['rejected-reload', () => checkRejectedReload(harness)],
    ['rejected-binding-changed', () => checkBindingChanged(harness)],
    ['rejected-active-lane', () => checkActiveLane(harness)],
    ['rejected-ineligible-failures', () => checkIneligibleFailures(harness)],
    ['rejected-cleanup-paused', () => checkCleanupPaused(harness)],
    ['rejected-renewal-survives', () => checkRenewalSurvives(harness)],
    [
      'unresolved-after-rejection-abandon',
      () => checkUnresolvedAfterRejectionAbandon(harness),
    ],
    [
      'unresolved-recreated-abandon',
      () => checkUnresolvedRecreatedAbandon(harness),
    ],
    [
      'unresolved-network-exhaustion',
      () => checkUnresolvedNetworkExhaustion(harness),
    ],
    [
      'unresolved-committed-purge',
      () => checkUnresolvedCommittedPurge(harness),
    ],
  ];
  if (selected !== undefined && !cases.some(([name]) => name === selected))
    throw new Error('Unknown rejection sequence');
  for (const [name, check] of cases) {
    if (selected === undefined || selected === name) await check();
  }
}

async function setup(harness, withCleanup = false) {
  harness.configure('normal');
  const artifact = await harness.createArtifact();
  const page = await harness.setup({ artifact });
  await installRejectionHarness(page, withCleanup);
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

/** Installs the page-side rejection helpers again after a reload drops them. */
async function installRejectionHarness(page, withCleanup) {
  await page.evaluate((withCleanup) => {
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
    globalThis.rejectionIdentity = {};
    globalThis.rejectionModes = [];
    globalThis.rejectionCleanup = {
      gate: new deliveryApi.LocalOperationGate(),
      async authorize(mode, signal) {
        rejectionModes.push(mode);
        authorize();
        if (!locallyAuthenticated)
          throw new deliveryApi.OutboxError('owner_not_ready');
        if (mode === 'online') {
          const current = await deliveryApi.readBinding(
            location.origin,
            deliveryContext.accessToken,
            signal,
            (status) => {
              if (status === 401) deliveryContext.invalidateAuthentication();
            },
          );
          if (JSON.stringify(current) !== JSON.stringify(auth))
            throw new deliveryApi.OutboxError('owner_not_ready');
        }
        return Object.freeze({
          identity: rejectionIdentity,
          signal: stop.signal,
          check() {
            authorize();
            if (!locallyAuthenticated)
              throw new deliveryApi.OutboxError('owner_not_ready');
          },
        });
      },
    };
    // A fresh gate per scheduler: closing a scheduler closes its host's gate.
    globalThis.freshRejectionCleanup = () => ({
      gate: new deliveryApi.LocalOperationGate(),
      authorize: rejectionCleanup.authorize,
    });
    globalThis.lastRejectedSelection = undefined;
    globalThis.selectionOutcome = async (mode = 'online') => {
      lastRejectedSelection = undefined;
      try {
        lastRejectedSelection = await scheduler.selectRejected(lane, mode);
        return 'accepted';
      } catch (error) {
        return error instanceof deliveryApi.OutboxError
          ? error.code
          : String(error);
      }
    };
    globalThis.rejectionObservation = async () => ({
      storage: await rejectionSnapshot(),
      events: events.length,
      failures: scheduler.aggregateFailures,
      epoch: scheduler.epoch,
    });
    installScheduler(withCleanup ? rejectionCleanup : undefined);
  }, withCleanup);
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

/** One competing direct write, then one dispatch that the server rejects. */
async function dispatchIntoConflict(harness, page, artifact, retryPreparation) {
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
}

/**
 * Runs selectRejected in the page and asserts its outcome. Neither outcome may
 * change rows, checkpoints, usage, counters, events or HTTP requests.
 */
async function checkRejectedSelection(
  harness,
  page,
  expected,
  mode = 'online',
) {
  const before = await page.evaluate(() => rejectionObservation());
  const requests = harness.requests().length;
  const mutations = harness.forwardedMutations();
  assert.equal(
    await page.evaluate((mode) => selectionOutcome(mode), mode),
    expected,
  );
  assert.deepEqual(await page.evaluate(() => rejectionObservation()), before);
  assert.equal(harness.requests().length, requests);
  assert.equal(harness.forwardedMutations(), mutations);
}

async function checkUnknownNoDowngrade(harness) {
  const { page } = await setup(harness, true);
  harness.configure('drop_before_forward');
  await startUnknown(page);
  // Forwarding is restored before the selection, so only the scheduler's own
  // authority read can run there.
  harness.setMode('normal');
  await checkRejectedSelection(harness, page, 'invalid_selection');
  await harness.close(page);
  console.log('outbox rejection: rejected-no-downgrade-unknown passed');
}

async function checkRejectedRecreated(harness) {
  const { page, artifact } = await setup(harness, true);
  await dispatchIntoConflict(harness, page, artifact, false);
  await checkRejectedSelection(harness, page, 'accepted');
  // The proof is memory-only, so a recreated scheduler has no lane to select.
  await page.evaluate(() => {
    scheduler.close();
    installScheduler(freshRejectionCleanup());
  });
  await checkRejectedSelection(harness, page, 'invalid_selection');
  await harness.close(page);
  console.log('outbox rejection: rejected-recreated passed');
}

async function checkIneligiblePauses(harness) {
  const preparation = await setup(harness, true);
  await preparation.page.evaluate(() => {
    faultPut('intents', 'prepared');
    scheduler.wake(lane);
  });
  await preparation.page.waitForFunction(() =>
    events.some(
      (event) =>
        event.kind === 'paused' && event.pause.kind === 'preparation_paused',
    ),
  );
  assert.equal(harness.requests().length, 0, 'preparation abort sends no HTTP');
  await checkRejectedSelection(harness, preparation.page, 'invalid_selection');
  await harness.close(preparation.page);

  const budget = await setup(harness, true);
  await retryAfterPreparationAbort(harness, budget.page, true);
  await budget.page.waitForFunction(() =>
    events.some(
      (event) => event.kind === 'paused' && event.pause.kind === 'retry_paused',
    ),
  );
  assert.equal(harness.requests().length, 0);
  await checkRejectedSelection(harness, budget.page, 'invalid_selection');
  await harness.close(budget.page);
  console.log('outbox rejection: rejected-ineligible-pauses passed');
}

const attemptKeys = [
  'failure',
  'initialState',
  'kind',
  'outcome',
  'priorDelivery',
];

/** The newest delivery_failed attempt, as the page published it. */
async function latestFailure(page) {
  return page.evaluate(
    () =>
      events.findLast(
        (event) =>
          event.kind === 'attempt' && event.result.kind === 'delivery_failed',
      ).result,
  );
}

/** The newest pause, reduced to the fields the contract names. */
async function latestPause(page) {
  return page.evaluate(() => {
    const { pause } = events.findLast((event) => event.kind === 'paused');
    return JSON.parse(
      JSON.stringify({
        kind: pause.kind,
        reason: pause.reason,
        scope: pause.scope,
        failure: pause.failure,
      }),
    );
  });
}

/**
 * The selection brand is module-private, so the observable contract is a frozen
 * object whose only own key is that brand, set to true.
 */
async function checkSelectionShape(page) {
  assert.deepEqual(
    await page.evaluate(() => {
      const keys = Reflect.ownKeys(lastRejectedSelection);
      return {
        frozen: Object.isFrozen(lastRejectedSelection),
        keys: keys.map((key) => String(key)),
        value: lastRejectedSelection[keys[0]],
      };
    }),
    { frozen: true, keys: ['Symbol()'], value: true },
  );
}

async function checkDefinitiveSelections(harness) {
  const { page, artifact, expected } = await setup(harness, true);
  await dispatchIntoConflict(harness, page, artifact, false);
  const failure = {
    reason: 'data_conflict',
    status: 409,
    currentDataVersion: 1,
  };
  const attempt = await latestFailure(page);
  assert.deepEqual(Object.keys(attempt).sort(), attemptKeys);
  assert.deepEqual(attempt, {
    kind: 'delivery_failed',
    failure,
    outcome: 'unknown',
    initialState: 'unprepared',
    priorDelivery: 'none',
  });
  // The published events carry no prepared row and no request body.
  const published = await page.evaluate(() => JSON.stringify(events));
  assert.equal(published.includes('base_data_version'), false);
  assert.equal(published.includes('original'), false);
  assert.deepEqual(await latestPause(page), { kind: 'blocked', failure });
  await checkRejectedSelection(harness, page, 'accepted', 'online');
  await checkSelectionShape(page);
  await checkRejectedSelection(harness, page, 'accepted', 'offline');
  await checkSelectionShape(page);
  assert.deepEqual(await page.evaluate(() => rejectionModes), [
    'online',
    'offline',
  ]);
  assert.deepEqual(
    harness.requests().map((request) => requestIdentity(request)),
    [expected],
  );
  await harness.close(page);
  console.log('outbox rejection: rejected-definitive-selections passed');
}

async function checkRejectedCode(harness, status, code, mode) {
  const { page, expected } = await setup(harness, true);
  harness.configure('status');
  harness.setStatus(status);
  harness.setErrorBody({ error: { code, message: 'fixed rejection' } });
  await page.evaluate(() => scheduler.wake(lane));
  await page.waitForFunction(() =>
    events.some(
      (event) => event.kind === 'paused' && event.pause.kind === 'blocked',
    ),
  );
  const failure = { reason: 'rejected', status, code };
  assert.deepEqual(await latestFailure(page), {
    kind: 'delivery_failed',
    failure,
    outcome: 'unknown',
    initialState: 'unprepared',
    priorDelivery: 'none',
  });
  assert.deepEqual(await latestPause(page), { kind: 'blocked', failure });
  assert.deepEqual(
    harness.requests().map((request) => requestIdentity(request)),
    [expected],
  );
  await checkRejectedSelection(harness, page, 'accepted', mode);
  await checkSelectionShape(page);
  await harness.close(page);
  console.log(`outbox rejection: rejected-code-${code} passed`);
}

async function checkRejectedCodes(harness) {
  await checkRejectedCode(harness, 409, 'data_path_conflict', 'online');
  await checkRejectedCode(harness, 400, 'invalid_request', 'offline');
}

async function checkLostResponseConflict(harness) {
  const { page, artifact, expected } = await setup(harness, true);
  harness.configure('drop_before_forward');
  await startUnknown(page);
  // The lost first response leaves delivery unknown. The competing write then
  // turns the retry into a conflict, which must not prove a rejection.
  await harness.independentWrite(artifact, 0, { canonical: 1 });
  harness.setMode('normal');
  await page.evaluate(() =>
    timers
      .find((timer) => timer.active && timer.milliseconds === 250)
      .callback(),
  );
  await page.waitForFunction(() =>
    events.some(
      (event) => event.kind === 'paused' && event.pause.kind === 'retry_paused',
    ),
  );
  assert.deepEqual(await latestFailure(page), {
    kind: 'delivery_failed',
    failure: { reason: 'data_conflict', status: 409, currentDataVersion: 1 },
    outcome: 'unknown',
    initialState: 'prepared',
    priorDelivery: 'unknown',
  });
  assert.deepEqual(await latestPause(page), {
    kind: 'retry_paused',
    reason: 'unresolved_after_rejection',
    scope: 'lane',
  });
  assert.deepEqual(
    harness.requests().map((request) => requestIdentity(request)),
    [expected, expected],
  );
  assert.equal(harness.responses().length, 1);
  assert.equal(harness.responses()[0].status, 409);
  await checkRejectedSelection(harness, page, 'invalid_selection');
  await harness.close(page);
  console.log('outbox rejection: rejected-no-downgrade-lost-conflict passed');
}

async function checkRejectedReload(harness) {
  const { page, artifact } = await setup(harness, true);
  await dispatchIntoConflict(harness, page, artifact, false);
  await checkRejectedSelection(harness, page, 'accepted');
  // The proof is memory-only: a reload drops it while the durable head stays.
  await harness.reload(page, artifact);
  await installRejectionHarness(page, true);
  await checkRejectedSelection(harness, page, 'invalid_selection');
  await harness.close(page);
  console.log('outbox rejection: rejected-reload passed');
}

async function checkBindingChanged(harness) {
  const { page, expected } = await setup(harness, true);
  harness.configure('binding_changed');
  await page.evaluate(() => scheduler.wake(lane));
  await page.waitForFunction(() =>
    events.some(
      (event) =>
        event.kind === 'attempt' && event.result.kind === 'delivery_failed',
    ),
  );
  assert.deepEqual(await latestFailure(page), {
    kind: 'delivery_failed',
    failure: { reason: 'binding_changed', status: 409 },
    outcome: 'unknown',
    initialState: 'unprepared',
    priorDelivery: 'none',
  });
  assert.deepEqual(
    harness.requests().map((request) => requestIdentity(request)),
    [expected],
  );
  // The binding refusal closes the scheduler, so selection fails its owner check.
  await checkRejectedSelection(harness, page, 'owner_not_ready');
  await harness.close(page);
  console.log('outbox rejection: rejected-binding-changed passed');
}

async function checkActiveLane(harness) {
  const { page } = await setup(harness, true);
  harness.configure('hold_forward');
  const arrival = harness.waiting();
  await page.evaluate(() => scheduler.wake(lane));
  // The held PUT has reached the proxy, so the lane's attempt is in flight.
  await arrival;
  // Selection waits for the in-flight attempt, so it starts before the release.
  // The released write is acknowledged, so the lane is then not blocked.
  await page.evaluate(() => {
    globalThis.activeSelection = selectionOutcome('offline');
  });
  harness.releaseHeld();
  assert.equal(await page.evaluate(() => activeSelection), 'invalid_selection');
  assert.equal(harness.requests().length, 1);
  await harness.close(page);
  console.log('outbox rejection: rejected-active-lane passed');
}

const ineligibleFailures = [
  {
    name: 'access-401',
    mode: 'status',
    status: 401,
    body: { error: { code: 'unauthorized', message: 'fixed refusal' } },
    closes: true,
  },
  {
    name: 'access-403',
    mode: 'status',
    status: 403,
    body: { error: { code: 'forbidden', message: 'fixed refusal' } },
    pause: {
      kind: 'blocked',
      failure: { reason: 'access_unavailable', status: 403 },
    },
  },
  {
    name: 'access-404',
    mode: 'status',
    status: 404,
    body: { error: { code: 'not_found', message: 'fixed refusal' } },
    pause: {
      kind: 'blocked',
      failure: { reason: 'access_unavailable', status: 404 },
    },
  },
  {
    name: 'network-error',
    mode: 'drop_before_forward',
    budget: true,
    pause: {
      kind: 'retry_paused',
      reason: 'network_error',
      scope: 'coordinator',
    },
  },
  {
    name: 'invalid-response',
    mode: 'invalid_json',
    budget: true,
    pause: {
      kind: 'retry_paused',
      reason: 'invalid_response',
      scope: 'coordinator',
    },
  },
  {
    name: 'rate-limited',
    mode: 'status',
    status: 429,
    body: { error: { code: 'rate_limited', message: 'fixed refusal' } },
    budget: true,
    pause: {
      kind: 'retry_paused',
      reason: 'rate_limited',
      scope: 'coordinator',
    },
  },
  {
    name: 'server-error',
    mode: 'status',
    status: 500,
    body: { error: { code: 'internal', message: 'fixed refusal' } },
    budget: true,
    pause: {
      kind: 'retry_paused',
      reason: 'server_error',
      scope: 'coordinator',
    },
  },
  {
    name: 'unclassified',
    mode: 'status',
    status: 418,
    body: { error: { code: 'teapot', message: 'fixed refusal' } },
    pause: {
      kind: 'retry_paused',
      reason: 'unclassified_error_response',
      scope: 'lane',
    },
  },
  {
    name: 'ack-storage',
    mode: 'normal',
    ackFault: true,
    pause: {
      kind: 'retry_paused',
      reason: 'ack_storage_failed',
      scope: 'lane',
    },
  },
];

async function checkIneligibleFailure(harness, entry) {
  const { page, expected } = await setup(harness, true);
  harness.configure(entry.mode);
  if (entry.status !== undefined) harness.setStatus(entry.status);
  if (entry.body !== undefined) harness.setErrorBody(entry.body);
  // One more failure exhausts the epoch budget, so this failure's own reason is
  // the retry pause, with no retry timer.
  if (entry.budget === true) {
    await page.evaluate(() => {
      scheduler.aggregateFailures = 31;
    });
  }
  if (entry.ackFault === true) {
    await page.evaluate(() => faultPut('intents', 'acknowledged'));
  }
  await page.evaluate(() => scheduler.wake(lane));
  if (entry.closes === true) {
    // Invalidated authentication closes the scheduler before any pause publishes.
    await page.waitForFunction(() => stop.signal.aborted);
    await checkRejectedSelection(harness, page, 'owner_not_ready');
  } else {
    await page.waitForFunction(() =>
      events.some((event) => event.kind === 'paused'),
    );
    assert.deepEqual(await latestPause(page), entry.pause);
    await checkRejectedSelection(harness, page, 'invalid_selection');
  }
  assert.deepEqual(
    harness.requests().map((request) => requestIdentity(request)),
    [expected],
  );
  await harness.close(page);
  console.log(`outbox rejection: rejected-ineligible-${entry.name} passed`);
}

async function checkIneligibleFailures(harness) {
  for (const entry of ineligibleFailures)
    await checkIneligibleFailure(harness, entry);
}

async function checkCleanupPaused(harness) {
  const { page, expected } = await setup(harness, true);
  // The durable ACK succeeds, then the post-ACK trim fails: a deliberate
  // cleanup continuation, as the scheduler smoke's durable-ACK case shows.
  await page.evaluate(() => {
    faultPut('usage');
    scheduler.wake(lane);
  });
  await page.waitForFunction(() =>
    events.some(
      (event) =>
        event.kind === 'paused' && event.pause.kind === 'cleanup_paused',
    ),
  );
  await page.evaluate(() => restorePut());
  assert.deepEqual(await latestPause(page), { kind: 'cleanup_paused' });
  assert.deepEqual(
    harness.requests().map((request) => requestIdentity(request)),
    [expected],
  );
  await checkRejectedSelection(harness, page, 'invalid_selection');
  await harness.close(page);
  console.log('outbox rejection: rejected-cleanup-paused passed');
}

async function checkRenewalSurvives(harness) {
  const { page, artifact } = await setup(harness, true);
  await dispatchIntoConflict(harness, page, artifact, false);
  // A second lane for another artifact, budget-paused so its resume renews the epoch.
  const other = await harness.createArtifact();
  await page.evaluate(async (other) => {
    globalThis.laneB = [
      owner,
      location.origin,
      auth.actor_id,
      auth.storage_epoch,
      other,
    ];
    await enqueueLikeHead({
      artifactId: other,
      lane: laneB,
      binding: laneB.slice(0, 4),
      key: 'b'.repeat(32),
    });
    scheduler.aggregateFailures = 32;
    scheduler.wake(laneB);
  }, other);
  await page.waitForFunction(() =>
    events.some(
      (event) => event.kind === 'paused' && event.pause.kind === 'retry_paused',
    ),
  );
  await page.evaluate(() => {
    const choice = events.findLast((event) => event.kind === 'paused').pause
      .selection;
    globalThis.renewal = scheduler.resumeDelivery(choice);
  });
  await page.evaluate(() => renewal);
  await page.evaluate(() => scheduler.activeTask);
  assert.equal(await page.evaluate(() => scheduler.epoch), 1);
  // The other lane's renewal keeps the blocked lane's proof and pause.
  await checkRejectedSelection(harness, page, 'accepted');
  await harness.close(page);
  console.log('outbox rejection: rejected-renewal-survives passed');
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

async function checkRecreatedPreparation(
  harness,
  recreate = true,
  selectRejected = false,
) {
  const { page, artifact, expected } = await setup(harness, selectRejected);
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
    await page.evaluate((selectRejected) => {
      scheduler.close();
      installScheduler(selectRejected ? freshRejectionCleanup() : undefined);
      scheduler.wake(lane);
    }, selectRejected);
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
  if (selectRejected)
    await checkRejectedSelection(
      harness,
      page,
      recreate ? 'invalid_selection' : 'accepted',
    );
  await harness.close(page);
  console.log(
    'outbox rejection: ' +
      (recreate
        ? 'preparation-recreated-rejection'
        : 'preparation-budget-conflict') +
      ' passed',
  );
}

async function checkFreshConflict(
  harness,
  retryPreparation = false,
  selectRejected = false,
) {
  const { page, artifact, expected } = await setup(harness, selectRejected);
  await dispatchIntoConflict(harness, page, artifact, retryPreparation);
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
  if (selectRejected) await checkRejectedSelection(harness, page, 'accepted');
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

/** The one network call an online unresolved selection or abandonment makes. */
const bindingRead = 'GET /v1/data-outbox/binding';
/** Backoff of a lane with random() of 0: 250 ms doubling, capped at 15 s. */
const retryDelays = [250, 500, 1000, 2000, 4000, 8000, 15000];
/** The first attempt, then one per retry delay: the last one exhausts. */
const exhaustionAttempts = retryDelays.length + 1;

/**
 * Installs the unresolved selection and abandonment helpers, and a log of every
 * fetch the page starts, so each window can assert exactly which calls it made.
 */
async function installUnresolvedHarness(page) {
  await page.evaluate(() => {
    globalThis.networkLog = [];
    const nativeFetch = globalThis.fetch;
    globalThis.fetch = (input, init) => {
      const path = new URL(String(input), location.href).pathname;
      networkLog.push(`${init?.method ?? 'GET'} ${path}`);
      return nativeFetch.call(globalThis, input, init);
    };
    globalThis.unresolvedSelection = undefined;
    globalThis.unresolvedOutcome = async (mode) => {
      unresolvedSelection = undefined;
      try {
        unresolvedSelection = await scheduler.selectUnresolved(lane, mode);
        return 'accepted';
      } catch (error) {
        return error instanceof deliveryApi.OutboxError
          ? error.code
          : String(error);
      }
    };
    globalThis.abandonOutcome = async () => {
      try {
        return await scheduler.abandonUnresolved(unresolvedSelection);
      } catch (error) {
        return error instanceof deliveryApi.OutboxError
          ? error.code
          : String(error);
      }
    };
  });
}

/** Returns the page's fetches since the last call, and clears them. */
function takeNetwork(page) {
  return page.evaluate(() => networkLog.splice(0));
}

/** The lost-response write is committed: version 1 with its value. */
async function assertCommittedValue(harness, artifact) {
  const canonical = await harness.canonical(artifact);
  assert.equal(canonical.data_version, 1);
  assert.deepEqual(canonical.value, { marker: { original: 1 } });
}

/** The 409 of a head with unknown first delivery: paused, no proof. */
async function assertUnresolvedConflict(page) {
  assert.deepEqual(await latestFailure(page), {
    kind: 'delivery_failed',
    failure: { reason: 'data_conflict', status: 409, currentDataVersion: 1 },
    outcome: 'unknown',
    initialState: 'prepared',
    priorDelivery: 'unknown',
  });
  assert.deepEqual(await latestPause(page), {
    kind: 'retry_paused',
    reason: 'unresolved_after_rejection',
    scope: 'lane',
  });
}

/**
 * Selects, then abandons, the lane's prepared head. The result is the bare
 * ADR 0049 shape. Only the closure leaves storage, with its quota and the lane
 * checkpoint that goes with its last row. Receipts, requests, forwarded writes
 * and attempts stay as they were. `network` is the exact list of fetches each
 * window may make.
 */
async function abandonUnresolvedHead(harness, page, mode, network) {
  await takeNetwork(page);
  const before = await page.evaluate(() => rejectionSnapshot());
  const requests = harness.requests().length;
  const forwarded = harness.forwardedMutations();
  const attempts = await page.evaluate(
    () => events.filter((event) => event.kind === 'attempt').length,
  );
  assert.equal(
    await page.evaluate((mode) => unresolvedOutcome(mode), mode),
    'accepted',
  );
  assert.deepEqual(await takeNetwork(page), network);
  assert.deepEqual(await page.evaluate(() => abandonOutcome()), {
    kind: 'abandoned_unknown',
    removedDependents: 0,
  });
  assert.deepEqual(await takeNetwork(page), network);
  // The committed selection is spent, and the lane no longer exists.
  assert.equal(
    await page.evaluate(() => abandonOutcome()),
    'invalid_selection',
  );
  assert.equal(
    await page.evaluate(() => unresolvedOutcome('offline')),
    'invalid_selection',
  );
  assert.deepEqual(await takeNetwork(page), []);
  const after = await page.evaluate(() => rejectionSnapshot());
  assert.deepEqual(after.rows, []);
  // Neither result is an ACK: receipts are untouched. The lane checkpoint goes
  // with its last row, since no survivor remains (ADR 0049 rule 7).
  assert.deepEqual(after.receipts, before.receipts);
  assert.deepEqual(
    before.checkpoints.map(({ rows, bytes }) => ({ rows, bytes })),
    [{ rows: 1, bytes: before.usage.bytes }],
  );
  assert.deepEqual(after.checkpoints, []);
  // The quota ledger returns to empty; its sequence counter is not recovered.
  assert.equal(before.usage.rows, 1);
  assert.notEqual(before.usage.bytes, 0);
  assert.deepEqual(after.usage, { ...before.usage, rows: 0, bytes: 0 });
  assert.equal(harness.requests().length, requests);
  assert.equal(harness.forwardedMutations(), forwarded);
  assert.equal(
    await page.evaluate(
      () => events.filter((event) => event.kind === 'attempt').length,
    ),
    attempts,
  );
  assert.equal(await page.evaluate(() => rejectionUnhandled), 0);
}

/** Drops the first response; a competing write turns the retry into a 409. */
async function conflictAfterUnknown(harness, page, artifact) {
  harness.configure('drop_before_forward');
  await startUnknown(page);
  await harness.independentWrite(artifact, 0, { canonical: 1 });
  harness.setMode('normal');
  await page.evaluate(() => fire(250));
  await page.waitForFunction(() =>
    events.some(
      (event) => event.kind === 'paused' && event.pause.kind === 'retry_paused',
    ),
  );
}

async function checkUnresolvedAfterRejectionAbandon(harness) {
  const { page, artifact, expected } = await setup(harness, true);
  await installUnresolvedHarness(page);
  await conflictAfterUnknown(harness, page, artifact);
  await assertUnresolvedConflict(page);
  assert.deepEqual(
    harness.requests().map((request) => requestIdentity(request)),
    [expected, expected],
  );
  // Without a rejection proof, the rejected kind stays refused.
  await checkRejectedSelection(harness, page, 'invalid_selection');
  await abandonUnresolvedHead(harness, page, 'online', [bindingRead]);
  await harness.close(page);
  console.log('outbox rejection: unresolved-after-rejection-abandon passed');
}

async function checkUnresolvedRecreatedAbandon(harness) {
  const { page, artifact, expected } = await setup(harness, true);
  await installUnresolvedHarness(page);
  harness.configure('drop_before_forward');
  await startUnknown(page);
  await harness.independentWrite(artifact, 0, { canonical: 1 });
  // The pause is memory-only. The recreated scheduler re-dispatches the durable
  // prepared head, still of unknown delivery, and the 409 pauses it again.
  await page.evaluate(() => {
    scheduler.close();
    installScheduler(freshRejectionCleanup());
  });
  harness.setMode('normal');
  await page.evaluate(() => scheduler.wake(lane));
  await page.waitForFunction(() =>
    events.some(
      (event) => event.kind === 'paused' && event.pause.kind === 'retry_paused',
    ),
  );
  await assertUnresolvedConflict(page);
  assert.deepEqual(
    harness.requests().map((request) => requestIdentity(request)),
    [expected, expected],
  );
  await checkRejectedSelection(harness, page, 'invalid_selection');
  await abandonUnresolvedHead(harness, page, 'online', [bindingRead]);
  await harness.close(page);
  console.log('outbox rejection: unresolved-recreated-abandon passed');
}

async function checkUnresolvedNetworkExhaustion(harness) {
  const { page, artifact, expected } = await setup(harness, true);
  await installUnresolvedHarness(page);
  // Each forwarded write commits and its response is lost, so every attempt is
  // a network failure, and the eighth one exhausts the lane.
  harness.configure('lost_response');
  await startUnknown(page);
  for (const [index, milliseconds] of retryDelays.entries()) {
    await page.waitForFunction(
      (milliseconds) =>
        timers.some(
          (timer) => timer.active && timer.milliseconds === milliseconds,
        ),
      milliseconds,
    );
    await page.evaluate((milliseconds) => fire(milliseconds), milliseconds);
    await page.waitForFunction(
      (count) =>
        events.filter((event) => event.kind === 'attempt').length >= count,
      index + 2,
    );
  }
  await page.waitForFunction(() =>
    events.some(
      (event) => event.kind === 'paused' && event.pause.kind === 'retry_paused',
    ),
  );
  assert.deepEqual(await latestFailure(page), {
    kind: 'delivery_failed',
    failure: { reason: 'network_error', status: 0 },
    outcome: 'unknown',
    initialState: 'prepared',
    priorDelivery: 'unknown',
  });
  assert.deepEqual(await latestPause(page), {
    kind: 'retry_paused',
    reason: 'network_error',
    scope: 'lane',
  });
  assert.equal(
    await page.evaluate(() => scheduler.aggregateFailures),
    exhaustionAttempts,
  );
  assert.equal(harness.forwardedMutations(), exhaustionAttempts);
  assert.deepEqual(
    harness.requests().map((request) => requestIdentity(request)),
    Array.from({ length: exhaustionAttempts }, () => expected),
  );
  // The first forwarded write committed; the page saw only the lost response.
  assert.equal(harness.responses()[0].status, 200);
  // The lost-response mode also drops the binding reads that online cleanup
  // needs, so forwarding returns to normal before any cleanup authority read.
  harness.setMode('normal');
  await checkRejectedSelection(harness, page, 'invalid_selection');
  await abandonUnresolvedHead(harness, page, 'online', [bindingRead]);
  // Abandonment claims nothing about the server, which holds the commit.
  await assertCommittedValue(harness, artifact);
  await harness.close(page);
  console.log('outbox rejection: unresolved-network-exhaustion passed');
}

/**
 * A lost-response commit, then the artifact is trashed and purged through the
 * API. The replay of the prepared head gets the 404 access pause.
 */
async function committedThenPurged(harness) {
  const { page, artifact, expected } = await setup(harness, true);
  await installUnresolvedHarness(page);
  harness.configure('lost_response');
  await startUnknown(page);
  assert.equal(harness.forwardedMutations(), 1);
  await assertCommittedValue(harness, artifact);
  await harness.purgeArtifact(artifact);
  assert.equal(await harness.artifactStatus(artifact, '/data'), 404);
  assert.equal(await harness.artifactStatus(artifact, '/data/members'), 404);
  harness.setMode('normal');
  await page.evaluate(() => fire(250));
  await page.waitForFunction(() =>
    events.some(
      (event) => event.kind === 'paused' && event.pause.kind === 'blocked',
    ),
  );
  const failure = { reason: 'access_unavailable', status: 404 };
  assert.deepEqual(await latestFailure(page), {
    kind: 'delivery_failed',
    failure,
    outcome: 'unknown',
    initialState: 'prepared',
    priorDelivery: 'unknown',
  });
  assert.deepEqual(await latestPause(page), { kind: 'blocked', failure });
  assert.deepEqual(
    harness.requests().map((request) => requestIdentity(request)),
    [expected, expected],
  );
  assert.deepEqual(
    harness.responses().map((response) => response.status),
    [200, 404],
  );
  return { page, artifact };
}

async function checkUnresolvedCommittedPurge(harness) {
  const online = await committedThenPurged(harness);
  await abandonUnresolvedHead(harness, online.page, 'online', [bindingRead]);
  assert.equal(await harness.artifactStatus(online.artifact, '/data'), 404);
  await harness.close(online.page);
  // A second lane, abandoned offline: the page makes no network call at all.
  const offline = await committedThenPurged(harness);
  await abandonUnresolvedHead(harness, offline.page, 'offline', []);
  assert.equal(await harness.artifactStatus(offline.artifact, '/data'), 404);
  await harness.close(offline.page);
  console.log('outbox rejection: unresolved-committed-purge passed');
}
