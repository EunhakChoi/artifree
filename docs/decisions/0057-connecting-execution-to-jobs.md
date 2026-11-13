# 0057: Connecting the Linux provider to jobs

Date: 2026-11-14. Status: **accepted as a plan; not implemented**. Roadmap:
R02b.

> Builds on [0042](0042-linux-untrusted-execution.md) (the boundary),
> [0056](0056-linux-provider-delivery.md) (the provider, R02aa to R02ai),
> [0030](0030-persistent-jobs-and-worker.md) (jobs and the worker),
> [0036](0036-multi-user-identity.md) (users and `state_generation`) and
> [0055](0055-operating-background-work.md) (the supervised worker).

## Context

R02a ends with a provider that launches the repository's fixture under every
limit of 0042 and proves it on a prepared runner, but only from its own tests.
The launch, the admission lock, the attempt registry and the staging exist only
in the test build (0056 rule 16), the input is a package handed in by the test,
and the heartbeat that renews the guardian is a stand-in function. 0042 and
0056 give R02b the rest:

- **0042 rule 3:** an attempt pins actor, artifact, revision, job, fence,
  profile, tool digest and the actor's `users.state_generation`, read in the
  claim's transaction and compared at readiness, at every heartbeat and at
  publication. A disable and re-enable between two checkpoints ends the
  attempt; nothing repins it.
- **0042 rule 6:** cleanup before reclaim and before a terminal state;
  `cancelled` only after confirmed cleanup; trash revokes a running attempt's
  publication fence in its own transaction; purge keeps a `cleanup_pending`
  tombstone until settlement; disable invalidates the pinned generation; the
  storage transitions, backup and restore invariants and the cross-references
  change together.
- **0042 rule 7:** the output manifest binds actor, generation, artifact,
  revision, profile and tool digest, job and fence, and the publishing
  transaction checks them again. A persisted receipt needs its schema, backup,
  restore and purge rules in the same change.
- **0042 rule 8:** fixture injection exists only in tests, never as a
  production subcommand, environment override, job kind or selector; R02b's
  tests run the real server binary and SQLite.
- **0042 rule 1:** the exact wire error vocabulary is assigned before a new
  kind is exposed.
- **0056 rules 4, 8, 15 and 16:** the server binary gains its execution entry
  point and knows no profile; how a worker runs inside a delegated unit; the
  input's origin, the rest of the manifest binding and the store-backed
  renewals; R02 stays partial until R02b passes.
- **0055:** R02b keeps a supervisor alive beside the work it guards.

The store today knows one trusted kind, `verify_revision`. A job row has a
fence but no pinned generation. Trash touches no job, purge cancels open jobs at
once (0030 rule 15), a claim reclaims an expired lease with no cleanup gate, and
a heartbeat answers only continue, cancel requested or lease lost. `serve`
starts its worker as a plain child process (0055), which shares the server's
group and so cannot be the only process of a delegated group (0056 rule 8).

## Decisions

### The kind and the profile

1. **One untrusted kind, `run_profile`, with a profile column.** The store gains
   the kind and a `profile` text column that is null for `verify_revision`.
   The column names a reviewed profile (0042 rule 2). The production build knows
   no profile, so every value is unknown there, and a claim of such a job ends
   it at once as a nonretryable `execution_unavailable` failure, without an
   attempt (0042 rule 1). The schema holds no profile name; the code validates
   the value.
2. **The fixture profile exists only in a test build.** A cargo feature
   `enforcement-fixture` of `artifree-providers` and `artifree-server` compiles
   the launch, the lock, the registry, the staging and the collection (today
   `cfg(test)`) and one profile, `fixture`. Without the feature, `fixture` is
   an unknown value like any other. The feature is enabled only by the
   enforcement job's build. `npm run build`, the release archives, the
   container image and the clean-install checks build without it. The
   boundary checker fails when any workflow, script or manifest outside the
   enforcement job and `scripts/enforcement/` names the feature, and a test of
   the release build shows its worker refusing a `fixture` row. This is a test
   build, not a production selector: no flag, variable or request of a
   release binary can reach the profile (0042 rule 8).
