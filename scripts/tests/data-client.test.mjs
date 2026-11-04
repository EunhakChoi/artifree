import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import {
  ArtifreeClient,
  DATA_LIMITS,
  DataTooLargeError,
  followData,
  listDataMembers,
} from '../../packages/client/dist/index.js';
import {
  DataEventChecker,
  backoffMs,
  isRetryable,
} from '../../packages/client/dist/data-follow.js';
import {
  EventStreamParser,
  MAX_EVENT_BYTES,
  MAX_LINE_BYTES,
} from '../../packages/client/dist/event-stream.js';

// Decision 0040, client side: the byte-level event parser, the checks of every
// event, the validation of a members query and of its answer, and the retry
// and backoff of following, with an injected clock and sleep and a scripted
// `fetch`. The server itself is proved in the Rust tests.
const ART = `art_${'a'.repeat(32)}`;
const encoder = new TextEncoder();
const bytes = (text) => encoder.encode(text);

// ---------------------------------------------------------------- the parser

/** Every event the parser yields for `chunks`, then finishes the stream. */
function parse(chunks) {
  const parser = new EventStreamParser();
  const events = [];
  for (const chunk of chunks) events.push(...parser.push(chunk));
  parser.finish();
  return events;
}

const splitEvery = (data, size) => {
  const chunks = [];
  for (let at = 0; at < data.length; at += size)
    chunks.push(data.slice(at, at + size));
  return chunks;
};

const invalidResponse = (error) => {
  assert.equal(error?.code, 'invalid_response', String(error));
  return true;
};

const SPEC_CASES = [
  ['LF lines', 'event: change\ndata: x\n\n', [['change', 'x']]],
  ['CRLF lines', 'event: change\r\ndata: x\r\n\r\n', [['change', 'x']]],
  ['CR lines', 'event: change\rdata: x\r\r', [['change', 'x']]],
  ['mixed line ends', 'event: a\r\ndata: x\rdata: y\n\r\n', [['a', 'x\ny']]],
  ['no event name is message', 'data: x\n\n', [['message', 'x']]],
  ['data without a space', 'data:x\n\n', [['message', 'x']]],
  ['data with two spaces', 'data:  x\n\n', [['message', ' x']]],
  ['data without a colon is empty', 'data\n\n', [['message', '']]],
  ['two data lines', 'data: a\ndata: b\n\n', [['message', 'a\nb']]],
  [
    'an empty data line kept',
    'data: a\ndata:\ndata: b\n\n',
    [['message', 'a\n\nb']],
  ],
  ['comments ignored', ': keep-alive\ndata: x\n: more\n\n', [['message', 'x']]],
  [
    'unknown fields ignored',
    'foo: bar\nid: 7\nretry: 5\ndata: x\n\n',
    [['message', 'x']],
  ],
  ['a blank line alone', '\n\n\n', []],
  ['an event name alone is not an event', 'event: change\n\n', []],
  [
    'the name resets after an event',
    'event: a\ndata: 1\n\ndata: 2\n\n',
    [
      ['a', '1'],
      ['message', '2'],
    ],
  ],
  ['an event without its blank line', 'data: x\n', []],
  [
    'two events',
    'data: 1\n\ndata: 2\n\n',
    [
      ['message', '1'],
      ['message', '2'],
    ],
  ],
  ['a value with a colon', 'data: a:b\n\n', [['message', 'a:b']]],
  ['a multi-byte value', 'data: é€𝄞\n\n', [['message', 'é€𝄞']]],
];

for (const [name, input, expected] of SPEC_CASES) {
  const want = expected.map(([type, data]) => ({ type, data }));
  const data = bytes(input);
  test(`the parser reads ${name}, whole and split at every byte`, () => {
    assert.deepEqual(parse([data]), want);
    assert.deepEqual(parse(splitEvery(data, 1)), want);
    for (let at = 1; at < data.length; at += 1)
      assert.deepEqual(
        parse([data.slice(0, at), data.slice(at)]),
        want,
        `split at ${at}`,
      );
  });
}

test('the parser removes one byte order mark, however the chunks split it', () => {
  const mark = [0xef, 0xbb, 0xbf];
  const rest = bytes('data: x\n\n');
  const want = [{ type: 'message', data: 'x' }];
  const whole = new Uint8Array([...mark, ...rest]);
  assert.deepEqual(parse([whole]), want);
  assert.deepEqual(parse(splitEvery(whole, 1)), want);
  assert.deepEqual(parse([whole.slice(0, 1), whole.slice(1)]), want);
  assert.deepEqual(parse([whole.slice(0, 2), whole.slice(2)]), want);
  assert.deepEqual(
    parse([whole.slice(0, 1), whole.slice(1, 2), whole.slice(2)]),
    want,
  );
});

test('the parser keeps a second byte order mark, which breaks the line it starts', () => {
  const twice = bytes('﻿﻿data: x\n\ndata: y\n\n');
  assert.deepEqual(parse([twice]), [{ type: 'message', data: 'y' }]);
});

test('the parser keeps a byte order mark inside the stream', () => {
  assert.deepEqual(parse([bytes('data: ﻿x\n\n')]), [
    { type: 'message', data: '﻿x' },
  ]);
});

test('a stream that begins like a byte order mark but is not one is invalid UTF-8', () => {
  assert.throws(
    () => parse([new Uint8Array([0xef, 0xbb, 0x61, 0x0a])]),
    invalidResponse,
  );
  // Ending inside the mark is a broken last line too.
  assert.throws(() => parse([new Uint8Array([0xef, 0xbb])]), invalidResponse);
});

test('a line counts its bytes, not its characters, against the limit', () => {
  // ':' + 'a' + 2-byte letters: exactly MAX_LINE_BYTES bytes, 73,761 characters.
  const letters = (MAX_LINE_BYTES - 2) / 2;
  const exact = bytes(`:a${'é'.repeat(letters)}\n`);
  assert.equal(exact.length - 1, MAX_LINE_BYTES);
  assert.deepEqual(parse([exact]), []);
  assert.deepEqual(parse(splitEvery(exact, 7)), []);
  assert.throws(
    () => parse([bytes(`:ab${'é'.repeat(letters)}\n`)]),
    invalidResponse,
  );
  // Over the limit in bytes, under it in characters.
  assert.throws(
    () => parse([bytes(`:${'é'.repeat(MAX_LINE_BYTES * 0.6)}\n`)]),
    invalidResponse,
  );
});

test('a line over the limit is refused before its end arrives, in one-byte chunks', () => {
  const parser = new EventStreamParser();
  const line = bytes(`:${'x'.repeat(MAX_LINE_BYTES)}`);
  let pushed = 0;
  assert.throws(() => {
    for (const byte of line) {
      pushed += 1;
      parser.push(new Uint8Array([byte]));
    }
  }, invalidResponse);
  assert.equal(pushed, MAX_LINE_BYTES + 1, 'refused at the first byte over');
});

