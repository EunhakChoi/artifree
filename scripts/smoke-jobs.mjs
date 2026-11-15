import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { existsSync } from 'node:fs';
import { mkdtemp, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { createInterface } from 'node:readline';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { promisify } from 'node:util';
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
 * the list is read again after a reset. On this release build the head build
 * status and "Build revision N" answer profile_unknown (decision 0058, R03ea);
 * since no smoke host runs a profile, a pending and a failed build are given
 * to the tab by intercepting its reads and its enqueue in the browser. The
 * preview's build choice (R03eb) says profile_unknown on this build, and its
 * notice of an older build and its answer for a revision without one are
 * proven with the status, the tickets and the build page intercepted.
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
  profileUnknown:
    'This server cannot build the react-v1 profile (profile_unknown).',
  noExecution: 'No execution worker is ready, so this build waits',
  buildFailed: 'The build reported errors.',
  previewNotice:
    'Showing the build of revision 1, the latest build. Revision 2, the newest, has a build pending.',
  previewMissing: 'Revision 1 has no build.',
};

/** The build profile the tab asks for (decision 0058, rule 14). */
const PROFILE = 'react-v1';

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
  'build-unknown': checkBuildUnknown,
  'build-intercepted': checkBuildIntercepted,
  'preview-build-unknown': checkPreviewBuildUnknown,
  'preview-build-notice': checkPreviewBuildNotice,
  'preview-build-missing': checkPreviewBuildMissing,
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
  await damage(world.dataDir, artifact.id, content);
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

/**
 * On this release build the head build status says that the server cannot
 * build react-v1, and a press of "Build revision N" is refused with the same
 * code and commits nothing (decision 0058, R03ea).
 */
async function checkBuildUnknown(world) {
  const artifact = await createArtifact(world.server, 'Jobs build unknown');
  const opened = await openJobs(world, artifact);
  try {
    // The banner is a polite status region; a refused press is an alert.
    const banner = opened.panel
      .getByRole('status')
      .filter({ hasText: WORDS.profileUnknown });
    await expect(banner).toBeVisible({ timeout: WAIT_MS });
    await opened.panel
      .getByRole('button', { name: 'Build revision 1', exact: true })
      .click();
    const refusal = opened.panel
      .getByRole('alert')
      .filter({ hasText: WORDS.profileUnknown });
    await expect(refusal).toBeVisible({ timeout: WAIT_MS });
    assert.deepEqual(
      await jobsOf(world.server, artifact.id),
      [],
      'The refused build committed a job.',
    );
    await refusal.getByRole('button', { name: 'Dismiss' }).click();
    await expect(refusal).toBeHidden({ timeout: WAIT_MS });
    await expect(banner).toBeVisible();
    // A real job of the artifact sends job_changed changes, which read the list
    // again but not a status the server refused as profile_unknown.
    await enqueue(world.server, artifact.id);
    await expect(
      jobRow(opened.panel, 'Verify revision 1').getByText('Succeeded', {
        exact: true,
      }),
    ).toBeVisible({ timeout: FEED_MS });
    assert.equal(
      opened.log.statusReads.length,
      1,
      'The status was read again after profile_unknown.',
    );
    assertCalm(opened.log, world.browser, { refused: ['422'], cuts: 0 });
  } finally {
    await opened.context.close();
  }
}

/**
 * The tab's reads of one artifact's build, answered in the browser: the head
 * status, the enqueue of a build, the job list and the job reads all come from
 * `state`, which the check changes. Every other request reaches the server.
 */
