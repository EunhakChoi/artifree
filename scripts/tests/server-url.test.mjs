import assert from 'node:assert/strict';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// The rules are web code without DOM or React imports; bundle them as they
// ship.
const bundle = await build({
  entryPoints: [
    fileURLToPath(
      new URL('../../apps/web/src/artifacts/server-url.ts', import.meta.url),
    ),
  ],
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  write: false,
});
const { defaultServer, parseServerUrl, resolveInitialServer, serverToKeep } =
  await import(
    `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`
  );

const SECRET = 'hunter2';

test('accepted addresses come back as origin plus path without a trailing slash', () => {
  for (const [text, url] of [
    ['http://127.0.0.1:7410', 'http://127.0.0.1:7410'],
    ['  https://API.example.com/  ', 'https://api.example.com'],
    ['https://api.example.com:443', 'https://api.example.com'],
    ['https://example.com/artifree//', 'https://example.com/artifree'],
    ['http://[::1]:7410/', 'http://[::1]:7410'],
  ]) {
    assert.deepEqual(parseServerUrl(text), { ok: true, url }, text);
  }
});

test('refused addresses give a reason that does not repeat them', () => {
  for (const [text, reason] of [
    ['', /absolute/],
    ['/api', /absolute/],
    ['api.example.com', /absolute/],
    ['javascript:alert(1)', /absolute/],
    ['ftp://api.example.com', /absolute/],
    ['https:api.example.com', /absolute/],
    ['https//user:p%40ss@host', /absolute/],
    ['https://api.example.com:65536', /absolute/],
    ['https://', /absolute/],
    ['http:///evil.example', /absolute/],
    ['https:///', /absolute/],
    [`https://user:${SECRET}@api.example.com`, /user name or password/],
    [`https://${SECRET}@api.example.com`, /user name or password/],
    ['https://@api.example.com', /user name or password/],
    [`https://api.example.com\\@${SECRET}`, /absolute/],
    [`https://api.example.com?key=${SECRET}`, /query or fragment/],
    ['https://api.example.com?', /query or fragment/],
    [`https://api.example.com#${SECRET}`, /query or fragment/],
    ['https://api.example.com#', /query or fragment/],
    ['https://api.example.com/path?', /query or fragment/],
  ]) {
    const result = parseServerUrl(text);
    assert.equal(result.ok, false, text);
    assert.match(result.reason, reason, text);
    assert.ok(!result.reason.includes(SECRET), text);
    assert.ok(!result.reason.includes('example'), text);
  }
});

const input = (overrides = {}) => ({
  search: '',
  stored: undefined,
  buildDefault: undefined,
  ...overrides,
});

test('the link wins over the tab, the tab over the build default, and that over the page', () => {
  const all = {
    search: '?server=https://link.example',
    stored: 'https://tab.example',
    buildDefault: 'https://build.example',
  };
  assert.deepEqual(
    [
      resolveInitialServer(input(all)),
      resolveInitialServer(input({ ...all, search: '' })),
      resolveInitialServer(input({ ...all, search: '', stored: undefined })),
      resolveInitialServer(input()),
    ].map(({ url, source }) => ({ url, source })),
    [
      { url: 'https://link.example', source: 'link' },
      { url: 'https://tab.example', source: 'tab' },
      { url: 'https://build.example', source: 'build' },
      { url: '', source: 'page' },
    ],
  );
});

test('a token is restored only when no link named a server and nothing was ignored', () => {
  assert.equal(resolveInitialServer(input()).restoreToken, true);
  assert.equal(
    resolveInitialServer(input({ stored: 'https://tab.example' })).restoreToken,
    true,
  );
  assert.equal(
    resolveInitialServer(input({ buildDefault: 'https://build.example' }))
      .restoreToken,
    true,
  );
  // A link names a server: the person comes through the form.
  assert.equal(
    resolveInitialServer(input({ search: '?server=https://link.example' }))
      .restoreToken,
    false,
  );
});

test('an ignored address leaves a notice, falls through, and withholds the token', () => {
  for (const [name, overrides, expected] of [
    [
      'link',
      { search: `?server=javascript:${SECRET}`, stored: 'https://tab.example' },
      { url: 'https://tab.example', source: 'tab', notices: 1 },
    ],
    [
      'tab',
      {
        stored: `https://u:${SECRET}@tab.example`,
        buildDefault: 'https://build.example',
      },
      { url: 'https://build.example', source: 'build', notices: 1 },
    ],
    [
      'build',
      { buildDefault: `https://build.example?${SECRET}` },
      { url: '', source: 'page', notices: 1 },
    ],
    [
      'all three',
      {
        search: '?server=%2Fapi',
        stored: 'ftp://x',
        buildDefault: 'nope',
      },
      { url: '', source: 'page', notices: 3 },
    ],
  ]) {
    const result = resolveInitialServer(input(overrides));
    assert.deepEqual(
      {
        url: result.url,
        source: result.source,
        notices: result.notices.length,
      },
      expected,
      name,
    );
    assert.equal(result.restoreToken, false, `${name} withholds the token`);
    for (const notice of result.notices) {
      assert.ok(
        !notice.includes(SECRET) && !notice.includes('example'),
        notice,
      );
    }
  }
});

test('the build default is never kept for the tab, other servers are', () => {
  const built = 'https://build.example/';
  assert.equal(serverToKeep('https://build.example', built), undefined);
  assert.equal(serverToKeep('', built), undefined);
  assert.equal(serverToKeep('', undefined), undefined);
  assert.equal(
    serverToKeep('https://other.example', built),
    'https://other.example',
  );
  assert.equal(
    serverToKeep('https://build.example', undefined),
    'https://build.example',
  );
  assert.equal(defaultServer(built), 'https://build.example');
  assert.equal(defaultServer('nope'), '');
  assert.equal(defaultServer(undefined), '');
});
