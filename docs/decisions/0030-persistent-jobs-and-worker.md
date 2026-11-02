# 0030: Persistent jobs and a worker process

Date: 2026-11-02. Status: accepted. Builds on
[0003](0003-local-html-artifact-slice.md) (request keys),
[0004](0004-inbound-mcp-adapter.md), [0013](0013-deleting-artifacts-and-the-trash.md),
[0023](0023-single-owner-token-management.md),
[0027](0027-backup-restore-and-schema-upgrade.md), and
[0029](0029-retention-and-cleanup.md) (decision 10). Roadmap row: R01.
Independent Artifree design: the private evidence index gives this row only a
source-pinned job and its failure, cancel, and recovery behavior as
constraints, and names the queue and the worker as Artifree's own decisions.
Nothing below is an observation of the original product.

## Context

Until now nothing ran in the background. Roadmap R01 asks for one real job
connected end to end: enqueue, claim, progress, completion, failure, retry,
and cancel, pinned to an actor, a source revision, and a request key, with a
worker that can die and a job that is taken over without writing its result
twice. R02 (limits on untrusted work), R03 (React builds), and P01 (model
sampling) will need exactly this machinery, and each needs it to be correct
before it adds a way to hurt the machine.

This record therefore builds the machinery around one job that is useful and
harmless, and states what it is not.

## Decisions

1. **One job kind, `verify_revision`, run by trusted code.** The worker
   re-reads every file of the pinned revision through the artifact store,
   recomputes its SHA-256 digest and size, and compares both with the stored
   listing. It reads only the database. **It is not a sandbox, it runs no
   artifact code, it uses no network, and it does not make R02, R03, or P01
   safer or closer: none of them is implemented.** The job is real because it
   can find real damage (a file whose stored bytes no longer match what the
   revision lists), not because it is a stand-in. `JobKind` is a closed
   vocabulary; a kind added later needs its own decision.
2. **A job pins its actor, artifact, revision, kind, and request key.**
   `POST /v1/artifacts/{id}/jobs` takes `{"kind": "verify_revision",
"revision": n}` and an `Idempotency-Key`, and answers `201` with the job,
   `replayed`, and `Location: /v1/jobs/{job id}`. None of the pinned fields
   changes. The fingerprint is over `enqueue_job`, the artifact, the revision,
   and the kind. **A retry returns the job as it is now**, with `replayed:
true`: a job moves on after the request that created it, so replaying
   the first answer would show a state that is no longer true. The same key
   with another request is `422 request_key_reused`.
3. **One key space per actor, across four kinds of request.** Keys of
   artifact requests (`request_keys`), cleanups (`cleanup_runs`, 0029), and
   job requests (`job_requests`, new) are one space: a key used by any of
   them cannot be used by another kind, in either direction, checked in the
   same transaction as the write. A refused request (unknown revision,
   limit, ended job) records no key, so the key stays usable for a request
   that works. Keys are kept for the life of the database like all keys (0029,
   decision 11), including those of purged artifacts' jobs.
4. **Access follows reads, and a job request of an artifact that is not
   readable is not found.** An artifact that is missing, in the trash,
   purged, or another actor's makes every job route `404 not_found`: enqueue,
   show, cancel, and the per-artifact list, **a retry included**. This differs
   from 0013, where a retry of a request that was already recorded still
   replays after the artifact went to the trash; here it is deliberate,
   because a job's result describes the artifact's files, and the files of an
   artifact in the trash are not readable by any other route either. A
   revision that does not exist (0 or beyond the head) is `404`; a malformed
   job or artifact identifier is `404` and is never echoed. The checks are
   made in this order: artifact in place, revision exists, key, per-actor
   limit, insert. A revision whose files were purged cannot be enqueued, by
   construction (the artifact is gone). **The trash is opaque to the queue
   too:** the jobs of an artifact in the trash are hidden, cannot be
   cancelled, are not counted toward the limit of decision 6, and are not
   claimed by a worker (decision 8). They stay as they were (a queued job stays
   queued; a running job whose lease expires is not taken over or ended) and
   are claimable again, oldest first, when the artifact is recovered; recovery
   loses no job. A recovery can leave an actor with more than 16 open jobs;
   the limit only gates new requests.
5. **Issued agent tokens may enqueue, read, list, and cancel.** An agent token
   authenticates as the same owner (0023). Purge and cleanup need the
   administrative credential because they cannot be undone (0029). A
   verification job is read-only on the artifact, bounded (at most 16 open
   jobs and 3 attempts), and stoppable, so it is allowed to agents over HTTP
   and MCP, and each new route is tested with an issued token. This is a
   product decision, not an oversight; if a later job kind has effects, its
   own record decides who may start it. The agent tokens and the owner are one
   actor here, so they share the limit of decision 6: an agent can enqueue
   into slots the owner freed by cancelling, and an owner cannot reserve room
   against an agent.
