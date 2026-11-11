# 0056: Delivering the Linux execution provider

Date: 2026-11-12. Status: **accepted as a plan; nothing in it is implemented**.
Roadmap: R02a. Builds on
[0042](0042-linux-untrusted-execution.md) and
[0055](0055-operating-background-work.md).

0042 states the boundary an untrusted attempt must meet. It names the
mechanisms (a rootless namespace launcher, delegated cgroup v2, seccomp) and
the acceptance matrix, and leaves open how the provider is built, where its
acceptance can run, and in which order. This record decides those three things
so that R02a can be delivered in slices that each prove something on a real
kernel. It changes no rule of 0042. Where 0042 says "must" this record says
how; where the two could be read differently, 0042 holds.

## Context

### What was measured

A temporary job, on a branch that was deleted afterwards and never merged, read
what GitHub's hosted runners offer (2026-11-11). It ran no payload.

| Fact                                         | `ubuntu-24.04`                                   | `ubuntu-22.04`         |
| -------------------------------------------- | ------------------------------------------------ | ---------------------- |
| Kernel                                       | 7.0 (azure)                                      | 6.8 (azure)            |
| Bubblewrap                                   | not installed; the package is 0.9.0              | not installed; 0.6.1   |
| Unprivileged user namespaces                 | refused by AppArmor (`bwrap` and `unshare` fail) | allowed (`bwrap` runs) |
| `kernel.core_pattern`                        | a pipe to `systemd-coredump`                     | the same               |
| cgroup v2 with `cgroup.kill`                 | yes                                              | yes                    |
| A user scope with memory and task limits     | works (`systemd-run --user --scope`)             | not measured           |
| A unit with `Delegate=yes` run as the runner | owned by the runner, five controllers            | not measured           |
| Moving a process into a subtree made by root | refused                                          | not measured           |
| `sudo`                                       | yes                                              | yes                    |
| `prlimit`, `setpriv`, `capsh`                | installed                                        | not measured           |

The contributor host used for this repository (WSL2, kernel 6.18, Bubblewrap
0.9.0) also routes cores to a pipe. Under 0042, rule 5, it is an unsupported
host, and so is an unprepared hosted runner. Neither may run a payload to
obtain evidence.

### What follows from it

- The acceptance of 0042 can run only on a host an operator prepared. A hosted
  runner is a disposable virtual machine on which the job is the operator: it
  has `sudo`, nothing else runs there, and it is destroyed after the job.
- A process cannot be moved into a cgroup subtree somebody else made for it.
  The worker has to be started inside the delegated unit and make its own
  groups below it.
- The first-party Rust of this repository forbids `unsafe`. Setting resource
  limits, installing a seccomp filter and mounting need system calls the
  standard library does not offer.

## Decisions

### The provider

1. **One provider, in `artifree-providers`.** The module `execution` grows from
   the inspection of today into the Linux provider. No new crate, no generic
   runner trait and no caller-supplied command are added (0042, rule 2). On a
   host that is not Linux the module compiles to the refusal it has today.
2. **The launcher is Bubblewrap, used as a program.** The provider starts
   `bwrap` from the tool root of rule 5 with a fixed argument list built from
   the profile: every namespace unshared, the parent-death kill, a new
   session, a cleared environment, read-only binds of the tool root and the
   staged input, one size-bound tmpfs, a private `/proc` and a minimal
   `/dev`, and further user namespaces disabled inside. Nothing a request
   carries reaches that list.
3. **A trusted bootstrap runs inside the namespaces before the payload.** It is
   a subcommand of the server binary, `artifree-server execution-bootstrap`,
   bound read-only into the sandbox, documented, and refused unless it runs as
   the first process of a fresh PID namespace with the handshake of rule 4. In
   this order it: sets the soft and hard resource limits of 0042, rule 5
   (descriptors, core size) and reads them back; drops what the launcher left
   of capabilities and sets `no_new_privs`; installs the seccomp filter of the profile; checks the
   limits and the filter once more; and replaces itself with the payload by
   `exec`, without a fork. A step that fails ends the attempt before any
   payload instruction runs. The bootstrap takes the profile's identity and
   nothing else; it does not run a command it is given.
