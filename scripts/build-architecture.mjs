import { readFile, writeFile } from 'node:fs/promises';
import { transform } from 'esbuild';
import assert from 'node:assert/strict';

const args = process.argv.slice(2);
assert.ok(
  args.length === 0 || (args.length === 1 && args[0] === '--check'),
  'Usage: build-architecture.mjs [--check]',
);

const root = new URL('../', import.meta.url);
const [template, css, source, snapshot] = await Promise.all([
  readFile(new URL('docs/architecture/index.html', root), 'utf8'),
  readFile(new URL('docs/architecture/style.css', root), 'utf8'),
  readFile(new URL('docs/architecture/src.ts', root), 'utf8'),
  readFile(
    new URL('contracts/generated/scaffold-inventory.json', root),
    'utf8',
  ),
]);
const inventory = JSON.parse(snapshot);
for (const marker of ['/* ARTIFREE_STYLE */', '/* ARTIFREE_SCRIPT */']) {
  assert.equal(
    template.split(marker).length,
    2,
    `Expected exactly one template marker: ${marker}`,
  );
}
const result = await transform(source, {
  loader: 'ts',
  target: 'es2022',
  format: 'esm',
  charset: 'utf8',
});
const safeInventory = JSON.stringify(inventory).replaceAll('<', '\\u003c');
const script = `const INVENTORY = ${safeInventory};\n${result.code}`.replaceAll(
  '</script',
  '<\\/script',
);
const html = template
  .replace('/* ARTIFREE_STYLE */', css)
  .replace('/* ARTIFREE_SCRIPT */', script);
const output = new URL('docs/architecture.html', root);
if (args.includes('--check')) {
  assert.equal(
    await readFile(output, 'utf8'),
    html,
    'Architecture HTML drift; run npm run build:architecture.',
  );
} else {
  await writeFile(output, html);
}
console.log(
  `${args.includes('--check') ? 'Verified' : 'Built'} standalone architecture HTML (${Buffer.byteLength(html)} bytes), using ${inventory.components.length} actual source-linked modules.`,
);
