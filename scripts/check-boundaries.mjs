import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readdir, readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  checkRustPackages,
  checkInventory,
  checkClientImport,
  checkClientManifest,
  moduleSpecifiers,
} from './lib/boundaries.mjs';

const root = fileURLToPath(new URL('../', import.meta.url));
const metadata = JSON.parse(
  execFileSync(
    'cargo',
    ['metadata', '--no-deps', '--format-version', '1', '--locked'],
    { cwd: root, encoding: 'utf8' },
  ),
);
checkRustPackages(metadata.packages);
const inventory = JSON.parse(
  await readFile(
    new URL('../contracts/generated/scaffold-inventory.json', import.meta.url),
    'utf8',
  ),
);
await checkInventory(root, inventory);

async function sources(directory) {
  const files = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    assert.ok(!entry.isSymbolicLink(), `Unreviewed source symlink: ${path}`);
    if (entry.isDirectory()) files.push(...(await sources(path)));
    else if (/\.(?:ts|tsx)$/.test(entry.name)) files.push(path);
  }
  return files;
}

let clients = 0;
let imports = 0;
for (const workspace of ['apps', 'packages']) {
  for (const entry of await readdir(join(root, workspace), {
    withFileTypes: true,
  })) {
    if (!entry.isDirectory()) continue;
    const directory = join(root, workspace, entry.name);
    const manifest = JSON.parse(
      await readFile(join(directory, 'package.json'), 'utf8'),
    );
    checkClientManifest(manifest);
    clients += 1;
    for (const filename of await sources(join(directory, 'src'))) {
      for (const specifier of moduleSpecifiers(
        filename,
        await readFile(filename, 'utf8'),
      )) {
        checkClientImport(
          manifest.name,
          directory,
          filename,
          specifier,
          manifest,
        );
        imports += 1;
      }
    }
  }
}
assert.equal(clients, 5, 'Review added or removed TypeScript workspaces.');
console.log(
  `Verified ${metadata.packages.length} Rust crates, ${clients} TypeScript packages, ${imports} imports, and ${inventory.components.length} inventory paths.`,
);
