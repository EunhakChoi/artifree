import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { existsSync } from 'node:fs';
import { mkdtemp, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { createInterface } from 'node:readline';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { DatabaseSync } from 'node:sqlite';
import { chromium, expect, firefox } from '@playwright/test';
import {
  accessibilityViolations,
  summarizeViolations,
} from './lib/accessibility.mjs';
import { buildServer, startServer } from './lib/server-process.mjs';
import { serveStatic } from './lib/static-web.mjs';
import { forgeReset } from './smoke-history.mjs';
import { api, hold, raw } from './smoke-workspace.mjs';

/**
 * Browser checks for the Jobs tab of an artifact's page (decision 0055, rules
 * 13 to 15, acceptance 23 and the code part of 24) against a real
 * artifree-server and a real worker. A verification asked in the tab ends and
 * shows its verdict without a reload; a damaged file is a finding; a running
 * job is cancelled from the tab; with no worker the tab says so; a lost answer
 * is retried under its key and makes one job, also after the tab is left and
 * opened again; a refusal shows its code; "Load more" reads the next page; and
 * the list is read again after a reset.
 *
 * Each page has a browser context of its own, and what it reports is recorded
 * as the other checks record theirs: a refusal the check provokes is counted,
 * not failed, and anything else fails. Run this check alone with
 * `npm run build`, then `node scripts/smoke-jobs.mjs [case ...]`; without case
 * names every case runs.
 */

const root = fileURLToPath(new URL('../', import.meta.url));
const webRoot = resolve(root, 'apps/web/dist');
// A worker that is not the server's is read by the change feed up to 15
// seconds late (decision 0055, rule 10), so its changes get this long.
const FEED_MS = 30_000;
// The longest wait for an answer of the server or a notice the tab shows later.
const WAIT_MS = 15_000;
// The words the tab shows, written out here rather than read from the code
// under test.
const WORDS = {
  verified: 'Every file matches its listing.',
  mismatch: 'Some files do not match their listing.',
  finding: 'index.html: its content has another digest than its listing',
  noWorker: 'No worker is running, so this job waits',
  workerNone: 'Worker mode: none. Worker state: stopped.',
  tooMany: 'Too many jobs are waiting or running (too_many_jobs)',
  jobEnded: 'This job has already ended, so it cannot be cancelled (job_ended)',
  cancelNote: 'Cancelling stops the work; it undoes nothing.',
  cancelRequested: 'Cancel requested. The job stops at its next checkpoint.',
  cancelled: 'Cancelled. Nothing was undone.',
};

/**
 * Each check of the Jobs tab by its case name. The static check of the built
 * page runs last, so the browser checks report before it.
 */
const RUNNERS = {
  verdict: checkVerdict,
  damaged: checkDamaged,
  reset: checkReset,
  'no-worker': checkNoWorker,
  'lost-answer': checkLostAnswer,
  'lost-answer-away': checkLostAnswerAway,
  'too-many-jobs': checkTooManyJobs,
  'job-ended': checkJobEnded,
  'load-more': checkLoadMore,
  'running-cancel': checkRunningCancel,
  contract: checkContract,
};

/**
 * Runs the Jobs tab checks against `server`, the suite's shared server with its
 * worker, and `origin`, the page that reaches it. The checks that need a server
 * without a worker start their own, from `serverBinary`.
 */
export async function checkJobs({
  browser,
  origin,
  server,
  serverBinary,
  dataDir,
  checkAccessibility,
  cases = Object.keys(RUNNERS),
}) {
  const unknown = cases.filter((name) => !Object.hasOwn(RUNNERS, name));
  assert.deepEqual(
    unknown,
    [],
    `Unknown Jobs cases; use ${Object.keys(RUNNERS).join(', ')}.`,
  );
  const world = {
    browser,
    origin,
    server,
    serverBinary,
    dataDir,
    checkAccessibility,
  };
  for (const name of cases) await RUNNERS[name](world);
  return { cases: [...cases] };
}

/**
 * The built page carries no module preload, and the Jobs tab's text is in a
 * chunk of its own, not in the entry script (acceptance 24, code part).
 */
async function checkContract() {
  const html = await readFile(join(webRoot, 'index.html'), 'utf8');
  assert.doesNotMatch(html, /modulepreload/u, 'index.html preloads a module.');
  const entry = html.match(/<script\b[^>]*\bsrc="([^"]+\.js)"/u);
  assert.ok(entry, 'index.html has no entry script.');
  const entryText = await readFile(join(webRoot, entry[1]), 'utf8');
  assert.equal(
    entryText.includes(WORDS.noWorker),
    false,
    'The entry script holds the Jobs tab text.',
  );
  const assets = join(webRoot, 'assets');
  const holders = [];
  for (const name of await readdir(assets)) {
    if (!name.endsWith('.js')) continue;
    const text = await readFile(join(assets, name), 'utf8');
    if (text.includes(WORDS.noWorker)) holders.push(name);
  }
  assert.ok(holders.length > 0, 'No chunk holds the Jobs tab text.');
}