test('the data of an event counts bytes with its line ends against the limit', () => {
  const exact = bytes(`data: ${'é'.repeat(MAX_EVENT_BYTES / 2)}\n\n`);
  assert.equal(parse([exact])[0].data.length, MAX_EVENT_BYTES / 2);
  assert.throws(
    () => parse([bytes(`data: ${'é'.repeat(MAX_EVENT_BYTES / 2)}a\n\n`)]),
    invalidResponse,
  );
  // 100,000 characters are 200,000 bytes.
  assert.throws(
    () => parse([bytes(`data: ${'é'.repeat(100_000)}\n\n`)]),
    invalidResponse,
  );
  assert.throws(
    () => parse(splitEvery(bytes(`data: ${'é'.repeat(200_000)}\n\n`), 3)),
    invalidResponse,
  );
});

test('the data of an event is limited as a whole, over many lines', () => {
  const half = 'x'.repeat(MAX_EVENT_BYTES / 2);
  // Two lines of half the limit and the line end between them are over by one.
  assert.throws(
    () => parse([bytes(`data: ${half}\ndata: ${half}\n\n`)]),
    invalidResponse,
  );
  const under = 'x'.repeat(MAX_EVENT_BYTES / 2 - 1);
  assert.equal(parse([bytes(`data: ${half}\ndata: ${under}\n\n`)]).length, 1);
});

test('the bytes of an event are counted again for the next one', () => {
  const big = `data: ${'x'.repeat(MAX_EVENT_BYTES)}\n\n`;
  assert.equal(parse([bytes(big + big)]).length, 2);
});

const BAD_UTF8 = [
  [
    'a byte that never occurs',
    [0x64, 0x61, 0x74, 0x61, 0x3a, 0x20, 0xff, 0x0a, 0x0a],
  ],
  [
    'an overlong form',
    [0x64, 0x61, 0x74, 0x61, 0x3a, 0x20, 0xc0, 0x80, 0x0a, 0x0a],
  ],
  ['a lone continuation byte in a comment', [0x3a, 0x80, 0x0a]],
  ['a surrogate encoded in UTF-8', [0x3a, 0xed, 0xa0, 0x80, 0x0a]],
  ['bytes in an unknown field', [0x78, 0x3a, 0xff, 0x0a]],
  ['bytes in a field name', [0xff, 0x3a, 0x78, 0x0a]],
  ['a sequence cut by the line end', [0x3a, 0xe2, 0x82, 0x0a]],
  ['bytes in an event name', [0x65, 0x76, 0x65, 0x6e, 0x74, 0x3a, 0xff, 0x0a]],
];

for (const [name, data] of BAD_UTF8) {
  test(`invalid UTF-8 is refused: ${name}`, () => {
    const input = new Uint8Array(data);
    assert.throws(() => parse([input]), invalidResponse);
    assert.throws(() => parse(splitEvery(input, 1)), invalidResponse);
  });
}

test('a multi-byte character split across chunks is read whole', () => {
  const data = bytes('data: é€𝄞\n\n');
  assert.deepEqual(parse(splitEvery(data, 1)), [
    { type: 'message', data: 'é€𝄞' },
  ]);
});

test('an unfinished last line is never dispatched but is still checked', () => {
  // Valid, and without its end: not an event, and no error.
  assert.deepEqual(parse([bytes('data: a\n\ndata: b')]), [
    { type: 'message', data: 'a' },
  ]);
  assert.deepEqual(parse([bytes('data: b\n:')]), []);
  // The same bytes with one broken sequence at the end: invalid.
  assert.throws(
    () => parse([new Uint8Array([...bytes('data: a\n\ndata: '), 0xc3])]),
    invalidResponse,
  );
  assert.throws(
    () => parse([new Uint8Array([0x3a, 0xe2, 0x82])]),
    invalidResponse,
  );
  assert.throws(() => parse([new Uint8Array([0xff])]), invalidResponse);
});

test('a CR at the very end of the stream still ends its line', () => {
  assert.deepEqual(parse([bytes('data: a\r\r')]), [
    { type: 'message', data: 'a' },
  ]);
});

// --------------------------------------------------------------- the checker

const appData = (over = {}) => ({
  artifact_id: ART,
  path: 'a/b',
  data_version: 1,
  exists: true,
  value: { k: 1 },
  ...over,
});
const tooLarge = (over = {}) => ({
  artifact_id: ART,
  path: 'a/b',
  data_version: 1,
  ...over,
});
const event = (type, data) => ({
  type,
  data: typeof data === 'string' ? data : JSON.stringify(data),
});
const checker = (floor) => new DataEventChecker(ART, 'a/b', floor);
const refused = (run) => assert.throws(run, invalidResponse);

test('the checker accepts a snapshot, changes, and a final too_large', () => {
  const c = checker(undefined);
  assert.deepEqual(c.check(event('snapshot', appData({ data_version: 3 }))), {
    type: 'snapshot',
    data: appData({ data_version: 3 }),
  });
  assert.equal(
    c.check(event('change', appData({ data_version: 4 }))).type,
    'change',
  );
  assert.deepEqual(c.check(event('too_large', tooLarge({ data_version: 9 }))), {
    type: 'too_large',
    data: tooLarge({ data_version: 9 }),
  });
});

test('too_large may be the first event, and null and absent values are told apart', () => {
  assert.equal(
    checker(undefined).check(event('too_large', tooLarge())).type,
    'too_large',
  );
  const c = checker(undefined);
  assert.equal(
    c.check(event('snapshot', appData({ value: null }))).data.value,
    null,
  );
  const absent = {
    artifact_id: ART,
    path: 'a/b',
    data_version: 2,
    exists: false,
  };
  assert.deepEqual(c.check(event('change', absent)).data, absent);
});

const ORDER_FAULTS = [
  [
    'a second snapshot',
    [
      ['snapshot', 1],
      ['snapshot', 2],
    ],
  ],
  [
    'a snapshot after a change',
    [
      ['snapshot', 1],
      ['change', 2],
      ['snapshot', 3],
    ],
  ],
  ['a change first', [['change', 1]]],
  [
    'a change with the same version',
    [
      ['snapshot', 2],
      ['change', 2],
    ],
  ],
  [
    'a change with an older version',
    [
      ['snapshot', 2],
      ['change', 1],
    ],
  ],
  [
    'a change after a change with an older version',
    [
      ['snapshot', 1],
      ['change', 5],
      ['change', 4],
    ],
  ],
  [
    'a too_large with the same version',
    [
      ['snapshot', 2],
      ['too_large', 2],
    ],
  ],
  [
    'a too_large with an older version',
    [
      ['snapshot', 2],
      ['too_large', 1],
    ],
  ],
  [
    'a change after too_large',
    [
      ['snapshot', 1],
      ['too_large', 2],
      ['change', 3],
    ],
  ],
  [
    'a snapshot after too_large',
    [
      ['too_large', 1],
      ['snapshot', 2],
    ],
  ],
  [
    'a second too_large',
    [
      ['too_large', 1],
      ['too_large', 2],
    ],
  ],
];

for (const [name, sequence] of ORDER_FAULTS) {
  test(`the checker refuses ${name}`, () => {
    const c = checker(undefined);
    const run = () => {
      for (const [type, version] of sequence)
        c.check(
          event(
            type,
            type === 'too_large'
              ? tooLarge({ data_version: version })
              : appData({ data_version: version }),
          ),
        );
    };
    refused(run);
  });
}