async function interceptBuild(tab, artifactId, state) {
  const json = (route, status, body) =>
    route.fulfill({
      status,
      contentType: 'application/json',
      body: JSON.stringify(body),
    });
  await tab.route(
    (url) =>
      url.pathname === `/v1/artifacts/${artifactId}/builds/${PROFILE}` ||
      url.pathname === `/v1/artifacts/${artifactId}/jobs` ||
      url.pathname === `/v1/jobs/${state.job.id}`,
    async (route) => {
      const request = route.request();
      const { pathname } = new URL(request.url());
      if (pathname.endsWith(`/builds/${PROFILE}`))
        return json(route, 200, state.status);
      if (pathname === `/v1/jobs/${state.job.id}`)
        return json(route, 200, state.job);
      if (request.method() === 'POST') {
        state.enqueued.push(request.postDataJSON());
        state.listed = true;
        return json(route, 201, { job: state.job, replayed: false });
      }
      return json(route, 200, { jobs: state.listed ? [state.job] : [] });
    },
  );
}

/**
 * A build asked in the tab shows its row and the head status, waits with the
 * notice that no execution worker is ready, then fails: the banner says so in
 * words and names the job, and the row lists the diagnostics as plain text
 * (decision 0058, R03ea). The server's answers are replaced in the browser.
 */
async function checkBuildIntercepted(world) {
  const artifact = await createArtifact(world.server, 'Jobs build');
  const id = `job_${'e'.repeat(32)}`;
  const base = {
    id,
    artifact_id: artifact.id,
    kind: 'run_profile',
    profile: PROFILE,
    revision: 1,
    attempt: 0,
    max_attempts: 3,
    cancel_requested: false,
    created_at_ms: Date.now(),
  };
  const state = {
    job: { ...base, state: 'queued' },
    status: { profile: PROFILE, head_revision: 1, status: 'none' },
    listed: false,
    enqueued: [],
  };
  const opened = await openJobs(world, artifact, (tab) =>
    interceptBuild(tab, artifact.id, state),
  );
  try {
    const banner = opened.panel
      .getByRole('status')
      .filter({ hasText: 'Revision 1, the newest' });
    await expect(banner).toContainText('not built', { timeout: WAIT_MS });
    await expect(banner).toContainText('No revision has a build yet.');
    // The committed press reads the status again, which is pending now.
    state.status = { ...state.status, status: 'pending', job_id: id };
    await opened.panel
      .getByRole('button', { name: 'Build revision 1', exact: true })
      .click();
    const row = jobRow(opened.panel, 'Build 1');
    await expect(row.getByText('Queued', { exact: true })).toBeVisible({
      timeout: WAIT_MS,
    });
    await expect(row).toContainText(`Profile ${PROFILE}`);
    await expect(banner).toContainText('build pending', { timeout: WAIT_MS });
    await expect(banner).toContainText(`Job ${id}.`);
    assert.deepEqual(state.enqueued, [
      { kind: 'run_profile', profile: PROFILE, revision: 1 },
    ]);
    // This server has no execution worker, so its real `GET /v1/background`
    // says none is ready once the build has been queued for 5 seconds.
    await expect(
      row.getByRole('alert').filter({ hasText: WORDS.noExecution }),
    ).toBeVisible({ timeout: WAIT_MS });

    const markup = '<b>bold</b>';
    state.job = {
      ...base,
      state: 'failed',
      attempt: 1,
      failure: 'build_failed',
      finished_at_ms: Date.now(),
      diagnostics: [
        {
          location: { path: 'src/App.tsx', line: 3, column: 7 },
          message: `Unexpected ${markup}`,
        },
      ],
    };
    state.status = {
      ...state.status,
      status: 'failed',
      job_id: id,
      failure: 'build_failed',
      diagnostics: state.job.diagnostics,
    };
    // A real job of the artifact sends a job_changed, which reads the list and
    // the status again.
    await enqueue(world.server, artifact.id);
    await expect(banner).toContainText('build failed', { timeout: FEED_MS });
    await expect(banner).toContainText(WORDS.buildFailed);
    await expect(banner).toContainText(`Job ${id}.`);
    await expect(banner).not.toContainText(markup);
    await expect(row.getByText('Failed', { exact: true })).toBeVisible({
      timeout: WAIT_MS,
    });
    const diagnostics = row.getByRole('list', { name: 'Diagnostics' });
    await expect(diagnostics.getByRole('listitem')).toHaveText([
      `src/App.tsx:3:7: Unexpected ${markup}`,
    ]);
    assert.equal(
      await diagnostics.locator('b, a').count(),
      0,
      'A diagnostic was rendered as markup.',
    );
    await world.checkAccessibility('jobs-build-failed', opened.tab);
    assertCalm(opened.log, world.browser, { refused: [], cuts: 0 });
  } finally {
    await opened.context.close();
  }
}

