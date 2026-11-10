/** The deepest nesting a value may have, as the client's DATA_VALUE_LEVELS. */
const DATA_VALUE_LEVELS = 64;

/**
 * The JSON text of `value`, or undefined when JSON.stringify would change it
 * silently (decision 0053, rule 5). The same refusals as the client's
 * `plainData` in packages/client/src/client.ts; a test runs one table through
 * both.
 */
export function dataText(value: unknown): string | undefined {
  try {
    return JSON.stringify(plainData(value, DATA_VALUE_LEVELS));
  } catch {
    // A refusal throws inside plainData, and so does a proxy trap that fails.
    // Both mean the value cannot be sent; this function never throws.
    return undefined;
  }
}

/**
 * A plain copy of `value`, or a throw unless JSON.stringify would write
 * `value` faithfully. JSON.stringify would turn NaN and Infinity into null,
 * drop undefined, function and symbol members, write array holes as null,
 * ignore symbol keys and hidden properties, call toJSON, and write a Date, a
 * Map or a class instance as something else. The copy is what gets serialized,
 * and each property is read once, so a getter, a proxy or a later mutation
 * cannot change what is sent. `levels` is the nesting still allowed below
 * `value`.
 */
function plainData(value: unknown, levels: number): unknown {
  if (value === null || typeof value === 'string' || typeof value === 'boolean')
    return value;
  if (typeof value === 'number') {
    if (!Number.isFinite(value)) throw new Error('a number is not finite');
    return value;
  }
  if (typeof value !== 'object')
    throw new Error('only JSON values have a JSON form');
  if (levels <= 0)
    throw new Error('the value nests too deeply or refers to itself');
  const keys = Reflect.ownKeys(value);
  if (Array.isArray(value)) {
    const length = value.length;
    // The own keys are the indexes and `length`. A hole or an extra property
    // breaks the count before any element is read.
    if (keys.length !== length + 1)
      throw new Error('the array has holes or properties beyond its elements');
    const items: unknown[] = [];
    for (let index = 0; index < length; index += 1)
      items.push(plainData(ownData(value, String(index)), levels - 1));
    return items;
  }
  // A plain object's prototype is null, or is itself a root prototype: another
  // realm's Object.prototype, as for a value from vm or an iframe.
  const prototype: unknown = Object.getPrototypeOf(value);
  if (prototype !== null && Object.getPrototypeOf(prototype) !== null)
    throw new Error('only plain objects and arrays have a JSON form');
  const members: [string, unknown][] = [];
  for (const key of keys) {
    if (typeof key === 'symbol')
      throw new Error('a symbol key has no JSON form');
    members.push([key, plainData(ownData(value, key), levels - 1)]);
  }
  return Object.fromEntries(members);
}

/** The value of own data property `name`, read once; refuses anything else. */
function ownData(holder: object, name: string): unknown {
  const descriptor = Object.getOwnPropertyDescriptor(holder, name);
  if (descriptor === undefined) throw new Error('a member is missing');
  if (!('value' in descriptor))
    throw new Error('a member is a getter or setter, not a value');
  if (descriptor.enumerable !== true)
    throw new Error('a member is hidden from JSON.stringify');
  return descriptor.value;
}
