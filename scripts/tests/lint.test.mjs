import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { after, before, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import stylelint from 'stylelint';

const root = fileURLToPath(new URL('../../', import.meta.url));
let directory;
before(async () => {
  directory = await mkdtemp(join(root, 'apps/web/src/lint-probe-'));
});
after(async () => {
  if (directory) await rm(directory, { recursive: true, force: true });
});

async function lint(name, source) {
  const path = join(directory, `${name}.tsx`);
  await writeFile(path, source);
  const result = spawnSync(
    process.execPath,
    [join(root, 'node_modules/oxlint/bin/oxlint'), '--format=json', path],
    { cwd: root, encoding: 'utf8' },
  );
  if (result.error) throw result.error;
  assert.equal(
    result.stderr,
    '',
    'Lint configuration or theme warnings must not be ignored.',
  );
  return { status: result.status, ...JSON.parse(result.stdout) };
}

test('Actual UI lint configuration accepts tokens, variants, and layout classes', async () => {
  const result = await lint(
    'good',
    'import { Button } from "@/components/ui/button"; export function Good(){return <><Button size="lg" className="mt-4 w-full">Save</Button><div className="flex gap-4 bg-primary" /></>;}',
  );
  assert.equal(result.status, 0, JSON.stringify(result.diagnostics));
  assert.deepEqual(result.diagnostics, []);
  assert.equal(
    result.number_of_files,
    1,
    'A zero-file lint run is not a pass.',
  );
});

test('Actual UI lint configuration rejects all six design-system violations', async () => {
  const result = await lint(
    'bad',
    'import { Button } from "@/components/ui/button"; declare function generatedClass():string; export function Bad(){return <><Button className="p-4" /><div className="bg-red-500" /><div className="p-[13px]" /><div style={{padding:13}} /><div className="flex-cols" /><Button className={generatedClass()} /></>;}',
  );
  assert.equal(result.status, 1);
  const codes = result.diagnostics.map((diagnostic) => diagnostic.code);
  for (const rule of [
    'no-restyle',
    'no-raw-colors',
    'no-arbitrary-values',
    'no-inline-styles',
    'no-unknown-classes',
    'require-static-classes',
  ]) {
    assert.ok(
      codes.includes(`shadcn(${rule})`),
      `Rule ${rule} stopped checking the real UI project: ${JSON.stringify(codes)}`,
    );
  }
});

test('TypeScript lint rejects explicit any', async () => {
  const result = await lint(
    'untyped',
    'export function identity(value:any){return value;}',
  );
  assert.equal(result.status, 1);
  assert.ok(
    result.diagnostics.some((diagnostic) =>
      diagnostic.code.includes('no-explicit-any'),
    ),
    JSON.stringify(result),
  );
});

test('Actual CSS lint configuration rejects unknown properties and duplicate selectors', async () => {
  const options = {
    configFile: join(root, 'stylelint.config.mjs'),
    codeFilename: join(root, 'apps/web/src/lint-fixture.css'),
  };
  const valid = await stylelint.lint({
    ...options,
    code: '.example { color: var(--primary); }',
  });
  assert.equal(
    valid.errored,
    false,
    JSON.stringify(valid.results.map((result) => result.warnings)),
  );
  const invalid = await stylelint.lint({
    ...options,
    code: '.example { colr: red; } .example { color: blue; }',
  });
  assert.equal(invalid.errored, true);
  const rules = invalid.results.flatMap((result) =>
    result.warnings.map((warning) => warning.rule),
  );
  assert.ok(rules.includes('property-no-unknown'), JSON.stringify(rules));
  assert.ok(rules.includes('no-duplicate-selectors'), JSON.stringify(rules));
});
