import assert from 'node:assert/strict';
import vm from 'node:vm';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// Decision 0053, rule 5: the client and the SDK refuse the same values and
// write the same JSON text for every other value. The client's refusals are
// the list in decision 0031. Both are bundled from source as they ship, and
// one table of values runs through each. A row states the answer both must
// give: the exact JSON text, or REFUSED. The client refuses by throwing; the
// SDK answers undefined.
const bundle = await build({
  stdin: {
    contents: [
      "export { dataValueText } from './client.ts';",
      "export { dataText } from '../../runtime-sdk/src/data-text.ts';",
    ].join('\n'),
    resolveDir: fileURLToPath(
      new URL('../../packages/client/src/', import.meta.url),
    ),
  },
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { dataText, dataValueText } = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
);

/** The answer that both must give for a value they refuse. */
const REFUSED = Symbol('refused');

/** A class instance: its prototype is a class, not a root prototype. */
class Thing {
  constructor() {
    this.a = 1;
  }
}

/** The JSON text of an array nested `levels` deep around `null`. */
function nested(levels) {
  return `${'['.repeat(levels)}null${']'.repeat(levels)}`;
}

/** The client's answer: its JSON text, or REFUSED when it throws. */
function clientAnswer(value) {
  try {
    return dataValueText(value, 'value');
  } catch (error) {
    // The client's refusals are Errors. A throw that is not an Error is not one
    // of them, so it fails the row instead of counting as a refusal.
    assert.ok(error instanceof Error, 'the client refused with an Error');
    return REFUSED;
  }
}

/** The SDK's answer: its JSON text, or REFUSED when it is undefined. */
function sdkAnswer(value) {
  return dataText(value) ?? REFUSED;
}

// Each row builds its value inside its own test, so no value is shared between
// tests. `expected` is written out here; neither function computes it.
const ROWS = [
  // Primitives.
  { name: 'null is written as null', make: () => null, expected: 'null' },
  {
    name: 'a string is written as a JSON string',
    make: () => 'text',
    expected: '"text"',
  },
  {
    name: 'a string with quotes and a backslash is escaped',
    make: () => 'a "quoted" \\ word',
    expected: '"a \\"quoted\\" \\\\ word"',
  },
  { name: 'true is written as true', make: () => true, expected: 'true' },
  { name: 'false is written as false', make: () => false, expected: 'false' },
  {
    name: 'a finite number is written as its JSON number',
    make: () => 1.5,
    expected: '1.5',
  },
  { name: 'negative zero is written as 0', make: () => -0, expected: '0' },
  { name: 'NaN is refused', make: () => Number.NaN, expected: REFUSED },
  {
    name: 'Infinity is refused',
    make: () => Number.POSITIVE_INFINITY,
    expected: REFUSED,
  },
  {
    name: '-Infinity is refused',
    make: () => Number.NEGATIVE_INFINITY,
    expected: REFUSED,
  },
  { name: 'undefined is refused', make: () => undefined, expected: REFUSED },
  { name: 'a function is refused', make: () => () => 1, expected: REFUSED },
  {
    name: 'a symbol is refused',
    make: () => Symbol('value'),
    expected: REFUSED,
  },
  { name: 'a bigint is refused', make: () => 1n, expected: REFUSED },

  // Plain objects and arrays.
  {
    name: 'nested plain objects and arrays are written as JSON',
    make: () => ({ a: [1, { b: null }], c: 'd' }),
    expected: '{"a":[1,{"b":null}],"c":"d"}',
  },
  {
    name: 'an object with no prototype is written as JSON',
    make: () => Object.assign(Object.create(null), { a: 1 }),
    expected: '{"a":1}',
  },
  {
    name: 'an array with a hole is refused',
    make: () => {
      const array = [1, 2, 3];
      delete array[1];
      return array;
    },
    expected: REFUSED,
  },
  {
    name: 'an array with an extra property is refused',
    make: () => Object.assign([1, 2], { extra: 3 }),
    expected: REFUSED,
  },

  // Members.
  {
    name: 'a getter member is refused',
    make: () => ({
      get a() {
        return 1;
      },
    }),
    expected: REFUSED,
  },
  {
    name: 'a non-enumerable member is refused',
    make: () =>
      Object.defineProperty({}, 'hidden', { value: 1, enumerable: false }),
    expected: REFUSED,
  },
  {
    name: 'a symbol-keyed member is refused',
    make: () => ({ [Symbol('key')]: 1 }),
    expected: REFUSED,
  },
  {
    name: 'an object with a toJSON function is refused',
    make: () => ({
      toJSON() {
        return 'replaced';
      },
    }),
    expected: REFUSED,
  },
  {
    name: 'an object with a toJSON member that is not a function is written as JSON',
    make: () => ({ toJSON: 'data' }),
    expected: '{"toJSON":"data"}',
  },
  {
    name: 'a cycle is refused',
    make: () => {
      const cycle = { name: 'loop' };
      cycle.self = cycle;
      return cycle;
    },
    expected: REFUSED,
  },

  // Other kinds of object.
  { name: 'a Date is refused', make: () => new Date(0), expected: REFUSED },
  {
    name: 'a Map is refused',
    make: () => new Map([['a', 1]]),
    expected: REFUSED,
  },
  {
    name: 'a class instance is refused',
    make: () => new Thing(),
    expected: REFUSED,
  },

  // Depth: a value may nest 64 levels deep, and no deeper.
  {
    name: 'nesting 63 levels is written',
    make: () => JSON.parse(nested(63)),
    expected: nested(63),
  },
  {
    name: 'nesting 64 levels is written',
    make: () => JSON.parse(nested(64)),
    expected: nested(64),
  },
  {
    name: 'nesting 65 levels is refused',
    make: () => JSON.parse(nested(65)),
    expected: REFUSED,
  },

  // Other realms and proxies.
  {
    name: 'a plain object from another realm is written as JSON',
    make: () => vm.runInNewContext('({ a: 1 })'),
    expected: '{"a":1}',
  },
  {
    name: 'an array from another realm is written as JSON',
    make: () => vm.runInNewContext('[1, "two"]'),
    expected: '[1,"two"]',
  },
  {
    name: 'a proxy whose ownKeys trap throws is refused',
    make: () =>
      new Proxy(
        { a: 1 },
        {
          ownKeys() {
            throw new Error('the trap fails');
          },
        },
      ),
    expected: REFUSED,
  },
];

for (const row of ROWS) {
  test(row.name, () => {
    const value = row.make();
    assert.deepEqual(
      { client: clientAnswer(value), sdk: sdkAnswer(value) },
      { client: row.expected, sdk: row.expected },
    );
  });
}