3. **No wire kind yet.** R02b adds no API, MCP tool, CLI command or contract
   for `run_profile`. Only tests enqueue it, through the core's enqueue with a
   new kind. Rows of the kind are left out of `GET /v1/jobs`, `GET
/v1/jobs/{id}` (`404`), the job change feed and the MCP and CLI job reads,
   and they count toward the actor's 16 open jobs. R03 exposes the first
   public kind and assigns its wire error vocabulary then (0042 rule 1).
4. **The server binary's entry point.** `artifree-server execution
bootstrap|stub|guardian` exists in every build. Without the feature it
   knows no profile and refuses with a typed message on stderr and exit
   status 2 (0056 rule 4). With the feature it serves the fixture profile, so
   the enforcement job's tool root carries the server binary in place of the
   test-support program.

### The store

5. **The pinned generation.** A claim of `run_profile` reads the actor's
   `state_generation` and enabled state in its own transaction and stores the
   generation on the attempt (rule 6). The owner, whose identity is immutable
   (0036), pins its generation like any user, and it can never change. A
   disabled actor's jobs are not claimed (as today); a job that never started
   claims under the generation it reads when it is due.
6. **The attempt row.** A table `execution_attempts` holds one row per claimed
   `run_profile` attempt: job id, fence, a random 128-bit attempt identity, the
   pinned generation, a state (`live`, `cleanup_pending`, `settled`), whether
   publication is revoked and why (`cancel`, `trash`, `purge`, `generation`,
   `authority`, `lease`, `shutdown`), and times. It holds no source, path,
   output, log, digest or credential. It has no foreign key to the artifact,
   so it outlives a purge. It is the trusted control state and the purge
   tombstone of 0042 rule 6.
7. **The checkpoint.** A heartbeat of `run_profile` answers `continue` only when
   the job is `running` with the attempt's fence, no cancel is requested, the
   attempt is `live` and not revoked, the artifact is neither trashed nor
   purged, the actor is enabled with the pinned generation and still has
   access. Otherwise it revokes publication in the same transaction, if it is
   not revoked yet, and answers the reason: `fence`, `cancelled`, `trashed`,
   `purged`, `generation` or `authority`. The provider's guardian link renews
   only after `continue` (0056, amendment of 2026-11-13, the guardian); the
   stand-in function of R02ah becomes a port that the store implements.
8. **Trash and recovery.** For `run_profile`, trash pauses queued and
   retry-scheduled jobs as today. In the same transaction it marks every live
   attempt of the artifact revoked (`trash`), requests cancel and raises the
   fence; it does not wait for the process. Recovery changes neither the
   attempt nor the fence; the job reaches `cancelled` after cleanup (rule 10),
   and the recovered artifact needs a fresh keyed request. `verify_revision`
   keeps 0030's continue-and-hide behavior.
9. **Disable.** Disable and enable already raise `state_generation` (0036). A
   live attempt whose pinned generation differs is refused at its next
   checkpoint (rule 7) even when the user is enabled again, and after cleanup
   its job is `cancelled`; a new keyed request starts a new job.
10. **Cleanup before reclaim and before a terminal state.** A `run_profile` job
    with an attempt that is not `settled` is not due: an expired lease does not
    make it claimable. A transition to `succeeded`, `failed`, `cancelled` or
    `retry_scheduled` of such a job requires its attempt to be `settled` in
    the same transaction. An attempt becomes `settled` only when the provider
    reports the group empty and its registry record removed: by the attempt's
    own worker at the end of the attempt, or by the reconciliation of a later
    worker under the deployment lock (0056, amendment of 2026-11-14). A
    cleanup that fails keeps the row `cleanup_pending` and the provider's
    quarantine; no store transition can settle it.
11. **Purge.** For `run_profile`, purge raises the fence, marks live attempts
    revoked (`purge`) and `cleanup_pending`, ends queued and retry-scheduled
    jobs as `cancelled` (they have no attempt), and erases what 0030 rule 15
    erases. A job with an unsettled attempt stays `running` until settlement,
    then becomes `cancelled`. Purge does not wait. The reconciler reads only
    `execution_attempts` and the job header, never the purged artifact.
    `verify_revision` keeps 0030 rule 15.
12. **Publication.** `complete` of `run_profile` runs the checks of rule 7 in
    its transaction and also requires the attempt `settled` with publication
    not revoked. It stores only the typed outcome and two counts, files and
    bytes of the admitted output; no digest, manifest or bytes are persisted.
    The output and its manifest live in the worker's memory and are dropped
    after the transaction. Durable build bytes, receipts and preview selection
    are R03's (0042 rule 7).
13. **Backup and restore.** A backup copies `execution_attempts` with the
    rest. A restore marks every `live` row `cleanup_pending` with publication
    revoked (`lease`), next to the existing lease expiry. It never adopts a
    newer generation. The reconciler then applies 0056's boot and machine
    checks: another machine quarantines until the operator acts (0042 rule
    6).
14. **The schema step.** One schema version adds the kind, the `profile`
    column and `execution_attempts`. The migration changes no existing row.
    0030 rule 15 and 0042 rule 6 each get a note that names this record.

### The worker

15. **A port, not a provider, in the core.** `artifree-ports` gains an
    `Execution` port whose launch takes the pinned job, a source of the pinned
    input and a checkpoint, and returns a typed outcome. The core's worker
    dispatches on the kind and stays free of Linux and of the provider
    (AGENTS.md). Without a configured execution, the worker never claims
    `run_profile`.
16. **The input's origin.** The worker reads the pinned revision's files through
    the store's existing revision read, as `verify_revision` does, checks every
    file's length and SHA-256 against the revision's listing, and hands them to
    the provider's `VerifiedInput` (0056, amendment of 2026-11-13, input and
    output). It never reads the head or follows a link.
17. **The full binding.** The provider's manifest binding adds actor,
    generation, artifact, revision, job, fence and attempt identity to the
    digests R02af binds.
18. **The untrusted worker runs in its own unit.** `serve`'s supervised worker
    (0055) stays a plain child and never claims `run_profile`. An operator
    runs `artifree-server worker run --data-dir <dir> --execution-dir <dir>`
    as its own unit with `Delegate=yes` under the worker's account. Its
    `--execution-dir` is the owner-only directory of the lock, the registry
    and the attempts (0056 rules 8 and 15). The unit's restart policy restarts
    it, and the guardian of each attempt is the supervisor beside the work
    (0055). Without `--execution-dir` a worker claims only trusted kinds; with
    it, a worker refuses to start outside a delegated group, naming the
    missing facility. Several such workers share the deployment lock.
19. **Startup.** Before its first claim, a worker with an execution directory
    takes the lock, reconciles, settles every attempt whose group it saw
    empty and whose record it removed, and leaves the rest
    `cleanup_pending` and quarantined.

### Slices

20. R02b is delivered in this order. Each slice passes `npm run check`,
    `npm run build` and the enforcement job, adds no wire contract, and
    states which rows of 0042's matrix it now proves.

| Slice | What it adds                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              | Proven where                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| ----- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| R02ba | The store of rules 1, 3 and 5 to 14: the kind and profile column (unknown profiles end as `execution_unavailable`), `execution_attempts`, the pinned generation, the checkpoint and its reasons, trash, recovery, disable, purge with the tombstone, cleanup before reclaim and before a terminal state, publication of counts only, restore, the schema step and the notes in 0030 and 0042. No execution: tests settle attempts through a test-only store call.                                                                                                                         | SQLite tests on any host: each transition, the trash-then-recover race at claim, heartbeat and publication, disable and re-enable before a heartbeat and before publication, a never-started job claiming under the new generation, the tombstone surviving disable, backup and restore, `verify_revision` unchanged, and mutations of the generation check, the trash revocation, the reclaim gate, the terminal gate and the purge tombstone. |
| R02bb | The worker of rules 2, 4 and 15 to 19: the feature and its boundary check, the entry point, the port and the dispatch, the input from the store, the full binding, the store as the checkpoint, `--execution-dir`, the delegated-group check and startup reconciliation.                                                                                                                                                                                                                                                                                                                  | The enforcement job with the server binary built with the feature and a real database: a success with the expected bytes and counts, cancel, lease loss, a stale fence at publication, a SIGTERM-ignoring descendant at cancel, the renewals of R02ah against the real store, and a release-build worker refusing a `fixture` row.                                                                                                              |
| R02bc | The recovery matrix of 0042 rule 8 for R02b against the real binary: a supervisor SIGKILL at each phase, then restart and cleanup before reclaim; two workers that never run an old and a new attempt together; purge racing a descendant that ignores SIGTERM, with the tombstone surviving disable and restore and one fresh attempt after settlement; disable and re-enable around a heartbeat and a publication, repeated across restart and restore; trash and recovery at claim, heartbeat and publication; queued and retry jobs resuming; a stuck cleanup keeping the quarantine. | The enforcement job, with mutations that remove the reclaim gate, the restore marking, the startup reconciliation and the checkpoint's generation check.                                                                                                                                                                                                                                                                                        |

21. **R02 after R02b.** When R02bc passes, the roadmap may call R02 implemented
    for the fixture profile only. The production build still knows no profile,
    so every caller outside the enforcement job gets `execution_unavailable`
    or `IncompleteEnforcement`, and no host is called supported until R03 adds
    a real profile and the operator's packaging (0056 rule 6). The x32 class
    of R02ad stays an owner decision.

### What a slice still has to settle

22. These points are assumptions the named slice must prove, and amend this
    record if they fail:
    - R02ba: that one schema step can add the attempt table without a rewrite
      of `jobs`, and that the existing restore hook can mark rows in the same
      transaction as the lease expiry.
    - R02bb: that the provider's test-only modules compile under the feature
      without `cfg(test)` helpers leaking into it, and that a worker started
      by `systemd-run` as the unit's command passes the delegated-group check
      while `serve` runs elsewhere.
    - R02bc: that a worker killed between a store transition and the
      provider's cleanup leaves only states that rule 10 can settle.

## Alternatives considered

- **A job kind `run_fixture`.** Rejected: 0042 rule 8 forbids a fixture job
  kind. The kind names what production will run, and only the profile set
  differs between builds.
- **An environment variable or a hidden flag that enables the fixture.**
  Rejected for the same rule: a release binary would carry the selector.
- **Keeping the provider's modules under `cfg(test)` and testing through
  the library only.** Rejected: 0042 rule 8 asks for the real server binary
  and SQLite.
- **Persisting the manifest or its digest with the job.** Deferred to R03:
  a digest of admitted bytes retains content in the sense of 0042 rule 6 and
  would need purge and restore rules that R03 writes together with the build
  bytes.
- **Letting `serve` start the untrusted worker in a transient unit.** Rejected
  for now: it needs a user service manager and permissions the server does not
  ask for. An operator unit is one file and needs nothing at run time.
- **Reusing 0030's reclaim and purge.** Rejected by 0042 rule 6: they act
  before cleanup is confirmed.

## Consequences

- R02b is three pull requests. The store slice runs everywhere and the two
  execution slices need the enforcement job.
- The store gains a table that a purge does not erase. It holds only control
  identities and state.
- A release binary carries the execution entry point and the dispatch, but
  no profile, so it can run nothing untrusted.
- Operators who want untrusted execution must run a second unit; R03's
  documentation describes it with the tool root's packaging.
