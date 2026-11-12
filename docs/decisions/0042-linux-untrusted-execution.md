# 0042: Linux untrusted execution limits

> Browser engines: Firefox was suspended in CI from 2026-11-08 to 2026-11-11.
> A Firefox acceptance of this record that was met in Chromium only in that time
> was re-run by O08: the whole smoke suite passes in Firefox, which is back in
> CI ([quality](../quality.md#browser-engines)). An acceptance of a part that is
> not implemented has run in neither engine.

Date: 2026-11-06. Status: **proposed boundary; partial read-only inspection,
host admission and launches of a test fixture under resource limits
implemented; no untrusted execution outside the enforcement job's tests**. Roadmap: R02.

> Builds on [0030](0030-persistent-jobs-and-worker.md),
> [0036](0036-multi-user-identity.md), and
> [0003](0003-local-html-artifact-slice.md).

> **Note, 2026-11-12.** [0056](0056-linux-provider-delivery.md) decides how
> R02a is built and delivered (the provider's parts, the job its acceptance
> runs in, eight slices). It changes no rule of this record. Its first slice
> (R02aa) adds a host admission that runs no payload and still refuses every
> caller; a job on a prepared `ubuntu-24.04` runner checks it.

> **Note, 2026-11-12 (R02ab).** 0056's second slice launches the repository's
> fixture, which writes one line and exits, on the prepared runner: inside
> its own cgroup with the limits of rule 5, through the launcher, behind a
> bootstrap that sets the descriptor and core limits, leaves no capability,
> sets `no_new_privs` and installs a default-deny filter, and only after the
> provider read those facts from the kernel itself. The tool root is checked
> against a manifest of lengths and digests. Nothing outside the tests can
> start it, and every caller still gets `IncompleteEnforcement`. Of rule 8's
> matrix this proves, for host admission, a modified, extra or missing tool
> file; the rows for resources, files, network, cleanup under hostile
> descendants, recovery, output and the crash acceptance stay open.

> **Note, 2026-11-13 (R02ac).** 0056's third slice, after an amendment to
> its rules 2, 3 and 7, runs the fixture's resource and cleanup scenarios on
> the prepared runner: one bounded writable filesystem (rule 4), the task,
> memory, CPU, output and wall limits of rule 5, and no survivors after a
> normal exit or the deadline (rule 6). Of rule 8's matrix this proves the
> Resources row except the descriptor ceiling (R02ad), and the Cleanup row
> for normal exit and timeout; cancel, OOM cleanup records, supervisor
> death, lease loss and quarantine across processes stay open, as do the
> other rows.

> **Note, 2026-11-13 (R02ad).** 0056's fourth slice publishes the mapping
> that rule 4 and the acceptance section ask for,
> `docs/linux-syscall-mapping.md`: each restricted class, its probes, the
> kernel record that attributes each refusal to the filter, its control and
> its mutation, with the descriptor ceiling's checks. The x32 class fails
> acceptance on the reviewed kernel, which has no x32 entry point and so no
> control; R02 is not complete there until a kernel with x32 provides one
> or this decision is amended. The real-leak case of an inherited
> descriptor stays R02ae's.

## Context and evidence

The existing worker runs only trusted `verify_revision` code and opens SQLite
directly. A separate process alone does not protect that database or the host.
R02 must establish a real execution boundary before React builds (R03) or
untrusted conversion work can run. The initial specification changed no executable, contract, database schema,
dependency, or current platform support. The partial inspection below adds no
execution or platform support.

Original-product observations distinguish requesting cancellation, physical
resource settlement, producing a file, and delivering it. They do not reveal a
server sandbox, queue, Linux backend, or universal resource limits. The policies
below are independent Artifree design. The private requirement/evidence trace
remains outside Git; public implementation and tests must be independently
written. No formal clean-room separation is claimed.

## Implemented inspection only (partial R02a)

`artifree_providers::execution` reads the real Linux supervisor proc view's
`/proc/sys/kernel/core_pattern` with a 4,096-byte bound. It refuses pipe and
socket modes, unreadable values and unknown syntax. Its intentionally narrow
file syntax is a literal ASCII basename using letters, digits, `.`, `_`, `-`,
excluding `.` and `..`; one trailing proc newline is allowed. Directory paths
and percent expansions remain unreviewed. Even a matching file value always
ends in `IncompleteEnforcement`, never admission or supported-host status.
Windows and other non-Linux hosts are refused before reading the proc path.
Existing trusted worker/server paths do not call this module.

An immutable in-process `InspectionProfile` pins exact policy identity and
manifest bytes. That manifest explicitly says tools are unpackaged and
enforcement is incomplete. It is **not** an installed tool/dependency digest
manifest, kernel attestation or proof of immutable filesystem content. There is
no caller command, path, environment or executable selector. Tool-root digest
verification, holding a version against replacement, launcher packaging and
fixed executable profiles remain unimplemented.

An opaque routing observation records exact bytes and monotonic time. A
recheck refuses changed bytes, unreadable routing, backwards supplied monotonic
time and observations aged one second or more. One second is a conservative
inspection freshness bound, **not** a guardian interval, execution lease or
containment guarantee. Wall-clock changes do not participate. Production uses
`Instant`; synthetic clock values are confined to private test helpers.
These reads cannot detect change-and-revert or prevent an administrator changing
routing immediately afterwards. No filesystem TOCTOU protection, coordinated
host policy invariant, namespace identity attestation, launch recheck, guardian,
kill or publication fence is implemented by these observations.

Tests cover exact profile/manifest mismatch (including command/path-looking
strings), routing grammar/refusal, bounded reads, exact-byte change and
monotonic boundaries. A read-only unit test exercises the actual
proc read as a wiring check without assuming its value or proving that the
proc mount is genuine. Recheck freshness is measured after the read, including
time spent reading. Synthetic readers prove inspection logic
only, not kernel crash behavior. Dedicated-host execution, cgroup, core crash,
collector and complete rule 4–8 acceptance remain pending. R02a and R02 are
incomplete; no payload runs and no new job kind or API is exposed.

## Proposed rules

### 1. Supported execution host and availability

Untrusted execution first supports Linux hosts that satisfy every rule below.
Windows rejects untrusted execution; its existing server, storage, CLI and trusted
verification continue unchanged. Remote browser and CLI clients can use any
currently supported client OS to reach a qualifying Linux worker. This does not
promise a distributed queue: the worker still shares a local SQLite database
with the server under decision 0030.

Use a rootless Linux namespace launcher, initially Bubblewrap, plus delegated
cgroup v2 controls and a reviewed seccomp policy. Docker, a VM, a root worker,
and a setuid helper are not prerequisites. An administrator may provision a
private cgroup subtree using the host's normal service manager; Artifree never
changes global kernel settings, grants itself privileges, or silently enables
user namespaces. A service manager is provisioning infrastructure, not a new
public job interface.

Require a maintained Linux kernel with `cgroup.kill`, enabled
user/mount/PID/network/IPC/UTS namespaces, seccomp
filtering and usable delegated CPU, memory and process controllers. A release
number or the existence of `/sys/fs/cgroup` is insufficient. At worker startup
and before launch, verify actual namespace creation, controller writes/readback,
kill/empty observation, immutable tool identity and policy installation. A
supported host must also satisfy rule 5's core-dump admission policy; process
namespaces do not make a host crash collector private. A
distribution's LSM, container policy, WSL configuration or delegation may deny
them. Report the missing facility without launching any untrusted code. WSL and
container execution are unverified until their complete acceptance matrix passes.

Unsupported execution is a bounded, nonretryable availability failure for that
job kind; do not poison trusted verification or fail the entire API server.
R02b must assign its exact generated wire error vocabulary before exposing a new
kind. Never retry through a weaker backend or disable enforcement. Current
`verify_revision` semantics are unchanged.

### 2. One owner of the execution boundary

The trusted worker supervises an attempt; the untrusted process receives no
database access, bearer token, runtime grant, preview ticket or application port.
Keep the core synchronous and provider-independent. A concrete Linux provider
owns launch, containment, accounting, termination and collection together, hiding
namespace and cgroup details from HTTP, MCP, CLI and web. Do not introduce a new
crate or speculative generic runner API simply to mirror these steps.

Only reviewed, fixed operation profiles select an executable, arguments, entry
point, environment, dependency set and output root. Requests cannot supply a host
command, shell expression, launcher option, host path, environment override or
network permission. Artifact source remains hostile even when produced by an
agent. A new public job kind and its authorization require their own explicit
contract; this specification does not add one.

The host kernel, administrator and trusted worker are trusted. A malicious local
administrator, kernel exploit, hardware side channel, hostile same-UID host
process and physical host failure are outside this boundary. Run the worker
under an account that does not run unrelated untrusted host software. CPU and
memory limits reduce resource abuse; they do not promise absolute availability
against every kernel vulnerability. Existing iframe limits remain unchanged.

### 3. Immutable inputs and tools

An attempt pins actor, artifact, source revision, job ID, attempt/fence, operation
profile version, tool/dependency digest and the actor's enabled-state generation.
For a registered non-owner user, read `users.state_generation` and enabled state
in the same transaction that claims the attempt. Compare that pinned generation
at launch readiness, every store heartbeat/guardian renewal, and durable-result
publication, together with the current job fence and authority. A disable and
re-enable between checkpoints must invalidate the old attempt even though the
user is enabled again. Do not replace its pinned generation with the new one or
continue it through a retry. The owner uses decision 0036's immutable owner
identity; do not invent a disable operation for it. Read input through existing access
and revision checks, verify every listed file's length/digest, and stage a fresh
bounded copy. Never follow a workspace symlink or read the mutable source head
as execution input. Deny execution after purge or revoked authority. Rule 6
defines untrusted attempts' trash/disabled-user transitions; trusted verification
keeps its existing continue-and-hide behavior.

Use an operator-installed, versioned tool root with a complete digest manifest
and licenses. It includes the launcher policy identity, runtime executable,
loader/libraries and reviewed dependencies required by that profile. Verify it
before admission, hold the selected version for the attempt, and make it
non-writable by payloads. An update creates a new root; no in-place replacement
while referenced. Do not bind the host's whole `/usr`, home, checkout,
`node_modules`, package cache or configuration directories.

No runtime package download or lifecycle script runs in R02. R03 starts with
preinstalled fixed dependencies; later installation policies must meet this
boundary and require a separate specification if they introduce networking.
Do not discover executables through caller `PATH`, npm settings, project config,
preload variables or a shell. Launcher packaging and exact supported versions
must be pinned and tested in R02a before claiming a supported host.

### 4. Files, descriptors and network

Build a private mount tree with read-only source and tool roots, an empty home,
and one bounded writable temporary filesystem for scratch and output. No host
root bind, data directory, cgroup filesystem, devices with host access, user
runtime directory, desktop bus, SSH agent, Docker socket or package-manager
socket is exposed. Give the payload a fresh PID namespace and its own `/proc`;
avoid exposing host processes through alternate mounts. Mount propagation is
private. Source/scratch mounts are nodev and nosuid; source and scratch are
noexec where the profile permits (interpreted input is still untrusted code).

Clear the inherited environment and file descriptors. Only bounded stdout and
stderr pipes and an inert stdin are payload inputs; explicitly reject leaked
directory, network and control descriptors. Use no controlling terminal. Set
`no_new_privs`, drop all capabilities after setup and apply an architecture-aware
seccomp allowlist to all payload threads before execution. Deny namespace or
mount changes, privilege changes, tracing, kernel-module/BPF interfaces and
unneeded kernel APIs; profile-required thread/process creation must not create
new namespaces. Unknown syscall architectures or missing filters refuse launch.
Deny `clone3` unless a reviewed mechanism can enforce its pointed-to namespace
flags; a filter must not pretend to inspect memory arguments it cannot read.
Permit a profile's tested thread-creation fallback without relaxing namespace
denial.
Explicitly reject alternate syscall ABIs, including x32 on x86-64, and
unreviewed multiplexed or asynchronous syscall paths. Host-global changes
(including clock changes), keyrings and tracing must remain unavailable even
when a namespace would otherwise expose the relevant syscall.

Use a fresh network namespace with no host interfaces or routes. No inherited
network sockets, host resolver service or proxy is available. Deny external
IPv4/IPv6, DNS, raw/packet sockets, host loopback, metadata addresses and host
Unix sockets, including abstract sockets. Tests must prove these paths with
real listeners and observed absence of traffic, rather than trusting a
`fetch` failure. Local communication entirely inside an attempt can be allowed
by its fixed syscall profile. No cross-attempt listener or broker is exposed.

### 5. Resource profile and admission

The initial Linux fixture profile has these fixed upper bounds; they are
Artifree policy, not measured original-product limits or a React performance
promise. A later workload that needs different bounds first amends this record.

| Resource                     | Initial bound                                                     |
| ---------------------------- | ----------------------------------------------------------------- |
| Payload memory               | 1 GiB, group OOM termination; swap disabled                       |
| CPU bandwidth                | One CPU equivalent, 100 ms period                                 |
| Wall duration                | 120 seconds from admitted launcher start, monotonic clock         |
| Processes and threads        | 64 combined                                                       |
| Writable scratch and output  | One 128 MiB tmpfs, at most 4,096 inodes                           |
| Collected output             | 256 files, 5 MiB each, 25 MiB total, existing portable path rules |
| Captured stdout plus stderr  | 1 MiB combined; exceeding it terminates the attempt               |
| Descriptor count / core dump | 256 per process / zero core dump bytes                            |

Zero core bytes requires more than a soft resource limit. The first backend
admits only a reviewed kernel's ordinary file-mode `kernel.core_pattern`, with
both soft and hard `RLIMIT_CORE` set to zero before the launcher can execute
payload code. Keep those limits through `fork` and every `exec`, and deny
payload resource-limit setters (including `setrlimit`, `prlimit64` mutation and
ABI aliases) from altering them. A profile may permit read-only queries. Check
the effective limits at the final trusted bootstrap boundary, not only in the
worker's parent. No supported profile relies on an ELF constructor, injected
preload, language startup hook or pre-exec `PR_SET_DUMPABLE=0`: execution can
reset dumpability, and a crash can precede language startup.

Read the real host's routing policy using the trusted supervisor, outside the
payload's proc/mount view. Reject `|` pipe mode, `@` socket/request modes,
unreadable or unknown routing, unreviewed kernel behavior, and hosts whose
administrator cannot keep file-mode routing fixed for the entire admitted
attempt and cleanup. Do not interpret an inaccessible collector as disabled.
Kernel file-mode suppression must be verified for each admitted executable
format; arbitrary binary formats are not added by a fixture. Piped and socket
collectors run outside the payload's containment, and may receive memory even
when its core limit is zero. Do not use the kernel's internal pipe-recursion
sentinel as a portable security contract.

`core_pattern` is a trusted host-global configuration condition, not a setting
isolated by the namespaces used here. Artifree never writes it, stops a crash
service, installs a collector or requests privileged global changes. A host
already meeting the condition can qualify; another host gets the normal
nonretryable unsupported-execution failure, with trusted features unaffected.
The administrator must coordinate crash-policy changes by draining/stopping
untrusted execution first. Recheck routing before each launch and heartbeat;
on any change, stop admission and publication, kill/quarantine active attempts
under rule 6 and require operator reconciliation. Polling cannot close the
interval between a global policy change and a simultaneous crash or retract
data already delivered to a collector. An uncoordinated change violates the
trusted-host condition; do not claim its detection provides race-free
containment. If that invariant cannot be established, refuse untrusted work.

Apply cgroup `memory.max`, `memory.swap.max`, `memory.oom.group`, `cpu.max`
and `pids.max` before payload execution. A CPU quota controls bandwidth, not
total lifetime; the independent wall deadline is mandatory. Threads and
double-forked descendants share the attempt's resource budget. Keep the trusted
supervisor outside the payload group so it can terminate an OOM or fork bomb.

Allow only one untrusted attempt per worker deployment/data directory initially,
across worker processes, using a crash-released admission lock. Acquire capacity
before claiming an untrusted job; do not spend attempts while waiting. Continue
servicing trusted verification independently. Staging and collection are bounded
by package limits, with at most one package's bytes buffered at once. Input
staging and the writable tmpfs count toward a bounded deployment budget; do not
accumulate disk copies, logs or abandoned tmpfs mounts between attempts.

Drain both log pipes concurrently without an unbounded queue. Treat their
contents as untrusted text; do not interpret escape sequences or copy secrets,
host paths or free-form internal errors into public job failures. No persistent
public log endpoint is added here. Record typed outcomes and resource metrics;
a quota rejection must not be misreported as a syntax error or successful run.

### 6. Descendants, cancellation and crashes

Allocate a private attempt cgroup and ownership record before starting the
launcher. Attach only a trusted blocked bootstrap to that group, install all
limits, then release execution after namespace/policy readiness is confirmed.
There must be no interval where untrusted code runs outside its cgroup. Payloads
cannot migrate groups or access their control files. A process group, PID list,
`kill(parent)` or a signal handler alone is insufficient: descendants may
double-fork, call `setsid`, ignore signals or outlive their apparent parent.

Use launcher parent-death protection and a private PID-namespace init that
reaps descendants. The launcher/init relationship must actually kill the payload
if the trusted supervisor dies; flag names alone are not proof. Test SIGKILL at
each setup/handshake phase. Close setup descriptors before the payload starts.
The supervisor's monotonic deadline and a separate trusted attempt guardian
remain able to terminate the group when the payload hangs. The guardian watches
supervisor liveness and a bounded renewal interval no longer than one third of
the job lease; a lost supervisor or expired renewal kills the group. Renew the
guardian only after a successful store heartbeat confirms the current fence,
uncancelled state, pinned actor state generation and continuing authority; a
disable/re-enable epoch mismatch is a permanent stop for that attempt, not an
opportunity to repin it. Local liveness messages alone
cannot keep untrusted work alive. The guardian is outside the payload group,
receives only attempt control handles,
and never receives database credentials or source content.
The guardian independently enforces the absolute monotonic wall deadline set
at launch; no renewal, successful heartbeat or live supervisor extends it.

On cancel, timeout, output/log limit, OOM, policy failure, lease loss, trash, purge,
disabled actor or graceful worker shutdown: stop publication, kill the entire
cgroup and wait for `cgroup.events` to report `populated=0`; reap owned children
and unmount/release attempt resources. Cancellation acknowledgment remains
distinct from completed cleanup. For untrusted kinds, record terminal
`cancelled` only after cleanup is confirmed; a pending request is not rollback.
After expected trusted launcher/init teardown, normal exit also requires an
empty group before collection; untrusted background survivors
are killed and make the attempt fail rather than being silently accepted.

Use a five-second cleanup observation deadline, not a promise that SIGKILL can
interrupt every kernel wait. If emptiness cannot be confirmed, quarantine the
attempt, refuse further untrusted admission for that data directory and report
operator action required. Do not record successful cleanup, delete live
mounts/control records, reuse its directory, or retry. Trusted verification and
the API can remain available. A wedged guardian or supervisor is subject to the
same recovery checks; nobody may infer emptiness from lease expiration.

Persist a small trusted attempt registry under an owner-only execution directory
before launch: database identity, job ID/fence, random attempt identity, cgroup
name, resource ownership and boot identity, never file contents or credentials.
Validate names relative to pre-opened private roots; registry data cannot direct
cleanup at arbitrary host paths or unrelated cgroups. Serialize admission,
reconciliation and registry changes across workers with the same deployment
lock. A live attempt retains admission until empty; dead-owner records must be
reconciled before a new untrusted claim, including after reboot or restore.
Kill and confirm old groups before reclaiming expired work, remove abandoned
scratch only after that confirmation, and discard all partial output. A missing
group after a changed boot requires verified absence of its old mounts and
processes, not PID reuse heuristics. If the original machine is inaccessible
after restoring elsewhere, state that remote physical cleanup cannot be proved;
fencing is not a remote process kill. Such execution is outside the supported
single-machine recovery profile until reconciled by the operator.

Decision 0030's existing reclaim and cancel-on-completion paths cannot be reused
unchanged for these kinds: integrate cleanup-before-reclaim and
cleanup-before-terminal-transition in R02b. At-least-once execution remains;
only the current fence may record a result. Do not weaken trusted verification's
current state machine while adding this policy.

For untrusted kinds, trash pauses queued/retry-scheduled jobs that have no live
attempt, as decision 0030 does: they are hidden and unclaimable, and recovery
can make them due again. Trash cancels a claimed/running untrusted attempt.
In the same metadata transaction that moves the artifact to the trash, revoke
that attempt's publication fence and retain its cleanup obligation; do not wait
synchronously for process termination. Claim and trash transactions serialize,
so either trash prevents claim or it revokes the already claimed attempt before
further publication. Recovering the artifact never restores that fence, revives
the payload or automatically retries the cancelled attempt. The old attempt
reaches terminal `cancelled` only after confirmed cleanup; when recovered,
clients can see its eventual cancelled outcome and may explicitly request a
fresh keyed job. A background attempt's old result is never published after
recovery. Source content stays in trash until normal purge; no source is copied
into the cleanup record. Cleanup can run while the artifact is hidden, the user
is disabled or the artifact has already been purged. This kind-specific policy
does not change trusted verification's existing continue-and-hide behavior.

Disabling a user invalidates every running untrusted attempt's pinned enabled
epoch, even if re-enabled before a checkpoint. The next authoritative checkpoint
refuses renewal/publication and starts or confirms cleanup; the independently
bounded guardian ensures a stalled worker cannot retain its payload forever.
No claim is allowed while disabled. Jobs that have never started may remain
queued and first claim under the new enabled generation after re-enable, as
decision 0036 allows. A previously invalidated attempt cannot do so: after
cleanup it is cancelled, and a new keyed request is required. Persist the pinned
generation with trusted execution control state so restart/restore cannot
forget the mismatch. Recovery does not adopt a newer generation for an old
attempt. Current trusted jobs retain decisions 0030/0036 behavior unchanged.

In particular, decision 0030 rule 15 currently makes purge immediately cancel
open jobs. For new untrusted kinds, purge must instead revoke publication and
raise the fence in its normal transaction, erase source/results as before, and
retain a separate minimal `cleanup_pending` execution tombstone until physical
settlement is verified. The artifact and its jobs remain inaccessible; purge
does not synchronously wait for a stuck process. A reconciler can access the
cleanup tombstone without reading the purged artifact or authenticating as its
former owner. Only after emptiness is confirmed does it mark execution settled
and the untrusted job terminal. A failed cleanup keeps the tombstone and
quarantine; it cannot be erased by purge or by an actor's disablement. The
tombstone contains only trusted control identities and state, no source, logs,
output, credentials or digests that retain content. Disabled users and restore
retain the same cleanup obligation; they cannot turn a pending marker into
proof of settlement. R02b must update the kind-specific storage transitions,
backup/restore invariants and these ADR cross-references together. Existing
trusted verification still follows decision 0030 rule 15 unchanged.

### 7. Output admission and publication

R02a collects output in memory before releasing the private mount; no success
artifact is persisted yet. A trusted keeper outside the payload group retains
the bounded tmpfs mount until collection or rejection finishes. Keep a trusted,
close-on-exec directory handle for
the designated output root and prevent payload access to it. After the cgroup is
empty, traverse relative to that handle with no-follow, beneath-only resolution;
admit only regular files and bounded directories. Reject symlinks, magic links,
hard-linked files, devices, sockets, FIFOs, invalid/colliding paths, extra
unrecognized entries and all count/size limits. Bound traversal depth, entries
and bytes even for rejected trees. No archive extraction or recursive host-path
copying is part of collection. Test same-size replacement and symlink races;
re-reading bytes by an unchecked pathname is forbidden.

Calculate digests over admitted bytes, and bind the manifest to the pinned
actor, actor state generation, artifact, source revision, profile/tool digest,
job and fence. Nonzero
exit, violated limits, failed cleanup or invalid output produces no usable
result. Tool exit zero alone is insufficient. Check authority, artifact state,
cancel status, pinned enabled-state generation and fence again in the transaction that publishes a durable
receipt. If a newer revision is head, an older result may describe only its
pinned revision; it must never replace the newer revision's running preview.

R02b may persist a bounded manifest/receipt with the job, but must specify
schema, backup, restore, purge and digest-reference cleanup in that same change.
R03 owns durable build bytes and preview selection, including transactional
reference publication, orphan cleanup and preserving the last successful
preview. Do not introduce object storage or an alternative source write path.
Any source mutation still uses `ArtifactService`, base revision and actor-scoped
idempotency key. No cancelled, stale-fence, inaccessible or partially collected
result may become preview content.

### 8. Acceptance and delivery slices

R02s is this specification only. R02a implements one concrete Linux provider and
a fixed repository-owned adversarial fixture executable through an internal
integration test; no caller command API or public untrusted job is added.
Fixture injection exists only in tests, never as an undocumented production
subcommand, environment override, job kind or executable selector.
Its observable result is actual containment, bounded output collection and
cleanup under real OS enforcement. R02b then integrates that proven provider
with the job supervisor and recovery state machine using an internal fixed
fixture profile; its tests run the real server binary and SQLite. R03 first adds
the user-facing build job, immutable build output and preview contract. Do not
mark R02 complete until both implementation slices pass their matrix.

| Boundary                 | Required actual rejection or recovery checks                                                                                                                                                                  |
| ------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Host admission           | Windows rejection; disabled namespaces, absent controller/delegation, missing kill support, modified launcher/tool/policy: no payload starts; trusted verification still succeeds                             |
| Host files and processes | Host canary reads/writes, traversal, proc descriptors, mount changes, ptrace/signals, leaked FDs and executable configuration injection fail; host and another attempt remain intact                          |
| Network                  | IPv4, IPv6, DNS, host loopback, metadata targets, pathname/abstract Unix sockets and inherited sockets cannot reach host/external listeners; independent listener counters stay zero                          |
| Resources                | Fork/thread bomb, memory/OOM, scratch bytes/inodes, descriptor ceiling/inheritance/setter denial, log flood, wall hang and CPU saturation stop or throttle under measured bounds; supervisor stays responsive |
| Cleanup                  | Double fork, `setsid`, ignored SIGTERM, held pipes and background writer leave no survivors after normal exit, cancel, timeout, OOM, supervisor SIGKILL or lease loss; cleanup failure quarantines            |
| Recovery                 | Kill at registry/create/attach/readiness/execute/collect/commit phases; restart and concurrent workers cannot run an old and replacement attempt together; no partial output survives restore/purge           |
| Output and authority     | Symlink/hardlink/FIFO/device trees, directory bomb, file mutation, malformed paths, oversized output, zero exit with invalid output, changed authority and stale fence publish nothing                        |
| Success                  | Original synthetic bounded input produces expected bytes and manifest; a fresh operation succeeds after settled failure/cancellation; late revision result cannot replace the latest preview in R03           |

R02a must verify the descriptor ceiling independently of the log and output
limits. Observe effective soft and hard `RLIMIT_NOFILE=256` at the final trusted
bootstrap, then in forked children and after each tested exec path. Account for
stdio and any profile-owned descriptors: repeatedly open inert files and
exercise descriptor duplication until the ceiling is reached; the next
allocation must fail with the expected resource error, with no descriptor at
or above 256 and no supervisor failure. Check inherited descriptors too;
setting a limit does not close an already-open descriptor above it. Attempt
soft/hard increases and resource-limit mutations through native setters,
`prlimit64` for self and owned descendants, and alternate ABI entries. Assert
the denied operation and unchanged effective limits, including after exec.
Use a safe below-ceiling allocation as a positive control. Independently mutate
the initial limit guard with a bounded parent limit above 256 and require the
limit/ceiling assertion to fail. Mutate the setter guard separately and require
a harmless lower-and-restore of the soft limit within the hard bound to break
the setter-denial assertion. A kernel refusal to raise the hard limit alone
cannot prove that the setter filter works. Restore each guard and rerun the
normal inheritance and allocation checks.

R02a must maintain a public, independently written acceptance mapping from
every rule 4 syscall restriction to its fixed-profile policy entries, payload
probes, expected denial, independent observations and guard mutations. Cover
namespace and mount changes; privilege/credential/capability changes; tracing
and process interference; module and BPF interfaces; host-global state changes
including clocks; keyrings; and unreviewed multiplexed and asynchronous paths.
Include `clone3`, namespace flags in permitted process/thread creation,
unknown syscall architectures, and alternate ABIs (including x32 and compat
entries on x86-64). Enumerate profile-relevant syscall aliases and argument
forms rather than treating one representative syscall as proof for a class.
Exercise inherited enforcement across fork, exec and every payload thread;
verify the allowed thread-creation fallback and benign required syscalls still
work. Independently observe `no_new_privs` and empty capability sets at those
boundaries, and prove payload attempts cannot undo them. Verify default denial
for syscalls outside the reviewed allowlist, not just named dangerous entries.
Missing filters, unsupported policy installation and unknown architecture
admission must refuse before payload execution.

For each mapped class, establish that the fixture reached the intended syscall
and assert the policy's actual denial using independent trusted observation of
the installed filter and its enforcement. Include safe positive controls and
individually remove the relevant filter entry, architecture check or argument
guard in the disposable test copy; the corresponding enforcement assertion
must fail. An unrelated namespace, capability, LSM or kernel rejection must not
let a broken seccomp guard pass: distinguish that later rejection from the
filter's decision. `ENOSYS`, an unavailable ABI or an operation that already
fails without the filter is not positive enforcement evidence. A filter
inspection or decision test alone also cannot replace the real payload probe.
When safe controls or independent attribution cannot be provided, fail that
host's supported-profile acceptance and retain refusal; do not silently skip
the class or relax the policy. Probes and mutations that could affect global
state run only on the dedicated disposable host with synthetic targets and
operator preparation, never on the contributor host. No test may contact real
host services or modify the contributor's credentials, keyrings, clock, module
state or BPF state. Record the controls and observed assertion failures, then
restore the policy and pass the complete normal matrix.

Exercise a live supervisor that sends
local renewals after its database fence is revoked, and one that successfully
renews throughout the 120-second wall deadline: neither can keep the payload
alive. R02b must race purge with a descendant that ignores termination: purge
returns without waiting forever, all artifact/job reads become inaccessible,
the cleanup tombstone survives disablement and restore, and no replacement
attempt or usable output appears before confirmed group emptiness. A stuck
cleanup retains quarantine; completed cleanup permits one later fresh attempt
on an accessible artifact. Test these assertions after forced worker death too.

R02b must hold an attempt before a heartbeat and before result publication,
disable and re-enable its non-owner user, and prove that both paths reject the
old pinned `users.state_generation`, stop the guardian and discard output.
Re-enabling before the next poll must not conceal the transition. Show that a
never-started queued job can first claim under the new generation, while a
cancelled running attempt requires a new keyed request after cleanup. Repeat
the stale-generation case across worker restart/restore. Separately race
claim/launch, heartbeat and publication with trash followed immediately by
recovery; the old attempt cannot renew or publish, cleanup remains reachable,
and the recovered artifact requires a fresh job for that cancelled attempt.
Verify queued/retry waiting resumes and existing trusted continue-and-hide
behavior remains intact.

R02a's real crash acceptance runs only on a dedicated, safely isolated test host
with an operator-controlled crash policy, not on a contributor's ordinary host
or inside a namespace assumed to isolate global sysctls. In admitted file mode,
execute a fixture that holds only synthetic memory, forks, execs a second
fixture and then crashes through SIGSEGV and SIGABRT paths. Exercise attempts
to alter core limits and dumpability before the crash, including mutations
that remove limit enforcement. Assert effective soft/hard limits, expected
post-exec fixture execution, zero core files in scratch/output and host collector
locations, zero collector invocations/connections and completed descendant
cleanup. A reported signal or clean application log alone is not evidence of
suppression. A deliberately broken limit guard must make the test fail for the
unexpected core artifact; arrange a bounded nonzero parent core limit and a
controlled scratch-relative file pattern on that dedicated host so the mutation
cannot pass merely because its parent already disabled dumps. Restore it
afterwards.

On that dedicated host, use controlled pipe and socket policies with independent
collector counters to prove admission refuses before any payload runs and no
collector is contacted. Also test unreadable/unknown routing and changed policy
causing stop/quarantine. The contributor's actual host gets read-only routing
inspection and refusal tests only when its policy is unsafe; never install a
handler, rewrite sysctls or crash a payload there to obtain evidence. R02a must
document the test host's privileged preparation separately from the product's
rootless runtime requirements; missing safe-host crash evidence blocks the
supported-host claim rather than silently skipping that acceptance case.

No mocked kernel result establishes Linux enforcement. Kernel capability tests
must report the exact environment and fail the supported-host acceptance job
when required facilities are missing; ordinary unsupported-host tests separately
verify refusal. Record kernel, architecture, launcher/tool/policy digests,
delegation and effective limits, peak memory, cleanup observation and negative
listener evidence. Prove important guards by mutation with the expected
assertion failure, restore each mutation, and pass the normal checks.

Run the repository's complete `npm run check` and `npm run build` for every
slice; use the documented isolated verification workflow. Add dedicated Linux
enforcement CI without silently skipping it on restricted runners. Any new CI
job belongs in `Quality gate`. R02b needs real process/store integration tests;
R03's presentation changes additionally need Chromium and Firefox smoke checks.
The private trace links original evidence to public rules and implementation
tests without placing private paths, source or captures in Git.

## Platform rationale and references

These are mechanism references, not evidence that Artifree enforces this policy.

- [Linux cgroup v2 documentation](https://docs.kernel.org/admin-guide/cgroup-v2.html)
  defines delegation, resource controls, group kill and population observation.
  Neither controller availability nor sending a kill proves completed cleanup.
- [Bubblewrap documentation](https://github.com/containers/bubblewrap/blob/main/README.md)
  describes namespace construction and warns that callers own the security
  policy. The contributor test wrapper is for trusted repository checks; its
  broad cache/tool mounts are not a product sandbox profile.
- [Linux seccomp documentation](https://docs.kernel.org/userspace-api/seccomp_filter.html)
  explains inherited syscall filters and that filtering alone is not a sandbox.
- [Linux Landlock documentation](https://docs.kernel.org/userspace-api/landlock.html)
  describes ABI-dependent access restrictions. Landlock is not required by this
  first backend and is not used as a substitute for namespace/network isolation.
- [Linux 6.18 coredump implementation](https://github.com/torvalds/linux/blob/v6.18/fs/coredump.c)
  distinguishes file, pipe and socket routes: file mode checks the core limit,
  while pipe/socket routes can disregard it. This is kernel source evidence,
  not an Artifree crash experiment or a guarantee for an unreviewed kernel.
- [Linux 6.18 exec implementation](https://github.com/torvalds/linux/blob/v6.18/fs/exec.c)
  resets dumpability during execution. A pre-exec nondumpable flag is not the
  proposed cross-exec suppression boundary.
- [Linux core routing documentation](https://docs.kernel.org/admin-guide/sysctl/kernel.html#core-pattern)
  describes the host routing setting and pipe collector behavior. Product
  namespaces do not authorize changing that host policy.

No implementation quality score is raised by a proposed document. The existing
design assessment remains 8.75/10, seven of eight diagnostics: placeholder
provider modules still need concrete behavior and a narrow interface. R02a
must hide real launch/cleanup policy in one concrete provider to address that
remaining diagnostic; a speculative wrapper would not do so.