/**
 * A verification asked in the tab ends and shows its verdict, and the page is
 * not reloaded for it (acceptance 23).
 */
async function checkVerdict(world) {
  const artifact = await createArtifact(world.server, 'Jobs verdict');
  const opened = await openJobs(world, artifact);
  try {
    await opened.tab.evaluate(() => {
      globalThis.jobsPageMark = 'kept';
    });
    await verify(opened.panel, 1);
    const row = jobRow(opened.panel, 'Verify revision 1');
    await expect(row.getByText('Succeeded', { exact: true })).toBeVisible({
      timeout: FEED_MS,
    });
    await expect(row.getByText(WORDS.verified, { exact: true })).toBeVisible();
    await expect(
      row.getByText(/\d+ files? checked, \d+ bytes? checked/u),
    ).toBeVisible();
    assert.equal(
      await opened.tab.evaluate(() => globalThis.jobsPageMark),
      'kept',
      'The page reloaded for the verdict.',
    );
    const [job] = await jobsOf(world.server, artifact.id);
    assert.equal(job.kind, 'verify_revision');
    assert.equal(job.revision, 1);
    assert.equal(job.state, 'succeeded');
    assert.equal(job.result.verdict, 'ok');
    await world.checkAccessibility('jobs-verdict', opened.tab);
    assertCalm(opened.log, world.browser, { refused: [], cuts: 0 });
  } finally {
    await opened.context.close();
  }
}

/**
 * A file whose stored bytes no longer match its listing is a finding, shown
 * with its path and what is wrong with it (rule 13).
 */
async function checkDamaged(world) {
  const content = `<p>Damaged ${randomUUID()}</p>`;
  const artifact = await createArtifact(world.server, 'Jobs damaged', content);
  damage(world.dataDir, artifact.id, content);
  const opened = await openJobs(world, artifact);
  try {
    await verify(opened.panel, 1);
    const row = jobRow(opened.panel, 'Verify revision 1');
    await expect(row.getByText(WORDS.mismatch, { exact: true })).toBeVisible({
      timeout: FEED_MS,
    });
    const findings = row
      .getByRole('list', { name: 'Findings', exact: true })
      .locator('xpath=./li');
    await expect(findings).toHaveCount(1, { timeout: WAIT_MS });
    await expect(findings).toHaveText(WORDS.finding);
    const [job] = await jobsOf(world.server, artifact.id);
    assert.equal(job.result.verdict, 'mismatch');
    assert.deepEqual(job.result.mismatches, [
      { path: 'index.html', problem: 'digest_mismatch' },
    ]);
    assertCalm(opened.log, world.browser, { refused: [], cuts: 0 });
  } finally {
    await opened.context.close();
  }
}

/**
 * When the server says the page's cursor is not its own, the first page is read
 * again and the rows it lists are shown (rule 15). Nothing changed on the server
 * here, so the rows are the ones shown before.
 */
async function checkReset(world) {
  const artifact = await createArtifact(world.server, 'Jobs reset');
  await enqueue(world.server, artifact.id);
  const opened = await openJobs(world, artifact);
  try {
    await expect(jobRows(opened.panel)).toHaveCount(1, { timeout: WAIT_MS });
    // Enqueued through the API while the tab is open: its change arrives on
    // the feed and the tab lists the new job.
    await enqueue(world.server, artifact.id);
    await expect(jobRows(opened.panel)).toHaveCount(2, { timeout: WAIT_MS });
    const listed = (request) =>
      request.method() === 'GET' &&
      new URL(request.url()).pathname === `/v1/artifacts/${artifact.id}/jobs`;
    await Promise.all([
      opened.tab.waitForRequest(listed, { timeout: WAIT_MS }),
      forgeReset(opened.tab),
    ]);
    await expect(jobRows(opened.panel)).toHaveCount(2, { timeout: WAIT_MS });
    assertCalm(opened.log, world.browser, { refused: [], cuts: 0 });
  } finally {
    await opened.context.close();
  }
}

