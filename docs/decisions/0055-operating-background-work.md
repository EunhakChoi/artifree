# 0055: Operating background work

Date: 2026-11-11. Status: **proposed**. Nothing below is implemented.
Roadmap: R01b. Builds on [0006](0006-change-feed.md),
[0007](0007-live-web-workspace.md),
[0030](0030-persistent-jobs-and-worker.md),
[0040](0040-data-changes-and-member-listing.md) and
[0051](0051-roadmap-and-architecture-review.md). When its slices merge it
amends three statements of 0030: decision 14 ("Nothing starts a worker"),
decision 17 ("Job changes are not in the change feed" and "The web workspace
has no job panel") and the matching lines of its limits. It changes no other
rule of 0030: the job kind, the states, the claim, the lease, the fence, the
attempts, the result and the cancel stay as they are.

## Context

0030 built the queue and a worker and deliberately stopped there: an operator
has to run `worker run` by hand, a client learns of a job's progress only by
polling, and the web shows no job at all. A user who asks for a verification
in a default installation therefore sees a job that stays `queued` for ever
and is not told why.

Three later rows need more than that. R03 has to show the state of a build
while it runs. R02b has to keep a supervisor alive beside the work it guards.
O05b's automatic purge has to run on a schedule without a request. 0051 asked
for one decision that all three share, so that none of them invents its own
timer, its own process handling or its own notification.

This record decides four things: who starts a worker and what happens when it
dies, how a client is told that a job changed, what the web shows, and the
rules a scheduled task follows. The first three are implemented by this row.
The fourth is decided here and implemented with its first task (rule 19),
because a scheduler with nothing to run would be a placeholder.

## Decisions

### Starting and supervising a worker

1. **`serve` starts one worker.** A persistent server starts one child
   process, the same binary as `worker run --data-dir <dir> --supervised`,
   after its database is open and before it prints its ready line. The work
   still runs in a separate process (0030, alternatives): the server only
   starts it, watches it and reads what it prints. `--no-worker` (environment
   `ARTIFREE_NO_WORKER=1`) starts none, and a one-shot server never starts
   one. Jobs then stay `queued`, as today.
2. **The worker ends with the server.** `--supervised` is a new flag of
   `worker run`: the worker treats the end of its standard input as a stop
   request, exactly as it treats a signal (0030, decision 13). The server
   holds the other end of that pipe and closes it when it shuts down, so the
   worker gives its job back and exits; if the server is killed, the
   operating system closes the pipe and the same happens. The server waits
   for the worker for at most 5 seconds at shutdown and then kills it. A
   killed worker's job is taken over when its lease expires, as for any
   worker that died (0030, decision 8). No signal and no process group is
   used, so Linux and Windows behave alike.
3. **A worker that exits is started again.** The server starts a new worker
   1 second after an exit, then 2, 4, 8 and so on up to 60 seconds, counted
   on a monotonic clock. A worker that ran for 60 seconds resets the delay to
   1 second. The server does not give up: a worker that cannot start (for
   example a database it refuses) is retried every 60 seconds and reported
   (rule 5). A worker that exits because the server asked is not restarted.
4. **Other workers are still allowed.** An operator may run `worker run`
   beside a server, with or without the server's own worker; the claim
   already keeps two workers from taking one job (0030, decision 8). The
   server knows only the worker it started.
5. **The server says what it runs.** `GET /v1/background` (any token of the
   owner, as for reading jobs) answers
   `{ "worker": { "mode", "state", "restarts", "last_exit" } }`. `mode` is
   `supervised` or `none`. `state` is `running`, `restarting` (between an
   exit and the next start) or `stopped` (mode `none`, or shutting down).
   `restarts` counts the starts after the first since the server started.
   `last_exit` is absent, or `{ "at_ms", "clean" }` for the last exit, where
   `clean` is whether the exit status was 0. No output of the worker, path
   or process identifier is returned. A change of this answer is not in the
   change feed: a client reads it when it shows jobs and again when a job
   stays `queued` (rule 14).
6. **The worker's standard output is the server's.** The server reads the
   worker's JSON lines (0030, decision 13). A line it cannot parse, or one
   longer than 64 KiB, is counted and dropped, never logged as content; the
   worker's standard error goes to the server's standard error unchanged.
   The lines matter for one thing only: rule 10.

### Job changes in the change feed

7. **A change kind `job_changed`.** Every commit that creates a job or
   changes its `state` or its `cancel_requested` appends one row to the
   change feed of the actor that owns the job, in the same transaction. That
   is: accepted (`queued`), claimed (`running`, also when a worker takes
   over an expired lease), given back at a shutdown (`queued` again), a
   cancel requested for a running job, a retry scheduled, and each end
   (`succeeded`, `failed`, `cancelled`). A heartbeat and a retried request
   that changed nothing append none.
8. **What the change carries.** The change has the fields of every change
   (0006) and `job_id`. `job_id` is present exactly for `job_changed`, as
   `data_version` is for `data_changed` (0040, rule 12): the published schema
   requires it for that kind and forbids it for every other, and the client
   treats a breach as an invalid event. `revision` and `metadata_revision`
   are the artifact's current ones, as for a data change; the revision a job
   is pinned to is read from the job. The change carries no state: a client
   reads the job (`GET /v1/jobs/{id}`), so it never shows a state from a
   change that a later one already replaced.
9. **The trash stays opaque.** A transition of a job whose artifact is in
   the trash appends no row, since nothing of that job is readable (0030,
   decision 4). A purge appends none for the jobs it ends: the `purged`
   change already says that everything of the artifact is gone. A client
   that sees `recovered` reads the artifact's jobs again.
10. **The feed is woken at once for the server's worker.** A commit by
    another process reaches an open stream at the stream's next reading of
    storage, up to 15 seconds later (0006). For the worker the server
    started, the server wakes its streams when it reads a line from the
    worker, so a transition reaches the client without that wait. The worker
    prints a line `claimed` with the job's identifier after each claim
    commits; its existing `job` line covers the ends, the retries and a job
    given back. A lost
    or unparsable line costs only the delay: the feed is read from storage.
    For a worker the server did not start, the delay stays.
11. **Clients.** The contract (0.26.0) adds the kind and the field. The
    TypeScript client parses it and offers `followJob(id, { signal })`, which
    resolves with the job when it has ended and rereads it on each
    `job_changed` for that job, and at least every 15 seconds while the feed
    is not connected. MCP and the CLI keep reading a job on request; `artifree
job show --wait` uses the same rule. The web's reducer leaves its
    artifact state as it is for a `job_changed` change, as it does for
    `data_changed` (0040).
12. **No schema for progress.** A job has no progress value and this record
    adds none. A job kind that needs one decides its own in its own record.

### The web job view

13. **A Jobs tab per artifact.** The artifact page gains a tab "Jobs" after
    "Assets". It lists the artifact's jobs newest first, 50 at a time with
    "Load more" (0030, decision 17): kind, pinned revision, state, attempts,
    the time it was asked for and, for an ended job, the time it ended. A
    succeeded job shows its verdict, the files and bytes checked, and its
    findings (path and problem; "and N more" when truncated). A failed job
    shows its failure in words. No file content is shown, since none exists
    in a job (0030, decision 11).
14. **Asking and cancelling.** "Verify revision N" asks for a
    `verify_revision` of the revision the page shows, with a new request
    key for each press; a press whose answer is lost is retried with the
    same key. "Cancel" is offered for a job that has not ended and says that
    cancelling stops work and undoes nothing (0030, decision 10). `409
too_many_jobs` and `409 job_ended` are shown as such. While a job of the
    list is `queued` for more than 5 seconds, the tab reads `GET
/v1/background` and, when no worker is running, says so in place: "No
    worker is running, so this job waits" with the mode and the state.
15. **Following.** The tab reads the list when it opens and reads one job
    again on each `job_changed` for it; a `job_changed` for a job it does not
    list reads the first page again. While the change feed is not connected
    the tab reads the first page every 15 seconds. It uses the request queue
    of 0007 like every other read and write of the workspace, and it is
    loaded with the tab, not with the page.
16. **Not in this row.** A list of all jobs across artifacts, a job view for
    an artifact in the trash, and anything about builds (R03).

### Scheduled tasks

17. **A scheduled task is a named, trusted function of the server, and its
    schedule is a row.** Each task has a name from a closed list, an
    interval and a row `(name, next_run_at_ms, run, last_started_at_ms,
last_ended_at_ms, last_outcome)`. A server takes a run in one immediate
    transaction: if `next_run_at_ms <= now`, it adds one to `run` and sets
    `next_run_at_ms = now + interval`. Two servers on one data directory can
    therefore not take the same run, and a restart neither repeats a run nor
    owes the runs it missed: a task that was due while the server was down
    runs once.
18. **Time comes from `artifree_core::jobs::Clock`.** A test moves that
    clock; no task reads the wall clock itself. The server looks for due
    tasks once a minute and not before it has been up for one minute. A
    clock that steps backwards delays a task until the clock catches up, as
    it delays a queued job (0030, limits).
19. **What a task may do.** A task does one of two things: it enqueues a job
    through the queue, or it calls an application operation that a request
    could have called, as the owner, with the request key
    `schedule:<name>:<run>`. A run that was interrupted is therefore safe to
    repeat, and a task has no write path of its own. A task never runs
    artifact code. `--no-schedule` (environment `ARTIFREE_NO_SCHEDULE=1`)
    runs none, and `GET /v1/background` gains `"tasks"`: each task's name,
    interval, next run, and the time and outcome (`ok` or `failed`) of its
    last run. **No task exists yet, and this row adds neither the table nor
    the timer.** They are added by the first row that brings a task
    (O05b's automatic purge, which also decides how old an artifact in the
    trash must be), and that row implements rules 17 to 19 as written.

### Acceptance

20. Worker supervision, against the real binary: a job asked of a default
    `serve` ends without anyone running `worker run`; a worker that is
    killed is started again and the job it held ends after its lease; the
    delays 1, 2, 4 ... 60 seconds and their reset, with an injected clock; a
    server that is stopped leaves no worker process, and a server that is
    killed leaves none either (Linux and Windows); `--no-worker` and a
    one-shot server start none; `GET /v1/background` in each state, with an
    agent token, and nothing of the worker's output in it.
21. Job changes: one row for each commit of rule 7 and none for a
    heartbeat or a replayed request; none while the artifact is
    in the trash and none from a purge; `job_id` required for the kind and
    refused for the others, in the schema, the server and the client; a
    change by the server's worker reaches an open stream in under 2 seconds
    and one by another worker within the 15 second reading; a frozen
    database of the previous schema upgrades. Each guard is proved by
    mutation.
22. The web, in Chromium and Firefox against a real server and worker: a
    verification asked in the tab ends and shows its verdict without a
    reload; a damaged file is shown as a finding; a running job is
    cancelled; with `--no-worker` the tab says that no worker is running;
    the tab's accessibility checks pass.
23. In every slice: `npm run check`, `npm run build`, and no growth of the
    web entry bundle from the tab's code.

### Slices

| Slice | Scope                                                           | Rules            | Status   |
| ----- | --------------------------------------------------------------- | ---------------- | -------- |
| R01ba | The supervised worker and `GET /v1/background`                  | 1 to 6, 20, 23   | proposed |
| R01bb | `job_changed` in the store, the server, the contract and client | 7 to 12, 21, 23  | proposed |
| R01bc | The web Jobs tab                                                | 13 to 16, 22, 23 | proposed |
| O05b  | The first scheduled task, with the table and the timer          | 17 to 19         | proposed |

## Alternatives considered

- **Running jobs in a task of the server.** It would need no process
  handling. 0030 rejected it because the process boundary is what R02
  builds on; that still holds, and a child process keeps a failing job from
  taking the server with it.
- **Leaving the worker to the operator and to the container image.** A
  service manager can restart a process. It does nothing for the user who
  runs one binary, which is the installation the project documents first,
  and it would leave "why is my job queued" to documentation.
- **Stopping the worker with a signal or a process group.** Windows has no
  equivalent that a child can handle. A closed pipe is the same on both and
  also covers a server that was killed.
- **Carrying the job's state in the change.** A client could skip one read.
  It would also show a state out of order when it reads changes late, and
  the feed would repeat what the job already says.
- **A stream per job.** It would spare the artifact feed some rows. Every
  client already follows the feed, with its cursor, its reconnection and its
  limits (0006); a second stream would copy all of that for a few rows per
  job.
- **Waking the feed through a database trigger or a file watch.** Both
  depend on the platform and on SQLite's journal. The worker's own line is
  already there and costs nothing when it is lost.
- **A scheduler now, with a task that does nothing.** It could be tested by
  itself. It would be exactly the placeholder the project's rules ask not
  to merge, and its rules can be checked once against a real task.
- **Scheduling by the process's uptime.** It needs no table. A server that
  restarts often would then never reach a daily task, or run it at every
  start.

## Consequences

- A default installation runs two processes. Its memory and its start time
  grow by one worker, and an operator who wants one process must say so.
- A job that is never given back produces at most eight feed rows (one
  acceptance, three claims, two retries, one cancel request, one end), and
  one more pair for each shutdown that gives it back. The feed is not
  trimmed today, so it grows with the number of jobs; a retention rule for
  the feed is not decided here.
- A client older than contract 0.26.0 reports `job_changed` as an unknown
  kind and goes on, as it does for any kind it does not know.
- A worker that an operator runs by hand is not shown by `GET
/v1/background`, and its changes arrive up to 15 seconds late. The web's
  "no worker is running" can therefore be wrong for such an installation; it
  names the mode so the operator can tell.
- At-least-once execution is unchanged: a job can run again after its worker
  was killed, and still records one result.