test('the first event of a connection is not older than anything delivered before', () => {
  refused(() =>
    checker(5).check(event('snapshot', appData({ data_version: 4 }))),
  );
  refused(() =>
    checker(5).check(event('too_large', tooLarge({ data_version: 4 }))),
  );
  // Equal is the same state again after a reconnect.
  assert.equal(
    checker(5).check(event('snapshot', appData({ data_version: 5 }))).type,
    'snapshot',
  );
  assert.equal(
    checker(5).check(event('too_large', tooLarge({ data_version: 5 }))).type,
    'too_large',
  );
  assert.equal(
    checker(5).check(event('snapshot', appData({ data_version: 6 }))).type,
    'snapshot',
  );
});

const FIELD_FAULTS = [
  [
    'the wrong artifact',
    appData({ artifact_id: `art_${'b'.repeat(32)}` }),
    tooLarge({ artifact_id: `art_${'b'.repeat(32)}` }),
  ],
  ['the wrong path', appData({ path: 'a' }), tooLarge({ path: 'a' })],
  ['the root for another path', appData({ path: '' }), tooLarge({ path: '' })],
  [
    'a version above 2^53 - 1',
    appData({ data_version: 2 ** 53 }),
    tooLarge({ data_version: 2 ** 53 }),
  ],
  [
    'a negative version',
    appData({ data_version: -1 }),
    tooLarge({ data_version: -1 }),
  ],
  [
    'a fractional version',
    appData({ data_version: 1.5 }),
    tooLarge({ data_version: 1.5 }),
  ],
  [
    'a version as text',
    appData({ data_version: '1' }),
    tooLarge({ data_version: '1' }),
  ],
  [
    'a missing version',
    appData({ data_version: undefined }),
    tooLarge({ data_version: undefined }),
  ],
  ['a source revision', appData({ revision: 3 }), tooLarge({ revision: 3 })],
  [
    'a metadata revision',
    appData({ metadata_revision: 1 }),
    tooLarge({ metadata_revision: 1 }),
  ],
  ['an unknown field', appData({ extra: true }), tooLarge({ extra: true })],
];

for (const [name, data, large] of FIELD_FAULTS) {
  test(`the checker refuses ${name}, in a snapshot, a change, and too_large`, () => {
    refused(() => checker(undefined).check(event('snapshot', data)));
    refused(() => {
      const c = checker(undefined);
      c.check(event('snapshot', appData({ data_version: 0 })));
      c.check(event('change', data));
    });
    refused(() => checker(undefined).check(event('too_large', large)));
  });
}

const SHAPE_FAULTS = [
  ['a value where nothing exists', appData({ exists: false, value: 1 })],
  ['nothing where a value exists', appData({ value: undefined })],
  ['a root that is not an object', appData({ path: '', value: [] })],
  [
    'a root that does not exist',
    appData({ path: '', exists: false, value: undefined }),
  ],
  ['exists as text', appData({ exists: 'yes' })],
  ['data that is not an object', []],
  ['data that is null', null],
];

for (const [name, data] of SHAPE_FAULTS) {
  test(`the checker refuses ${name}`, () => {
    const path = data?.path === '' ? '' : 'a/b';
    refused(() =>
      new DataEventChecker(ART, path, undefined).check(event('snapshot', data)),
    );
    refused(() => {
      const c = new DataEventChecker(ART, path, undefined);
      c.check(
        event(
          'snapshot',
          appData({ path, data_version: 0, value: path === '' ? {} : 1 }),
        ),
      );
      c.check(event('change', data));
    });
  });
}

test('the checker refuses an unknown event name, text that is not JSON, and an empty event', () => {
  refused(() => checker(undefined).check(event('message', appData())));
  refused(() => checker(undefined).check(event('ready', appData())));
  refused(() => checker(undefined).check(event('snapshot', '{"artifact_id"')));
  refused(() => checker(undefined).check(event('snapshot', '')));
  refused(() => checker(undefined).check(event('too_large', 'null')));
});

// ------------------------------------------------------------ list members

/** A client whose fetch answers from `answer` and records every request. */
function listing(answer) {
  const requests = [];
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:1',
    accessToken: 'token-for-tests',
    fetch: async (url, init) => {
      requests.push({ url: String(url), init });
      const body = typeof answer === 'function' ? answer(String(url)) : answer;
      return new Response(JSON.stringify(body), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      });
    },
  });
  return { client, requests };
}

const page = (over = {}) => ({
  artifact_id: ART,
  path: '',
  select: 'names',
  limit: 50,
  data_version: 3,
  exists: true,
  collection: true,
  members: [{ name: 'a' }, { name: 'b' }],
  ...over,
});

test('a members query sends every parameter explicitly and returns the page', async () => {
  const { client, requests } = listing(page());
  const result = await listDataMembers(client, ART);
  assert.equal(requests.length, 1);
  assert.equal(
    requests[0].url,
    `http://127.0.0.1:1/v1/artifacts/${ART}/data/members?select=names&limit=50`,
  );
  assert.equal(requests[0].init.method, 'GET');
  assert.equal(
    requests[0].init.headers.authorization,
    'Bearer token-for-tests',
  );
  assert.deepEqual(result, page());
});

test('a members query with every part encodes it and checks the cursor', async () => {
  const body = page({
    path: 'a/b-c_d',
    select: 'values',
    limit: 2,
    after: 'a b/é',
    members: [
      { name: 'x', value: null },
      { name: 'y', too_large: true },
    ],
    next_after: 'y',
  });
  const { client, requests } = listing(body);
  const result = await listDataMembers(client, ART, {
    path: 'a/b-c_d',
    select: 'values',
    limit: 2,
    after: 'a b/é',
  });
  assert.equal(
    requests[0].url,
    `http://127.0.0.1:1/v1/artifacts/${ART}/data/members?path=a%2Fb-c_d&select=values&limit=2&after=a+b%2F%C3%A9`,
  );
  assert.deepEqual(result, body);
});

const BAD_QUERIES = [
  ['an artifact identifier of the wrong form', 'art_xyz', {}],
  ['an upper-case artifact identifier', `art_${'A'.repeat(32)}`, {}],
  ['a leading slash', ART, { path: '/a' }],
  ['a trailing slash', ART, { path: 'a/' }],
  ['an empty segment', ART, { path: 'a//b' }],
  ['a dot segment', ART, { path: 'a/../b' }],
  ['a space', ART, { path: 'a b' }],
  ['a non-ASCII letter', ART, { path: 'é' }],
  ['a segment of 65 characters', ART, { path: 'a'.repeat(65) }],
  ['17 segments', ART, { path: Array(17).fill('a').join('/') }],
  ['a select of another kind', ART, { select: 'keys' }],
  ['a limit of 0', ART, { limit: 0 }],
  ['a limit of 101', ART, { limit: 101 }],
  ['a fractional limit', ART, { limit: 1.5 }],
  ['a limit of NaN', ART, { limit: Number.NaN }],
  ['a limit as text', ART, { limit: '5' }],
  ['an empty cursor', ART, { after: '' }],
  ['a cursor of 257 bytes', ART, { after: 'a'.repeat(257) }],
  ['a cursor of 129 two-byte letters', ART, { after: 'é'.repeat(129) }],
  ['a cursor with a lone surrogate', ART, { after: 'a\ud800' }],
];