/**
 * A queued job that no worker takes says so in its row once it has waited more
 * than five seconds, with the worker's mode and state (rule 14).
 */
async function checkNoWorker(world) {
  await withServer(world, ['--no-worker'], async (served) => {
    const artifact = await createArtifact(served.server, 'Jobs no worker');
    const opened = await openJobs(served, artifact);
    try {
      await verify(opened.panel, 1);
      const row = jobRow(opened.panel, 'Verify revision 1');
      await expect(row.getByText('Queued', { exact: true })).toBeVisible({
        timeout: WAIT_MS,
      });
      const notice = opened.panel
        .getByRole('alert')
        .filter({ hasText: WORDS.noWorker });
      // The notice waits for the five seconds, so it is not there at once.
      await expect(notice).toHaveCount(0);
      await expect(notice).toContainText(WORDS.workerNone, {
        timeout: WAIT_MS,
      });
      await world.checkAccessibility('jobs-no-worker', opened.tab);
      assertCalm(opened.log, world.browser, { refused: [], cuts: 0 });
    } finally {
      await opened.context.close();
    }
  });
}

/**
 * A Verify whose answer is lost is retried under the same request key, and the
 * server makes one job of the two requests (rule 14).
 */
async function checkLostAnswer(world) {
  await withServer(world, ['--no-worker'], async (served) => {
    const artifact = await createArtifact(served.server, 'Jobs lost answer');
    const opened = await openJobs(served, artifact);
    try {
      await cutFirstEnqueue(opened.tab, artifact.id);
      await verify(opened.panel, 1);
      const unknown = opened.panel
        .getByRole('alert')
        .filter({ hasText: 'Outcome unknown' });
      await expect(unknown).toBeVisible({ timeout: WAIT_MS });
      assert.equal(
        (await jobsOf(served.server, artifact.id)).length,
        1,
        'The lost request did not commit one job.',
      );
      await unknown
        .getByRole('button', { name: 'Retry verify revision 1', exact: true })
        .click();
      await expect(unknown).toBeHidden({ timeout: WAIT_MS });
      await expect(jobRows(opened.panel)).toHaveCount(1, { timeout: WAIT_MS });
      assert.equal(opened.log.enqueues.length, 2, 'The presses sent');
      const [first, second] = opened.log.enqueues;
      assert.equal(second, first, 'The retry used another request key.');
      assert.equal(
        (await jobsOf(served.server, artifact.id)).length,
        1,
        'The retry made a second job.',
      );
      assertCalm(opened.log, world.browser, { refused: [], cuts: 1 });
    } finally {
      await opened.context.close();
    }
  });
}

/**
 * Cuts the answer of the first enqueue request for the artifact's jobs. The
 * server commits the job and the page never receives the answer; later requests
 * go on as they are.
 */
async function cutFirstEnqueue(tab, artifactId) {
  let cut = 0;
  await tab.route(
    (url) => url.pathname === `/v1/artifacts/${artifactId}/jobs`,
    async (route) => {
      if (route.request().method() !== 'POST' || cut > 0)
        return route.fallback();
      cut += 1;
      // The server commits the job; the page never receives the answer.
      await route.fetch();
      return route.abort('connectionreset');
    },
  );
}

/** Opens the view `name` of the artifact's page, as a reader would. */
async function openView(tab, name) {
  await tab
    .locator('#artifacts')
    .getByRole('tablist', { name: 'Artifact views' })
    .getByRole('tab', { name, exact: true })
    .click();
}

/**
 * A lost answer keeps its outcome while the tab is left and opened again: the
 * retry is sent under the first key, and the server makes one job of the two
 * requests. Leaving the tab does not drop the press (rule 14).
 */
