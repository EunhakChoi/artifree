import assert from 'node:assert/strict';
import { realpath } from 'node:fs/promises';
import { dirname, isAbsolute, relative, resolve, sep } from 'node:path';
import { parseSync } from 'oxc-parser';

const rustEdges = {
  'artifree-domain': [],
  'artifree-ports': ['artifree-domain'],
  'artifree-core': ['artifree-domain', 'artifree-ports'],
  'artifree-providers': ['artifree-ports', 'artifree-domain'],
  'artifree-contracts': ['serde', 'serde_json', 'schemars'],
  'artifree-server': [
    'artifree-core',
    'artifree-contracts',
    'artifree-providers',
    'serde_json',
  ],
};

const clientEdges = {
  '@artifree/contracts': [],
  '@artifree/client': ['@artifree/contracts'],
  '@artifree/runtime-sdk': ['@artifree/contracts'],
  '@artifree/cli': ['@artifree/contracts', '@artifree/client'],
  '@artifree/web': [
    '@artifree/contracts',
    '@artifree/client',
    '@artifree/runtime-sdk',
  ],
};

/** Review every Rust edge, including build/dev dependencies and renamed crates. */
export function checkRustPackages(packages) {
  assert.deepEqual(
    packages.map((pkg) => pkg.name).sort(),
    Object.keys(rustEdges).sort(),
    'Review added or removed Rust crates.',
  );
  for (const pkg of packages) {
    assert.ok(
      Object.hasOwn(rustEdges, pkg.name),
      `Unreviewed crate: ${pkg.name}`,
    );
    for (const dependency of pkg.dependencies) {
      assert.ok(
        rustEdges[pkg.name].includes(dependency.name),
        `${pkg.name} cannot depend on ${dependency.name}; review the layer boundary first.`,
      );
    }
  }
}

function inside(root, target) {
  const path = relative(root, target);
  return (
    path !== '' &&
    !isAbsolute(path) &&
    path !== '..' &&
    !path.startsWith(`..${sep}`)
  );
}

/** Inventory paths are portable repository paths, not user artifact paths. */
export async function checkInventory(root, inventory) {
  assert.equal(
    inventory.http_listening,
    false,
    'The scaffold has no HTTP listener.',
  );
  assert.equal(
    inventory.server_framework,
    null,
    'HTTP framework remains undecided.',
  );
  assert.equal(
    inventory.async_runtime,
    null,
    'Async runtime remains undecided.',
  );
  assert.equal(
    new Set(inventory.components.map((component) => component.id)).size,
    inventory.components.length,
    'Component IDs must be unique.',
  );
  const realRoot = await realpath(root);
  for (const component of inventory.components) {
    const path = component.path;
    assert.ok(
      /^[\w.-]+(?:\/[\w.-]+)*$/.test(path) &&
        !path.split('/').some((part) => part === '.' || part === '..'),
      `Non-portable inventory path: ${path}`,
    );
    const actual = await realpath(resolve(root, path));
    assert.ok(
      inside(realRoot, actual),
      `Inventory path escapes the repository: ${path}`,
    );
  }
}

/** Inspect import/export syntax, including type-only and dynamic imports. */
export function moduleSpecifiers(filename, source) {
  const parsed = parseSync(filename, source, { sourceType: 'module' });
  assert.equal(
    parsed.errors.length,
    0,
    `Cannot parse ${filename}: ${JSON.stringify(parsed.errors)}`,
  );
  const specifiers = [];
  function visit(node) {
    if (node === null || typeof node !== 'object') return;
    if (
      [
        'ImportDeclaration',
        'ExportNamedDeclaration',
        'ExportAllDeclaration',
        'ImportExpression',
        'TSImportType',
      ].includes(node.type)
    ) {
      const value = node.source ?? node.argument?.literal ?? node.argument;
      if (value !== null && value !== undefined) {
        assert.equal(
          typeof value.value,
          'string',
          `Computed module imports require a reviewed boundary: ${filename}`,
        );
        specifiers.push(value.value);
      }
    }
    for (const value of Object.values(node)) {
      if (Array.isArray(value)) value.forEach(visit);
      else if (value !== null && typeof value === 'object') visit(value);
    }
  }
  visit(parsed.program);
  return specifiers;
}

/** Clients can share contracts, but cannot reach across another client's files. */
export function checkClientImport(
  owner,
  ownerRoot,
  filename,
  specifier,
  manifest,
) {
  assert.ok(
    Object.hasOwn(clientEdges, owner),
    `Unreviewed TypeScript package: ${owner}`,
  );
  if (specifier.startsWith('.') || specifier.startsWith('@/')) {
    const target = specifier.startsWith('@/')
      ? resolve(ownerRoot, 'src', specifier.slice(2))
      : resolve(dirname(filename), specifier);
    assert.ok(
      inside(ownerRoot, target),
      `${owner} import escapes its package: ${specifier}`,
    );
    return;
  }
  if (specifier.startsWith('node:')) {
    assert.equal(
      owner,
      '@artifree/cli',
      `Node APIs do not belong in browser packages: ${owner}`,
    );
    return;
  }
  const name = specifier.startsWith('@')
    ? specifier.split('/').slice(0, 2).join('/')
    : specifier.split('/')[0];
  if (name.startsWith('@artifree/'))
    assert.ok(
      clientEdges[owner].includes(name),
      `${owner} cannot import ${name}.`,
    );
  assert.ok(
    Object.hasOwn(manifest.dependencies ?? {}, name) ||
      Object.hasOwn(manifest.peerDependencies ?? {}, name),
    `${owner} imports undeclared production dependency: ${name}`,
  );
}

export function checkClientManifest(manifest) {
  assert.ok(
    Object.hasOwn(clientEdges, manifest.name),
    `Unreviewed TypeScript package: ${manifest.name}`,
  );
  for (const name of Object.keys(manifest.dependencies ?? {})) {
    if (name.startsWith('@artifree/'))
      assert.ok(
        clientEdges[manifest.name].includes(name),
        `${manifest.name} cannot depend on ${name}.`,
      );
  }
}
