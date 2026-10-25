import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { access, readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../', import.meta.url));
const metadata = JSON.parse(
  execFileSync(
    'cargo',
    ['metadata', '--no-deps', '--format-version', '1', '--locked'],
    { cwd: root, encoding: 'utf8' },
  ),
);
const allowed = {
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
for (const pkg of metadata.packages) {
  assert.ok(
    Object.hasOwn(allowed, pkg.name),
    `Unreviewed crate boundary: ${pkg.name}`,
  );
  for (const dep of pkg.dependencies)
    assert.ok(
      allowed[pkg.name].includes(dep.name),
      `${pkg.name} cannot depend on ${dep.name}; review the layer boundary first.`,
    );
}
const inventory = JSON.parse(
  await readFile(
    new URL('../contracts/generated/scaffold-inventory.json', import.meta.url),
    'utf8',
  ),
);
assert.equal(
  new Set(inventory.components.map((item) => item.id)).size,
  inventory.components.length,
  'Component IDs must be unique.',
);
for (const component of inventory.components) {
  assert.ok(
    !component.path.startsWith('/') &&
      !component.path.split('/').includes('..'),
    'Inventory paths must stay within the repository.',
  );
  await access(new URL(`../${component.path}`, import.meta.url));
}
assert.equal(
  inventory.http_listening,
  false,
  'Do not advertise a running API in the scaffold.',
);
console.log(
  `Verified ${metadata.packages.length} Rust dependency boundaries and ${inventory.components.length} source paths.`,
);