for (const [name, id, query] of BAD_QUERIES) {
  test(`a members query with ${name} is refused before anything is sent`, async () => {
    const { client, requests } = listing(page());
    await assert.rejects(listDataMembers(client, id, query), (error) => {
      assert.equal(error.code, 'invalid_request');
      return true;
    });
    assert.deepEqual(requests, []);
  });
}

test('a members query takes the largest values the grammar allows', async () => {
  const path = Array(16).fill('a'.repeat(64)).join('/');
  const after = 'é'.repeat(128);
  const { client } = listing(page({ path, limit: 100, after, members: [] }));
  const result = await listDataMembers(client, ART, {
    path,
    limit: 100,
    after,
  });
  assert.equal(result.path, path);
});

const ANSWER_FAULTS = [
  ['another artifact', page({ artifact_id: `art_${'b'.repeat(32)}` })],
  ['another path', page({ path: 'x' })],
  ['another select', page({ select: 'values', members: [] })],
  ['another limit', page({ limit: 51 })],
  [
    'an echoed cursor that was not sent',
    page({ after: 'a', members: [{ name: 'b' }] }),
  ],
  ['a missing version', page({ data_version: undefined })],
  ['a version above 2^53 - 1', page({ data_version: 2 ** 53 })],
  ['a source revision', page({ revision: 1 })],
  ['a metadata revision', page({ metadata_revision: 1 })],
  [
    'members in descending order',
    page({ members: [{ name: 'b' }, { name: 'a' }] }),
  ],
  ['a repeated member', page({ members: [{ name: 'a' }, { name: 'a' }] })],
  ['members that are not an array', page({ members: {} })],
  ['more members than the limit', page({ limit: 1 }), { limit: 1 }],
  ['a member with an empty name', page({ members: [{ name: '' }] })],
  [
    'a member name of 257 bytes',
    page({ members: [{ name: 'a'.repeat(257) }] }),
  ],
  [
    'a member with a value under names',
    page({ members: [{ name: 'a', value: 1 }] }),
  ],
  [
    'a member with too_large under names',
    page({ members: [{ name: 'a', too_large: true }] }),
  ],
  ['a member with an extra field', page({ members: [{ name: 'a', size: 1 }] })],
  ['a member that is not an object', page({ members: ['a'] })],
  [
    'the root as not existing',
    page({ exists: false, collection: false, members: [] }),
  ],
  ['exists as text', page({ exists: 'yes' })],
  ['collection as text', page({ collection: 'yes' })],
  [
    'a collection that does not exist',
    page({ path: 'a', exists: false, members: [] }),
    { path: 'a' },
  ],
  [
    'members of something that is not a collection',
    page({ path: 'a', collection: false }),
    { path: 'a' },
  ],
  ['a next_after with no members', page({ members: [], next_after: 'a' })],
  ['a next_after that is not the last member', page({ next_after: 'a' })],
  ['a next_after that is a number', page({ next_after: 2 })],
];

for (const [name, answer, query = {}] of ANSWER_FAULTS) {
  test(`a members answer with ${name} is invalid_response`, async () => {
    const { client } = listing(answer);
    await assert.rejects(listDataMembers(client, ART, query), (error) => {
      assert.equal(error.code, 'invalid_response');
      return true;
    });
  });
}

const VALUE_FAULTS = [
  ['neither a value nor too_large', { name: 'a' }],
  ['both a value and too_large', { name: 'a', value: 1, too_large: true }],
  ['too_large as false', { name: 'a', too_large: false }],
  [
    'a value over twice 32 KiB',
    { name: 'a', value: 'x'.repeat(2 * DATA_LIMITS.memberValueBytes - 1) },
  ],
];

for (const [name, member] of VALUE_FAULTS) {
  test(`a values answer with a member with ${name} is invalid_response`, async () => {
    const { client } = listing(page({ select: 'values', members: [member] }));
    await assert.rejects(
      listDataMembers(client, ART, { select: 'values' }),
      (error) => {
        assert.equal(error.code, 'invalid_response');
        return true;
      },
    );
  });
}

test('a values answer accepts a value of twice 32 KiB and null, and no more', async () => {
  // The canonical text of a string is its length plus the two quotes; the
  // client allows twice the server's bound for the spelling of numbers.
  const exact = 'x'.repeat(2 * DATA_LIMITS.memberValueBytes - 2);
  const members = [
    { name: 'a', value: exact },
    { name: 'b', value: null },
  ];
  const { client } = listing(page({ select: 'values', members }));
  const result = await listDataMembers(client, ART, { select: 'values' });
  assert.deepEqual(result.members, members);
});

test('an answer is ordered by UTF-8 bytes, which differs from the order of the text', async () => {
  // U+FF5E is EF BD 9E and U+10000 is F0 90 80 80: the first sorts first in
  // bytes, the second first in UTF-16 code units.
  const low = '～';
  const high = '\u{10000}';
  assert.ok(high < low, 'the premise: UTF-16 order is the other one');
  const ok = listing(page({ members: [{ name: low }, { name: high }] }));
  assert.equal((await listDataMembers(ok.client, ART)).members.length, 2);
  const wrong = listing(page({ members: [{ name: high }, { name: low }] }));
  await assert.rejects(
    listDataMembers(wrong.client, ART),
    (error) => error.code === 'invalid_response',
  );
});

test('an answer must start after the cursor, in bytes', async () => {
  const sent = { after: 'm' };
  const before = listing(page({ after: 'm', members: [{ name: 'a' }] }));
  await assert.rejects(
    listDataMembers(before.client, ART, sent),
    (error) => error.code === 'invalid_response',
  );
  const same = listing(page({ after: 'm', members: [{ name: 'm' }] }));
  await assert.rejects(
    listDataMembers(same.client, ART, sent),
    (error) => error.code === 'invalid_response',
  );
  const later = listing(page({ after: 'm', members: [{ name: 'n' }] }));
  assert.equal((await listDataMembers(later.client, ART, sent)).after, 'm');
});

test('a page is limited to twice 256 KiB of names and values, to the byte', async () => {
  // A member of name m00 and a string of 32,763 characters is 3 + 32,765 =
  // 32,768 bytes: sixteen of them are exactly 524,288.
  const member = (i, extra = 0) => ({
    name: `m${String(i).padStart(2, '0')}`,
    value: 'x'.repeat(32_763 + extra),
  });
  const sixteen = Array.from({ length: 16 }, (_, i) => member(i));
  const exact = listing(
    page({ select: 'values', members: sixteen, next_after: 'm15' }),
  );
  const result = await listDataMembers(exact.client, ART, { select: 'values' });
  assert.equal(result.members.length, 16);
  const over = [...sixteen.slice(0, 15), member(15, 1)];
  const larger = listing(
    page({ select: 'values', members: over, next_after: 'm15' }),
  );
  await assert.rejects(
    listDataMembers(larger.client, ART, { select: 'values' }),
    (error) => error.code === 'invalid_response',
  );
  const eighteen = Array.from({ length: 18 }, (_, i) => member(i));
  const many = listing(
    page({ select: 'values', members: eighteen, next_after: 'm17' }),
  );
  await assert.rejects(
    listDataMembers(many.client, ART, { select: 'values' }),
    (error) => error.code === 'invalid_response',
  );
});

