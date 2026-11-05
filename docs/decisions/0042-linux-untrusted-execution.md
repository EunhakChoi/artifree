# 0042: Linux untrusted execution limits

Date: 2026-11-06. Status: **proposed; no implementation**. Roadmap: R02.
Builds on [0030](0030-persistent-jobs-and-worker.md),
[0036](0036-multi-user-identity.md), and
[0003](0003-local-html-artifact-slice.md).

## Context and evidence

The existing worker runs only trusted `verify_revision` code and opens SQLite
directly. A separate process alone does not protect that database or the host.
R02 must establish a real execution boundary before React builds (R03) or
untrusted conversion work can run. This record changes no executable, contract,
database schema, dependency, or current platform support.

Original-product observations distinguish requesting cancellation, physical
resource settlement, producing a file, and delivering it. They do not reveal a
server sandbox, queue, Linux backend, or universal resource limits. The policies
below are independent Artifree design. The private requirement/evidence trace
remains outside Git; public implementation and tests must be independently
written. No formal clean-room separation is claimed.

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
profile version and tool/dependency digest. Read input through existing access
and revision checks, verify every listed file's length/digest, and stage a fresh
bounded copy. Never follow a workspace symlink or read the mutable source head
as execution input. Deny execution after purge or revoked authority; a new kind
must explicitly define trash and disabled-user handling instead of inheriting
trusted verification's permission to finish.

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
uncancelled state and continuing authority; local liveness messages alone
cannot keep untrusted work alive. The guardian is outside the payload group,
receives only attempt control handles,
and never receives database credentials or source content.
The guardian independently enforces the absolute monotonic wall deadline set
at launch; no renewal, successful heartbeat or live supervisor extends it.

On cancel, timeout, output/log limit, OOM, policy failure, lease loss, purge,
disabled owner or graceful worker shutdown: stop publication, kill the entire
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
output, credentials or digests that retain content. Disabled owners and restore
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
actor, artifact, source revision, profile/tool digest, job and fence. Nonzero
exit, violated limits, failed cleanup or invalid output produces no usable
result. Tool exit zero alone is insufficient. Check authority, artifact state,
cancel status and fence again in the transaction that publishes a durable
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

| Boundary                 | Required actual rejection or recovery checks                                                                                                                                                        |
| ------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Host admission           | Windows rejection; disabled namespaces, absent controller/delegation, missing kill support, modified launcher/tool/policy: no payload starts; trusted verification still succeeds                   |
| Host files and processes | Host canary reads/writes, traversal, proc descriptors, mount changes, ptrace/signals, leaked FDs and executable configuration injection fail; host and another attempt remain intact                |
| Network                  | IPv4, IPv6, DNS, host loopback, metadata targets, pathname/abstract Unix sockets and inherited sockets cannot reach host/external listeners; independent listener counters stay zero                |
| Resources                | Fork/thread bomb, memory/OOM, scratch bytes/inodes, log flood, wall hang and CPU saturation stop or throttle under measured bounds; supervisor stays responsive                                     |
| Cleanup                  | Double fork, `setsid`, ignored SIGTERM, held pipes and background writer leave no survivors after normal exit, cancel, timeout, OOM, supervisor SIGKILL or lease loss; cleanup failure quarantines  |
| Recovery                 | Kill at registry/create/attach/readiness/execute/collect/commit phases; restart and concurrent workers cannot run an old and replacement attempt together; no partial output survives restore/purge |
| Output and authority     | Symlink/hardlink/FIFO/device trees, directory bomb, file mutation, malformed paths, oversized output, zero exit with invalid output, changed authority and stale fence publish nothing              |
| Success                  | Original synthetic bounded input produces expected bytes and manifest; a fresh operation succeeds after settled failure/cancellation; late revision result cannot replace the latest preview in R03 |

R02a must attempt `clone3` namespace creation, alternate ABI syscalls and
host-clock changes from the payload. Exercise a live supervisor that sends
local renewals after its database fence is revoked, and one that successfully
renews throughout the 120-second wall deadline: neither can keep the payload
alive. R02b must race purge with a descendant that ignores termination: purge
returns without waiting forever, all artifact/job reads become inaccessible,
the cleanup tombstone survives disablement and restore, and no replacement
attempt or usable output appears before confirmed group emptiness. A stuck
cleanup retains quarantine; completed cleanup permits one later fresh attempt
on an accessible artifact. Test these assertions after forced worker death too.

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

No implementation quality score is raised by a proposed document. The existing
design assessment remains 8.75/10, seven of eight diagnostics: placeholder
provider modules still need concrete behavior and a narrow interface. R02a
must hide real launch/cleanup policy in one concrete provider to address that
remaining diagnostic; a speculative wrapper would not do so.
