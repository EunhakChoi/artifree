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
const searched = {
  ...START.query,
  search: 'report',
  sort: 'created',
};
const artifact = { id: 'a', title: 'Report' };

test('A recovery from the trash opens it with the search the page shows then', () => {
  const trash = apply(
    START,
    { type: 'query', query: { ...START.query, deleted: true } },
    { type: 'more' },
    // The user searched while the recovery was in flight.
    { type: 'query', query: { ...searched, deleted: true } },
  );
  const after = navigate(trash, { type: 'recovered', artifact });
  assert.deepEqual(after.query, searched);
  assert.equal(after.count, START.count);
  assert.equal(after.selected, 'a');
  assert.equal(after.notice?.text, 'Recovered “Report”.');
});

test('A recovery answered after the page left the trash keeps what it shows', () => {
  const elsewhere = apply(
    START,
    { type: 'query', query: { ...START.query, deleted: true } },
    { type: 'query', query: searched },
    // The open artifact may hold a draft.
    { type: 'choose', id: 'b' },
  );
  const after = navigate(elsewhere, { type: 'recovered', artifact });
  assert.deepEqual(
    [after.query, after.selected],
    [elsewhere.query, 'b'],
    'Nothing the user chose since is replaced.',
  );
  assert.equal(after.notice?.text, 'Recovered “Report”.');
});

test('The page says why the open artifact closed, once', () => {
  const own = apply(
    START,
    { type: 'choose', id: 'a' },
    { type: 'trashed', artifact },
  );
  // The page's own move said so already.
  assert.equal(navigate(own, { type: 'left', id: 'a' }), own);
  const elsewhere = navigate(apply(START, { type: 'choose', id: 'a' }), {
    type: 'left',
    id: 'a',
  });
  assert.deepEqual(elsewhere.notice, {
    id: 'a',
    text: 'The open artifact was moved to the trash.',
    toTrash: true,
  });
  // A recovery said before it went to the trash again is out of date.
  const recovered = apply(
    START,
    { type: 'query', query: { ...START.query, deleted: true } },
    { type: 'recovered', artifact },
  );
  assert.equal(recovered.notice.text, 'Recovered “Report”.');
  assert.deepEqual(
    navigate(recovered, { type: 'left', id: 'a' }).notice,
    elsewhere.notice,
  );
  // An artifact opened because it is listed first keeps the notice; one
  // the user chooses clears it.
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
  const trash = navigate(START, {
    type: 'query',
    query: { ...searched, deleted: true },
  });
  const after = navigate(trash, { type: 'choose', id: 'c' });
  assert.deepEqual([after.query, after.selected], [searched, 'c']);
});

const ids = (...names) => names.map((id) => ({ id }));
const page = {
  selected: 'a',
  trashShown: false,
  unsaved: false,
  inTrash: () => false,
  fresh: undefined,
  listed: ids('a', 'b'),
};

test('An artifact the page knows is in the trash closes, unless a draft holds it', () => {
  const trashed = (id) => id === 'a';
  assert.equal(opened({ ...page, inTrash: trashed }), 'b');
  assert.equal(opened({ ...page, inTrash: trashed, unsaved: true }), 'a');
  // A partial or searched list does not close it; only knowing does.
  assert.equal(
    opened({ ...page, fresh: { list: ids('b'), everything: false } }),
    'a',
  );
  assert.equal(
    opened({ ...page, fresh: { list: ids('b'), everything: true } }),
    'b',
  );
});

test('The first listed artifact outside the trash opens, also from a list read before the move', () => {
  // The list was answered for the current request, but read before the
  // page learned that the first artifact went to the trash.
  const fresh = { list: ids('t', 'b'), everything: true };
  assert.equal(
    opened({
      ...page,
      selected: undefined,
      fresh,
      inTrash: (id) => id === 't',
    }),
    'b',
  );
  assert.equal(opened({ ...page, trashShown: true }), undefined);
});

test('An empty list says the workspace is empty only when it read everything', () => {
  const all = START.query;
  const trash = { ...all, deleted: true };
  // Every artifact read so far left the list, and more follow.
  assert.equal(emptiness(all, 0, false), 'left');
  assert.equal(emptiness(trash, 0, false), 'left');
  assert.equal(emptiness(all, 0, true), 'none');
  assert.equal(emptiness(searched, 0, true), 'unmatched');
  assert.equal(emptiness(trash, 0, true), 'unmatched');
  assert.equal(emptiness(all, 1, false), undefined);
  assert.equal(emptiness(all, undefined, true), undefined);
});