4. **A handshake, so that nothing runs outside its group.** The provider makes
   the attempt's cgroup, writes its limits and reads them back, starts the
   launcher blocked on a descriptor, moves it into the group, confirms from
   `cgroup.procs` that the group holds it, and only then releases it (0042,
   rule 6). The bootstrap reports readiness on a pipe before it execs; the
   provider treats an attempt that never reported as never started.
5. **The tool root.** One directory per version holds the launcher, the server
   binary that provides the bootstrap, the payload's runtime and its libraries,
   with a manifest of every file's length and SHA-256 and the licences. The
   provider verifies the manifest before admission and binds that directory
   read-only. In R02a the only payload is the repository's fixture and the tool
   root is assembled by the test harness; packaging a tool root for operators
   is R03's, when a real runtime exists. Until then no host is called
   supported outside the enforcement job.
6. **Two dependencies, for the system calls.** The bootstrap and the provider
   use `rustix` (resource limits, mounts, `prctl`, descriptor handling) and
   `seccompiler` (compiling and installing the filter), both through their
   safe interfaces (versions 1.1 and 0.5 were current when this was written;
   the slice pins what it adds). First-party code stays free of `unsafe`. Each is added in
   the slice that first needs it, with its licence in the notices, its
   features reduced to what is used, and a note of the review. If a needed
   call has no safe interface in them, the slice stops and this record is
   amended; a first-party `unsafe` block is not an option a slice may take.