6. **A bound on open jobs.** An actor may have at most 16 jobs queued,
   running, or waiting for a retry (`MAX_ACTIVE_JOBS_PER_ACTOR`); the
   seventeenth is `409 too_many_jobs`. A retry of an accepted request needs no
   room. Only jobs of artifacts outside the trash are counted (decision 4):
   jobs nobody can see or cancel must not block new work elsewhere. Requests
   also pass the per-actor rate limiter that HTTP and MCP share (0005).
7. **The states.**

   | State             | Meaning                                                                                                |
   | ----------------- | ------------------------------------------------------------------------------------------------------ |
   | `queued`          | Waiting for a worker; claimable now.                                                                   |
   | `running`         | A worker holds the lease; `worker`, `lease_expires_at` are set.                                        |
   | `retry_scheduled` | An attempt failed in a retryable way; claimable at `retry_at_ms`.                                      |
   | `succeeded`       | A result was written. Terminal. A mismatch is a result: the job worked.                                |
   | `failed`          | No result: attempts ran out or the failure is not retryable. `failure` says why. Terminal.             |
   | `cancelled`       | Ended by a cancel, by a purge of its artifact, or by an expired lease with a cancel pending. Terminal. |

   Terminal rows never change again; the database refuses rows the machine
   cannot produce (a running job without a lease, an ended job without an end
   time, a result on a job that did not succeed, more attempts than allowed).

8. **Claim, lease, and fencing.** A worker claims the oldest due job in one
   immediate transaction: a `queued` or `retry_scheduled` job whose time has
   come, or a `running` job whose lease expired. A claim sets `running`,
   adds one to `attempts`, raises the **fence** (a separate counter that only
   rises on a claim and on a purge), and sets the lease to `now + lease`.
   The fence is the proof of ownership: `heartbeat`, `complete`, `fail`,
   `confirm_cancel`, and `release` each check `state = 'running' AND fence =
<the worker's>` in the same transaction as the write they make, and
   otherwise fail with `LeaseLost`. So a worker that stalled past its lease,
   was replaced, and wakes up cannot renew, finish, fail, or release the
   job, and cannot write a result: **at most one result is ever recorded,
   though a verification may run more than once** (at-least-once execution,
   one recorded result). Two connections, in one process or two, cannot claim
   the same job (tested with concurrent claimers on separate connections).
   An expired lease on the last attempt fails the job with `lease_expired`
   (no attempt is invented); an expired lease with a cancel pending ends it
   `cancelled`; any other expired lease is claimed again. Only jobs of an
   artifact outside the trash are due (decision 4). A claim returns the jobs
   it ended this way together with the job it claimed, and the worker reports
   each as a `job` event, so no job reaches an end unseen.
9. **Attempts and retry.** `max_attempts` is 3. A graceful shutdown is not
   an attempt: it gives the attempt back (`attempts` is reduced; the fence is
   not, so the released run still cannot write). Only `storage_unavailable`
   is retried: the worker schedules the next attempt after `1 s`, `2 s`, `4 s`
   ... up to `60 s` (`RetryPolicy`), computed from an injected clock, never
   the wall clock. A failure on the last attempt ends the job `failed` with
   its failure. The failure vocabulary is fixed and holds no free text:
   `revision_unavailable`, `storage_unavailable`, `lease_expired`, `internal`.
   Store errors are never copied into a job.
10. **Cancel stops work and rolls nothing back.** `POST /v1/jobs/{id}/cancel`
    takes `{}` and an `Idempotency-Key`. A job that has not started
    (`queued`, `retry_scheduled`) ends `cancelled` at once. A `running` job
    gets `cancel_requested: true` and stays `running`; the worker sees the
    flag at its next checkpoint and confirms, and the job ends `cancelled`. A
    result or a failure that arrives after the request also ends the job
    `cancelled`, and the result is dropped. A second cancel of a running job
    with another key is accepted and changes nothing. A job that already
    ended is `409 job_ended` and records no key. The verification does
    nothing outside its own record, so there is nothing to undo; the
    contract and the tool descriptions say that cancelling stops work and is
    never a rollback, so a later job kind with effects cannot inherit the
    impression that it is.
11. **The result.** A succeeded job carries `files_checked`, `bytes_checked`,
    `mismatch_count`, a `verdict` (`ok` or `mismatch`), and the first 50
    mismatches in path order (`MAX_FINDINGS`), each a package path and a
    problem: `digest_mismatch`, `size_mismatch`, or `content_missing`.
    `truncated` is true when `mismatch_count` exceeds the list. Paths are
    relative to the artifact; **no file content is ever stored in a job or
    returned**. Findings are rows in `job_findings`, written in the
    transaction that ends the job.