test('a next_after is returned when it names the last member', async () => {
  const { client } = listing(page({ next_after: 'b' }));
  assert.equal((await listDataMembers(client, ART)).next_after, 'b');
});

test('a path with nothing at it or a scalar is a page without members', async () => {
  const none = listing(
    page({ path: 'a', exists: false, collection: false, members: [] }),
  );
  const result = await listDataMembers(none.client, ART, { path: 'a' });
  assert.equal(result.exists, false);
  assert.deepEqual(result.members, []);
  const scalar = listing(page({ path: 'a', collection: false, members: [] }));
  assert.equal(
    (await listDataMembers(scalar.client, ART, { path: 'a' })).collection,
    false,
  );
});

test('a members query is cancelled by its signal', async () => {
  const controller = new AbortController();
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:1',
    accessToken: 't',
    fetch: (_url, init) =>
      new Promise((_resolve, reject) =>
        init.signal.addEventListener('abort', () =>
          reject(new Error('aborted')),
        ),
      ),
  });
  const pending = listDataMembers(
    client,
    ART,
    {},
    { signal: controller.signal },
  );
  controller.abort();
  await assert.rejects(pending, (error) => error.code === 'network_error');
});

// ---------------------------------------------------------------- following

const errorBody = (code) =>
  JSON.stringify({ error: { code, message: 'for the test' } });

/**
 * Runs `followData` against a script of connections on a virtual clock. A
 * step is `{ status, retryAfter }` for a refused request, `{ throws }` for a
 * connection that could not be made, or `{ chunks, openMs }`, a list of byte chunks,
 * `{ advance: ms }` (the clock moves), `{ error }` (the connection breaks),
 * and finally `{ end }` or `{ hold }` (the stream stays open). When the
 * script is used up, the follower is aborted.
 */
async function follow(script, options = {}) {
  const controller = options.controller ?? new AbortController();
  let time = 10_000;
  const sleeps = [];
  const events = [];
  const requests = [];
  const cancelled = [];
  const connects = [];
  const disconnects = [];
  let next = 0;
  const unhandled = [];
  const onUnhandled = (reason) => unhandled.push(reason);
  process.on('unhandledRejection', onUnhandled);
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:1',
    accessToken: 'token-for-tests',
    fetch: async (url, init) => {
      requests.push({ url: String(url), init });
      if (next >= script.length) {
        controller.abort();
        throw new DOMException('aborted', 'AbortError');
      }
      const step = script[next];
      next += 1;
      if (step.throws !== undefined) throw step.throws;
      if (step.status !== undefined)
        return new Response(
          step.body ?? errorBody(step.code ?? 'unavailable'),
          {
            status: step.status,
            headers: {
              'content-type': 'application/json',
              ...(step.retryAfter === undefined
                ? {}
                : { 'retry-after': step.retryAfter }),
            },
          },
        );
      const parts = [...step.chunks];
      time += step.openMs ?? 0;
      let gone = false;
      let controllerOfBody;
      // Like a real fetch, the body fails when the request is aborted.
      if (step.errorOnAbort === true)
        init.signal?.addEventListener('abort', () => {
          try {
            controllerOfBody.error(new DOMException('aborted', 'AbortError'));
          } catch {
            // The follower had cancelled the body first.
          }
        });
      // A high water mark of 0: nothing is read until the follower asks, so
      // the virtual clock moves at the moment of the read.
      const body = new ReadableStream(
        {
          start(stream) {
            controllerOfBody = stream;
          },
          async pull(stream) {
            for (;;) {
              if (gone) return;
              const part = parts.shift();
              if (part === undefined || part.end) {
                stream.close();
                return;
              }
              if (part.advance !== undefined) {
                time += part.advance;
                continue;
              }
              if (part.error !== undefined) {
                stream.error(part.error);
                return;
              }
              if (part.hold) {
                // Silent until the follower aborts or cancels the body.
                await new Promise((resolve) => {
                  init.signal?.addEventListener('abort', resolve);
                });
                return;
              }
              stream.enqueue(part);
              return;
            }
          },
          cancel() {
            gone = true;
            cancelled.push(requests.length);
          },
        },
        { highWaterMark: 0 },
      );
      return new Response(body, {
        status: 200,
        headers: { 'content-type': step.type ?? 'text/event-stream' },
      });
    },
  });
  const settings = {
    artifactId: ART,
    path: options.path ?? 'a/b',
    signal: controller.signal,
    onEvent: (event) => {
      events.push(event);
      options.onEvent?.(event, controller);
    },
    onConnect: () => connects.push(requests.length),
    onDisconnect: (error, ms) => disconnects.push({ error, ms }),
    now: () => time,
    sleep: async (ms, signal) => {
      sleeps.push(ms);
      time += ms;
      options.onSleep?.(sleeps.length, controller);
      if (signal.aborted) return;
      await Promise.resolve();
    },
    ...(options.retryDelayMs === undefined
      ? {}
      : { retryDelayMs: options.retryDelayMs }),
    ...(options.stableMs === undefined ? {} : { stableMs: options.stableMs }),
  };
  let outcome;
  try {
    await followData(client, settings);
    outcome = { resolved: true };
  } catch (error) {
    outcome = { error };
  }
  // Let anything still pending reject before looking for unhandled rejections.
  await new Promise((resolve) => setImmediate(resolve));
  process.off('unhandledRejection', onUnhandled);
  assert.deepEqual(unhandled, [], 'no unhandled rejection');
  return {
    outcome,
    sleeps,
    events,
    requests,
    cancelled,
    connects,
    disconnects,
    controller,
  };
}

const sse = (type, data) =>
  bytes(`event: ${type}\ndata: ${JSON.stringify(data)}\n\n`);
const snapshot = (version, over = {}) =>
  sse('snapshot', appData({ data_version: version, ...over }));
const change = (version, over = {}) =>
  sse('change', appData({ data_version: version, ...over }));
const refusal = (status, retryAfter) => ({ status, retryAfter });
const stream = (...chunks) => ({ chunks: [...chunks, { end: true }] });

test('following asks for the event stream of the path with the token and nothing else', async () => {
  const run = await follow([stream(snapshot(1))]);
  assert.equal(run.outcome.resolved, true);
  assert.equal(
    run.requests[0].url,
    `http://127.0.0.1:1/v1/artifacts/${ART}/data/events?path=a%2Fb`,
  );
  const headers = run.requests[0].init.headers;
  assert.equal(headers.accept, 'text/event-stream');
  assert.equal(headers.authorization, 'Bearer token-for-tests');
  assert.equal(headers['last-event-id'], undefined);
  assert.equal(run.requests[0].init.method, 'GET');
});