async function checkLostAnswerAway(world) {
  await withServer(world, ['--no-worker'], async (served) => {
    const artifact = await createArtifact(served.server, 'Jobs away');
    const opened = await openJobs(served, artifact);
    try {
      await cutFirstEnqueue(opened.tab, artifact.id);
      await verify(opened.panel, 1);
      const unknown = opened.panel
        .getByRole('alert')
        .filter({ hasText: 'Outcome unknown' });
      await expect(unknown).toBeVisible({ timeout: WAIT_MS });
      await openView(opened.tab, 'Source');
      await expect(opened.panel).toBeHidden({ timeout: WAIT_MS });
      await openView(opened.tab, 'Jobs');
      await expect(opened.panel).toBeVisible({ timeout: WAIT_MS });
      await expect(unknown).toBeVisible({ timeout: WAIT_MS });
      await unknown
        .getByRole('button', { name: 'Retry verify revision 1', exact: true })
        .click();
      await expect(unknown).toBeHidden({ timeout: WAIT_MS });
      await expect(jobRows(opened.panel)).toHaveCount(1, { timeout: WAIT_MS });
      assert.equal(opened.log.enqueues.length, 2, 'The presses sent');
      const [first, second] = opened.log.enqueues;
      assert.equal(second, first, 'The retry used another request key.');
      assert.equal(
        (await jobsOf(served.server, artifact.id)).length,
        1,
        'The retry made a second job.',
      );
      assertCalm(opened.log, world.browser, { refused: [], cuts: 1 });
    } finally {
      await opened.context.close();
    }
  });
}

/**
 * A verification refused as too_many_jobs is shown as such and commits
 * nothing. Each press has a request key of its own, and a cancelled job frees
 * its place for the next press (rule 14).
 */
async function checkTooManyJobs(world) {
  await withServer(world, ['--no-worker'], async (served) => {
    const artifact = await createArtifact(served.server, 'Jobs at the limit');
    for (let count = 0; count < 16; count += 1)
      await enqueue(served.server, artifact.id);
    const opened = await openJobs(served, artifact);
    try {
      await expect(jobRows(opened.panel)).toHaveCount(16, {
        timeout: WAIT_MS,
      });
      await verify(opened.panel, 1);
      const refusal = opened.panel
        .getByRole('alert')
        .filter({ hasText: WORDS.tooMany });
      await expect(refusal).toBeVisible({ timeout: WAIT_MS });
      const listed = await jobsOf(served.server, artifact.id);
      assert.equal(listed.length, 16, 'The refused press committed a job.');
      // A cancelled job no longer counts against the limit, so the next press
      // is accepted.
      await api(served.server, 'POST', `/v1/jobs/${listed[0].id}/cancel`, {});
      await verify(opened.panel, 1);
      await expect(refusal).toBeHidden({ timeout: WAIT_MS });
      await expect(jobRows(opened.panel)).toHaveCount(17, {
        timeout: WAIT_MS,
      });
      const [first, second] = opened.log.enqueues;
      assert.notEqual(second, first, 'A press reused an earlier request key.');
      assertCalm(opened.log, world.browser, { refused: ['409'], cuts: 0 });
    } finally {
      await opened.context.close();
    }
  });
}

/**
 * A Cancel of a job that ended first is refused as job_ended, and the tab shows
 * the refusal as such (rule 14). The tab's reads wait, so the row still shows
 * the job as queued when Cancel is pressed.
 */
async function checkJobEnded(world) {
  await withServer(world, ['--no-worker'], async (served) => {
    const artifact = await createArtifact(served.server, 'Jobs ended');
    const job = await enqueue(served.server, artifact.id);
    const opened = await openJobs(served, artifact);
    try {
      const row = jobRow(opened.panel, 'Verify revision 1');
      await expect(row.getByText('Queued', { exact: true })).toBeVisible({
        timeout: WAIT_MS,
      });
      const release = await hold(
        opened.tab,
        'GET',
        (url) =>
          url.pathname.startsWith('/v1/jobs/') ||
          url.pathname === `/v1/artifacts/${artifact.id}/jobs`,
      );
      try {
        await api(served.server, 'POST', `/v1/jobs/${job.id}/cancel`, {});
        await row.getByRole('button', { name: 'Cancel', exact: true }).click();
        await expect(
          opened.panel.getByRole('alert').filter({ hasText: WORDS.jobEnded }),
        ).toBeVisible({ timeout: WAIT_MS });
      } finally {
        await release();
      }
      await expect(row.getByText('Cancelled', { exact: true })).toBeVisible({
        timeout: WAIT_MS,
      });
      assertCalm(opened.log, world.browser, { refused: ['409'], cuts: 0 });
    } finally {
      await opened.context.close();
    }
  });
}

/**
 * The list reads fifty jobs at a time, and "Load more" reads the rest (rule
 * 13).
 */