/** Adds revision 2 to an artifact made by `createArtifact`. */
async function reviseOnce(server, artifact) {
  await api(server, 'POST', `/v1/artifacts/${artifact.id}/revisions`, {
    base_revision: 1,
    changes: [
      {
        op: 'put',
        path: 'index.html',
        encoding: 'utf8',
        content: `<p>${artifact.title}, revision 2</p>`,
      },
    ],
  });
}

/** Chooses "Build" in the preview, after choosing `revision` when given. */
async function showBuild(panel, revision) {
  if (revision !== undefined)
    await panel
      .getByLabel('Revision', { exact: true })
      .selectOption(String(revision));
  await panel.getByLabel('Preview of', { exact: true }).selectOption('build');
}

/**
 * Answers the build choice's reads in the browser: the head status from
 * `state.status`, a ticket for each revision in `state.tickets` (a revision
 * missing from it is `not_found`), and the build page each ticket names. The
 * revisions asked for are recorded in `state.asked`.
 */
async function interceptBuildPreview(tab, server, artifactId, state) {
  const json = (route, status, body) =>
    route.fulfill({
      status,
      contentType: 'application/json',
      body: JSON.stringify(body),
    });
  const ticketPath = new RegExp(
    `^/v1/artifacts/${artifactId}/revisions/(\\d+)/builds/${PROFILE}/previews$`,
    'u',
  );
  await tab.route(
    (url) =>
      url.pathname === `/v1/artifacts/${artifactId}/builds/${PROFILE}` ||
      ticketPath.test(url.pathname),
    async (route) => {
      const { pathname } = new URL(route.request().url());
      const ticket = ticketPath.exec(pathname);
      if (ticket === null) return json(route, 200, state.status);
      const revision = Number(ticket[1]);
      state.asked.push(revision);
      if (!state.tickets.includes(revision))
        return json(route, 404, {
          error: { code: 'not_found', message: 'No build.' },
        });
      return json(route, 201, {
        url: `${server.content_origin}/b/${'f'.repeat(31)}${revision}/`,
        revision,
        expires_at_ms: Date.now() + 60_000,
      });
    },
  );
  await tab.route(
    (url) =>
      url.origin === new URL(server.content_origin).origin &&
      url.pathname.startsWith('/b/'),
    (route) =>
      route.fulfill({
        status: 200,
        contentType: 'text/html',
        body: '<!doctype html><title>Build</title><p>The intercepted build</p>',
      }),
  );
}

/**
 * On this release build the preview's build choice says that the server
 * cannot build react-v1 and asks for no ticket (decision 0058, R03eb).
 */
async function checkPreviewBuildUnknown(world) {
  const artifact = await createArtifact(world.server, 'Preview build unknown');
  const opened = await openJobs(world, artifact, undefined, 'Preview');
  try {
    await showBuild(opened.panel);
    await expect(
      opened.panel
        .getByRole('status')
        .filter({ hasText: WORDS.profileUnknown }),
    ).toBeVisible({ timeout: WAIT_MS });
    await expect(opened.panel.locator('iframe')).toHaveCount(0);
    assert.deepEqual(opened.log.tickets, [], 'A build ticket was asked for.');
    assert.equal(opened.log.statusReads.length, 1, 'The status reads.');
    assertCalm(opened.log, world.browser, { refused: [], cuts: 0 });
  } finally {
    await opened.context.close();
  }
}