test('following the root sends an empty path', async () => {
  const run = await follow([stream(snapshot(1, { path: '', value: {} }))], {
    path: '',
  });
  assert.equal(run.requests[0].url.endsWith('/data/events?path='), true);
  assert.equal(run.events.length, 1);
});

test('following delivers a snapshot and then each change in order', async () => {
  const run = await follow(
    [{ chunks: [snapshot(1), change(2), change(5), { hold: true }] }],
    {
      onEvent: (event, controller) => {
        if (event.data.data_version === 5) controller.abort();
      },
    },
  );
  assert.equal(run.outcome.resolved, true);
  assert.deepEqual(
    run.events.map((e) => [e.type, e.data.data_version]),
    [
      ['snapshot', 1],
      ['change', 2],
      ['change', 5],
    ],
  );
  assert.deepEqual(run.events[1].data.value, { k: 1 });
  assert.equal(run.connects.length, 1);
});

test('following reads events split at any byte', async () => {
  const whole = new Uint8Array([...snapshot(1), ...change(2)]);
  const run = await follow(
    [{ chunks: [...splitEvery(whole, 1), { hold: true }] }],
    {
      onEvent: (event, controller) => {
        if (event.type === 'change') controller.abort();
      },
    },
  );
  assert.deepEqual(
    run.events.map((e) => e.type),
    ['snapshot', 'change'],
  );
});

const RETRIED = [
  ['429', refusal(429), 'rate_limited'],
  ['502', refusal(502), 'unavailable'],
  ['503', refusal(503), 'unavailable'],
  ['504', refusal(504), 'unavailable'],
  [
    'a connection that could not be made',
    { throws: new TypeError('fetch failed') },
    'network_error',
  ],
];

for (const [name, first, code] of RETRIED) {
  test(`following retries after ${name} and gets a fresh snapshot`, async () => {
    const run = await follow([first, stream(snapshot(1))]);
    assert.equal(run.outcome.resolved, true);
    assert.equal(run.requests.length, 3, 'the script ran out on the third');
    assert.equal(run.disconnects[0].error.code, code);
    assert.equal(run.disconnects[0].ms, 500);
    assert.equal(run.events.length, 1);
    assert.equal(run.events[0].type, 'snapshot');
  });
}

test('following retries after a clean end with a snapshot, and refuses an older one', async () => {
  const run = await follow([
    stream(snapshot(1), change(4)),
    stream(snapshot(4)),
    stream(snapshot(6)),
  ]);
  assert.deepEqual(
    run.events.map((e) => [e.type, e.data.data_version]),
    [
      ['snapshot', 1],
      ['change', 4],
      ['snapshot', 4],
      ['snapshot', 6],
    ],
  );
  const stale = await follow([
    stream(snapshot(1), change(4)),
    stream(snapshot(3)),
  ]);
  assert.equal(stale.outcome.error.code, 'invalid_response');
  assert.equal(stale.requests.length, 2);
  assert.equal(stale.sleeps.length, 1, 'no wait after the fault');
});

test('following retries after a lost connection and keeps the newest version', async () => {
  const run = await follow([
    { chunks: [snapshot(1), change(2), { error: new TypeError('reset') }] },
    stream(snapshot(2)),
  ]);
  assert.equal(run.outcome.resolved, true);
  assert.equal(run.disconnects[0].error.code, 'network_error');
  assert.deepEqual(
    run.events.map((e) => e.data.data_version),
    [1, 2, 2],
  );
});

const PERMANENT = [
  ['400', { status: 400, code: 'invalid_request' }, 'invalid_request'],
  ['401', { status: 401, code: 'unauthenticated' }, 'unauthenticated'],
  ['403', { status: 403, code: 'forbidden' }, 'forbidden'],
  ['404', { status: 404, code: 'not_found' }, 'not_found'],
  ['422', { status: 422, code: 'invalid_request' }, 'invalid_request'],
  ['500', { status: 500, code: 'internal' }, 'internal'],
  ['501', { status: 501, code: 'internal' }, 'internal'],
  [
    'a 500 whose body is not an API error',
    { status: 500, body: 'oops' },
    'invalid_response',
  ],
  [
    'a success that is not an event stream',
    { chunks: [bytes('{}')], type: 'application/json' },
    'invalid_response',
  ],
];

for (const [name, first, code] of PERMANENT) {
  test(`following rejects without retrying: ${name}`, async () => {
    const run = await follow([first, stream(snapshot(1))]);
    assert.equal(run.outcome.error?.code, code);
    assert.equal(run.requests.length, 1);
    assert.deepEqual(run.sleeps, []);
    assert.deepEqual(run.events, []);
  });
}

const PERMANENT_STREAMS = [
  ['an event of an unknown name', [bytes('event: ready\ndata: {}\n\n')]],
  ['a change first', [change(1)]],
  ['bytes that are not UTF-8', [new Uint8Array([0x3a, 0xff, 0x0a])]],
  ['a line over the limit', [bytes(`:${'x'.repeat(MAX_LINE_BYTES + 1)}\n`)]],
  ['an event for another path', [snapshot(1, { path: 'x' })]],
  ['a version above 2^53 - 1', [snapshot(2 ** 53)]],
  ['a source revision in the data', [snapshot(1, { revision: 1 })]],
];

for (const [name, chunks] of PERMANENT_STREAMS) {
  test(`following rejects with invalid_response and does not reconnect: ${name}`, async () => {
    const run = await follow([
      { chunks: [...chunks, { hold: true }] },
      stream(snapshot(1)),
    ]);
    assert.equal(run.outcome.error?.code, 'invalid_response');
    assert.equal(run.requests.length, 1);
    assert.deepEqual(run.sleeps, []);
    assert.equal(run.cancelled.length, 1, 'the body was cancelled');
  });
}

test('following rejects with invalid_response for a broken last line at the end of the stream', async () => {
  const run = await follow([
    stream(snapshot(1), new Uint8Array([0x3a, 0xc3])),
    stream(snapshot(2)),
  ]);
  assert.equal(run.outcome.error?.code, 'invalid_response');
  assert.equal(run.events.length, 1);
  assert.equal(run.requests.length, 1);
  assert.deepEqual(run.sleeps, []);
});

test('following does not deliver an event whose last line was cut off', async () => {
  const cut = bytes('event: change\ndata: {"artifact_id"');
  const run = await follow([stream(snapshot(1), cut), stream(snapshot(1))]);
  assert.equal(run.outcome.resolved, true);
  assert.deepEqual(
    run.events.map((e) => e.type),
    ['snapshot', 'snapshot'],
  );
});

test('following delivers too_large and then rejects without reconnecting', async () => {
  const run = await follow([
    stream(snapshot(1), sse('too_large', tooLarge({ data_version: 2 }))),
    stream(snapshot(3)),
  ]);
  assert.ok(run.outcome.error instanceof DataTooLargeError);
  assert.equal(run.outcome.error.dataVersion, 2);
  assert.equal(run.outcome.error.path, 'a/b');
  assert.deepEqual(
    run.events.map((e) => e.type),
    ['snapshot', 'too_large'],
  );
  assert.equal(run.requests.length, 1);
  assert.deepEqual(run.sleeps, []);
});

