import assert from 'node:assert/strict';

/**
 * Static rules for deploy/container/Dockerfile (decision 0026). They run in
 * `npm run check`, so a change that unpins the base, runs as root, fetches
 * from the network while building, or drops the data volume, ports, or
 * health check fails without Docker. The image behavior itself is checked by
 * scripts/verify-container.mjs, which needs Docker.
 */

/** Splits a Dockerfile into `{ keyword, rest }` instructions. */
export function parseDockerfile(text) {
  const joined = text
    .replace(/\r\n/g, '\n')
    .split('\n')
    .filter((line) => !line.trimStart().startsWith('#'))
    .join('\n')
    .replace(/\\\n/g, ' ');
  return joined
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line !== '')
    .map((line) => {
      const [keyword, ...rest] = line.split(/\s+/);
      return { keyword: keyword.toUpperCase(), rest: rest.join(' ') };
    });
}

/** Throws an assertion error naming the first rule the Dockerfile breaks. */
export function checkDockerfile(text) {
  const instructions = parseDockerfile(text);
  const all = (keyword) => instructions.filter((i) => i.keyword === keyword);

  const from = all('FROM');
  assert.equal(from.length, 1, 'Use exactly one FROM (no build stage).');
  assert.match(
    from[0].rest,
    /^[\w./:-]+@sha256:[0-9a-f]{64}$/,
    'The base image must be pinned by digest.',
  );

  const users = all('USER');
  assert.ok(users.length > 0, 'Set a USER.');
  assert.match(
    users[users.length - 1].rest,
    /^[1-9]\d*(:[1-9]\d*)?$/,
    'The final USER must be a numeric, non-root id.',
  );

  assert.ok(
    all('VOLUME').some((i) => /(^|[\s"[,])\/data(["\],\s]|$)/.test(i.rest)),
    'Declare /data as a volume.',
  );
  const exposed = all('EXPOSE').flatMap((i) => i.rest.split(/\s+/));
  for (const port of ['7410', '7411'])
    assert.ok(exposed.includes(port), `EXPOSE ${port}.`);

  const health = all('HEALTHCHECK');
  assert.equal(health.length, 1, 'Define one HEALTHCHECK.');
  assert.ok(!/^NONE\b/i.test(health[0].rest), 'HEALTHCHECK must not be NONE.');

  const entry = all('ENTRYPOINT');
  assert.equal(entry.length, 1, 'Define one ENTRYPOINT.');
  assert.match(
    entry[0].rest,
    /^\["\/app\/bin\/artifree-server", "serve",/,
    'ENTRYPOINT must be the exec form of the packaged server.',
  );
  assert.equal(all('CMD').length, 0, 'Operator arguments append; no CMD.');

  assert.equal(all('ADD').length, 0, 'Use COPY, not ADD.');
  for (const copy of all('COPY'))
    assert.ok(!/--from/.test(copy.rest), 'Copy only from the package context.');
  for (const run of all('RUN'))
    assert.ok(
      !/\b(curl|wget|apt|apt-get|pip|npm|cargo)\b/.test(run.rest),
      `RUN must not fetch or install anything: ${run.rest}`,
    );
}
