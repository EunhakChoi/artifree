# 0056: Delivering the Linux execution provider

Date: 2026-11-12. Status: **accepted as a plan; R02aa (host admission and the
enforcement job) and R02ab (the first launch, of a fixture that only exits and
only from the job's tests) are implemented**.
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

> **Amendment, 2026-11-12.** The R02aa row of rule 14 read "every refusal of
> rule 10 but the tool root's, which R02ab adds with the manifest": it kept
> the other refusals in R02aa and left all of the tool root's to R02ab. That
> was written with the manifest in mind and reads wider than intended.
> R02aa's admission starts two programs of the tool root: the launcher, and
> the test-support program of rule 4 in two trusted modes that only print its
> own namespaces or wait on its input. Neither mode is the payload of rules 3
> and 14, and R02aa still runs no payload. Because its checks need both
> files, R02aa refuses a tool root in which the launcher or that program is
> missing or is not a regular executable file, names which one, and the job
> takes each away in turn (rule 10). Four limits apply:
>
> - This check comes before the manifest of rule 6 and does not replace it.
>   The refusal of a tool root in which any file differs from its recorded
>   length and digest, and of one that lacks any other file, stays R02ab's.
> - When R02ab adds the manifest's refusal it places it after this check in
>   the documented order, so a missing launcher or program keeps the refusal
>   that names it.
> - The test-support program is required only of the tool root that R02a's
>   harness assembles. What an operator's tool root must hold is decided with
>   its packaging (rule 6: R03's), and rule 4 still keeps that program out of
>   every release archive.
> - A pass of this check says that two files exist. It is no evidence of a
>   tool's identity, and R02aa proves nothing of 0042's row for a modified
>   launcher, tool or policy.
>
> The row is reworded below to say so.

> **Amendment, 2026-11-12 (the scratch).** Rule 17 left open which of the
> launcher and the bootstrap applies the scratch's inode bound. A temporary
> job (deleted, never merged) measured it on `ubuntu-24.04` before R02ac was
> written, with a trusted program and synthetic files only:
>
> - Under the launch of R02ab a payload created 20,000 files in the scratch
>   without a refusal. It could also create files in the root of the sandbox
>   and under `/dev`: both are the launcher's own tmpfs mounts, writable and
>   without a size. Only the memory limit of the group bounded them.
> - With the launcher's two read-only remounts, of `/` and of `/dev`, those
>   writes failed.
> - A bootstrap that the launcher gave `CAP_SYS_ADMIN` could neither remount
>   the scratch nor mount another one: the launcher disables further user
>   namespaces by entering one more, so the bootstrap's capabilities belong
>   to a namespace that does not own the mount namespace.
> - After the bootstrap made a mount namespace of its own, it mounted a tmpfs
>   with a size and an inode count on `/tmp`. The payload then created 4,095
>   files (the root directory is the 4,096th inode) and 134,217,728 bytes
>   and was refused past either; a removed file made room for one more.
> - `CAP_SETPCAP` beside it let the bootstrap empty its bounding set; with
>   `CAP_SYS_ADMIN` alone the capability stayed in the bounding set.
> - `rustix` offers `unshare` only as an unsafe function. `nix` offers it as
>   a safe one.
>
> So the bootstrap applies both bounds of the scratch, and four rules are
> reworded below:
>
> - **Rule 2.** The launcher no longer mounts the scratch. It makes the mount
>   point, remounts the sandbox's root and `/dev` read-only, and hands the
>   bootstrap two capabilities inside the sandbox's user namespace,
>   `CAP_SYS_ADMIN` and `CAP_SETPCAP`. They are capabilities over the
>   sandbox's own namespaces and none over the host.
> - **Rule 3.** Before anything else of its order the bootstrap makes its own
>   mount namespace, mounts the scratch there with the size and the inode
>   count of 0042, rule 5, and reads both back from its mount table. The
>   privilege drop that follows removes the two capabilities from every set,
>   the bounding set included, and the provider's check before the release
>   (rule 5) already requires all sets empty. The payload therefore runs
>   without them and cannot change a mount.
> - **Rule 7.** A third dependency, `nix`, for `unshare` alone, under the same
>   conditions as the other two.
> - **Rule 17.** The R02ac point is settled as above.
>
> What this does not change: 0042; the order of the remaining bootstrap steps;
> the rule that no payload instruction runs before the limits, the privilege
> drop and the filter; the ban on first-party `unsafe`. Until R02ac is merged
> the code is R02ab's, whose payload only exits and writes no file.

> **Note, 2026-11-12 (R02aa).** The provider's admission and the job
> `Linux enforcement` exist. On the runner the provider's own code did what
> the second measurement did with shell commands, which settles the first
> assumption of rule 17. The job withholds the memory controller for rule 10
> by starting the unit in a transient slice with `DisableControllers=memory`;
> the provider then refused naming that controller, which settles the second.
> The tool root is `/opt/artifree/tools/v1`. The admission's fixed order of
> refusals is documented in the module `execution`. The deadlines and
> bounds of the trusted namespace check (10 seconds, 4,096 bytes, a 1 MiB
> scratch) are that slice's choices and bind no attempt. The job pins the
> Bubblewrap package to one revision, so a superseded revision fails the job
> until the pin is moved. Rules 2 to 7 and the
> slices R02ab to R02ah were not implemented then; the next note covers R02ab.

> **Note, 2026-11-12 (R02ab).** The first launch exists and runs in the job.
> What the slice settled of rule 17, measured on the runner before it was
> written and then by its own tests:
>
> - **Readiness and release (rule 5).** The provider gives the stub one end
>   of a socket pair as its standard input; the stub, the launcher and the
>   bootstrap inherit it. The bootstrap first moves it to a close-on-exec
>   descriptor and points descriptor 0 at `/dev/null`, so the payload holds
>   no copy and cannot reopen one through `/proc` (a socket cannot be opened
>   there). The records are one byte to launch, a fixed 16-byte readiness
>   record, one byte to release and one byte for a failed `exec`. This is the
>   input-stream candidate of rule 17 carrying both directions; no further
>   descriptor and no crate for passing one is used.
> - **The provider does not take the bootstrap's word.** Before it releases,
>   it reads the members of the attempt's group and requires exactly one in
>   seccomp filter mode, with `no_new_privs`, one filter, empty capability
>   sets, the two limits, another PID namespace than its own, and every
>   member inside the group. A test-support mode that reports ready without
>   any of it is not released.
> - **A failed `exec`** is told by the byte the bootstrap writes when `exec`
>   returns. A successful one is not told positively: the launcher and its
>   init keep their copies of the stub's input, so no descriptor closes at
>   the `exec` alone. A bootstrap that dies between the release and the
>   `exec` leaves the same end of stream as a payload that ran; such an
>   attempt ends by a signal or a non-zero status and is a failure either
>   way.
> - **Safe interfaces (rule 7).** `rustix` 1.1.6 (features `std`, `process`,
>   `thread`, `stdio`) and `seccompiler` 0.5.0 offer every call of rule 3
>   safely; both are pinned and declared for Linux targets only, under
>   `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` and
>   `Apache-2.0 OR BSD-3-Clause`. Syscall numbers are a first-party table for
>   x86-64, compared in the job with the runner's kernel headers; another
>   architecture has no filter and fails before the payload.
> - **Two things the measurement found.** A debug build of the standard
>   library calls `fcntl` when it drops a descriptor, which the filter does
>   not allow: after the filter the bootstrap drops none, and the `exec`
>   closes them. Descriptors the provider's process inherited without
>   close-on-exec reach its children: the bootstrap refuses to go on when it
>   holds any descriptor beyond the three streams and the control channel.
>
> The launch is compiled into the tests only, so no caller can reach it and
> rule 16 holds unchanged. The two entry functions of the stub and the
> bootstrap are public, because the test-support program calls them (rule 4);
> they check nothing of the host and are of use only under the launch. The tool root's manifest is `MANIFEST`, one line of
> digest, length and path per file; it is checked at each admission and not
> held against replacement afterwards. The admission lock lives in a private
> directory of the worker's account that R02b will take from the deployment.
> The manifest lies in the root it describes and only its digest is recorded:
> it tells a root that changed from the one an operator installed, and gives no
> protection against whoever may write the root, whose ownership and modes are
> not checked yet (rule 6 leaves packaging to R03). The readiness check asks
> for exactly one filter, so a worker that itself runs under a seccomp filter
> is refused at every attempt without a facility that names the reason; such
> hosts stay unverified like containers (0042, rule 1). A start that fails
> before readiness returns only its reason; what the launcher or the bootstrap
> wrote to standard error is dropped. The standard library's `exec` retries a
> file that is not a program through `/bin/sh`; the sandbox holds no shell, so
> it fails as measured, and a slice that binds one must close this first.
> A cleanup that is not observed stops further attempts of that process only;
> the persisted quarantine is R02ah's. Not proven here and left to the named
> slices: the wall deadline after release and the output bound (R02ac, whose
> payloads can hang and flood), survivors after a normal exit (R02ac), and
> the refusal of leaked descriptors with a real leak (R02ae). The allowlist
> is the fixture's and gets its public mapping in R02ad.

## Context

### What was measured

A temporary job, on a branch that was deleted afterwards and never merged, read
what GitHub's hosted runners offer (2026-11-11). It installed Bubblewrap
from the image's package source and ran one trivial trusted command with it;
it ran no untrusted payload.

| Fact                                         | `ubuntu-24.04`                         | `ubuntu-22.04`         |
| -------------------------------------------- | -------------------------------------- | ---------------------- |
| Kernel                                       | 7.0 (azure)                            | 6.8 (azure)            |
| Bubblewrap                                   | not installed; the package is 0.9.0    | not installed; 0.6.1   |
| Unprivileged user namespaces                 | restricted by AppArmor (`bwrap` fails) | allowed (`bwrap` runs) |
| `kernel.core_pattern`                        | a pipe to `systemd-coredump`           | the same               |
| cgroup v2 with `cgroup.kill`                 | yes                                    | yes                    |
| A user scope with memory and task limits     | works (`systemd-run --user --scope`)   | not measured           |
| A unit with `Delegate=yes` run as the runner | owned by the runner, five controllers  | not measured           |
| Moving a process into a subtree made by root | refused                                | not measured           |
| `sudo`                                       | yes                                    | yes                    |
| `prlimit`, `setpriv`, `capsh`                | installed                              | not measured           |

The contributor host used for this repository (WSL2, kernel 6.18, Bubblewrap
0.9.0) also routes cores to a pipe. Under 0042, rule 5, it is an unsupported
host, and so is an unprepared hosted runner. Neither may run a payload to
obtain evidence.

A second temporary job (2026-11-12, deleted likewise) tried the parts of this
plan that need no payload on `ubuntu-24.04`, with shell commands and a trusted
`sleep`:

- With an AppArmor profile naming a copy of the launcher at a fixed path, that
  copy made its namespaces; the system's own copy still failed, and removing
  the profile made the copy fail again. On the unprepared host a user
  namespace can be made but holds no capabilities, which is why the launcher
  fails there.
- In a unit started with `Delegate=yes` as the runner's account, enabling
  controllers for children failed while the unit's command sat in the group
  and worked once it had moved itself into a child group. Memory, swap, task
  and CPU limits written to a sibling group read back as written. Making a
  group outside the unit was refused. A unit without delegation was owned by
  root.
- A waiting process that was moved into the sibling group and then replaced
  itself with the launcher left the launcher, its init and the command all in
  that group. `cgroup.kill` emptied it, and the group could be removed.
- Inside the launcher a further user namespace could be made unless the
  launcher was told to disable them. The scratch's size bound held; its inode
  count was not bound.
- Delegating only the memory controller did not hide the other controllers
  from the unit.

### What follows from it

- The acceptance of 0042 can run only on a host an operator prepared. A hosted
  runner is a disposable virtual machine on which the job is the operator: it
  has `sudo`, carries no other workload than the runner's own agent, and by
  GitHub's description is discarded after the job.
- In the one case measured, a process could not be moved into a subtree that
  root had made and handed over. A unit started with delegation was owned by
  the runner's account. The plan therefore starts the worker inside the
  delegated unit and lets it make its own groups below it.
- The first-party Rust of this repository forbids `unsafe`. Setting resource
  limits, installing a seccomp filter and mounting need system calls the
  standard library does not offer.

## Decisions

### The provider

1. **One provider, in `artifree-providers`.** The module `execution` grows from
   the inspection of today into the Linux provider. No new crate, no generic
   runner trait and no caller-supplied command are added (0042, rule 2). On a
   host that is not Linux the module compiles to the refusal it has today, and
   the dependencies of rule 7 are declared for Linux targets only.
2. **The launcher is Bubblewrap, used as a program.** The provider starts the
   `bwrap` of the tool root (rule 6) with a fixed argument list built from the
   profile: every namespace unshared, the parent-death kill, a new session, a
   cleared environment, read-only binds of the tool root and the staged input,
   the mount point of the scratch, a private `/proc` and a minimal `/dev`,
   the sandbox's root and `/dev` remounted read-only, further user namespaces
   disabled inside, and the two capabilities the bootstrap needs for the
   scratch (`CAP_SYS_ADMIN` and `CAP_SETPCAP`, inside the sandbox's user
   namespace only). The launcher keeps its own reaping init
   as the first process of the PID namespace; the payload is never that
   process. Nothing a request carries reaches the argument list.
3. **A trusted bootstrap runs inside the namespaces before the payload.** It is
   the launcher's direct child, under the launcher's init. In this order it:
   makes its own mount namespace, mounts the scratch there as one tmpfs with
   the size and the inode count of 0042, rule 5, and reads both back;
   sets the soft and hard resource limits of 0042, rule 5 (descriptors, core
   size) and reads them back; drops what the launcher left of capabilities,
   the two of rule 2 among them and the bounding set included, and
   sets `no_new_privs`; installs the seccomp filter of the profile; checks the
   limits and the filter once more; reports readiness and waits for the
   release of rule 5; and replaces itself with the payload by `exec`, without
   a fork. The filter is installed after the limits and denies changing them
   (a query stays allowed) while it allows the one `exec` that follows. A step
   that fails ends the attempt before any payload instruction runs. The
   bootstrap takes a profile's identity and nothing else; it runs no command
   it is given.
4. **Where the bootstrap lives.** The bootstrap is a function of the provider.
   In R02a its only entry point is a test-support program of
   `artifree-providers` that is built by the tests, knows the fixture profile
   and is in no release archive: 0042, rule 8 keeps fixture injection out of
   production, and a production subcommand whose only profile is the fixture
   would be one. The server binary gains its entry point in R02b and knows no
   profile, so it refuses, until R03 adds the first real one.
5. **A handshake, so that nothing runs outside its group.** The provider makes
   the attempt's cgroup, writes its limits and reads them back. It then starts
   a trusted stub (the same test-support program in R02a) that does nothing
   but wait on its input. The provider moves the stub into the group and
   confirms from `cgroup.procs` that the group holds it; only then does the
   stub `exec` the launcher, so the launcher, its init, the bootstrap and the
   payload are born inside the group (0042, rule 6). The bootstrap reports
   readiness when every step of rule 3 is done, and execs the payload only
   after the provider's release. An attempt that never reported, or whose
   `exec` failed, counts as not started and as failed, never as a run.
6. **The tool root.** One directory per version holds the launcher, the
   bootstrap with the loader and libraries it needs to start, the payload's
   runtime and its libraries, with a manifest of every file's length and
   SHA-256 and the licences. The provider verifies the manifest before
   admission, refuses a root in which a file differs, and binds that directory
   read-only. In R02a the only payload is the repository's fixture and the
   tool root is assembled by the test harness at a fixed path; packaging a
   tool root for operators is R03's, when a real runtime exists. Until then no
   host is called supported outside the enforcement job.
7. **Three dependencies, for the system calls.** The bootstrap and the provider
   use `rustix` (resource limits, `prctl`, capabilities, descriptor handling,
   mounting), `seccompiler` (compiling and installing the filter) and `nix`
   (`unshare`, which `rustix` offers only as an unsafe function), all through
   their safe interfaces and only on Linux targets. First-party code stays free of
   `unsafe`. Each is added in the slice that first needs it, pinned, with its
   licence in the notices, its features reduced to what is used, and a note of
   the review. If a needed call has no safe interface in them, the slice stops
   and this record is amended; a first-party `unsafe` block is not an option a
   slice may take.
8. **The cgroup comes from the service manager.** The worker is started inside
   a unit the operator made with delegation (for systemd: `Delegate=yes`, run
   as the worker's account). At start the provider finds its own group, checks
   that it owns it, that memory, CPU and task control are delegated, and that
   it is the only process in it; moves itself into a child group `supervisor`;
   enables the controllers for the siblings; and makes each attempt's group
   beside it. A group that still holds another process cannot enable
   controllers for its children, so the provider refuses and names that, and
   the enforcement job runs the test program as the unit's own command. It
   never writes outside its own subtree and never asks for privileges. Without
   such a unit the provider refuses, naming the missing facility (0042, rule
   1). How `serve` starts a worker inside such a unit is R02b's.

### Where acceptance runs

9. **A job `Linux enforcement` on `ubuntu-24.04`,** part of `Quality gate`,
   never skipped and never allowed to fail. It asserts the kernel release it
   was reviewed for and fails on another one. The operator's preparation is
   one script under `scripts/enforcement/`, documented as the privileged
   preparation 0042 asks to keep apart from the product's rootless
   requirements. The job runs in three parts:
   1. Install Bubblewrap at a pinned package version and assemble the tool
      root at its fixed path.
   2. On the host as found (cores routed to a pipe, user namespaces restricted),
      run the admission tests and require the refusal, with no payload
      started.
   3. Prepare the rest: allow the tool root's launcher, by its path, to create
      user namespaces through an AppArmor profile for that one program (the
      system-wide restriction stays on); set `kernel.core_pattern` to a plain
      file name; then run the matrix inside a delegated unit as the runner's
      account.
10. **Refusals do not depend on the image.** After preparation the job also
    takes each condition away again, one at a time (the routing, the profile,
    a controller, the delegation, a file of the tool root), and requires the
    refusal that names it, so a later image that already lacks or already
    grants one of them cannot make these tests pass or vanish silently. The
    provider reports the first missing facility in a fixed, documented order.
11. **The hosted runner is the dedicated host of 0042's crash acceptance.** It
    is a virtual machine made for one job, not a namespace on a shared host,
    and its crash policy is under the job's control. The controlled pipe and
    socket routes with their counters, and the deliberately weakened core
    limit, run there and nowhere else. The product itself never writes the
    routing policy; only the job's preparation does.
12. **Nothing of this runs on a contributor's host.** There the suite reads
    the routing policy, requires the refusal when the host is unsuitable, and
    runs no payload, as today. The local sandbox runner of this repository is
    a convenience for trusted checks and is not evidence for any rule.
13. **Each run records its environment** as 0042 asks: kernel, architecture,
    the launcher's version and digest, the policy's digest, the delegated
    controllers, the effective limits, the peak memory, the cleanup
    observation and the listener counters, as an artifact of the job.

### Slices

14. R02a is delivered in this order. Each slice passes `npm run check`,
    `npm run build` and the enforcement job, adds no job kind, no API and no
    caller command, and states in its PR which rows of 0042's matrix it now
    proves and which stay open. No slice runs a payload without the limits,
    the privilege drop and a default-deny filter of rule 3.

| Slice | What it adds                                                                                                                                                                                                                                                                                                                                                                          | 0042                  |
| ----- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------- |
| R02aa | The enforcement job with its three parts; the provider's admission: namespaces actually made, controllers written and read back, a group killed and seen empty, the routing policy, the kernel; every refusal of rule 10, for the tool root only a launcher or test-support program that is missing or not a regular executable file (the manifest's refusal is R02ab's). No payload. | 1, 5 (routing), 8     |
| R02ab | The first launch: the tool root and its manifest, the stub and the handshake, the group's limits, the bootstrap with limits, privilege drop and a default-deny filter, the wall deadline, kill and the empty group, one attempt at a time by a crash-released lock. The fixture only exits.                                                                                           | 3 (tools), 4, 5, 6, 8 |
| R02ac | Resources and cleanup under that launch: fork and thread bomb, memory, scratch bytes and inodes, log flood, hang, CPU; double fork, `setsid`, ignored signals, held pipes, a background writer.                                                                                                                                                                                       | 5, 6, 8               |
| R02ad | The public mapping from each system-call restriction to its filter entry, probe, independent observation and mutation, and the descriptor ceiling with its inheritance, setter and guard checks.                                                                                                                                                                                      | 4, 8                  |
| R02ae | Files, descriptors and network: host canaries, leaked descriptors, real listeners with counters.                                                                                                                                                                                                                                                                                      | 4, 8                  |
| R02af | Input staging from a pinned revision's files, and output collection in memory through a directory handle, with the hostile trees.                                                                                                                                                                                                                                                     | 3 (input), 7, 8       |
| R02ag | The crash acceptance: cores under file routing after `fork` and `exec`, the weakened limit, the pipe and socket routes with counters, a routing change that stops admission.                                                                                                                                                                                                          | 5 (cores), 8          |
| R02ah | The guardian with its own deadline, death of the supervisor at each phase, the attempt registry and quarantine. Renewals run against a stand-in for the store, including one that keeps renewing after its fence is revoked and one that renews for the whole 120 seconds.                                                                                                            | 6, 8                  |

15. **What R02a leaves to R02b.** R02ab to R02ag launch without the ownership
    record of 0042, rule 6; R02ah adds it. Everything that needs the job store
    is R02b's: the pinned actor generation, lease loss, cleanup before reclaim
    and before a terminal state, trash, purge and its tombstone, restore, the
    stale fence at publication, and the renewals of R02ah repeated against the
    real store.
16. **R02 is not complete after R02a.** Until R02b passes as well, the roadmap
    says partial, the module says `IncompleteEnforcement` to every caller
    outside the tests, and no text claims a supported host.

### What a slice still has to settle

17. This record was written from the measurements above and from the
    documentation of the launcher and the two crates, not from a working
    launch. These points are assumptions that the named slice must prove on
    the runner, and amend this record if they fail:
    - R02aa: that the provider's own code does what the second job did with
      shell commands (rules 8 and 9), and how the job withholds one controller
      for rule 10, since delegating a subset did not.
    - R02ab: how the readiness report and the release travel between the
      provider and the bootstrap without first-party `unsafe` (rule 5). The
      standard library hands a child three streams. The candidates are the
      input stream for the release and a fixed-length record ahead of
      anything the payload can write for the report, or a reviewed crate that
      passes further descriptors; a descriptor left inheritable in the
      provider is not one of them, since a concurrent launch would inherit it.
      The same slice settles how a failed `exec` of the payload is told from a
      successful one, for example by a descriptor that closes only when the
      `exec` succeeds.
    - R02ab: that the two crates offer every call of rule 3 through a safe
      interface (rule 7).
    - R02ac: which of the launcher and the bootstrap applies the scratch's
      inode bound (0042, rule 5: 4,096 inodes); the launcher's tmpfs option
      takes a size only. Settled by the amendment of 2026-11-12 (the scratch):
      the bootstrap, in a mount namespace of its own.
    - R02af: how the keeper of 0042, rule 7 holds the scratch until
      collection ends, now that the mount is made inside the sandbox and
      lives in the bootstrap's mount namespace.
    - R02ag: that the coredump behaviour 0042 cites for kernel 6.18 holds for
      the kernel of the runner, read from that kernel's source before the
      slice relies on it.

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
- **Blocking the launcher itself and moving it into the group.** Rejected: the
  launcher may already have made its namespace child when it blocks, and
  moving one process does not move that child. A stub that execs the launcher
  from inside the group has no such child.
- **The bootstrap as a subcommand of the server binary from the start.**
  Rejected for R02a by rule 4.
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

- R02a becomes eight pull requests, each with evidence from a real kernel, where
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