async function checkLoadMore(world) {
  await withServer(world, ['--no-worker'], async (served) => {
    const artifact = await createArtifact(served.server, 'Jobs many');
    await makeEndedJobs(served.server, artifact.id, 51);
    const opened = await openJobs(served, artifact);
    try {
      await expect(jobRows(opened.panel)).toHaveCount(50, {
        timeout: WAIT_MS,
      });
      const more = opened.panel.getByRole('button', {
        name: 'Load more',
        exact: true,
      });
      await more.click();
      await expect(jobRows(opened.panel)).toHaveCount(51, {
        timeout: WAIT_MS,
      });
      await expect(more).toBeHidden({ timeout: WAIT_MS });
      assertCalm(opened.log, world.browser, { refused: [], cuts: 0 });
    } finally {
      await opened.context.close();
    }
  });
}

/**
 * A running job is cancelled from the tab. The worker stops at its next
 * checkpoint and the job ends cancelled, which the tab shows (acceptance 23).
 * The worker is a process of its own, so its changes arrive by the feed's
 * reading of storage.
 */
async function checkRunningCancel(world) {
  await withServer(world, ['--no-worker'], async (served) => {
    const artifact = await createArtifact(served.server, 'Jobs running');
    const holdFile = join(served.directory, 'hold');
    const worker = spawn(
      world.serverBinary,
      [
        'worker',
        'run',
        '--data-dir',
        served.directory,
        '--lease-secs',
        '3',
        '--hold-file',
        holdFile,
      ],
      { stdio: ['ignore', 'pipe', 'ignore'] },
    );
    const lines = [];
    const reader = createInterface({ input: worker.stdout });
    reader.on('line', (line) => lines.push(line));
    try {
      await expect
        .poll(() => lines.some((line) => line.includes('"event":"ready"')), {
          timeout: WAIT_MS,
          message: 'The worker did not report ready.',
        })
        .toBe(true);
      const opened = await openJobs(served, artifact);
      try {
        await verify(opened.panel, 1);
        await expect
          .poll(
            () => lines.some((line) => line.includes('"event":"claimed"')),
            {
              timeout: WAIT_MS,
              message: 'The worker did not claim the job.',
            },
          )
          .toBe(true);
        const row = jobRow(opened.panel, 'Verify revision 1');
        await expect(row.getByText('Running', { exact: true })).toBeVisible({
          timeout: FEED_MS,
        });
        const cancel = row.getByRole('button', { name: 'Cancel', exact: true });
        await expect(cancel).toHaveAccessibleDescription(WORDS.cancelNote);
        await cancel.click();
        await expect(
          row.getByText(WORDS.cancelRequested, { exact: true }),
        ).toBeVisible({ timeout: WAIT_MS });
        // The worker holds the job until this file exists, before it reads the
        // file. The cancel is seen at its final checkpoint, after the hold.
        await writeFile(holdFile, '');
        await expect(row.getByText('Cancelled', { exact: true })).toBeVisible({
          timeout: FEED_MS,
        });
        await expect(
          row.getByText(WORDS.cancelled, { exact: true }),
        ).toBeVisible();
        const [job] = await jobsOf(served.server, artifact.id);
        assert.equal(job.state, 'cancelled');
        assertCalm(opened.log, world.browser, { refused: [], cuts: 0 });
      } finally {
        await opened.context.close();
      }
    } finally {
      reader.close();
      await stopWorker(worker);
    }
  });
}

/** Creates an artifact with one page whose title the search finds. */
async function createArtifact(server, label, content) {
  const title = `${label} ${randomUUID().slice(0, 8)}`;
  const { artifact } = await api(server, 'POST', '/v1/artifacts', {
    title,
    files: [
      {
        path: 'index.html',
        encoding: 'utf8',
        content: content ?? `<p>${title}</p>`,
      },
    ],
  });
  return { id: artifact.id, title };
}

/** Asks for a verification of `revision` through the API, under a new key. */
async function enqueue(server, artifactId, revision = 1) {
  const { status, json } = await raw(
    server,
    'POST',
    `/v1/artifacts/${artifactId}/jobs`,
    { kind: 'verify_revision', revision },
    `smoke-jobs-${randomUUID()}`,
  );
  assert.equal(status, 201, `Enqueueing a job failed: ${status}`);
  return json.job;
}