/**
 * The newest revision has no build, so the build choice shows the latest one
 * with a notice that names it and the head's status, and asks for no ticket of
 * the head (rule 15).
 */
async function checkPreviewBuildNotice(world) {
  const artifact = await createArtifact(world.server, 'Preview build notice');
  await reviseOnce(world.server, artifact);
  const state = {
    status: {
      profile: PROFILE,
      head_revision: 2,
      status: 'pending',
      job_id: `job_${'e'.repeat(32)}`,
      latest_build_revision: 1,
    },
    tickets: [1],
    asked: [],
  };
  const opened = await openJobs(
    world,
    artifact,
    (tab) => interceptBuildPreview(tab, world.server, artifact.id, state),
    'Preview',
  );
  try {
    await showBuild(opened.panel);
    await expect(
      opened.panel.getByRole('status').filter({ hasText: WORDS.previewNotice }),
    ).toBeVisible({ timeout: WAIT_MS });
    await expect(
      opened.panel
        .frameLocator('iframe[title^="Build preview of"]')
        .getByText('The intercepted build'),
    ).toBeVisible({ timeout: WAIT_MS });
    assert.deepEqual(state.asked, [1], 'The tickets asked for.');
    await world.checkAccessibility('preview-build-notice', opened.tab);
    // The head is built now. A real job of the artifact sends job_changed
    // changes, which read the status again, so the notice goes and the head's
    // own build is shown.
    state.status = {
      profile: PROFILE,
      head_revision: 2,
      status: 'built',
      latest_build_revision: 2,
    };
    state.tickets = [1, 2];
    await enqueue(world.server, artifact.id);
    await expect(
      opened.panel.getByRole('status').filter({ hasText: WORDS.previewNotice }),
    ).toBeHidden({ timeout: FEED_MS });
    await expect.poll(() => state.asked, { timeout: WAIT_MS }).toEqual([1, 2]);
    assertCalm(opened.log, world.browser, { refused: [], cuts: 0 });
  } finally {
    await opened.context.close();
  }
}

/**
 * An older revision without a build is said to have none, and no newer
 * build is shown in its place (the R03e split note).
 */
