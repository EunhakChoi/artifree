import assert from 'node:assert/strict';

/** Settle every cleanup, always close owned listeners, and retain the first failure. */
export async function cleanupOutboxDelivery(
  { contexts, artifacts, users, serverApi, listeners },
  failure,
) {
  const failures = failure === undefined ? [] : [failure];
  async function attempt(action) {
    try {
      await action();
    } catch (error) {
      failures.push(error);
    }
  }
  try {
    for (const context of contexts) await attempt(() => context.close());
    for (const { origin, id, token } of artifacts) {
      const headers = {
        Authorization: 'Bearer ' + token,
        'Content-Type': 'application/json',
      };
      await attempt(async () => {
        const response = await fetch(origin + '/v1/artifacts/' + id, {
          method: 'PATCH',
          headers: { ...headers, 'Idempotency-Key': crypto.randomUUID() },
          body: JSON.stringify({ base_metadata_revision: 1, deleted: true }),
        });
        assert.equal(
          response.status,
          200,
          'Outbox artifact trash cleanup failed',
        );
      });
      await attempt(async () => {
        const response = await fetch(
          origin + '/v1/artifacts/' + id + '/purge',
          {
            method: 'POST',
            headers: { ...headers, 'Idempotency-Key': crypto.randomUUID() },
            body: JSON.stringify({ base_metadata_revision: 2 }),
          },
        );
        assert.equal(
          response.status,
          200,
          'Outbox artifact purge cleanup failed',
        );
      });
    }
    for (const user of users)
      await attempt(async () => {
        const response = await fetch(
          serverApi.origin + `/v1/users/${user}/disable`,
          {
            method: 'POST',
            headers: { Authorization: 'Bearer ' + serverApi.token },
          },
        );
        assert.equal(response.status, 200, 'Outbox actor cleanup failed');
      });
  } finally {
    for (const listener of listeners) {
      await attempt(() => listener.closeAllConnections());
      await attempt(
        () =>
          new Promise((resolve, reject) => {
            listener.close((error) => (error ? reject(error) : resolve()));
          }),
      );
    }
  }
  if (failures.length === 1) throw failures[0];
  if (failures.length > 1)
    throw new AggregateError(failures, 'Outbox delivery smoke failed', {
      cause: failures[0],
    });
}