/**
 * Makes `count` jobs that have ended, cancelling each group before the next one
 * is asked for, so the limit of waiting jobs is never reached.
 */
async function makeEndedJobs(server, artifactId, count) {
  for (let done = 0; done < count; done += 16) {
    const group = [];
    for (let index = done; index < Math.min(done + 16, count); index += 1)
      group.push(await enqueue(server, artifactId));
    for (const job of group)
      await api(server, 'POST', `/v1/jobs/${job.id}/cancel`, {});
  }
}

/** Every job of the artifact that the server lists in its first page. */
async function jobsOf(server, artifactId) {
  const { status, json } = await raw(
    server,
    'GET',
    `/v1/artifacts/${artifactId}/jobs`,
  );
  assert.equal(status, 200, `Listing jobs failed: ${status}`);
  return json.jobs;
}

/**
 * Replaces the stored bytes of the artifact's index.html with the same number
 * of other bytes, so its digest no longer matches its listing. The same update
 * as crates/artifree-server/tests/worker_jobs.rs.
 */
function damage(dataDir, artifactId, content) {
  const altered = Buffer.from(content);
  altered[0] ^= 0x01;
  // The server's database file (crates/artifree-providers/src/storage.rs). Opening
  // a missing file would create an empty database, so it must exist already.
  const file = join(dataDir, 'artifree.sqlite3');
  assert.ok(existsSync(file), `No database at ${file}.`);
  const database = new DatabaseSync(file);
  try {
    database.exec('PRAGMA busy_timeout = 5000');
    const { changes } = database
      .prepare(
        `UPDATE blobs SET content = ?
         WHERE digest = (SELECT digest FROM revision_files
                         WHERE artifact_id = ? AND revision = 1 AND path = ?)`,
      )
      .run(altered, artifactId, 'index.html');
    assert.equal(changes, 1, 'The damaged file is not in the database.');
  } finally {
    database.close();
  }
}

/**
 * Connects a new page of its own to the server and opens the artifact's Jobs
 * tab. The caller closes the returned context.
 */
async function openJobs(world, artifact) {
  const context = await world.browser.newContext();
  try {
    const tab = await context.newPage();
    tab.setDefaultTimeout(WAIT_MS);
    const log = watch(
      tab,
      world.origin,
      artifact.id,
      world.server.content_origin,
    );
    await tab.goto(world.origin);
    const workspace = tab.locator('#artifacts');
    await workspace.getByLabel('Access token').fill(world.server.token);
    await workspace.getByRole('button', { name: 'Connect' }).click();
    await workspace.getByLabel('Search titles').fill(artifact.title);
    await workspace
      .getByRole('navigation', { name: 'Artifacts' })
      .getByRole('button', { name: new RegExp(`^${artifact.title}`, 'u') })
      .click();
    await expect(
      workspace.getByRole('heading', { name: artifact.title, exact: true }),
    ).toBeVisible({ timeout: WAIT_MS });
    await workspace
      .getByRole('tablist', { name: 'Artifact views' })
      .getByRole('tab', { name: 'Jobs', exact: true })
      .click();
    const panel = workspace.getByRole('tabpanel', {
      name: 'Jobs',
      exact: true,
    });
    await expect(panel).toBeVisible({ timeout: WAIT_MS });
    return { context, tab, panel, log };
  } catch (error) {
    await context.close();
    throw error;
  }
}

/**
 * Runs `body` with a fresh server started with `args`, and the built web app
 * served for it. The server and its files are gone afterwards.
 */
async function withServer(world, args, body) {
  const directory = await mkdtemp(join(tmpdir(), 'artifree-jobs-'));
  let server;
  let web;
  try {
    server = await startServer(world.serverBinary, directory, args);
    web = await serveStatic(webRoot, 0, { apiUrl: server.api_url });
    return await body({
      ...world,
      server,
      origin: web.origin,
      directory,
    });
  } finally {
    await web?.close();
    await server?.stop();
    await rm(directory, { recursive: true, force: true });
  }
}

/** Stops a worker process this check started, and waits until it has exited. */
async function stopWorker(worker) {
  if (worker.exitCode !== null || worker.signalCode !== null) return;
  const exited = once(worker, 'exit');
  worker.kill('SIGTERM');
  const timer = setTimeout(() => worker.kill('SIGKILL'), WAIT_MS);
  try {
    await exited;
  } finally {
    clearTimeout(timer);
  }
}

