import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { test } from 'node:test';
import { build } from 'esbuild';

// The notice beside an adopted preview (decision 0054, rules 8 and 12), bundled
// as it ships and rendered to markup. It offers a button that loads the newer
// revision only where its caller passes a way to load it. The source panel's
// saved preview passes none: it pins the revision of its draft, so a restart
// would pin that same revision again.
const web = new URL('../../apps/web/', import.meta.url);
const { AdoptionNotice, createElement, renderToStaticMarkup } =
  await loadNotice();

/**
 * Bundles the notice with React and its server renderer, then loads the bundle
 * from a file. The browser build of react-dom/server opens a message port that
 * keeps the process alive, so the node build is bundled instead.
 */
async function loadNotice() {
  const dir = await mkdtemp(join(tmpdir(), 'adoption-notice-'));
  try {
    const outfile = join(dir, 'adoption-notice.mjs');
    await build({
      stdin: {
        contents: [
          "export { AdoptionNotice } from './src/artifacts/adoption-notice.tsx';",
          "export { createElement } from 'react';",
          "export { renderToStaticMarkup } from 'react-dom/server';",
        ].join('\n'),
        resolveDir: fileURLToPath(web),
      },
      bundle: true,
      format: 'esm',
      platform: 'node',
      jsx: 'automatic',
      // esbuild's ESM output cannot require Node builtins, and react-dom's node
      // build requires some, so the bundle gets a require of its own.
      banner: {
        js: "import { createRequire } from 'node:module'; const require = createRequire(import.meta.url);",
      },
      // The '@/' alias of the web app's components is read from its tsconfig.
      tsconfig: fileURLToPath(new URL('tsconfig.json', web)),
      outfile,
    });
    return await import(pathToFileURL(outfile).href);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
}

/** The notice for a frame that pins revision 2 while the head is revision 3. */
function noticeFor(props) {
  return renderToStaticMarkup(
    createElement(AdoptionNotice, {
      documentRevision: 2,
      view: { revision: 2, head_revision: 3 },
      ...props,
    }),
  );
}

test('a newer revision the caller cannot load is reported, not offered', () => {
  const markup = noticeFor({});
  assert.match(markup, /A newer revision exists/);
  assert.match(
    markup,
    /Revision 3 is newer than revision 2, which this preview runs\./,
  );
  assert.doesNotMatch(markup, /Load revision/);
  assert.doesNotMatch(markup, /<button/);
});

test('a caller that can load the newer revision offers it as a button', () => {
  const markup = noticeFor({ onLoadNewer: () => {} });
  assert.match(markup, /A newer revision exists/);
  assert.match(markup, /<button[^>]*>Load revision 3<\/button>/);
});

/** The notice for a frame of revision 2 that saved revision 3. */
function savedNoticeFor(following) {
  return renderToStaticMarkup(
    createElement(AdoptionNotice, {
      documentRevision: 2,
      view: { revision: 3, head_revision: 3 },
      following,
    }),
  );
}

test('a preview that follows the latest revision is told that refreshing loads its save', () => {
  const markup = savedNoticeFor(true);
  assert.match(markup, /Preview kept running across a save/);
  assert.match(markup, /Refresh the preview to load the saved revision\./);
});

test('a preview of a chosen revision is not told that refreshing loads its save', () => {
  const markup = savedNoticeFor(false);
  assert.match(markup, /Preview kept running across a save/);
  assert.doesNotMatch(markup, /Refresh the preview/);
  assert.match(markup, /Show the latest revision to load the saved one\./);
});
