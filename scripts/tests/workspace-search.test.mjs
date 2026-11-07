import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { setImmediate } from 'node:timers/promises';
import { test } from 'node:test';
import { searchWorkspaceArtifacts } from '../lib/workspace-search.mjs';

// Control the browser I/O boundary, not the search helper or application UI.
function boundary({ fillFailure, failed = false, changed = false } = {}) {
  const request = Promise.withResolvers();
  const response = Promise.withResolvers();
  const rendered = Promise.withResolvers();
  const calls = [];
  let matches;
  let answerMatches;
  const frame = {};
  let text;
  const page = {
    mainFrame: () => frame,
    waitForResponse(predicate) {
      answerMatches = predicate;
      return response.promise;
    },
    waitForRequest(predicate) {
      matches = predicate;
      return request.promise;
    },
    locator(selector) {
      assert.equal(selector, '#artifacts');
      return {
        getByLabel(label) {
          assert.equal(label, 'Search titles');
          return {
            async fill(value) {
              text = value;
              calls.push(['fill', value]);
              if (fillFailure !== undefined) throw fillFailure;
            },
            inputValue: async () => (changed ? 'Different search' : text),
          };
        },
        getByText(text, options) {
          assert.deepEqual(options, { exact: true });
          if (text === 'Could not list artifacts')
            return { isVisible: async () => failed };
          assert.equal(text, 'Loading artifacts…');
          return {
            waitFor(options) {
              assert.deepEqual(options, { state: 'hidden' });
              calls.push(['await-render']);
              return rendered.promise;
            },
          };
        },
      };
    },
  };
  return {
    page,
    calls,
    request,
    response,
    rendered,
    createRequest(method, url, top = true) {
      return {
        method: () => method,
        url: () => url,
        frame: () => (top ? frame : {}),
      };
    },
    emit(event) {
      if (matches(event)) request.resolve(event);
      return matches(event);
    },
    answer(event) {
      const answer = { request: () => event };
      if (answerMatches(answer)) response.resolve(answer);
      return answerMatches(answer);
    },
  };
}

test('a listed artifact cannot be chosen until the exact search starts and its answer renders', async () => {
  const browser = boundary();
  const searching = searchWorkspaceArtifacts(
    browser.page,
    'Runtime probe',
  ).then(() => browser.calls.push(['choose-result']));
  await setImmediate();
  assert.deepEqual(browser.calls, [['fill', 'Runtime probe']]);
  assert.equal(
    browser.emit(
      browser.createRequest(
        'POST',
        'http://localhost/v1/artifacts?q=Runtime+probe',
      ),
    ),
    false,
  );
  assert.equal(
    browser.emit(
      browser.createRequest('GET', 'http://localhost/v1/artifacts?q=Other'),
    ),
    false,
  );
  assert.equal(
    browser.emit(
      browser.createRequest(
        'GET',
        'http://localhost/v1/changes?q=Runtime+probe',
      ),
    ),
    false,
  );
  assert.equal(
    browser.emit(
      browser.createRequest(
        'GET',
        'http://localhost/v1/artifacts?q=Runtime+probe',
        false,
      ),
    ),
    false,
  );
  const old = browser.createRequest(
    'GET',
    'http://localhost/v1/artifacts?q=Runtime+probe',
  );
  assert.equal(
    browser.answer(old),
    false,
    'An earlier same-query response is not the new request.',
  );
  await setImmediate();
  assert.deepEqual(browser.calls, [['fill', 'Runtime probe']]);
  const current = browser.createRequest(
    'GET',
    'http://localhost/v1/artifacts?q=Runtime+probe',
  );
  assert.equal(browser.emit(current), true);
  await setImmediate();
  assert.deepEqual(browser.calls, [['fill', 'Runtime probe']]);
  assert.equal(browser.answer(current), true);
  await setImmediate();
  assert.deepEqual(browser.calls, [
    ['fill', 'Runtime probe'],
    ['await-render'],
  ]);
  browser.rendered.resolve();
  await searching;
  assert.deepEqual(browser.calls, [
    ['fill', 'Runtime probe'],
    ['await-render'],
    ['choose-result'],
  ]);
});

