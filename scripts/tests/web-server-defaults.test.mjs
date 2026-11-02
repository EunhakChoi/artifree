import assert from 'node:assert/strict';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// Decision 0028: the static web workspace can be built with a default API
// server (VITE_ARTIFREE_SERVER). These rules are web code without DOM or React imports;
// bundle them as they ship.
const bundle = await build({
  stdin: {
    contents: [
      "export { parseServerUrl } from './server-url.ts';",
      "export { checkBuildDefault, resolveInitialServer } from './initial-server.ts';",
    ].join('\n'),
    resolveDir: fileURLToPath(
      new URL('../../apps/web/src/artifacts/', import.meta.url),
    ),
  },
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { checkBuildDefault, parseServerUrl, resolveInitialServer } =
  await import(
    `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
  );

const accepted = [
  ['http://127.0.0.1:7410', 'http://127.0.0.1:7410'],
  ['  https://API.Example.com/  ', 'https://api.example.com'],
  ['https://api.example.com:443', 'https://api.example.com'],
  ['https://example.com/artifree/', 'https://example.com/artifree'],
  ['http://[::1]:7410', 'http://[::1]:7410'],
];
for (const [raw, expected] of accepted) {
  test(`a server address of ${JSON.stringify(raw)} is accepted as ${expected}`, () => {
    assert.deepEqual(parseServerUrl(raw), { ok: true, url: expected });
  });
}

const refused = [
  ['', /empty/],
  ['   ', /empty/],
  ['api.example.com', /absolute/],
  ['/v1', /absolute/],
  ['javascript:alert(1)', /http:\/\/ or https:\/\//],
  ['data:text/html,x', /http:\/\/ or https:\/\//],
  ['ftp://api.example.com', /http:\/\/ or https:\/\//],
  ['file:///etc/passwd', /http:\/\/ or https:\/\//],
  ['https://user:secret@api.example.com', /credentials/],
  ['https://user@api.example.com', /credentials/],
  ['https://api.example.com/?token=1', /query string or fragment/],
  ['https://api.example.com/#x', /query string or fragment/],
];
for (const [raw, reason] of refused) {
  test(`a server address of ${JSON.stringify(raw)} is refused`, () => {
    const result = parseServerUrl(raw);
    assert.equal(result.ok, false);
    assert.match(result.reason, reason);
    assert.doesNotMatch(result.reason, /secret/, 'a secret is never echoed');
  });
}

const none = '';
const configured = 'https://Default.example/';

test('without any setting the page uses its own origin, as before', () => {
  assert.deepEqual(
    resolveInitialServer({ search: '', stored: null, buildDefault: none }),
    { url: '', fromQuery: false, notices: [] },
  );
});

test('the build-time default is used (and normalized) when nothing else names a server', () => {
  assert.deepEqual(
    resolveInitialServer({
      search: '',
      stored: null,
      buildDefault: configured,
    }),
    { url: 'https://default.example', fromQuery: false, notices: [] },
  );
});

test('?server= beats the tab setting and the default, and withholds a stored token', () => {
  const result = resolveInitialServer({
    search: '?server=https://link.example/',
    stored: 'https://tab.example',
    buildDefault: configured,
  });
  assert.deepEqual(result, {
    url: 'https://link.example',
    fromQuery: true,
    notices: [],
  });
});

test('the address kept for this tab beats the default', () => {
  const result = resolveInitialServer({
    search: '',
    stored: 'https://tab.example',
    buildDefault: configured,
  });
  assert.equal(result.url, 'https://tab.example');
  assert.equal(result.fromQuery, false);
});

const unsafeLinks = [
  'javascript:alert(1)',
  'ftp://evil.example',
  'https://user:secret@evil.example',
  'evil.example',
];
for (const link of unsafeLinks) {
  test(`?server=${link} is reported and never used`, () => {
    const result = resolveInitialServer({
      search: `?server=${encodeURIComponent(link)}`,
      stored: null,
      buildDefault: configured,
    });
    assert.equal(result.url, 'https://default.example');
    assert.equal(result.fromQuery, true, 'a stored token stays withheld');
    assert.equal(result.notices.length, 1);
    assert.match(
      result.notices[0],
      /^The server address in the link was ignored\./,
    );
    assert.doesNotMatch(result.notices[0], /secret/);
  });
}

test('an invalid stored address is skipped for the default', () => {
  const result = resolveInitialServer({
    search: '',
    stored: 'javascript:alert(1)',
    buildDefault: configured,
  });
  assert.equal(result.url, 'https://default.example');
});

test("an invalid build-time default is shown and leaves today's behavior", () => {
  const result = resolveInitialServer({
    search: '',
    stored: null,
    buildDefault: 'ftp://default.example',
  });
  assert.equal(result.url, '');
  assert.equal(result.fromQuery, false);
  assert.equal(result.notices.length, 1);
  assert.match(
    result.notices[0],
    /^The default server built into this page was ignored\. The address must start with http/,
  );
});

test('a build-time default is checked like any address', () => {
  assert.deepEqual(checkBuildDefault(''), { url: '' });
  assert.deepEqual(checkBuildDefault('  '), { url: '' });
  assert.deepEqual(checkBuildDefault('https://API.example.com/'), {
    url: 'https://api.example.com',
  });
  for (const bad of ['https://u:secret@a.example', 'javascript:alert(1)']) {
    const result = checkBuildDefault(bad);
    assert.equal(result.url, '', bad);
    assert.ok(result.notice, bad);
    assert.doesNotMatch(result.notice, /secret/);
  }
});