/** Presses "Verify revision N" in the tab. */
async function verify(panel, revision) {
  await panel
    .getByRole('button', { name: `Verify revision ${revision}`, exact: true })
    .click();
}

/** The rows of the Jobs list, one per job. */
function jobRows(panel) {
  return panel
    .getByRole('list', { name: 'Jobs', exact: true })
    .locator('xpath=./li');
}

/** The row of the Jobs list that shows `text`. */
function jobRow(panel, text) {
  return jobRows(panel).filter({ hasText: text });
}

/**
 * Records what a Jobs page reports: its JavaScript errors and its console
 * messages. A 409 the API answers is a refusal the check provokes, and a
 * connection reset is an answer the check cuts; both are counted. What the
 * artifact's preview frame logs is kept as evidence, as the suite keeps it. The
 * keys of the page's own enqueue requests are kept, in order.
 */
function watch(tab, origin, artifactId, contentOrigin) {
  const log = {
    errors: [],
    unexpected: [],
    preview: [],
    refused: [],
    cuts: [],
    enqueues: [],
  };
  tab.on('pageerror', (error) => log.errors.push(error.message));
  tab.on('console', (message) => {
    if (message.type() !== 'error' && message.type() !== 'warning') return;
    const text = message.text();
    if (message.location().url.startsWith(contentOrigin)) {
      log.preview.push(text);
      return;
    }
    const fromApi = message.location().url.startsWith(`${origin}/v1/`);
    const status = text.match(/status of (\d{3}) /u);
    if (fromApi && status?.[1] === '409') log.refused.push(status[1]);
    else if (fromApi && text.includes('ERR_CONNECTION_RESET'))
      log.cuts.push(text);
    else log.unexpected.push(text);
  });
  tab.on('request', (request) => {
    if (request.method() !== 'POST') return;
    const { pathname } = new URL(request.url());
    if (pathname === `/v1/artifacts/${artifactId}/jobs`)
      log.enqueues.push(request.headers()['idempotency-key']);
  });
  return log;
}

/**
 * The page logged no error and no unexpected message, and its refusals and cut
 * answers are the ones the check provokes. Chromium logs each refused or cut
 * answer; Firefox does not, so only Chromium is held to the counts.
 */
function assertCalm(log, browser, expected) {
  assert.deepEqual(log.errors, [], 'Browser JavaScript errors.');
  assert.deepEqual(log.unexpected, [], 'Unexpected console messages.');
  if (browser.browserType().name() === 'chromium') {
    assert.deepEqual(
      log.refused,
      expected.refused,
      'The refusals the check did not provoke.',
    );
    assert.equal(log.cuts.length, expected.cuts, 'The cut answers.');
  }
}

/** Runs this check alone against a fresh server and the built web app. */
async function runAlone() {
  const serverBinary =
    process.env.ARTIFREE_SERVER_BIN === undefined
      ? buildServer(root)
      : resolve(process.env.ARTIFREE_SERVER_BIN);
  const directory = await mkdtemp(join(tmpdir(), 'artifree-jobs-'));
  let server;
  let web;
  let browser;
  try {
    server = await startServer(serverBinary, directory);
    web = await serveStatic(webRoot, 0, { apiUrl: server.api_url });
    // The engine the whole suite uses (scripts/smoke.mjs), chosen the same way.
    const engines = { chromium, firefox };
    const name = process.env.ARTIFREE_BROWSER ?? 'chromium';
    assert.ok(Object.hasOwn(engines, name), `Unsupported browser: ${name}`);
    browser = await engines[name].launch();
    const named = process.argv.slice(2);
    const result = await checkJobs({
      browser,
      origin: web.origin,
      server,
      serverBinary,
      dataDir: directory,
      checkAccessibility: runAccessibility,
      ...(named.length > 0 && { cases: named }),
    });
    console.log(JSON.stringify({ status: 'passed', browser: name, ...result }));
  } finally {
    await browser?.close();
    await web?.close();
    await server?.stop();
    await rm(directory, { recursive: true, force: true });
  }
}

/** The accessibility check of `runAlone`, which has no suite report to write. */
async function runAccessibility(label, target) {
  const violations = await accessibilityViolations(target);
  assert.deepEqual(
    summarizeViolations(violations),
    [],
    `Accessibility failures: ${label}`,
  );
}

const isEntry =
  process.argv[1] !== undefined &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href;
if (isEntry) await runAlone();