test('an immediate answer to the new request is not missed', async () => {
  const browser = boundary();
  const searching = searchWorkspaceArtifacts(browser.page, 'Runtime probe');
  const current = browser.createRequest(
    'GET',
    'http://localhost/v1/artifacts?q=Runtime+probe',
  );
  browser.emit(current);
  browser.answer(current);
  browser.rendered.resolve();
  await searching;
  assert.deepEqual(browser.calls, [
    ['fill', 'Runtime probe'],
    ['await-render'],
  ]);
});

test('a failed rendered search cannot allow a result to be chosen', async () => {
  const browser = boundary({ failed: true });
  const searching = searchWorkspaceArtifacts(browser.page, 'Runtime probe');
  const current = browser.createRequest(
    'GET',
    'http://localhost/v1/artifacts?q=Runtime+probe',
  );
  browser.emit(current);
  browser.answer(current);
  browser.rendered.resolve();
  await assert.rejects(searching, /The artifact search failed\./);
});

test('a different current input cannot stand in for the completed query', async () => {
  const browser = boundary({ changed: true });
  const searching = searchWorkspaceArtifacts(browser.page, 'Runtime probe');
  const current = browser.createRequest(
    'GET',
    'http://localhost/v1/artifacts?q=Runtime+probe',
  );
  browser.emit(current);
  browser.answer(current);
  browser.rendered.resolve();
  await assert.rejects(searching, /The artifact search changed/);
});

test('a cancelled request response rejects instead of allowing selection', async () => {
  const browser = boundary();
  const searching = searchWorkspaceArtifacts(browser.page, 'Runtime probe');
  const current = browser.createRequest(
    'GET',
    'http://localhost/v1/artifacts?q=Runtime+probe',
  );
  browser.emit(current);
  const cancelled = new Error('context closed while awaiting search response');
  browser.response.reject(cancelled);
  await assert.rejects(searching, (error) => error === cancelled);
  assert.deepEqual(browser.calls, [['fill', 'Runtime probe']]);
});

test('a failed search waiter rejects instead of allowing selection', async () => {
  const browser = boundary();
  const failure = new Error('search request failed');
  const searching = searchWorkspaceArtifacts(browser.page, 'Runtime probe');
  browser.request.reject(failure);
  browser.response.reject(new Error('response stopped after request failure'));
  await assert.rejects(searching, (error) => error === failure);
  assert.deepEqual(browser.calls, [['fill', 'Runtime probe']]);
});

test('a failed input retains its original error when the request waiter also fails', async () => {
  const input = new Error('search input detached');
  const browser = boundary({ fillFailure: input });
  const searching = searchWorkspaceArtifacts(browser.page, 'Runtime probe');
  browser.request.reject(new Error('context closed before request'));
  browser.response.reject(new Error('context closed before response'));
  await assert.rejects(searching, (error) => error === input);
  await setImmediate();
  assert.deepEqual(browser.calls, [['fill', 'Runtime probe']]);
});

test('early request and response failures stay handled while input failure is reported', () => {
  const result = spawnSync(
    process.execPath,
    [
      '--input-type=module',
      '--eval',
      `
    import assert from 'node:assert/strict';
    import { setImmediate } from 'node:timers/promises';
    const { searchWorkspaceArtifacts } = await import(process.argv[1]);
    const unhandled = [];
    process.on('unhandledRejection', (error) => unhandled.push(error));
    const input = new Error('input detached');
    const page = {
      waitForRequest: () => Promise.reject(new Error('request closed')),
      waitForResponse: () => Promise.reject(new Error('response closed')),
      locator: () => ({ getByLabel: () => ({ fill: async () => { throw input; } }) }),
    };
    await assert.rejects(searchWorkspaceArtifacts(page, 'Runtime probe'), (error) => error === input);
    await setImmediate();
    assert.deepEqual(unhandled, []);
    console.log('input retained; unhandled=0');
  `,
      new URL('../lib/workspace-search.mjs', import.meta.url).href,
    ],
    { encoding: 'utf8', timeout: 5000 },
  );
  assert.equal(result.error, undefined);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout.trim(), 'input retained; unhandled=0');
});