12. **Worker checkpoints.** The worker renews its lease and reads the cancel
    flag before the first file, then at most once per third of the lease, and
    once more before it writes the result. It checks the shutdown flag before
    every file. A file is read whole (at most 5 MiB), so a cancel or a
    shutdown takes effect between files, not inside one read. A renewal that
    fails because the store is unavailable is tried again at the next
    checkpoint. **The write of the result does not look at the clock:** it
    is accepted while the worker's fence is the job's current claim, even
    after the lease time passed (a late result with no takeover is recorded),
    and rejected once another worker claimed the job or a purge revoked it.
    The fence decides, not the time; a time check would only reject work that
    is correct and safe to record. The checkpoint before the result ignores a
    stop request: a finished verification is recorded first, and the worker then
    stops without starting another job. Only unfinished work is given back.
13. **The worker is a subcommand of the existing binary:** `artifree-server
worker run --data-dir <dir> [--once] [--poll-ms n] [--lease-secs n]
[--hold-file path]`. It opens the existing database directly through the
    same store layer as the server (WAL, busy timeout, full synchronous
    writes), not over HTTP, so it needs no credential and no listener, and
    the server need not be running. It never creates a database (a missing one
    is an error), and a database of a newer schema is refused as for every
    open. An empty file is not a database: it is refused and left as it was.
    `--once` stops when no job is due (a job waiting for a retry is not due).
    Defaults: poll 1 s (10 ms to 60 s), lease 30 s (1 s to 1 h). An idle poll
    only reads (a hint that something is due); the write lock is taken when a
    job is due, and the claim decides again inside its transaction, so the
    probe cannot cause a double claim.
    Arguments are parsed strictly (unknown, repeated, empty, or non-canonical
    numeric values are refused with `Unknown arguments.`, never echoing the
    value) and the worker reads no environment variable. stdout is JSON lines:
    `ready`, one `job` per job it ended or gave back (including a job the claim
    ended without running it: an expired lease on the last attempt, or with a
    cancel pending), `lease_lost`, `stopped`;
    diagnostics go to stderr. Exit 0 when drained or signalled, 1 on a fatal
    error (missing or refused database, five store failures in a row). The
    worker name is `worker-` and eight random hex digits; it is a label for
    operators, not a credential. **`--hold-file` is for tests**: after each
    claim the worker waits until the file exists, which pins a job at a known
    point so a test can stop the process there. The loop is synchronous; a
    small Tokio runtime exists only to wait for signals (handlers are
    installed before `ready` is printed, so a signal sent after that line is
    always handled). On Unix SIGINT and SIGTERM, on Windows Ctrl-C, set a
    flag; the worker then gives its job back at the next checkpoint and exits 0. A worker that is killed (or Windows `TerminateProcess`) keeps its lease
    until it expires, and another worker takes the job.
14. **Nothing starts a worker.** The server does not spawn or supervise one;
    the container image runs `serve` only. Until an operator runs `worker
run`, jobs stay `queued`, and every surface says so (the tool description,
    the CLI help, and `docs/`). A worker and a server may share a data
    directory on one machine; a database file on a network file system is not
    a supported place for either (SQLite's own limit).
15. **Purge, trash, backup.** Trashing an artifact does not stop a running job
    (it finishes and records its result, which is hidden by the `404` of
    decision 4 until the artifact is recovered; if its lease expires while the
    artifact is in the trash it is left alone, decision 4). Purging (0029) ends
    the artifact's open jobs as `cancelled`, raises their fence (a running
    worker's next call is `LeaseLost`; the state change alone already causes
    that, the fence is a second guard, and a test reads it), deletes every
    job's findings and result counts, and keeps the job headers and request
    keys, in the same transaction and with the same secure-delete as the rest
    of the purge; a retry of an enqueue after the purge is `404`. As for
    purged content (0029): the freed pages are overwritten, but a copy of an
    erased finding can remain in a write-ahead log that has not been
    checkpointed and truncated yet, and **in every earlier backup**, which
    still holds the findings and result counts of the job. A backup is a copy of the database, so jobs come
    along. **A restore sets every running job's lease to expired**
    (`expire_job_leases`), because a lease belongs to a worker on the machine
    that made the backup; the restored directory's worker runs the job again,
    with a new fence. Nothing of the old lease is resumed.
16. **Content cleanup (0029, decision 10).** The tables hold no content
    digest: a job reaches file content only through `revision_files`, which
    the unreferenced-content rule already counts, so a job pins nothing the
    rule would delete. A test that lists every binary column of the schema
    fails when a new one appears that is not in a reviewed list, so a later
    table that stores a digest cannot be forgotten.
