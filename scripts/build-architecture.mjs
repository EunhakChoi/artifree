import { readFile, writeFile } from 'node:fs/promises';
import { transform } from 'esbuild';

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
await writeFile(output, html);
console.log(
  `Built standalone architecture HTML (${Buffer.byteLength(html)} bytes), using ${inventory.components.length} actual source-linked modules.`,
);