test('following rejects when anything follows too_large', async () => {
  const run = await follow([stream(sse('too_large', tooLarge()), change(2))]);
  assert.equal(run.outcome.error?.code, 'invalid_response');
  assert.deepEqual(
    run.events.map((e) => e.type),
    ['too_large'],
  );
});

test('following rejects with what the handler threw and does not reconnect', async () => {
  const boom = new Error('handler failed');
  const run = await follow(
    [{ chunks: [snapshot(1), change(2), { hold: true }] }, stream(snapshot(3))],
    {
      onEvent: () => {
        throw boom;
      },
    },
  );
  assert.equal(run.outcome.error, boom);
  assert.equal(run.requests.length, 1);
  assert.equal(run.events.length, 1, 'nothing is delivered after the throw');
  assert.equal(run.cancelled.length, 1);
});

// The delays: a table over what the connection did.
const sleepsOf = async (script, options) =>
  (await follow(script, options)).sleeps;
const LONG = 60_000;

test('slow 503s double the delay up to the maximum', async () => {
  const script = Array.from({ length: 7 }, () => refusal(503));
  assert.deepEqual(
    (await sleepsOf(script, { retryDelayMs: { min: 500, max: 4000 } })).slice(
      0,
      7,
    ),
    [500, 1000, 2000, 4000, 4000, 4000, 4000],
  );
});

test('the default delays start at 500 ms and stop doubling at 30 s', async () => {
  const script = Array.from({ length: 9 }, () => refusal(503));
  assert.deepEqual(
    (await sleepsOf(script)).slice(0, 9),
    [500, 1000, 2000, 4000, 8000, 16_000, 30_000, 30_000, 30_000],
  );
});

test('a Retry-After replaces the delay and is kept between 500 ms and one hour', async () => {
  const sleeps = await sleepsOf([
    refusal(429, '7'),
    refusal(503, '0'),
    refusal(429, '100000'),
    refusal(503, '2'),
    refusal(503),
  ]);
  assert.deepEqual(sleeps.slice(0, 5), [7000, 500, 3_600_000, 2000, 8000]);
});

test('a slow open then a short stream does not restart the delays', async () => {
  // 503 (500), accepted and short with one event (1000), accepted and short (2000).
  const sleeps = await sleepsOf([
    refusal(503),
    stream(snapshot(1), { advance: 5 }),
    stream(snapshot(1), { advance: 9_999 }),
    stream(snapshot(1)),
  ]);
  assert.deepEqual(sleeps.slice(0, 3), [500, 1000, 2000]);
});

test('opening a stream does not restart the delays', async () => {
  // Two accepted streams without events, however long they were open.
  const sleeps = await sleepsOf([
    refusal(503),
    refusal(503),
    stream({ advance: LONG }),
    stream({ advance: LONG }),
  ]);
  assert.deepEqual(sleeps.slice(0, 4), [500, 1000, 2000, 4000]);
});

test('a silent long stream does not restart the delays', async () => {
  const sleeps = await sleepsOf([
    refusal(503),
    stream({ advance: LONG }),
    refusal(503),
  ]);
  assert.deepEqual(sleeps.slice(0, 3), [500, 1000, 2000]);
});

test('a stable clean end restarts the delays', async () => {
  const sleeps = await sleepsOf([
    refusal(503),
    refusal(503),
    stream(snapshot(1), { advance: 10_000 }),
    refusal(503),
  ]);
  assert.deepEqual(sleeps.slice(0, 4), [500, 1000, 500, 1000]);
});

test('a stable lost connection restarts the delays', async () => {
  const sleeps = await sleepsOf([
    refusal(503),
    refusal(503),
    {
      chunks: [
        snapshot(1),
        { advance: 12_000 },
        { error: new TypeError('reset') },
      ],
    },
    refusal(503),
  ]);
  assert.deepEqual(sleeps.slice(0, 4), [500, 1000, 500, 1000]);
});

test('a stream that lasted one millisecond less than stableMs does not restart the delays', async () => {
  const sleeps = await sleepsOf([
    refusal(503),
    stream(snapshot(1), { advance: 9_999 }),
    refusal(503),
  ]);
  assert.deepEqual(sleeps.slice(0, 3), [500, 1000, 2000]);
});

test('stableMs is taken from the option and counted from the accepted stream', async () => {
  const slow = await sleepsOf(
    [
      refusal(503),
      refusal(503),
      stream(snapshot(1), { advance: 99 }),
      refusal(503),
    ],
    { stableMs: 100 },
  );
  assert.deepEqual(slow.slice(0, 4), [500, 1000, 2000, 4000]);
  const exact = await sleepsOf(
    [
      refusal(503),
      refusal(503),
      stream(snapshot(1), { advance: 100 }),
      refusal(503),
    ],
    { stableMs: 100 },
  );
  assert.deepEqual(exact.slice(0, 4), [500, 1000, 500, 1000]);
});

test('the time spent waiting before the stream was accepted does not count as stable', async () => {
  // The open itself takes a minute; the stream is accepted, delivers one
  // event, and ends at once.
  const sleeps = await sleepsOf([
    refusal(503),
    { openMs: LONG, chunks: [snapshot(1), { end: true }] },
    refusal(503),
  ]);
  assert.deepEqual(sleeps.slice(0, 3), [500, 1000, 2000]);
});

test('the delay helpers follow the retry table', () => {
  assert.equal(backoffMs(0, 500, 30_000, undefined), 500);
  assert.equal(backoffMs(6, 500, 30_000, undefined), 30_000);
  assert.equal(backoffMs(2000, 500, 30_000, undefined), 30_000);
  assert.equal(backoffMs(3, 500, 30_000, 1), 1000);
  const error = (status, code) => ({ status, code });
  for (const [status, code, retried] of [
    [0, 'network_error', true],
    [429, 'rate_limited', true],
    [502, 'unavailable', true],
    [503, 'unavailable', true],
    [504, 'unavailable', true],
    [500, 'internal', false],
    [501, 'internal', false],
    [400, 'invalid_request', false],
    [401, 'unauthenticated', false],
    [404, 'not_found', false],
    [200, 'invalid_response', false],
  ])
    assert.equal(
      isRetryable(error(status, code)),
      retried,
      `${status} ${code}`,
    );
});

// Abort: at any point the follower resolves and nothing is delivered after it.
test('an already aborted signal resolves without a request', async () => {
  const controller = new AbortController();
  controller.abort();
  const run = await follow([stream(snapshot(1))], { controller });
  assert.equal(run.outcome.resolved, true);
  assert.deepEqual(run.requests, []);
  assert.deepEqual(run.events, []);
});

test('aborting while waiting to reconnect resolves and does not reconnect', async () => {
  const run = await follow([refusal(503), stream(snapshot(1))], {
    onSleep: (_count, controller) => controller.abort(),
  });
  assert.equal(run.outcome.resolved, true);
  assert.equal(run.requests.length, 1);
  assert.deepEqual(run.events, []);
});

