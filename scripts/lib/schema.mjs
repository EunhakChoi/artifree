import Ajv2020 from 'ajv/dist/2020.js';

/**
 * Strict Ajv with the integer formats Schemars emits for Rust u64/i64.
 * JSON numbers above 2^53 lose precision in JavaScript; Artifree revision
 * numbers and Unix milliseconds stay far below that.
 */
export function createAjv() {
  const ajv = new Ajv2020({ strict: true, allErrors: true });
  ajv.addFormat('uint64', {
    type: 'number',
    validate: (value) => Number.isSafeInteger(value) && value >= 0,
  });
  ajv.addFormat('int64', {
    type: 'number',
    validate: (value) => Number.isSafeInteger(value),
  });
  return ajv;
}
