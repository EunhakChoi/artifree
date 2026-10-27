import assert from 'node:assert/strict';
import { mkdtemp, mkdir, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { test } from 'node:test';
import {
  checkInventory,
  checkRustPackages,
  checkClientImport,
  checkClientManifest,
  moduleSpecifiers,
} from '../lib/boundaries.mjs';

const rustNames = [
  'artifree-domain',
  'artifree-ports',
  'artifree-core',
  'artifree-providers',
  'artifree-contracts',
  'artifree-server',
];
const emptyPackages = () =>
  rustNames.map((name) => ({ name, dependencies: [] }));

test('Rust core rejects a concrete provider even when renamed or dev-only', () => {
  const packages = emptyPackages();
  packages[2].dependencies.push({
    name: 'artifree-providers',
    rename: 'safe_name',
    kind: 'dev',
  });
  assert.throws(
    () => checkRustPackages(packages),
    /artifree-core cannot depend on artifree-providers/,
  );
});

test('Rust server may use test tooling only as a dev-dependency', () => {
  const packages = emptyPackages();
  packages[5].dependencies.push({ name: 'tower', kind: 'dev' });
  checkRustPackages(packages);
  for (const kind of [null, 'build']) {
    packages[5].dependencies = [{ name: 'tower', kind }];
    assert.throws(
      () => checkRustPackages(packages),
      /artifree-server may use tower only as a dev-dependency/,
    );
  }
});

test('Rust policy rejects an unreviewed workspace member', () => {
  assert.throws(
    () =>
      checkRustPackages([
        ...emptyPackages(),
        { name: 'unreviewed', dependencies: [] },
      ]),
    /Review added or removed/,
  );
});

test('Rust core accepts its domain and port dependencies', () => {
  const packages = emptyPackages();
  packages[2].dependencies.push(
    { name: 'artifree-domain' },
    { name: 'artifree-ports' },
  );
  checkRustPackages(packages);
});

const ownerRoot = resolve('/workspace/packages/client');
const filename = join(ownerRoot, 'src/index.ts');
const client = {
  name: '@artifree/client',
  dependencies: { '@artifree/contracts': '0.1.0' },
};

test('Client policy accepts a declared contract dependency', () => {
  checkClientImport(
    client.name,
    ownerRoot,
    filename,
    '@artifree/contracts',
    client,
  );
});

for (const [name, specifier, error] of [
  ['another client package', '@artifree/runtime-sdk', /cannot import/],
  ['a relative escape', '../../../apps/web/src/app.tsx', /escapes its package/],
  ['an alias escape', '@/../../../apps/web/src/app.tsx', /escapes its package/],
  ['Node APIs in browser code', 'node:fs', /Node APIs do not belong/],
  [
    'an undeclared dependency',
    'unreviewed-package',
    /undeclared production dependency/,
  ],
]) {
  test(`Client policy rejects ${name}`, () =>
    assert.throws(
      () =>
        checkClientImport(client.name, ownerRoot, filename, specifier, client),
      error,
    ));
}

test('Manifest declarations cannot bypass the client dependency policy', () => {
  assert.throws(
    () =>
      checkClientManifest({
        name: '@artifree/contracts',
        dependencies: { '@artifree/client': '0.1.0' },
      }),
    /cannot depend on/,
  );
});

test('Parser finds type-only imports, re-exports, import types, and lazy imports', () => {
  assert.deepEqual(
    moduleSpecifiers(
      'example.ts',
      'import type { A } from "a"; export { B } from "b"; export * from "c"; type D = import("d").D; const lazy = import("e");',
    ),
    ['a', 'b', 'c', 'd', 'e'],
  );
});

test('Computed imports cannot hide a dependency', () => {
  assert.throws(
    () => moduleSpecifiers('example.ts', 'const lazy = import(packageName);'),
    /Computed module imports/,
  );
});

test('Malformed source fails closed rather than skipping import analysis', () => {
  assert.throws(
    () => moduleSpecifiers('example.ts', 'import {'),
    /Cannot parse/,
  );
});

test('Inventory rejects non-portable paths, duplicate IDs, and escaping symlinks', async () => {
  const parent = await mkdtemp(join(tmpdir(), 'artifree-boundary-'));
  const root = join(parent, 'repo');
  await mkdir(join(root, 'src'), { recursive: true });
  await mkdir(join(parent, 'outside'));
  await writeFile(join(root, 'src/file.ts'), 'export {};');
  const inventory = (components) => ({
    http_api: false,
    mcp_api: false,
    server_framework: null,
    async_runtime: null,
    components,
  });
  const check = (value) => checkInventory(root, value, []);
  try {
    await check(inventory([{ id: 'one', path: 'src/file.ts' }]));
    for (const path of [
      '../outside',
      '/tmp',
      'src/../src',
      'src\\file.ts',
      'src/%2e%2e',
      'src/file.ts?ignored',
      '.',
    ]) {
      await assert.rejects(
        check(inventory([{ id: 'one', path }])),
        /Non-portable inventory path/,
        path,
      );
    }
    await assert.rejects(
      check(
        inventory([
          { id: 'same', path: 'src' },
          { id: 'same', path: 'src/file.ts' },
        ]),
      ),
      /IDs must be unique/,
    );
    await symlink(join(parent, 'outside'), join(root, 'escape'), 'junction');
    await assert.rejects(
      check(inventory([{ id: 'one', path: 'escape' }])),
      /escapes the repository/,
    );
  } finally {
    await rm(parent, { recursive: true, force: true });
  }
});

test('Inventory cannot claim a framework or API the server does not use', async () => {
  const root = await mkdtemp(join(tmpdir(), 'artifree-inventory-'));
  const claim = (fields) => ({
    http_api: false,
    mcp_api: false,
    server_framework: null,
    async_runtime: null,
    components: [],
    ...fields,
  });
  try {
    await checkInventory(
      root,
      claim({
        http_api: true,
        server_framework: 'axum',
        async_runtime: 'tokio',
      }),
      ['axum', 'tokio'],
    );
    await assert.rejects(
      checkInventory(root, claim({ server_framework: 'axum' }), ['tokio']),
      /server_framework axum is not an artifree-server dependency/,
    );
    await assert.rejects(
      checkInventory(root, claim({ async_runtime: 'tokio' }), ['tower']),
      /async_runtime tokio is not/,
    );
    await assert.rejects(
      checkInventory(
        root,
        claim({ http_api: true, server_framework: 'axum' }),
        ['axum'],
      ),
      /requires a selected framework and runtime/,
    );
    await checkInventory(
      root,
      claim({
        http_api: true,
        mcp_api: true,
        server_framework: 'axum',
        async_runtime: 'tokio',
      }),
      ['axum', 'tokio', 'rmcp'],
    );
    for (const [fields, dependencies] of [
      [{ mcp_api: true }, ['rmcp']],
      [
        {
          http_api: true,
          mcp_api: true,
          server_framework: 'axum',
          async_runtime: 'tokio',
        },
        ['axum', 'tokio'],
      ],
    ])
      await assert.rejects(
        checkInventory(root, claim(fields), dependencies),
        /MCP claim requires the HTTP API and the rmcp/,
      );
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('Rust storage provider cannot reach the HTTP framework', () => {
  const packages = emptyPackages();
  packages[3].dependencies.push({ name: 'axum' });
  assert.throws(
    () => checkRustPackages(packages),
    /artifree-providers cannot depend on axum/,
  );
});