test('aborting while a stream is open resolves and cancels the body', async () => {
  const run = await follow([{ chunks: [snapshot(1), { hold: true }] }], {
    onEvent: (_event, controller) => controller.abort(),
  });
  assert.equal(run.outcome.resolved, true);
  assert.equal(run.events.length, 1);
  assert.equal(run.cancelled.length, 1);
  assert.equal(run.requests.length, 1);
  assert.deepEqual(run.sleeps, [], 'no wait after an abort');
  assert.deepEqual(
    run.disconnects,
    [],
    'no disconnect is reported after an abort',
  );
});

test('aborting while a stream is silent resolves', async () => {
  const controller = new AbortController();
  const done = follow([{ chunks: [{ hold: true }] }], { controller });
  setImmediate(() => controller.abort());
  const run = await done;
  assert.equal(run.outcome.resolved, true);
  assert.equal(run.cancelled.length, 1);
  assert.deepEqual(run.sleeps, []);
  assert.deepEqual(run.disconnects, []);
});

test('aborting while a stream is silent resolves also when the body then fails, as a real fetch does', async () => {
  const controller = new AbortController();
  const done = follow([{ chunks: [{ hold: true }], errorOnAbort: true }], {
    controller,
  });
  setImmediate(() => controller.abort());
  const run = await done;
  assert.equal(run.outcome.resolved, true);
  assert.deepEqual(run.sleeps, []);
  assert.deepEqual(run.disconnects, []);
});

test('an abort is not reported as a broken last line of the stream', async () => {
  // One chunk holds an event and the first byte of a character that never
  // ends: the abort comes with the event, and the rest of the body is dropped.
  const chunk = new Uint8Array([...snapshot(1), 0x3a, 0xc3]);
  const run = await follow([{ chunks: [chunk, { hold: true }] }], {
    onEvent: (_event, controller) => controller.abort(),
  });
  assert.equal(run.outcome.resolved, true);
  assert.deepEqual(
    run.events.map((e) => e.type),
    ['snapshot'],
  );
  assert.deepEqual(run.sleeps, []);
  assert.deepEqual(run.disconnects, []);
});

test('events after an abort in the same chunk are not delivered', async () => {
  const both = new Uint8Array([...snapshot(1), ...change(2), ...change(3)]);
  const run = await follow([stream(both)], {
    onEvent: (_event, controller) => controller.abort(),
  });
  assert.equal(run.outcome.resolved, true);
  assert.deepEqual(
    run.events.map((e) => e.data.data_version),
    [1],
  );
  assert.deepEqual(run.sleeps, []);
  assert.deepEqual(run.disconnects, []);
});

test('aborting while the request is in flight resolves', async () => {
  const controller = new AbortController();
  const client = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:1',
    accessToken: 't',
    fetch: (_url, init) =>
      new Promise((_resolve, reject) =>
        init.signal.addEventListener('abort', () =>
          reject(new DOMException('aborted', 'AbortError')),
        ),
      ),
  });
  const done = followData(client, {
    artifactId: ART,
    signal: controller.signal,
    onEvent: () => assert.fail('no event'),
    onDisconnect: () => assert.fail('an abort is not a disconnect'),
    sleep: () => assert.fail('no wait after an abort'),
  });
  controller.abort();
  await done;
});

test('following validates its arguments before sending anything', async () => {
  const controller = new AbortController();
  let sent = 0;
  const none = {
    signal: controller.signal,
    onEvent: () => {},
    // A request that was sent would fail and be retried; stop that here.
    sleep: async () => controller.abort(),
  };
  const never = new ArtifreeClient({
    baseUrl: 'http://127.0.0.1:1',
    accessToken: 't',
    fetch: async () => {
      sent += 1;
      throw new TypeError('nothing may be sent');
    },
  });
  const cases = [
    { artifactId: 'art_x' },
    { artifactId: ART, path: '/a' },
    { artifactId: ART, path: 'a//b' },
    { artifactId: ART, retryDelayMs: { min: 0, max: 10 } },
    { artifactId: ART, retryDelayMs: { min: 10, max: 5 } },
    {
      artifactId: ART,
      retryDelayMs: { min: 1, max: Number.POSITIVE_INFINITY },
    },
    { artifactId: ART, stableMs: -1 },
  ];
  for (const options of cases) {
    await assert.rejects(
      followData(never, { ...none, ...options }),
      (error) => error.code === 'invalid_request',
      JSON.stringify(options),
    );
    assert.equal(sent, 0, JSON.stringify(options));
  }
});

// ------------------------------------------------- shared with the server

/** `pub const NAME: type = a * b;` of a Rust file, evaluated. */
async function rustConstants(file) {
  const source = await readFile(
    new URL(`../../${file}`, import.meta.url),
    'utf8',
  );
  const found = new Map();
  for (const match of source.matchAll(
    /pub const ([A-Z_]+): \w+ = ([0-9_ *]+);/g,
  ))
    found.set(
      match[1],
      match[2]
        .split('*')
        .reduce(
          (product, factor) =>
            product * Number(factor.replaceAll('_', '').trim()),
          1,
        ),
    );
  return found;
}

test('the grammar and bounds of the client are those of the server', async () => {
  const rust = await rustConstants('crates/artifree-domain/src/data.rs');
  const pairs = {
    MAX_PATH_DEPTH: 'maxPathSegments',
    MAX_SEGMENT_BYTES: 'maxSegmentBytes',
    MAX_MEMBER_NAME_BYTES: 'maxMemberNameBytes',
    MAX_MEMBER_LIMIT: 'maxMemberLimit',
    DEFAULT_MEMBER_LIMIT: 'defaultMemberLimit',
    MEMBER_VALUE_BYTES: 'memberValueBytes',
    MEMBER_PAGE_BYTES: 'memberPageBytes',
    WATCHED_VALUE_BYTES: 'watchedValueBytes',
  };
  for (const [name, key] of Object.entries(pairs)) {
    assert.ok(rust.has(name), `${name} is in the Rust source`);
    assert.equal(DATA_LIMITS[key], rust.get(name), `${key} is ${name}`);
  }
  assert.equal(Object.keys(DATA_LIMITS).length, Object.keys(pairs).length);
  assert.ok(MAX_EVENT_BYTES > rust.get('WATCHED_VALUE_BYTES') + 1024);
});

test('the query the client sends has the parameters of the generated contract', async () => {
  const schema = JSON.parse(
    await readFile(
      new URL('../../contracts/generated/api-v1.schema.json', import.meta.url),
      'utf8',
    ),
  );
  const query = schema.$defs.ListDataMembersQuery;
  assert.deepEqual(Object.keys(query.properties).sort(), [
    'after',
    'limit',
    'path',
    'select',
  ]);
  assert.equal(query.additionalProperties, false);
  assert.deepEqual(Object.keys(schema.$defs.DataMembers.properties).sort(), [
    'after',
    'artifact_id',
    'collection',
    'data_version',
    'exists',
    'limit',
    'members',
    'next_after',
    'path',
    'select',
  ]);
  assert.deepEqual(Object.keys(schema.$defs.DataTooLarge.properties).sort(), [
    'artifact_id',
    'data_version',
    'path',
  ]);
  assert.deepEqual(
    schema.$defs.DataMemberSelect.enum ??
      schema.$defs.DataMemberSelect.oneOf.map((v) => v.const),
    ['names', 'values'],
  );
});
