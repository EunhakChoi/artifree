import assert from 'node:assert/strict';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The rules are web code without DOM or React imports; bundle them as they
// ship.
const bundle = await build({
  stdin: {
    contents:
      "export * from './navigation.ts'; export { emptiness } from './list-query.ts';",
    resolveDir: fileURLToPath(
      new URL('../../apps/web/src/artifacts/', import.meta.url),
    ),
  },
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { START, emptiness, navigate, opened } = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);

const apply = (state, ...events) => events.reduce(navigate, state);
const trash = { ...START.query, deleted: true };
const report = { id: 'a', title: 'Report' };

test('A recovery answered in the trash opens it with the search shown then', () => {
  const searching = apply(
    START,
    { type: 'query', query: trash },
    { type: 'more' },
    // The user searched while the recovery was in flight.
    { type: 'query', query: { ...trash, search: 'rep', sort: 'created' } },
  );
  const after = navigate(searching, { type: 'recovered', artifact: report });
  assert.deepEqual(after.query, {
    ...START.query,
    search: 'rep',
    sort: 'created',
  });
  assert.equal(after.count, START.count);
  assert.equal(after.selected, 'a');
  assert.equal(after.notice.text, 'Recovered “Report”.');
});

test('A recovery answered after the page left the trash keeps what it shows', () => {
  const elsewhere = apply(
    START,
    { type: 'query', query: trash },
    { type: 'choose', id: 'b' },
  );
  const after = navigate(elsewhere, { type: 'recovered', artifact: report });
  assert.equal(after.selected, 'b');
  assert.deepEqual(after.query, elsewhere.query);
  assert.equal(after.notice.text, 'Recovered “Report”.');
});

test('The page says once why the open artifact closed', () => {
  const chosen = navigate(START, { type: 'choose', id: 'a' });
  // The page's own move said so already.
  const own = navigate(chosen, { type: 'trashed', artifact: report });
  assert.equal(navigate(own, { type: 'left', id: 'a' }), own);
  const elsewhere = navigate(chosen, { type: 'left', id: 'a' });
  assert.deepEqual(elsewhere.notice, {
    id: 'a',
    text: 'The open artifact was moved to the trash.',
    toTrash: true,
  });
  // A recovery said before it went to the trash again is out of date.
  const recovered = apply(
    START,
    { type: 'query', query: trash },
    { type: 'recovered', artifact: report },
  );
  assert.deepEqual(
    navigate(recovered, { type: 'left', id: 'a' }).notice,
    elsewhere.notice,
  );
  // One opened because it is listed first keeps the notice; one the user
  // chooses clears it.
  assert.equal(
    navigate(elsewhere, { type: 'open', id: 'b' }).notice,
    elsewhere.notice,
  );
  assert.equal(
    navigate(elsewhere, { type: 'choose', id: 'b' }).notice,
    undefined,
  );
});

test('An artifact chosen or saved while the trash is shown opens outside it', () => {
  const after = apply(
    START,
    { type: 'query', query: { ...trash, search: 'rep' } },
    { type: 'choose', id: 'c' },
  );
  assert.deepEqual(after.query, { ...START.query, search: 'rep' });
  assert.equal(after.selected, 'c');
});

const page = {
  selected: 'a',
  trashShown: false,
  unsaved: false,
  inTrash: () => false,
  fresh: undefined,
  listed: [{ id: 'a' }, { id: 'b' }],
};

test('An artifact the page knows is in the trash closes, unless a draft holds it', () => {
  const trashed = (id) => id === 'a';
  assert.equal(opened(page), 'a');
  assert.equal(opened({ ...page, inTrash: trashed }), 'b');
  assert.equal(opened({ ...page, inTrash: trashed, unsaved: true }), 'a');
  // Nothing opens while the trash is shown.
  assert.equal(opened({ ...page, trashShown: true }), undefined);
  // A fresh list of every artifact closes one it does not name; a partial
  // or filtered list does not.
  const fresh = { list: [{ id: 'b' }], everything: true };
  assert.equal(opened({ ...page, fresh }), 'b');
  assert.equal(
    opened({ ...page, fresh: { ...fresh, everything: false } }),
    'a',
  );
});

test('The first listed artifact outside the trash opens, also from a list read before the move', () => {
  const trashed = (id) => id === 'a';
  const fresh = { list: [{ id: 'a' }, { id: 'b' }], everything: true };
  assert.equal(
    opened({ ...page, selected: undefined, inTrash: trashed, fresh }),
    'b',
  );
  assert.equal(
    opened({ ...page, selected: undefined, inTrash: () => true }),
    undefined,
  );
});

test('An empty list says the workspace is empty only when it read everything', () => {
  const all = START.query;
  // Every artifact read so far left the view, and more follow.
  assert.equal(emptiness(all, 0, false), 'left');
  assert.equal(emptiness(trash, 0, false), 'left');
  assert.equal(emptiness(all, 0, true), 'none');
  assert.equal(emptiness({ ...all, search: 'rep' }, 0, true), 'unmatched');
  assert.equal(emptiness(trash, 0, true), 'unmatched');
  assert.equal(emptiness(all, 1, false), undefined);
  assert.equal(emptiness(all, undefined, true), undefined);
});
