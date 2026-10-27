import assert from 'node:assert/strict';
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

/**
 * Check the MCP tool catalog that `tools/list` serves. MCP clients compile
 * these schemas with their own validators, so they must be self-contained
 * (no `$ref`) and use only standard formats: a strict Ajv without the integer
 * formats above must accept them. Arguments are closed objects, every tool
 * that commits requires an idempotency key, and every output schema accepts
 * the error body, because clients validate error results too.
 */
export function checkMcpCatalog(catalog) {
  const ajv = new Ajv2020({ strict: true, allErrors: true });
  assert.ok(
    typeof catalog.instructions === 'string' && catalog.instructions.length > 0,
    'MCP server instructions are required.',
  );
  assert.ok(
    Array.isArray(catalog.tools) && catalog.tools.length > 0,
    'The MCP catalog must list tools.',
  );
  const names = catalog.tools.map((tool) => tool.name);
  assert.equal(
    new Set(names).size,
    names.length,
    'MCP tool names must be unique.',
  );
  ajv.compile(catalog.error_result);
  const errorBody = { error: { code: 'not_found', message: 'gone' } };
  for (const tool of catalog.tools) {
    assert.match(tool.name, /^[a-z][a-z_]*$/, `MCP tool name ${tool.name}`);
    assert.ok(
      ['read', 'commit', 'issue_link'].includes(tool.effect),
      `${tool.name}: unknown effect ${tool.effect}`,
    );
    for (const key of ['inputSchema', 'outputSchema']) {
      const schema = tool[key];
      assert.ok(
        !JSON.stringify(schema).includes('"$ref"'),
        `${tool.name}.${key} must not use $ref.`,
      );
      assert.equal(
        schema.type,
        'object',
        `${tool.name}.${key} must be an object.`,
      );
      ajv.compile(schema);
    }
    const validateOutput = ajv.compile(tool.outputSchema);
    assert.ok(
      validateOutput(errorBody),
      `${tool.name}.outputSchema must accept an error result.`,
    );
    assert.equal(
      tool.inputSchema.additionalProperties,
      false,
      `${tool.name} must reject unknown arguments.`,
    );
    if (tool.effect === 'commit')
      assert.ok(
        tool.inputSchema.required?.includes('idempotency_key'),
        `${tool.name} commits and must require idempotency_key.`,
      );
  }
}
