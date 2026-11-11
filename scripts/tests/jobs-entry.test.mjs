import assert from 'node:assert/strict';
import { readFile, readdir } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { before, describe, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

/*
 * The Jobs tab is a lazy chunk, loaded when the tab opens and not with the page
 * (decision 0055, rule 15), so the web entry bundle does not grow with it
 * (acceptance 24). The checks read the web sources and the bundle that esbuild
 * makes of them: the tab's modules are imported only by the lazy reference in
 * artifact-detail.tsx, and they import nothing that only a lazy chunk holds.
 */

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const WEB_SOURCE = join(REPO_ROOT, 'apps/web/src');
const ENTRY = 'apps/web/src/main.tsx';
const ARTIFACT_DETAIL = 'apps/web/src/artifacts/artifact-detail.tsx';
const JOBS_PANEL = 'apps/web/src/artifacts/jobs-panel.tsx';

/** The tab's modules, all under `apps/web/src/artifacts`. */
const JOBS_MODULES = [
  'jobs-model.ts',
  'jobs-text.ts',
  'jobs-driver.ts',
  'jobs-row.tsx',
  'jobs-panel.tsx',
];

/** Modules a jobs module must not import, under any specifier form. */
const HELD_ELSEWHERE = [
  'followers',
  'follow-job',
  'preview-frame',
  'source-editor',
];
/** Name prefixes of runtime modules, which a jobs module must not import. */
const HELD_ELSEWHERE_PREFIXES = ['runtime-'];

/*
 * `import type` and `export type` are erased, so they load nothing. Every other
 * `import ... from`, `export ... from` and `import '...'` is a value statement,
 * including `import { type X }`, which keeps a side-effect import under
 * verbatimModuleSyntax. `import()` loads its module on demand.
 */
const STATIC_IMPORT =
  /\b(?:import|export)\b(\s+type\b)?[^'"`;]*?\bfrom\s*(['"])([^'"`]+)\2/g;
const SIDE_EFFECT_IMPORT = /\bimport\s*(['"])([^'"`]+)\1/g;
const DYNAMIC_IMPORT = /\bimport\s*\(\s*(['"])([^'"`]+)\1\s*\)/g;

/** The text of a web source file, by its path under `apps/web/src`. */
function readWebSource(file) {
  return readFile(join(WEB_SOURCE, file), 'utf8');
}

/** Every `.ts` or `.tsx` file below `directory`, as paths relative to it. */
async function sourceFiles(directory, prefix = '') {
  const found = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = `${prefix}${entry.name}`;
    if (entry.isDirectory()) {
      const below = await sourceFiles(join(directory, entry.name), `${path}/`);
      found.push(...below);
    } else if (/\.tsx?$/.test(entry.name)) {
      found.push(path);
    }
  }
  return found;
}

/** The last segment of a path or specifier, without its `.ts` or `.tsx`. */
function moduleName(path) {
  const leaf = path.slice(path.lastIndexOf('/') + 1);
  return leaf.replace(/\.tsx?$/, '');
}

/** Whether a path or specifier names a jobs module. */
function isJobsModule(path) {
  return moduleName(path).startsWith('jobs-');
}

/** Whether a specifier names a module that a jobs module must not import. */
function isHeldElsewhere(specifier) {
  const name = moduleName(specifier);
  return (
    HELD_ELSEWHERE.includes(name) ||
    HELD_ELSEWHERE_PREFIXES.some((prefix) => name.startsWith(prefix))
  );
}

/**
 * Every module specifier a source names. `kind` is `dynamic` for `import()`,
 * and `static` otherwise. `typeOnly` is true only for a type-only statement.
 */
function specifiersOf(source) {
  const found = [];
  for (const match of source.matchAll(STATIC_IMPORT)) {
    found.push({
      kind: 'static',
      typeOnly: match[1] !== undefined,
      specifier: match[3],
    });
  }
  for (const match of source.matchAll(SIDE_EFFECT_IMPORT)) {
    found.push({ kind: 'static', typeOnly: false, specifier: match[2] });
  }
  for (const match of source.matchAll(DYNAMIC_IMPORT)) {
    found.push({ kind: 'dynamic', typeOnly: false, specifier: match[2] });
  }
  return found;
}

/** Whether a statement is a value import, which loads its module up front. */
function isValueImport(found) {
  return found.kind === 'static' && !found.typeOnly;
}

/** How many times `text` occurs in `source`. */
function occurrences(source, text) {
  return source.split(text).length - 1;
}

/** The bundle's output for the web entry, as `[path, output]`. */
function entryOutput(metafile) {
  const outputs = Object.entries(metafile.outputs).filter(
    ([, output]) => output.entryPoint === ENTRY,
  );
  assert.equal(outputs.length, 1, `the bundle has one output for ${ENTRY}`);
  return outputs[0];
}

test('no static import of jobs modules outside the tab', async () => {
  const others = (await sourceFiles(WEB_SOURCE)).filter(
    (file) => !isJobsModule(file),
  );
  assert.ok(
    others.includes('artifacts/artifact-detail.tsx'),
    'the scan reaches artifact-detail.tsx',
  );
  const offenders = [];
  for (const file of others) {
    const source = await readWebSource(file);
    for (const found of specifiersOf(source)) {
      if (isValueImport(found) && isJobsModule(found.specifier)) {
        offenders.push(`${file} imports ${found.specifier}`);
      }
    }
  }
  assert.deepEqual(offenders, []);
});

test('artifact-detail.tsx loads jobs-panel only through a literal dynamic import', async () => {
  const source = await readWebSource('artifacts/artifact-detail.tsx');
  const literal = "import('./jobs-panel')";
  assert.equal(
    occurrences(source, literal),
    1,
    `artifact-detail.tsx names ${literal} exactly once`,
  );
});

for (const file of JOBS_MODULES) {
  test(`${file} imports nothing that only a lazy chunk holds`, async () => {
    const source = await readWebSource(`artifacts/${file}`);
    const held = specifiersOf(source)
      .map((found) => found.specifier)
      .filter(isHeldElsewhere);
    assert.deepEqual(held, []);
  });
}

describe('web entry bundle', () => {
  let metafile;

  before(async () => {
    const result = await build({
      absWorkingDir: REPO_ROOT,
      entryPoints: [ENTRY],
      bundle: true,
      splitting: true,
      format: 'esm',
      jsx: 'automatic',
      tsconfig: join(REPO_ROOT, 'apps/web/tsconfig.json'),
      loader: { '.css': 'empty' },
      outdir: join(REPO_ROOT, 'entry-bundle'),
      metafile: true,
      write: false,
      logLevel: 'silent',
    });
    metafile = result.metafile;
  });

  test('entry inputs exclude jobs files', () => {
    const [, entry] = entryOutput(metafile);
    assert.ok(
      ARTIFACT_DETAIL in entry.inputs,
      'the entry reaches artifact-detail.tsx',
    );
    assert.deepEqual(Object.keys(entry.inputs).filter(isJobsModule), []);
  });

  test('jobs-panel.tsx is in an output other than the entry', () => {
    const [entryPath] = entryOutput(metafile);
    const holders = Object.entries(metafile.outputs).filter(
      ([path, output]) => path !== entryPath && JOBS_PANEL in output.inputs,
    );
    assert.ok(
      holders.length > 0,
      `${JOBS_PANEL} is in no output but the entry`,
    );
  });
});