7. **The cgroup comes from the service manager.** The worker is started inside
   a unit the operator made with delegation (for systemd: `Delegate=yes`, run
   as the worker's account). At start the provider finds its own group, checks
   that it owns it and that memory, CPU and task control are delegated, moves
   itself into a child group `supervisor`, enables the controllers for the
   siblings, and makes each attempt's group beside it. It never writes outside
   its own subtree and never asks for privileges. Without such a unit the
   provider refuses, naming the missing facility (0042, rule 1). How `serve`
   starts a worker inside such a unit is R02b's.

### Where acceptance runs

8. **A job `Linux enforcement` on `ubuntu-24.04`,** part of `Quality gate`,
   never skipped and never allowed to fail. Its first step is the operator's
   preparation, kept in one script under `scripts/enforcement/` and documented
   as the privileged preparation 0042 asks to keep apart from the product's
   rootless requirements: install Bubblewrap at a pinned package version;
   allow the installed `bwrap` to create user namespaces by an AppArmor
   profile for that one program (the system-wide restriction stays on); set
   `kernel.core_pattern` to a plain file name; and run the tests inside a
   delegated unit as the runner's account.
9. **Refusal is tested before preparation, in the same job.** The unprepared
   runner routes cores to a pipe and refuses user namespaces. The job first
   runs the admission tests there and requires the refusal that names each
   missing facility, with no payload started; then it prepares the host and
   runs the matrix. A change of the runner image that removes one of these
   conditions fails the job instead of passing it silently.
10. **The hosted runner is the dedicated host of 0042's crash acceptance.** It
    is disposable, runs nothing else, and its crash policy is under the job's
    control. The controlled pipe and socket routes with their counters, and the
    deliberately weakened core limit, run there and nowhere else.
11. **Nothing of this runs on a contributor's host.** There the suite reads
    the routing policy, requires the refusal when the host is unsuitable, and
    runs no payload, as today. The local sandbox runner of this repository is
    a convenience for trusted checks and is not evidence for any rule.
12. **Each run records its environment** as 0042 asks: kernel, architecture,
    the launcher's version and digest, the policy's digest, the delegated
    controllers, the effective limits, the peak memory, the cleanup
    observation and the listener counters, as an artifact of the job.

### Slices

13. R02a is delivered in this order. Each slice passes `npm run check`,
    `npm run build` and the enforcement job, adds no job kind, no API and no
    caller command, and states in its PR which rows of 0042's matrix it now
    proves and which stay open.

| Slice | What it adds                                                                                                                                                                                                                    | 0042                          |
| ----- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------- |
| R02aa | The enforcement job with its preparation; the provider's admission: namespaces actually made, controllers written and read back, a group killed and seen empty, the routing policy; refusal on the unprepared host. No payload. | 1, 5 (routing), 8 (admission) |
| R02ab | Launch of the fixture by rules 2 to 5 of this record, without a filter yet: the group's limits, the wall deadline, the bounded logs, kill and the empty group. Resource and cleanup rows.                                       | 5, 6 (kill, empty), 8         |
| R02ac | The seccomp profile, the resource limits in the bootstrap, and the public mapping from each restriction to its entry, probe, observation and mutation.                                                                          | 4 (system calls), 5, 8        |
| R02ad | Files, descriptors and network: canaries, leaked descriptors, real listeners with counters.                                                                                                                                     | 4, 8                          |
| R02ae | Output collection in memory through a directory handle, with the hostile trees.                                                                                                                                                 | 7, 8                          |
| R02af | The guardian, death of the supervisor at each phase, the attempt registry and quarantine, and the crash acceptance.                                                                                                             | 5 (cores), 6, 8               |

14. **R02 is not complete after R02a.** R02b connects the provider to the job
    supervisor and the store (0042, rules 3 and 6). Until both pass, the
    roadmap says partial, the module says `IncompleteEnforcement` to every
    caller outside the tests, and no text claims a supported host.

### What a slice still has to settle

15. This record was written from the measurements above and from the
    documentation of the launcher and the two crates, not from a working
    launch. Three points are therefore assumptions that the named slice must
    prove on the runner, and amend this record if they fail:
    - R02aa: that an AppArmor profile for the one launcher program is enough
      for it to make user namespaces on `ubuntu-24.04` (rule 8).
    - R02ab: which of the launcher and the bootstrap applies the scratch's
      inode bound (0042, rule 5: 4,096 inodes); the launcher's tmpfs option
      takes a size only.
    - R02ac: that the two crates offer every call of rule 3 through a safe
      interface (rule 6).

## Alternatives considered

- **A first-party `unsafe` block around the few system calls.** Rejected: the
  repository forbids it, and a reviewed crate with a safe interface gives the
  same calls.
- **Shell utilities instead of a bootstrap** (`prlimit`, `setpriv`, a shell to
  pass descriptors). Rejected: each is another program in the tool root to pin
  and to trust, a shell is exactly what 0042 keeps out of the launch path, and
  none of them can install the filter after the limits are set.
- **Bubblewrap's own filter option alone, without a bootstrap.** Rejected: the
  launcher applies the filter before it execs its command, so whatever sets
  the limits afterwards would need the calls the filter must deny.
- **A self-hosted runner as the dedicated host.** Rejected for now: it adds a
  machine to operate and to trust, and a hosted runner is already disposable.
  If a rule of 0042 turns out not to be testable on a hosted runner, the slice
  that meets it says so and this record is amended.
- **Turning the system-wide user-namespace restriction off in the job.**
  Rejected: an operator of a real host would rather allow one program, and the
  job should prepare the host as an operator would.
- **Ubuntu 22.04 for the job as well.** Not now: one image is enough for a
  first backend, and its launcher (0.6.1) and kernel are the older ones. A
  second image is a later row of the matrix, not a second backend.

## Consequences

- R02a becomes six pull requests, each with evidence from a real kernel, where
  one would have been too large to review.
- The enforcement job makes CI longer and depends on the hosted image; rule 9
  turns a silent change of that image into a failure.
- Two dependencies enter the provider and the server binary and must be
  reviewed when they are bumped.
- A host is supported only when an operator prepared it. The product still
  asks for no privilege at run time, but the preparation is real work that the
  documentation of R03 has to describe.
- WSL and containers stay unverified (0042, rule 1); the contributor host of
  this repository is refused by its crash routing.
