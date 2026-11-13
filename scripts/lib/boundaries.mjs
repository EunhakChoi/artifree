import assert from 'node:assert/strict';
import { realpath } from 'node:fs/promises';
import { dirname, isAbsolute, relative, resolve, sep } from 'node:path';
import { parseSync } from 'oxc-parser';

// Third-party edges are reviewed with their purpose (decision 0003):
// sha2 fingerprints requests and addresses stored content, rusqlite is local
// storage, getrandom creates identifiers and tokens, axum/tokio serve HTTP,
// base64 decodes uploads, and tower drives in-process router tests; http-body
// names the frame and size-hint types of the request-body wrapper that hands
// back what a refused request left unread, which must not alter size hints or
// trailers (decision 0048), and builds bodies that never finish in tests. rmcp is
// the official MCP SDK serving the inbound MCP adapter, and serde parses its
// tool arguments into contract DTOs (decision 0004). futures-core provides
// only the Stream trait that Axum's server-sent events response consumes; it
// is already in the dependency tree through Axum (decision 0006). png,
// miniz_oxide and crc32fast give artifree-core the maintained decoder and the
// independent exact zlib and CRC checks that decision 0045 rule 3 requires
// before any PNG upload is admitted. zune-jpeg is the maintained JPEG decoder
// that runs after the Artifree-owned entropy walker for the same rule; the
// walker, not the decoder, guarantees truncated data is refused.
const rustEdges = {
  'artifree-domain': [],
  'artifree-ports': ['artifree-domain'],
  'artifree-core': [
    'artifree-domain',
    'artifree-ports',
    'crc32fast',
    'miniz_oxide',
    'png',
    'sha2',
    'zune-jpeg',
  ],
  'artifree-providers': [
    'artifree-ports',
    'artifree-domain',
    'getrandom',
    'rusqlite',
    // Decision 0056 rule 7: the Linux execution provider's system calls, through
    // safe interfaces and for Linux targets only.
    'nix',
    'rustix',
    'seccompiler',
    'sha2',
  ],
  'artifree-contracts': ['serde', 'serde_json', 'schemars'],
  'artifree-server': [
    'artifree-core',
    'artifree-contracts',
    'artifree-domain',
    'artifree-ports',
    'artifree-providers',
    'axum',
    'base64',
    'futures-core',
    'getrandom',
    'http-body',
    'rmcp',
    'serde',
    'serde_json',
    'tokio',
  ],
};

// Edges allowed only as dev-dependencies, so test tooling cannot become part
// of a shipped binary unreviewed. rusqlite lets server tests plant the
// leftover content that a healthy server never produces, to prove the
// cleanup route removes it (decision 0029). signal-hook lets the provider's
// test-support payload stay alive through SIGTERM without first-party unsafe
// code, for the cleanup acceptance of decision 0042 rule 8.
const rustDevEdges = {
  'artifree-providers': ['signal-hook'],
  'artifree-server': ['rusqlite', 'tower'],
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
      const devOnly = rustDevEdges[pkg.name]?.includes(dependency.name);
      assert.ok(
        rustEdges[pkg.name].includes(dependency.name) ||
          (devOnly === true && dependency.kind === 'dev'),
        devOnly === true
          ? `${pkg.name} may use ${dependency.name} only as a dev-dependency.`
          : `${pkg.name} cannot depend on ${dependency.name}; review the layer boundary first.`,
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

/**
 * The inventory may name a framework or runtime only when the server crate
 * really depends on it, and may claim an HTTP API only with both. An MCP
 * claim also requires the HTTP API and the MCP SDK.
 * Inventory paths are portable repository paths, not user artifact paths.
 */
export async function checkInventory(root, inventory, serverDependencies) {
  for (const field of ['server_framework', 'async_runtime']) {
    const name = inventory[field];
    if (name !== null)
      assert.ok(
        serverDependencies.includes(name),
        `Inventory ${field} ${name} is not an artifree-server dependency.`,
      );
  }
  if (inventory.http_api)
    assert.ok(
      inventory.server_framework !== null && inventory.async_runtime !== null,
      'An HTTP API claim requires a selected framework and runtime.',
    );
  if (inventory.mcp_api)
    assert.ok(
      inventory.http_api && serverDependencies.includes('rmcp'),
      'An MCP claim requires the HTTP API and the rmcp server dependency.',
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

/**
 * Rejects `panic = "abort"` in any profile of the workspace manifest. JPEG validation contains a
 * decoder panic with `catch_unwind` (docs/asset-validation.md); under `abort` that containment
 * does nothing and one crafted upload ends the process.
 */
export function checkPanicStrategy(manifestText) {
  const abort = /^\s*panic\s*=\s*["']abort["']/m;
  assert.ok(
    !abort.test(manifestText),
    'Cargo.toml sets panic = "abort": JPEG decoder panic containment needs unwinding.',
  );
}

/** Names that a release build must not carry (decision 0057 rule 2). */
const RELEASE_BUILD_FORBIDDEN = ['enforcement-fixture', '--all-features'];

/**
 * Rejects a release build that names the enforcement fixture feature or `--all-features`
 * (decision 0057 rule 2). `whole` are release build sources read whole: the server build of the
 * release archives and clean-install checks, the release packaging, the container image build and
 * the container image. In `workflows` only the lines that run `cargo ... build` are read, since the
 * enforcement job and the minimum-version job may test with the feature.
 */
export function checkReleaseBuilds({ whole, workflows }) {
  for (const { path, text } of whole)
    for (const name of RELEASE_BUILD_FORBIDDEN)
      assert.ok(
        !text.includes(name),
        `${path} names ${name}: release builds use the default features (decision 0057 rule 2).`,
      );
  for (const { path, text } of workflows)
    for (const line of text.split('\n'))
      if (/\bcargo\b.*\bbuild\b/.test(line))
        for (const name of RELEASE_BUILD_FORBIDDEN)
          assert.ok(
            !line.includes(name),
            `${path} builds with ${name}: release builds use the default features (decision 0057 rule 2).`,
          );
}