17. **Reading, and no push.** `GET /v1/jobs/{id}`, `GET /v1/jobs` (the actor's
    jobs of artifacts outside the trash), and `GET /v1/artifacts/{id}/jobs`,
    newest first, with keyset pages (`limit` 1 to 100, default 50, and a
    `page_token` of the form `j1.<scope>.<created>.<id>` that is parsed
    strictly and belongs to the list it came from; another list's, a
    malformed, or a non-canonical token is `400`). **Job changes are not in
    the change feed** (0006): a client polls `GET /v1/jobs/{id}`. No new change
    kind exists, so the web reducer is unchanged. The web workspace has no job
    panel; none is claimed.
18. **Schema version 8** adds `jobs`, `job_requests`, and `job_findings`
    (frozen fixtures of versions 1 to 7 upgrade, a failed step rolls back, a
    restore of an older backup upgrades, and the structural comparison of
    0027 covers the new tables and their checks).
19. **Surface.** Contract version 0.8.0: `EnqueueJobRequest/Response`,
    `CancelJobRequest/Response`, `Job`, `JobList`, `ListJobsQuery`, the closed
    vocabularies `JobKind`, `JobState`, `JobFailure`, `JobProblem`,
    `JobVerdict`, the error codes `job_ended` and `too_many_jobs` (both
    `409`), MCP tools `enqueue_job`, `get_job`, `list_jobs`, `cancel_job`
    (a new effect `job`), client methods `enqueueJob`, `getJob`, `listJobs`,
    `cancelJob`, and the CLI `artifree job enqueue|show|list|cancel`. The
    client rejects a state, kind, failure, or problem it does not know as an
    invalid response instead of guessing. `worker run` exists in the Rust
    binary only.

## Limits and what is not implemented

- Untrusted code execution, resource limits, build workers, model providers,
  and any second job kind are not implemented (R02, R03, P01).
- Execution is at least once. A verification can run twice; the result is
  recorded once. A job kind with external effects needs its own idempotency
  story before it is added.
- Time is the machine's wall clock, read through one injectable seam. A clock
  that jumps can expire a lease early or late; the fence, not the clock,
  decides who may write. A clock that steps backwards delays queued jobs
  until it catches up (`run_after <= now`), and a retry scheduled before the
  step waits as long as the step.
- A single worker process runs one job at a time. Many workers on one machine
  are supported by the claim; a worker fleet over a shared database server is
  X04.
- A cancel or a shutdown is seen between files; one file read is not
  interruptible, and a stalled storage call is bounded only by the lease.
- No web job panel, no push for job changes, no automatic starting or
  supervision of a worker, no cleanup of old terminal jobs (they are small;
  decision 15 erases what a purge must erase), and no retention policy for
  job rows.

## Tests

`crates/artifree-providers/tests/jobs.rs` (store: enqueue, replay, key reuse
and the shared key space, claim, concurrent claimers, lease and heartbeat,
fencing against a stale worker, attempts, retry, every cancel path, release,
lists, trash, purge with a raw-file erasure check, row constraints, the
binary-column guard), `schema_upgrade.rs` and `backup_restore.rs` (version 7
fixture, failed step, jobs across a restore, expired leases),
`crates/artifree-server/tests/worker_jobs.rs` (the worker against a real store
with a fake clock and fault injection: damage kinds, bounds, backoff,
exhaustion, cancel at checkpoints, shutdown, lost lease, purge during a run),
`worker_process.rs` (the real binary: a killed worker, a terminated worker,
the hold file, bad usage; every wait is bounded and observes a condition),
`jobs_api.rs` and `mcp_adapter.rs` (routes, errors, agent tokens, 404 rules,
paging), and the TypeScript client, CLI, MCP, and server tests.

## Alternatives

- **A separate worker binary or an HTTP worker API.** Rejected: a second
  artifact to build and release, and a credential for the worker, for no
  benefit while the worker is trusted and local.
- **Leases without a fence (compare the worker name).** A worker that
  stalls, is replaced, and is claimed again under the same name would pass;
  the fence rises on every claim.
- **Exactly-once execution.** Not achievable across a crash for a general job;
  one recorded result is.
- **Putting job changes in the change feed.** It would add a change kind and
  a reducer in every client for a state that is polled once per job.
- **Treating a mismatch as a failed job.** A job that finds damage did its
  work; `failed` is for a job that could not.
- **Replaying a trashed artifact's job request (as 0013).** A job result
  describes files that are not otherwise readable in the trash.
- **Letting the server run the verification in a Tokio task.** It would
  remove the process boundary this row exists to establish for R02.

## Costs

A third process kind to operate, a schema step, three tables that a purge and
a restore must handle, a vocabulary clients must keep in step with, and a
promise (at most one result, at least one run) that later job kinds must
keep or explicitly break.
