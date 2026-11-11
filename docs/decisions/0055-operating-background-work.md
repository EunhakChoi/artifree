# 0055: Operating background work

Date: 2026-11-11. Status: **accepted in part**. R01ba (rules 1 to 6) and
R01bb (rules 7 to 12) are implemented; the other slices are proposed (see the slice table).
Roadmap: R01b. Builds on [0006](0006-change-feed.md),
[0007](0007-live-web-workspace.md),
[0030](0030-persistent-jobs-and-worker.md),
[0040](0040-data-changes-and-member-listing.md) and
[0051](0051-roadmap-and-architecture-review.md). When its slices merge it
amends four statements of 0030: decision 13 (the worker gains the flag
`--supervised` and the output line `claimed`), decision 14 ("Nothing starts a
worker", "the container image runs `serve` only" and "every surface says
so"), decision 17 ("Job changes are not in the change feed" and "The web
workspace has no job panel") and the matching lines of its limits. Each slice
adds a dated note to 0030 for what it changes and corrects the texts that
repeat the old statement (rule 24). It changes no other rule of 0030: the job
kind, the states, the claim, the lease, the fence, the attempts, the result
and the cancel stay as they are.

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
The fourth is decided here and implemented with its first task (rule 20),
because a scheduler with nothing to run would be a placeholder.

## Decisions

> **Note of 2026-11-11, from the implementation of R01ba (#181).** The code
> settled three points, and this record now reads as follows. (a) This amends
> rule 3: once the server has begun to stop, neither a worker that exits nor
> a start that failed is followed by another start, although the pipe of rule
> 2 is not closed yet. The exit is still recorded as the last exit and is not
> counted as a restart. A signal sent to the whole process group (Ctrl+C in a
> terminal) reaches the worker too, and a new worker would take a job only to
> give it back. (b) This narrows rule 5: while the listeners finish their
> requests and the worker lives, the state stays `running`. It becomes
> `stopped` when the server is about to close the pipe, before the 5 seconds
> of rule 2, or as soon as the server begins to stop if no worker is alive.
> (c) Rule 1: `ARTIFREE_NO_WORKER` accepts `1` and `true`; `0`, `false` and an
> empty value change nothing; any other text is refused at start, because
> guessing would start a worker the operator tried to turn off. A value that
> is not UTF-8 reads as unset, as for every server variable. Also recorded:
> built on glibc 2.39 or newer, a binary that starts a process would record
> that version as required, so `crates/artifree-server/build.rs` keeps the
> Linux binary on the floor of
> [0037](0037-release-publishing-and-supply-chain.md).

### Starting and supervising a worker

1. **`serve` starts one worker.** A persistent server starts one child
   process, the same binary as `worker run --data-dir <dir> --supervised`,
   after its database is open and before it prints its ready line. The
   child is the server's own executable file. The work still runs in a
   separate process (0030, alternatives): the server only starts it, watches
   it and reads what it prints. `--no-worker` (environment
   `ARTIFREE_NO_WORKER=1`) starts none, and a one-shot server never starts
   one. Jobs then stay `queued`, as today. The container image starts the
   worker like any other `serve`. Two servers on one data directory each
   start their own worker, which rule 4 already allows.
2. **The worker ends with the server.** `--supervised` is a new flag of
   `worker run`: the worker treats the end of its standard input as a stop
   request, exactly as it treats a signal (0030, decision 13). The server
   holds the other end of that pipe and closes it when it shuts down, so the
   worker gives its job back and exits; if the server is killed, the
   operating system closes the pipe and the same happens. At shutdown the
   server first stops accepting requests, then closes the pipe, waits for
   the worker for at most 5 seconds and then kills it. A killed worker's job
   is taken over when its lease expires, as for any worker that died (0030,
   decision 8). No signal and no process group is used, so Linux and Windows
   behave alike. `--supervised` is meant for a parent process: run by hand,
   the worker stops at the terminal's end of input, and at once when its
   standard input is closed or empty. `--supervised` with `--once` is a
   usage error.
3. **A worker that exits is started again.** Every exit the server did not
   ask for counts, whatever its status, and so does a start that failed. The
   server starts a new worker 1 second after an exit, then 2, 4, 8 and so on
   up to 60 seconds. A worker that ran for 60 seconds resets the delay to
   1 second. The server does not give up: a worker that cannot start, or
   that exits at once (for example a database it refuses), ends up retried
   every 60 seconds and is reported (rule 5). A worker that exits because
   the server asked is not restarted. The delays and the 60 seconds are
   timers of the server's async runtime, which do not follow the wall clock
   and which a test advances without waiting. This is not a new clock: every
   time that is stored or reported (`at_ms` in rule 5, the schedule of rule 18) is read from `artifree_core::jobs::Clock`.
4. **Other workers are still allowed.** An operator may run `worker run`
   beside a server, with or without the server's own worker; the claim
   already keeps two workers from taking one job (0030, decision 8). The
   server knows only the worker it started.
5. **The server says what it runs.** `GET /v1/background` answers
   `{ "worker": { "mode", "state", "restarts", "last_exit", "dropped_lines" } }`.
   Every token the server accepts may read it: the administrator's and any
   token of an enabled user (0036). It describes the server and carries
   nothing of any user, and a user who cannot read it could not tell why a
   job waits. `mode` is `supervised` or `none`. `state` is `running` (a
   worker process exists), `restarting` (between an exit or a failed start
   and the next start) or `stopped` (mode `none`, or shutting down).
   `restarts` counts the start attempts after the first since the server
   started. `last_exit` is absent, or `{ "at_ms", "outcome" }` for the last
   exit or failed start, where `outcome` is `clean` (status 0), `failed`
   (any other status, or ended by a signal) or `not_started` (the process
   could not be started). `dropped_lines` counts the lines of rule 6 that
   were dropped. No output of the worker, path or process identifier is
   returned. `running` says that the process exists, not that it makes
   progress: a worker that hangs reads as `running` (consequences). A change
   of this answer is not in the change feed: a client reads it when it shows
   jobs and again when a job stays `queued` (rule 14).
6. **The worker's standard output is the server's.** The server reads the
   worker's JSON lines (0030, decision 13). A line it cannot parse, or one
   longer than 64 KiB, is counted and dropped, never logged as content; the
   worker's standard error goes to the server's standard error unchanged.
   The lines matter for one thing only: rule 10.

### Job changes in the change feed

7. **A change kind `job_changed`.** A transaction appends one row for each
   job it creates and for each job whose `state` or `cancel_requested` it
   changes, to the change feed of the actor that owns that job, in the same
   transaction. That is: accepted (`queued`), claimed (`running`, also when
   a worker takes over an expired lease), given back at a shutdown (`queued`
   again), a cancel requested for a running job, a retry scheduled, and each
   end (`succeeded`, `failed`, `cancelled`). One claim can end several jobs
   whose lease expired or whose cancel was pending, of several actors, and
   take another: it appends one row for each of them, each in its owner's
   feed. A heartbeat and a retried request that changed nothing append none.
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
   therefore treats three artifact changes as job changes too: `deleted`
   and `purged` make the artifact's jobs unreadable, and `recovered` makes
   them readable again in whatever state they reached meanwhile (rules 11
   and 15).
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
11. **Clients.** The contract (0.27.0; rule 5's answer is 0.26.0) adds the
    kind and the field. The TypeScript client parses it and offers
    `followJob(id, { signal })`, which resolves with the job when it has
    ended. It subscribes before it reads (0006): it opens the feed, waits
    for `ready` and only then reads the job, so no change falls between the
    read and the stream. It reads the job again on each `job_changed` for
    that job, on `recovered` for the job's artifact, on every later `ready`
    and on `reset`, and at least every 15 seconds while the feed is not
    connected. It rejects with the not-found error when a read answers
    `404`, and it reads at once on `deleted` or `purged` for the job's
    artifact, so a job that became unreadable ends the wait instead of
    holding it for ever; the caller may follow again after a recover. MCP
    and the CLI keep reading a job on request; `artifree
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
    list reads the first page again. It also reads the first page again on
    every `ready` after the first, on `reset` (the database may be another
    one) and on `recovered` for its artifact. While the change feed is not
    connected the tab reads the first page every 15 seconds. It uses the request queue
    of 0007 like every other read and write of the workspace, and it is
    loaded with the tab, not with the page.
16. **Not in this row.** A list of all jobs across artifacts, a job view for
    an artifact in the trash, and anything about builds (R03).

### Scheduled tasks

17. **A scheduled task is a named, trusted function of the server, and its
    schedule is a row.** Each task has a name from a closed list (lower case
    letters and `_`, at most 16), an interval and a row `(name,
next_run_at_ms, run, last_started_at_ms, last_ended_at_ms, last_outcome)`.
    A server that finds no row for a task inserts one with `run = 0` and
    `next_run_at_ms = now + interval`, so a new installation, an upgrade or a
    new task never runs in its first minute. A server takes a run in one
    immediate transaction: if `next_run_at_ms <= now`, it adds one to `run`,
    sets `last_started_at_ms = now` and `next_run_at_ms = now + interval`.
    When the run ends it writes `last_ended_at_ms` and `last_outcome` (`ok`
    or `failed`), only if `run` is still the one it took. Two servers on one
    data directory can therefore not take the same run, and a restart does
    not owe the runs it missed: a task that was due while the server was
    down runs once.
18. **Time comes from `artifree_core::jobs::Clock`.** A test moves that
    clock; no task reads the wall clock itself. The server looks for due
    tasks once a minute and not before it has been up for one minute. A
    clock that steps backwards delays a task until the clock catches up, as
    it delays a queued job (0030, limits).
19. **A run that was interrupted is not repeated.** A run is taken before
    its work, so a server that dies in the middle leaves a run that started
    and never ended. It is reported as `interrupted` (its start is later
    than the last end) and nothing takes it again; the next run comes one
    interval after it started. A task must therefore be written to act on
    what is due when it runs, not on a list an earlier run made, so that the
    next run does what the interrupted one left.
20. **What a task may do.** A task does one of two things: it enqueues a job
    through the queue, or it calls application operations that a request
    could have called. It calls each operation as the actor that owns what
    the operation changes, so the application's own owner check stays in
    force and a task reaches nothing its owner could not (0036): a task that
    purges the trash purges each artifact as that artifact's owner. Each
    operation gets its own request key, built by the server from a reserved
    prefix (`sch~`), the task's name, the run and the identifier of what it
    changes. The `~` is outside the grammar of a key a caller can send, as
    for the runtime's keys, so no caller can use or burn such a key, and one
    key is never used for two operations. A task has no write path of its
    own and never runs artifact code. `--no-schedule` (environment
    `ARTIFREE_NO_SCHEDULE=1`) runs none, and `GET /v1/background` gains
    `"tasks"`: each task's name, interval, next run, and the start, the end
    and the outcome (`ok`, `failed` or `interrupted`) of its last run, which
    again carry nothing of any user. **No task exists yet, and this row adds
    neither the table nor the timer.** They are added by the first row that
    brings a task (O05b's automatic purge, which also decides how old an
    artifact in the trash must be and what happens to those of a disabled
    user), and that row implements rules 17 to 20 and their checks (rule 25)
    as written.

### Acceptance

21. Worker supervision, against the real binary: a job asked of a default
    `serve` ends without anyone running `worker run`; a worker that is
    killed is started again and the job it held ends after its lease; a
    worker that exits with status 0 unasked is started again; the delays 1,
    2, 4 ... 60 seconds and their reset, with the runtime's time advanced by
    the test; a server that is stopped leaves no worker process, and a
    server that is killed leaves none either (Linux and Windows);
    `--no-worker` and a one-shot server start none; `--supervised` with
    `--once` is refused; `GET /v1/background` in each state and with each
    `outcome`, with the administrator's token, an agent token and a second
    user's token, refused without one, and nothing of the worker's output in
    it; the container check asks for a verification and sees it succeed.
22. Job changes: one row for each job a transaction creates or changes and
    none for a heartbeat or a replayed request; a claim that ends two jobs
    of two actors and takes a third appends three rows, each in its owner's
    feed and in no other; none while the artifact is in the trash and none
    from a purge; `job_id` required for the kind and refused for the others,
    in the schema, the server and the client; `followJob` resolves for a job
    that ends between its subscription and its first read, rejects after a
    trash and after a purge, and reads again after a `reset`; a change by
    the server's worker reaches an open stream in under 2 seconds and one by
    another worker within the 15 second reading; a frozen database of the
    previous schema upgrades. Each guard is proved by mutation.
23. The web, in Chromium and Firefox against a real server and worker: a
    verification asked in the tab ends and shows its verdict without a
    reload; a damaged file is shown as a finding; a running job is
    cancelled; with `--no-worker` the tab says that no worker is running;
    the list is read again after a `reset`; the tab's accessibility checks
    pass.
24. In every slice: `npm run check`, `npm run build`, no growth of the web
    entry bundle from the tab's code, a dated note in 0030 for each
    statement the slice amends, and no text left that repeats the old one.
    R01ba corrects "nothing starts a worker" in the README, the container
    guide (its "No worker" row), the backup guide, the MCP guide, the MCP
    server's description and the CLI's job help. R01bb corrects "job changes
    are not pushed" in the same places; MCP still reads a job on request.
25. Scheduled tasks, with the first task: a new row does not run before one
    interval; two stores on one database take a due run once; a task that
    was due several times while the server was down runs once; a run that
    never ended reads as `interrupted` and is not taken again, and the next
    run does what it left; an end written for an older run changes nothing;
    a key of rule 20 is refused when a caller sends it, and two operations
    of one run have different keys; an operation runs as the artifact's
    owner and another user's token cannot replay it; the clock is moved by
    the test; `--no-schedule` runs none; a frozen database of the previous
    schema upgrades.

### Slices

| Slice | Scope                                                           | Rules            | Contract | Status      |
| ----- | --------------------------------------------------------------- | ---------------- | -------- | ----------- |
| R01ba | The supervised worker and `GET /v1/background`                  | 1 to 6, 21, 24   | 0.26.0   | implemented |
| R01bb | `job_changed` in the store, the server, the contract and client | 7 to 12, 22, 24  | 0.27.0   | implemented |
| R01bc | The web Jobs tab                                                | 13 to 16, 23, 24 | none     | proposed    |
| O05b  | The first scheduled task, with the table and the timer          | 17 to 20, 25     | its own  | proposed    |

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
- **Taking a run after its work, so that an interrupted run is repeated.**
  Two servers would then both do the work before either took the run, and a
  task that always dies in the middle would run again every minute. Taking
  it first costs one interval of delay for what was left.
- **One request key for a whole run.** A key belongs to one request (0013):
  a second operation with the same key and another target is refused as a
  reused key. One key per operation keeps each replay exact.
- **A system actor for tasks.** It would need an identity that owns nothing
  and may change everything, which 0036 does not have. Acting as each owner
  uses the checks that exist.
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
- A client older than contract 0.27.0 reports `job_changed` as an unknown
  kind and goes on, as it does for any kind it does not know.
- `GET /v1/background` knows that a worker process exists, not that it
  works. A worker that hangs reads as `running` and its jobs wait for a
  lease nobody takes over; noticing that needs a sign of life from the
  worker, which this record does not add.
- Any enabled user can read whether the server runs a worker and when it
  last exited. Nothing of another user is in that answer.
- A worker that an operator runs by hand is not shown by `GET
/v1/background`, and its changes arrive up to 15 seconds late. The web's
  "no worker is running" can therefore be wrong for such an installation; it
  names the mode so the operator can tell.
- At-least-once execution is unchanged: a job can run again after its worker
  was killed, and still records one result.