async function checkPreviewBuildMissing(world) {
  const artifact = await createArtifact(world.server, 'Preview build missing');
  await reviseOnce(world.server, artifact);
  const state = {
    status: {
      profile: PROFILE,
      head_revision: 2,
      status: 'built',
      latest_build_revision: 2,
    },
    tickets: [2],
    asked: [],
  };
  const opened = await openJobs(
    world,
    artifact,
    (tab) => interceptBuildPreview(tab, world.server, artifact.id, state),
    'Preview',
  );
  try {
    await showBuild(opened.panel, 1);
    await expect(
      opened.panel
        .getByRole('status')
        .filter({ hasText: WORDS.previewMissing }),
    ).toBeVisible({ timeout: WAIT_MS });
    await expect(opened.panel.locator('iframe')).toHaveCount(0);
    assert.deepEqual(state.asked, [1], 'The tickets asked for.');
    assertCalm(opened.log, world.browser, { refused: ['404'], cuts: 0 });
  } finally {
    await opened.context.close();
  }
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
async function damage(dataDir, artifactId, content) {
  const altered = Buffer.from(content);
  altered[0] ^= 0x01;
  // The server's database file (crates/artifree-providers/src/storage.rs). Opening
  // a missing file would create an empty database, so it must exist already.
  const file = join(dataDir, 'artifree.sqlite3');
  assert.ok(existsSync(file), `No database at ${file}.`);
  // node:sqlite needs --experimental-sqlite on the oldest Node this
  // repository supports (22.12) and takes the flag on later ones, so the
  // write runs in a child that is given it.
  const { stdout } = await promisify(execFile)(
    process.execPath,
    [
      '--experimental-sqlite',
      '--no-warnings',
      '--input-type=module',
      '--eval',
      DAMAGE_SCRIPT,
      file,
      altered.toString('base64'),
      artifactId,
    ],
    { timeout: 30_000 },
  );
  assert.equal(stdout, '1', 'The damaged file is not in the database.');
}

/** Replaces the stored bytes of revision 1's index.html; prints the row count. */
const DAMAGE_SCRIPT = `
import { DatabaseSync } from 'node:sqlite';
const [file, content, artifactId] = process.argv.slice(1);
const database = new DatabaseSync(file);
try {
  database.exec('PRAGMA busy_timeout = 5000');
  const { changes } = database
    .prepare(
      \`UPDATE blobs SET content = ?
       WHERE digest = (SELECT digest FROM revision_files
                       WHERE artifact_id = ? AND revision = 1 AND path = ?)\`,
    )
    .run(Buffer.from(content, 'base64'), artifactId, 'index.html');
  process.stdout.write(String(changes));
} finally {
  database.close();
}
`;

/**
 * Connects a new page of its own to the server and opens the artifact's Jobs
 * tab, or the view `view`. `prepare`, when given, sets up the page before it
 * loads. The caller closes the returned context.
 */
async function openJobs(world, artifact, prepare, view = 'Jobs') {
  const context = await world.browser.newContext();
  try {
    const tab = await context.newPage();
    tab.setDefaultTimeout(WAIT_MS);
    await prepare?.(tab);
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
    const panel = workspace.getByRole('tabpanel', {
      name: view,
      exact: true,
    });
    // The page may still be settling the search and the selection when the
    // heading shows; a detail view mounted again after the click opens on
    // Preview. One full Chromium run lost the click that way, so the tab is
    // pressed until its panel shows.
    await expect(async () => {
      await workspace
        .getByRole('tablist', { name: 'Artifact views' })
        .getByRole('tab', { name: view, exact: true })
        .click();
      await expect(panel).toBeVisible({ timeout: 2_000 });
    }).toPass({ timeout: WAIT_MS });
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
 * messages. A 409, or a 422 of an enqueue, the API answers is a refusal the
 * check provokes, and a connection reset is an answer the check cuts; both are
 * counted. The 422 of each read of the head build status is counted apart, and
 * every request for that status is recorded in both engines. What the
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
    statusRefused: [],
    statusReads: [],
    tickets: [],
  };
  tab.on('pageerror', (error) => log.errors.push(error.message));
  tab.on('console', (message) => {
    if (message.type() !== 'error' && message.type() !== 'warning') return;
    const text = message.text();
    if (message.location().url.startsWith(contentOrigin)) {
      log.preview.push(text);
      return;
    }
    const where = message.location().url;
    const fromApi = where.startsWith(`${origin}/v1/`);
    const status = text.match(/status of (\d{3}) /u);
    // A release build answers each read of the head build status with 422
    // profile_unknown (decision 0058, R03ea); those are counted apart.
    if (
      fromApi &&
      status?.[1] === '422' &&
      new URL(where).pathname.endsWith(`/builds/${PROFILE}`)
    )
      log.statusRefused.push(status[1]);
    else if (
      fromApi &&
      (status?.[1] === '409' ||
        (status?.[1] === '404' &&
          new URL(where).pathname.endsWith(`/builds/${PROFILE}/previews`)) ||
        (status?.[1] === '422' &&
          new URL(where).pathname === `/v1/artifacts/${artifactId}/jobs`))
    )
      log.refused.push(status[1]);
    else if (fromApi && text.includes('ERR_CONNECTION_RESET'))
      log.cuts.push(text);
    else log.unexpected.push(text);
  });
  tab.on('request', (request) => {
    const { pathname } = new URL(request.url());
    if (pathname === `/v1/artifacts/${artifactId}/builds/${PROFILE}`)
      log.statusReads.push(request.method());
    if (pathname.endsWith(`/builds/${PROFILE}/previews`))
      log.tickets.push(pathname);
    if (request.method() !== 'POST') return;
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
