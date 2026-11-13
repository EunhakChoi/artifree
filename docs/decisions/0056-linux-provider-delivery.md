# 0056: Delivering the Linux execution provider

Date: 2026-11-12. Status: **accepted as a plan; R02aa (host admission and the
enforcement job), R02ab (the first launch, only from the job's tests), R02ac
(resources and cleanup under that launch, with the scratch amended below)
and R02ad (the syscall mapping, as amended on 2026-11-13; its x32 class
fails acceptance on the reviewed kernel), R02ae (files, descriptors and
network, as planned in the second and third amendments of 2026-11-13)
R02af (input and output, as planned in the amendment of 2026-11-13 on
input and output), R02ag (crashes, as planned in the amendment of
2026-11-13 on crashes) and R02ah (the guardian, as planned in the amendment
of 2026-11-13 on the guardian) are implemented**.
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
> - `rustix` and `nix` both offer the mount call and `unshare` through safe
>   functions. `rustix` has deprecated its safe `unshare` in favour of an
>   unsafe one, because some of its flags (sharing of the descriptor table
>   among them) can break what safe code assumes; using the deprecated form
>   would mean silencing a lint, which this repository does not do. `nix`
>   offers the call as a safe function without that marker.
>
> So the bootstrap applies both bounds of the scratch, and these rules are
> reworded below:
>
> - **Rule 2.** The launcher no longer mounts the scratch. It makes the mount
>   point, remounts the sandbox's root and `/dev` read-only, and hands the
>   bootstrap two capabilities, `CAP_SYS_ADMIN` and `CAP_SETPCAP`. They are
>   held in the innermost user namespace, the one the launcher enters to
>   disable further ones. As measured, that namespace does not own the
>   launcher's mount namespace, so they reach what the bootstrap makes under
>   it, its own mount namespace, and nothing of the host. The mounts the
>   bootstrap's namespace inherits, the two read-only remounts among them,
>   belong to the outer namespace, and the bootstrap could not remount one
>   when it tried. This changes the attempt's argument list only. The trusted
>   namespace check of the admission keeps its own list, and the admission
>   does not probe the three new facilities (the capabilities beside the
>   disabled user namespaces, `unshare`, the mount). A host that passes
>   admission and refuses one of them fails every attempt in the bootstrap,
>   before readiness and before any payload instruction, without a refusal
>   that names the facility. That is a known limit, like the worker under its
>   own filter in the R02ab note; such a host is not a supported one.
> - **Rule 3.** After it has checked its profile identity, taken the control
>   channel and refused leaked descriptors (the steps of the R02ab note, which
>   need no capability), and before the limits, the bootstrap makes its own
>   mount namespace and mounts the scratch there: one tmpfs, `nosuid`, `nodev`
>   and, for the fixture profile, `noexec` (0042, rule 4; the fixture runs
>   nothing from the scratch), owner-only, with the size and the inode count
>   of 0042, rule 5. It reads the flags and both bounds back from its mount
>   table, with the root and `/dev` read-only. The privilege drop that
>   follows removes the two capabilities from every set, the bounding set
>   included. The payload therefore runs without them and cannot change a
>   mount. Between its start and that drop the bootstrap holds
>   `CAP_SYS_ADMIN`; in that time it reads its one argument, the control
>   channel and its own `/proc` entries, and nothing the payload supplies.
> - **Rule 5.** The provider does not take the bootstrap's word for the
>   scratch either: before the release it reads the bootstrap's mount table
>   itself and requires the same flags, bounds and read-only mounts, beside
>   the empty capability sets it already requires.
> - **Rule 7.** A third dependency, `nix`, with its `sched` feature alone, for
>   `unshare` with the one flag that makes a mount namespace, called in the
>   bootstrap while it is a single thread. That flag is not among those that
>   make the call hazardous. The call is safe by that restriction, which the
>   code must keep; the choice of crate only spares a silenced lint.
> - **Rule 14.** The R02ac row names the amended launch and its checks, and
>   0042, rule 4 in its last column.
> - **Rule 17.** The R02ac point is settled as above. A new R02af point is
>   added: the scratch lives in a mount namespace whose every member is in
>   the attempt's group, and 0042, rules 6 and 7 collect only once that group
>   is empty. How a keeper outside the group holds the mount until then is not
>   measured. The launcher's own tmpfs had the same property, so this
>   amendment does not create the question, but it does not answer it. A
>   handle opened through the bootstrap's `/proc` entry before the release is
>   a candidate and no more. If R02af finds no way to hold it without
>   first-party `unsafe` or a race against the kill, rules 2 and 3 are amended
>   again; 0042, rule 7 holds either way.
>
> What stays writable after the two remounts, as measured: the scratch, the
> private `/proc` (the kernel's per-process files) and the launcher's
> `/dev/pts`. Whether either of the last two can reach beyond the attempt is
> R02ae's to prove or to close; R02ac proves that file creation fails in `/`,
> `/dev`, `/dev/shm`, the tool root and the library binds.
>
> Until R02ac is merged the code is R02ab's. Its launch does not meet the
> part of 0042, rule 4 that asks for one bounded writable filesystem: the
> root and `/dev` are writable there and the scratch has no inode bound. Its
> payload only exits and writes no file, and it runs in the job alone.
>
> What this does not change: 0042; the order of the remaining bootstrap steps;
> the rule that no payload instruction runs before the limits, the privilege
> drop and the filter; the ban on first-party `unsafe`. The provider cannot
> see or unmount the scratch from the host's mount table; that no mount of an
> attempt is left (0042, rule 6) follows from the empty group, since the
> mount namespace ends with its last member.

> **Amendment, 2026-11-13 (the syscall mapping).** 0042 asks R02a for a
> public mapping from each system-call restriction of its rule 4 to the
> policy entry, a payload probe, the expected denial, an independent
> observation and a guard mutation, and for the descriptor ceiling with its
> inheritance, setter and guard checks. Rule 17 did not say how R02ad
> attributes a denial to the filter. Two temporary jobs (deleted, never
> merged) measured it on `ubuntu-24.04` (kernel 7.0) before R02ad was
> written, with generated probe programs and synthetic arguments only:
>
> - The kernel takes i386 entries (`int 0x80`) from a 64-bit process and has
>   no x32 entry point (`CONFIG_X86_X32_ABI` is not set): without a filter an
>   x32 number fails with `ENOSYS`.
> - Each of 61 refused calls ended its probe with `SIGSYS` and left one kernel
>   record of type 1326 naming the probe's program (`exe=`), `sig=31`, the
>   architecture and the number as the filter saw them (`arch=40000003` for an
>   i386 entry; the x32 bit inside the number). An allowed call left none, and
>   so did `clone3`, which the first program answers with `ENOSYS`. All 61
>   records arrived from one launch that started the probes one after another
>   as children of the payload, once the job had lifted the kernel's message
>   rate limit (ten messages in five seconds by default).
> - The same probes without a filter: the kernel refused most for another
>   reason (a missing capability, a bad address, the disabled user
>   namespaces, `ENOSYS` for x32), while `ptrace(PTRACE_TRACEME)`,
>   `process_vm_readv`, `kcmp` and an i386 `exit` succeeded, and `clone3`
>   with an empty argument failed with `EINVAL`. An exit status or an error
>   number therefore says nothing about the filter; the record does.
> - Mutants of the filter: with the architecture check made to pass every
>   architecture, the i386 `exit` ran and exited 0; without the namespace mask
>   on `clone`, `CLONE_NEWUSER` and `CLONE_NEWNS` failed in the kernel with
>   `ENOSPC` and `EPERM` instead of `SIGSYS`; with `prlimit64` allowed to set,
>   a payload lowered its soft descriptor limit and restored it, while
>   raising it failed with `EPERM`; with the bootstrap's ceiling at 512, the
>   payload held descriptors up to 511.
> - Under the ceiling of 256, a payload holding the three streams opened 253
>   descriptors, the highest 255, and the next one failed with `EMFILE`;
>   duplication stopped at the same ceiling, and a child after `exec` had the
>   same limits. A descriptor at 400, opened before the limit was set, stayed
>   open in the payload, as 0042 warns.
> - Read from outside, through `/proc`, every task of the payload, its threads
>   and its children after `exec` showed `NoNewPrivs` 1, seccomp mode 2 with
>   two filters, five empty capability sets, and both limits.
>
> So R02ad works as follows. Rules 6, 9, 14 and 17 are reworded at the end;
> the others stay as written.
>
> - **Probe programs.** Each probe is a static x86-64 program of a few
>   instructions, generated as bytes by the test-support program of rule 4,
>   without an assembler and without first-party `unsafe`. It sets the
>   arguments, enters the kernel once by `syscall` or by `int 0x80`, and exits
>   with the call's error number. The probes are part of the fixture (rule
>   6). In one launch the payload fixture starts each probe as a child and
>   waits for it, so every probe runs after a `fork` and an `exec`. Three
>   refusals need the fixture itself rather than a probe, and run in the same
>   launch with their own records: a refused call from a thread the payload
>   started after its `exec` (inheritance by every payload thread, 0042), and
>   a `prlimit64` that sets the limit of the payload's own child (an owned
>   descendant).
> - **Attribution.** A refusal counts only when a kernel record names the
>   expected program, `sig=31`, and the expected architecture and number. The
>   test reads the kernel log from its end before the launch and to its end
>   after it, and requires exactly the expected records in that window: a
>   missing record, a second one, or any other seccomp record (a kill of the
>   payload fixture included) fails it. The record also shows that the probe
>   reached the call: the filter decides at the kernel's entry.
> - **`clone3`.** The first program answers it with `ENOSYS` and no record,
>   so its observation differs. Its probe, with namespace flags in an
>   argument structure the kernel rejects, runs in a launch of its own as the
>   payload itself, and the provider reads the exit status that the launcher
>   passes on: it must be exactly `ENOSYS`. Beside it, the job runs the same
>   probe directly on the runner, outside any sandbox, where the kernel
>   rejects the argument with `EINVAL` before creating anything; this is the
>   in-job control that the kernel implements the call, so the `ENOSYS`
>   inside is the filter's and not the one 0042 discounts. Without the first
>   program, the allowlist admits `clone3` and the payload exits with
>   `EINVAL`, so the normal run's assertion fails. The mapping records the
>   class's evidence as these two exit statuses, both observed by trusted
>   code.
> - **Controls.** Allowed neighbours of the refused classes run in the same
>   launch and must succeed without a record: `gettid`, a `prlimit64` query,
>   `clone` without namespace flags (also from a forked child), and the
>   benign calls every scenario makes. Where a whole class is refused
>   (modules, BPF, keyrings, clocks, the asynchronous interfaces), no allowed
>   call of the class exists to control with; the record is the evidence and
>   the unfiltered measurement above shows what the kernel itself would have
>   done.
> - **x32.** The reviewed kernel has no x32 entry point, so no control can
>   show the filter refusing an x32 call that would otherwise run. As 0042
>   asks when a class has no control, the class fails this host's
>   supported-profile acceptance: the mapping marks it failed, claims nothing
>   from its records, and R02 is not complete on this kernel until a kernel
>   built with x32 provides the control or 0042 is amended. That is an owner
>   decision, open with the owner. Refusal is retained meanwhile, as every
>   caller is refused until R02b anyway. An allowlist entry compares the whole
>   number, so a number with the x32 bit never matches one.
> - **Undoing the boundary.** The refused probes of `prctl` (clearing
>   `no_new_privs`), `capset`, and `seccomp` are the payload's attempts to
>   undo `no_new_privs`, the empty capability sets and the filter; the mapping
>   names them as such.
> - **Mutations** are not built into the product. On a temporary branch, the
>   disposable copy of 0042, a script applies one patch at a time, runs the
>   named job test and requires it to fail: the architecture check, the
>   namespace mask, the `clone3` program, the setter guard, the default
>   action, each refused call allowed alone, the ceiling's value raised to
>   512, and the initial limit guard removed, so the payload inherits the
>   runner's limit above 256 (the parent limit 0042 asks for). The script and
>   its results are attached to the pull request and recorded in the
>   mapping. The mutations are run again whenever the filter, the probe
>   table or the bootstrap's limits change.
> - **The mapping** is a document of the repository,
>   `docs/linux-syscall-mapping.md`. A unit test keeps it and the probe
>   table in step: every class of 0042, rule 4, its probes, the record each
>   probe expects, its control and its mutation.
> - **Descriptors.** A scenario exhausts opens and duplications, then repeats
>   in a child after `exec`, against the ceiling and both limits, read by the
>   payload and from outside. The native setters and the i386 forms are
>   probes with records; the descendant's setter is the fixture's, above. A
>   lower and a restore of the soft limit within the hard limit is the setter
>   guard's case, because a refused raise would fail in the kernel anyway. A
>   descriptor inherited above the ceiling is refused by the bootstrap's
>   check of the R02ab note; its proof with a real leak stays R02ae's.
> - **The boundary from outside.** While a scenario holds a thread and a
>   child, the test reads `NoNewPrivs`, the seccomp mode and filter count,
>   the capability sets and both limits of every task of the attempt's group
>   from `/proc`.
>
> The rules, reworded:
>
> - **Rule 6.** The fixture's tool root also holds its generated probe
>   programs, under the same manifest.
> - **Rule 9, part 3** also lets the runner's account read the kernel log
>   (`kernel.dmesg_restrict` 0) and lifts the kernel's message rate limit
>   (`kernel.printk_ratelimit` 0), on the job's disposable runner only (rules
>   11 and 12). The records reach that log only while no audit daemon holds
>   the kernel's audit channel, so the preparation requires that, records it
>   with the two settings among the run's environment (rule 13), and fails
>   with a named reason otherwise. The job's harness also runs the `clone3`
>   control probe on the runner outside the sandbox; it is the only probe
>   that runs there. The product reads no kernel log.
> - **Rule 14.** The R02ad row adds that the real-leak case of an inherited
>   descriptor is R02ae's.
> - **Rule 17.** An R02ad point: how a refusal is attributed to the filter,
>   settled by this amendment.
>
> What this does not change: 0042; the filter, its two programs and its
> entries; the launch; the ban on first-party `unsafe`; and rule 12, under
> which no probe runs on a contributor's host.

> **Amendment, 2026-11-13 (files, descriptors and network).** 0042 asks for
> the rows "Host files and processes" and "Network" of its matrix: host
> canaries, traversal, `/proc` descriptors, mount changes, tracing and
> signals, leaked descriptors and executable configuration injection fail,
> and IPv4, IPv6, DNS, host loopback, metadata addresses, pathname and
> abstract Unix sockets and inherited sockets cannot reach host or external
> listeners, proved with real listeners whose counters stay zero. The
> amendment of 2026-11-12 also left to R02ae whether the private `/proc` and
> the launcher's `/dev/pts` reach beyond the attempt. Temporary jobs
> (deleted, never merged) measured on `ubuntu-24.04` (kernel 7.0) before
> R02ae was written. They ran static programs of their own under the
> launcher with the attempt's namespaces, binds and read-only remounts,
> **without the bootstrap and without the seccomp filter**, against
> listeners and canaries they had made on the runner. Without the bootstrap
> there was no scratch: the sandbox's `/tmp` was the launcher's read-only
> mount point, while in an attempt the bootstrap mounts the scratch there.
>
> - The filter refuses `socket`, so today every network attempt of a payload
>   ends at the filter, and the network namespace is never what stops it:
>   leaving out `--unshare-net` would pass every network test. That is the
>   case 0042 rules out when it asks for real listeners rather than a failed
>   call.
> - Inside, the network namespace held `lo` alone and no IPv4 route.
>   Connections to `127.0.0.1` and `::1` on the port of a host listener
>   were refused by the attempt's own loopback; the runner's own address,
>   the metadata address `169.254.169.254`, an external IPv4 and IPv6
>   address and two resolvers off the host failed with `ENETUNREACH`;
>   datagrams to a host listener's port on `127.0.0.1` and to the host's
>   resolver at `127.0.0.53` were sent into the attempt's own loopback. A
>   pathname socket in the host's `/tmp` did not exist inside, and a host
>   listener on an abstract name refused the connection: abstract names
>   belong to the network namespace. Raw and packet sockets failed with
>   `EPERM`; a netlink socket was made, for the attempt's own namespace. A
>   listener inside the attempt on `127.0.0.1` and on an abstract name
>   accepted a connection from inside. Every host listener counted the one
>   connection the job made to check it before the launch, and nothing
>   between the start and the end of the launch.
> - Canary files in the runner's home and in `/tmp` did not exist inside,
>   also not through `/tools/..`, `/proc/1/root` or `/proc/self/cwd/..`; the
>   root refused file creation (read-only); the canaries were unchanged
>   afterwards.
> - A host process of the same account was not visible: signalling its host
>   PID failed with `ESRCH`, and `kill(-1, SIGKILL)` killed the program's own
>   child and left the host process running.
> - `/proc`: writes to `kernel.hostname`, `vm.drop_caches`,
>   `kernel.core_pattern` and `/proc/sysrq-trigger` failed with `EACCES`, and
>   `kernel.ns_last_pid` with `EPERM`; the host's values were unchanged. The
>   per-process files of the attempt's own processes (`oom_score_adj` raised,
>   `comm`) were writable, and the attempt's PID namespace shows no other
>   process.
> - `/dev/pts` is a devpts instance of its own (`ptmxmode=666`), and
>   `/dev/ptmx` is a link to `pts/ptmx`: opening it made terminal 0 inside
>   and none on the host. Every devpts instance draws on the host-wide
>   `kernel.pty.max` budget, though, so a payload allowed to open terminals
>   could exhaust the budget of every other instance on the host. In a
>   process holding `CAP_SYS_ADMIN` in the sandbox's user namespace and a
>   mount namespace of its own, as the bootstrap does: unmounting
>   `/dev/pts` failed with `EINVAL` (the launcher's mounts are locked, since
>   it enters a nested user namespace); a new devpts instance with `max=0`
>   still let `/dev/ptmx` open; an empty read-only tmpfs mounted over
>   `/dev/pts` made opening `/dev/ptmx` fail with `ENOENT` and file creation
>   there with `EROFS`. `/dev/shm` is read-only.
> - A descriptor the launcher's parent held without close-on-exec reached
>   the program inside (number 9), with two more that the runner's agent had
>   leaked (142 and 145).
>
> So R02ae works as follows. Rules 3, 13, 14 and 17 are reworded at the
> end; the others stay as written.
>
> - **Local networking in the fixture profile.** As 0042, rule 4 permits,
>   the profile allows communication entirely inside the attempt, so that
>   the network namespace, not the filter, is what the network tests prove.
>   `socket` is allowed only for these forms: `AF_UNIX` stream or datagram
>   with protocol 0; `AF_INET` and `AF_INET6` stream with protocol 0 or TCP,
>   and datagram with protocol 0 or UDP; the close-on-exec and non-blocking
>   flags may be added to the type. `setsockopt` is allowed only at the
>   socket level for `SO_REUSEADDR`. `bind`, `listen`, `accept4`,
>   `connect`, `sendto`, `recvfrom`, `getsockname` and `shutdown` are
>   allowed. Every other domain (netlink and packet among them), type (raw
>   and sequenced packets among them) and protocol (SCTP, MPTCP and L2TP
>   among them, whose modules the kernel would load on demand), every other
>   socket option, and the calls that pass descriptors or messages with
>   control data (`sendmsg`, `recvmsg`, `socketpair`) stay refused, and the
>   i386 `socketcall` stays refused by the architecture check. The mapping
>   gains a network class: a probe with its record for each refused form,
>   and controls for the allowed ones. Its guard mutations, each alone: the
>   domain condition removed, the type condition removed, the protocol
>   condition removed, and the condition on `setsockopt` removed, each of
>   which the probes test must fail. Because the filter changes, the whole
>   mutation matrix of R02ad is run again.
> - **What reaches the payload.** Nothing new at run time. The listeners'
>   ports and the canaries' paths are constants that the test and the
>   payload fixture share. The runner's own address is a fact of the
>   prepared host: the preparation writes it into the tool root, under the
>   manifest, as it writes the scenario. The canary process is not named to
>   the payload: the payload signals every PID number up to the kernel's
>   `pid_max` with signal 0, and then sends `SIGKILL` to `-1`.
> - **Network.** The job test runs real listeners in its own process on the
>   host, each with a counter, and checks each by one connection of its own
>   before the launch: TCP on `127.0.0.1` and `::1`, TCP on the runner's own
>   address, UDP on `127.0.0.1`, a pathname Unix socket in the host's `/tmp`
>   and an abstract one. In one launch the payload tries every listener, the
>   metadata address, an external IPv4 and IPv6 address and a resolver, and
>   reports each result; as a positive control it listens on `127.0.0.1`
>   and on an abstract name inside the attempt and connects to both.
>   Passing means: no counter changes between the start and the end of the
>   launch; the controls succeed; and, read from outside through `/proc`
>   while the payload holds, its network namespace lists `lo` alone in
>   `net/dev`, no route in `net/route`, and in `net/ipv6_route` only routes
>   through `lo`. The payload's error numbers are reported but are not the
>   evidence. Packets towards the metadata address, the external addresses
>   or a resolver are not counted on the host: the runner's own agent uses
>   them, and a packet that cannot leave the attempt's namespace never
>   reaches the host's stack; the route tables read from outside are the
>   evidence for them. Its mutation: `--unshare-net` removed from the
>   attempt's argument list only (the admission's namespace check keeps
>   it), and the test must fail on a counter or on the outside reading, not
>   earlier.
> - **Host files and processes.** The test makes canaries before the launch
>   outside `/tmp`, which the scratch shadows in an attempt: a file in
>   `/var/tmp` and one in the runner's home, a directory with mode 0777
>   beside the first, and a process of the runner's account. The payload
>   tries to read the files, also through `/tools/..`, `/proc/1/root` and
>   `/proc/self/cwd/..`, to create files in the directory's path and in `/`,
>   to signal every PID number and then `kill(-1, SIGKILL)`. Passing means:
>   every read and every creation fails (`ENOENT`, or `EROFS` for `/`), and,
>   read from outside, the files' bytes and modification times are
>   unchanged, the directory is empty and the process is alive. Its
>   mutation: the host's `/var/tmp` bound read-only into the attempt, which
>   must fail the test on the reads alone. Mount changes and tracing are
>   refused by the filter and stay proved by the mapping.
> - **`/proc` and `/dev/pts`.** The bootstrap closes `/dev/pts` (rule 3,
>   reworded): in its own mount namespace, after the scratch, it mounts an
>   empty read-only tmpfs over it, and the provider's read of the mount
>   table requires that mount. The payload tries the five writes to
>   `/proc/sys` and `/proc/sysrq-trigger` measured above and opens
>   `/dev/ptmx`; the test reads those values before and after the launch
>   and requires them unchanged, every write refused and the open failing
>   with `ENOENT`. Its mutation: the bootstrap without the over-mount, which
>   must fail both the provider's mount check and, with that check removed
>   too, the open. This closes the point of the amendment of 2026-11-12:
>   `/proc` does not reach beyond the attempt, and `/dev/pts` is closed.
> - **Leaked descriptors, with a real leak.** The test opens, without
>   close-on-exec, a directory, a socket connected to one of its listeners,
>   and a pipe, then launches. The bootstrap's check of the R02ab note must
>   refuse: the attempt does not start, the payload prints nothing, and the
>   listener receives nothing. Its mutation: the check removed, so the
>   payload starts with the leaked descriptors and the test fails.
> - **Executable configuration injection.** The job builds a preload canary,
>   a small shared object of the repository's own whose initializer appends
>   the name of the program that loaded it to a log file on the host, and
>   runs the test's unit with `LD_PRELOAD` naming it, `LD_LIBRARY_PATH` and
>   `BASH_ENV` set. The test requires its own entry in the log (so the
>   injection is live) and no entry from any program the provider starts:
>   the stub, the launcher, the bootstrap or the payload. The payload also
>   reports its environment, which must hold no variable. Its mutation: the
>   provider's clearing of the stub's environment removed, the first of the
>   three clearings, which must leave an entry of the stub. A first attempt
>   writes a marker into its scratch and tries `/dev/shm`; a second attempt
>   must not find the marker. The payload cannot write the tool root, `/` or
>   `/dev` (R02ac), so no file it writes outlives the attempt.
>
> The rules, reworded:
>
> - **Rule 3.** After it mounts the scratch, the bootstrap mounts an empty
>   read-only tmpfs over `/dev/pts` in the same mount namespace and reads it
>   back with the scratch; the provider's read of the mount table requires
>   it.
> - **Rule 13** also records the canaries' observations.
> - **Rule 14.** The R02ae row names local networking in the fixture profile,
>   the network test with its listeners and its mutation, the canaries, the
>   `/proc` checks and the closed `/dev/pts`, the real leak and the
>   injection checks.
> - **Rule 17.** An R02ae point: whether `/proc` and `/dev/pts` reach beyond
>   the attempt, settled by this amendment.
>
> What this does not change: 0042; the launch and its arguments; the other
> steps of the bootstrap; rule 12. The listeners and the preload canary run
> in the job's test unit on the disposable runner; nothing of this runs on
> a contributor's host.

> **Amendment, 2026-11-13 (the launcher's `/dev/pts`).** The R02ae
> implementation's job run (PR #197, before merging) found a path that the
> second amendment of 2026-11-13 missed. The launcher's init is the first
> process of the attempt's PID namespace (rule 2) and stays in the
> launcher's mount namespace, not the bootstrap's. The payload opened
> `/proc/1/root/dev/ptmx` and `/proc/1/root/dev/pts/ptmx`, while
> `/dev/ptmx` gave `ENOENT`: the bootstrap's over-mount covers only its own
> mount namespace. So the second amendment's statements that the launch's
> arguments do not change and that `/dev/pts` is closed do not hold; this
> amendment replaces them.
>
> The temporary branch `probe/r02ae-pts` (commit `0374b1c`, deleted
> afterwards and never merged) measured the fix on `ubuntu-24.04` (kernel
> 7.0, Bubblewrap 0.9.0) in workflow run 43370355942. The attempt's
> arguments mounted an empty tmpfs over `/dev/pts` in the launcher's
> namespace (`--perms 0555 --size 4096 --tmpfs /dev/pts`, then
> `--remount-ro /dev/pts`). With that, `/dev/ptmx` and both paths through
> `/proc/1/root` gave `ENOENT`. The job tests before it passed unchanged.
> The proc-dev test then failed only on a defect of its own: its check of
> the `/dev/pts` listing matched the new lines' names.
>
> The bootstrap's mount namespace starts as a copy of the launcher's, so the
> launcher's cover is in both namespaces and the bootstrap's own over-mount
> adds nothing. So R02ae works as follows:
>
> - **The cover.** The attempt's arguments cover `/dev/pts` as above. The
>   bootstrap no longer mounts over it.
> - **The check.** The bootstrap's readback and the provider's read of the
>   mount table require the last `/dev/pts` mount to be the launcher's
>   cover: a read-only tmpfs of 4 KiB with mode `0555`. Bubblewrap cannot
>   bound its inodes, and a read-only mount needs no inode bound.
> - **The test.** The payload also opens both paths through `/proc/1/root`,
>   and the test requires `ENOENT` for each, as for `/dev/ptmx`.
> - **The mutations.** First, the launcher's cover removed, which must fail
>   the mount check. Second, the cover removed with the check bypassed too,
>   which must fail the test on the opens. The arguments test pins the new
>   options.
>
> The rules, reworded:
>
> - **Rule 2.** The fixed argument list also mounts an empty read-only
>   tmpfs over the minimal `/dev`'s `/dev/pts`.
> - **Rule 3.** The bootstrap reads the launcher's cover of `/dev/pts` back
>   with the scratch, and the provider's read of the mount table requires
>   it. The bootstrap mounts nothing over `/dev/pts`.
> - **Rule 14.** The R02ae row names the cover, its check, the opens
>   through `/proc/1/root` and both mutations.
> - **Rule 17.** The R02ae point is settled by this amendment.

> **Amendment, 2026-11-13 (input and output).** Rule 17 left to R02af how
> the keeper of 0042, rule 7 holds the scratch until collection ends, now
> that the bootstrap mounts it in a mount namespace of its own. Rule 2 says
> that the staged input is bound read-only, but not from where or how it is
> checked. The temporary branch `probe/r02af` (deleted afterwards and never
> merged) measured both on `ubuntu-24.04` (kernel 7.0, Bubblewrap 0.9.0)
> with the launcher, the bootstrap and the filter of R02ae, in workflow runs
> 43379681082 (commit `36b7fb1`), 43380088675 (commit `7e398cf`) and
> 43381857075 (commit `210908f`).
> Between its observation and the release, the provider's prototype opened
> the scratch and planted entries in it. After the attempt group had been
> emptied and removed, it read the scratch back. The measurements:
>
> - **The keeper.** Before the release the provider, outside the group,
>   opened `/proc/<bootstrap>/root/tmp` as a directory. The descriptor was
>   on a tmpfs, and its mount id from `statx` was the id of the scratch's
>   line in the bootstrap's mount table, which the provider had just read.
> - **After the group.** Once the group had been emptied and its directory
>   removed, the descriptor still named the same mount, with the same id.
>   The payload's files could be listed and read through it.
> - **Releasing the mount.** The payload had written 64 MiB into the
>   scratch. The host's `Shmem` rose by 65,472 KiB during the attempt.
>   After the provider closed its descriptors, it fell back to the value it
>   had before the attempt.
> - **Resolution beneath the output root.** `openat2` with
>   `RESOLVE_BENEATH`, `RESOLVE_NO_SYMLINKS`, `RESOLVE_NO_MAGICLINKS` and
>   `RESOLVE_NO_XDEV` refused `..`, an absolute path and a path through
>   `/proc` with `EXDEV`. With `O_NOFOLLOW` and `O_NONBLOCK` added:
>   - a symbolic link failed with `ELOOP`, and a socket failed with
>     `ENXIO`;
>   - a FIFO opened, so only the type of the opened descriptor tells it from
>     a file;
>   - a hard link opened, with a link count of 2.
> - **What can be in the scratch.** From outside, the provider made a
>   symbolic link, a hard link and a FIFO in the scratch. A character device
>   failed with `EPERM`. The fixture filter refuses `symlink`, `link`,
>   `mknod`, `rename` and `mount`, and a refusal kills the process. So the
>   fixture payload itself can make only regular files, directories and
>   socket files.
> - **The input bind.** A host directory bound with `--ro-bind` at `/input`
>   had the mount line `ro,nosuid,nodev` (ext4, propagation `master:1`). The
>   payload read a staged file. Opening it to append failed with `EACCES`
>   (mode `0444`), also through `/proc/1/root/input`. Creating, unlinking
>   and making a directory in it failed with `EROFS`.
> - **Private propagation and `noexec`.** In its own mount namespace, after
>   the scratch, the bootstrap made the whole tree private (`MS_REC` with
>   `MS_PRIVATE`) and remounted `/input` as a read-only bind with `nosuid`,
>   `nodev` and `noexec`. Both calls succeeded. The bootstrap's mount table
>   (which the payload sees) then had no propagation tag on any line, and
>   `/input` read `ro,nosuid,nodev,noexec`. The launcher's mount namespace,
>   which `/proc/1/root` reaches, kept `ro,nosuid,nodev` and `master:1` for
>   `/input`, because the bootstrap cannot change it. Through that path the
>   input is still read-only, and its files have no execute permission. The
>   same holds for the tool root, which is executable by design.
> - **No unsafe code.** Every call above has a safe interface in `rustix`
>   once its `fs` feature is on. That feature adds no crate.
>
> Staging into the scratch through the keeper worked too: the payload read
> the file and could not append to it. It is not chosen. The payload owns
> what is in the scratch, so only a read-only mount keeps a later profile
> from changing its own input. R02af works as follows.
>
> - **The input.** The provider stages a pinned revision's files, given as
>   a `FilePackage` with a listing of each file's path, length and SHA-256.
>   It refuses the input when the listing and the files differ in a path, a
>   length or a digest. The input's bytes are dropped once they are written.
>   Only the listing and its digest stay with the attempt, so staging and
>   collection never hold more than one package's bytes at once (0042,
>   rule 5). Where the files come from (the store, access checks
>   and the pinned revision) is R02b's (rule 15).
>   - The staging directory is the provider's own, inside its execution
>     directory, made fresh for each attempt under the admission lock. A
>     leftover of an earlier attempt is removed before staging.
>   - Files are written once, without following links, beneath a handle on
>     that directory, with mode `0444`. The directories get mode `0555`
>     after they are filled.
>   - The provider passes the directory's path to the stub, which puts it in
>     the launcher's fixed argument list as `--ro-bind <path> /input`. The
>     path is the provider's, never a request's.
>   - The directory is removed when the attempt ends, on every path. Its
>     directories are made `0700` again first, without following links,
>     since an owner cannot unlink entries in a `0555` directory. The same
>     applies to a leftover of an earlier attempt. A leftover that cannot be
>     removed refuses the attempt under a reason of its own, before
>     anything is launched.
>   - The bootstrap, in its own mount namespace and after the scratch, makes
>     its whole mount tree private and remounts `/input` as a read-only bind
>     with `nosuid`, `nodev` and `noexec` (0042, rule 4). Its readback, and
>     the provider's read of the mount table, require these four flags on
>     `/input` and no propagation tag on any line.
> - **The keeper.** Between its observation and the release, the provider
>   opens the scratch through `/proc/<bootstrap>/root/tmp`, close-on-exec.
>   - It requires the descriptor to be on a tmpfs, with the mount id of the
>     scratch line it has just checked. After opening it, the provider reads
>     the group's members again and requires the bootstrap's PID among them,
>     still in filter mode. A bootstrap that died, whose PID or mount id was
>     reused, fails one of these checks, since nothing outside the attempt
>     can join its group. Otherwise the attempt is not started.
>   - It makes the output root `/tmp/out` (mode `0700`) through that handle
>     and keeps a handle on it. The stub was started before either handle
>     was opened, so no process of the attempt can inherit one.
>   - Only the output root is collected; the rest of the scratch is ignored.
> - **Collection.** Collection runs only after the attempt's verdict has
>   passed: exit status 0, cleanup observed, no limit exceeded. Any other
>   verdict, and every failure before it, drops the handles unread, and no
>   output exists. Collection traverses from the output root's handle.
>   - Each entry is first classified by `statat` without following links,
>     relative to its directory's handle. That type names the reason for a
>     refusal: a link, a socket, a FIFO or a device.
>   - Only a directory or a regular file is then opened, with the four
>     resolve flags above, `O_NOFOLLOW` and `O_NONBLOCK`. The type that
>     `fstat` gives on the opened descriptor must equal the first one;
>     otherwise the entry is refused as changed. An entry that cannot be
>     opened is refused as unreadable.
>   - A directory is entered through its descriptor.
>   - A regular file must have one link. It is read once, through its
>     descriptor, with a bound of one byte over the file limit, and the
>     bytes read must match the size `fstat` gave.
>   - The traversal reads at most 2,048 entries in all: 256 files, each
>     under up to seven directories of its own. It enters no directory
>     deeper than seven levels below the output root, since a package path
>     has at most eight segments. It stops at the first excess, so a
>     rejected tree is bounded too.
>   - The collected files must form a valid `FilePackage`: portable paths,
>     no case collision, at most 256 files, 5 MiB each and 25 MiB in all.
>   - The refusal names the first reason found, and the attempt yields
>     nothing.
> - **The manifest.** A collected package has a manifest, a typed record
>   whose digest is computed over canonical text. Every field is printable
>   ASCII without tabs or newlines: package paths by their rules, identities
>   by construction, numbers in decimal and digests in lowercase hex. One
>   field per line, tab-separated, ending in a newline, so no framing is
>   ambiguous.
>   - The input listing's digest is the SHA-256 of
>     `artifree-input-listing-v1`, then one line `<path>\t<length>\t<sha256>`
>     per file in path order.
>   - The output manifest holds the operation profile's identity, the
>     SHA-256 of the profile's manifest, the SHA-256 of the tool root's
>     manifest, the filter's digest, the input listing's digest, and each
>     collected file's path, length and SHA-256 in path order. Its digest is
>     the SHA-256 of `artifree-output-manifest-v1`, then the lines `profile`,
>     `profile-manifest`, `tool-root`, `filter` and `input`, each with its
>     value, then one line `file\t<path>\t<length>\t<sha256>` per file.
>   - R02b adds the actor and its generation, the artifact, the revision, the
>     job and the fence, which it has, under a new version tag. A digest
>     under one tag is never compared with one under another.
> - **Releasing the mount.** The provider drops both handles when collection
>   ends, which frees the tmpfs. No mount outlives the attempt (0042,
>   rule 5).
> - **Races.** Collection starts after the group is empty. The bootstrap's
>   mount namespace is gone, and the keeper's handles are the only way into
>   the scratch, so no process can change the tree during collection.
>   0042's same-size replacement and link races are proved where they can
>   happen: by unit tests of the collector on a temporary directory, with a
>   hook that swaps an entry between listing and opening.
> - **The tests.** These run in the job:
>   - Success: a synthetic input becomes the expected bytes and manifest,
>     whose digest the test recomputes from the canonical text,
>     the staging directory is gone afterwards, and `Shmem` returns to its
>     value before the attempt within 16 MiB, after a payload that wrote
>     64 MiB.
>   - The input: creations, unlinks and new directories in `/input` fail
>     with `EROFS` exactly, and appends with `EACCES`, also through
>     `/proc/1/root`. The staged bytes do not change. `/input` is
>     `noexec` in the payload's mount table.
>   - Payload-made hostile trees: a socket, more than 256 files, a file over
>     5 MiB, more than 25 MiB in all, a directory bomb over the entry bound
>     (more than 2,048 entries, within the scratch's 4,096 inodes),
>     directories deeper than the bound, an invalid name, a case collision
>     and an unsupported extension. Each is refused by its reason.
>   - Planted trees: a test-only step of the plan, before the release, adds a
>     relative symbolic link, a hard link and a FIFO through the keeper.
>     Each is refused by its reason.
>   - Verdicts: exit 0 with invalid output yields nothing, a nonzero exit
>     with valid output is not collected, and a fresh attempt after a
>     refused one succeeds.
> - **Unit tests.** These run without the job: input verification, the mount-id comparison, the
>   collector's bounds and the races, and a device refused by type (through
>   `/dev/null`, since a device cannot be made in the scratch).
> - **The mutations.** Each must fail its test in the job:
>   - The collector following links (resolve flags and `O_NOFOLLOW`
>     removed) with its type check removed. The relative link is admitted.
>   - The link-count check removed. The hard link is admitted.
>   - The type check removed. The FIFO is admitted as an empty file.
>   - The entry bound removed. The directory bomb is admitted.
>   - The depth bound removed. The deep empty directories are admitted.
>   - The input bound with `--bind`, with the mount checks kept. The attempt
>     is refused before the release (`NoReadiness`).
>   - The input bound with `--bind`, the bootstrap's remount removed and
>     both mount checks bypassed. A creation in `/input` fails with
>     `EACCES` instead of `EROFS`, which fails the input test.
>   - The keeper dropped before the group is emptied. Success finds no
>     output.
>   - The handles kept after collection. `Shmem` does not return.
>   - Collection after a nonzero exit. Output is collected.
>
> The rules, reworded:
>
> - **Rule 2.** The fixed argument list binds the attempt's staging
>   directory read-only at `/input`. Its path comes from the provider.
> - **Rule 3.** After the scratch, the bootstrap makes its mount tree private
>   and remounts `/input` read-only with `nosuid`, `nodev` and `noexec`. Its
>   readback, and the provider's read of the mount table, require these
>   flags and no propagation tag.
> - **Rule 5.** Between its observation and the release, the provider opens
>   the keeper, checks it and makes the output root. It collects only after
>   a passing verdict, and drops the keeper on every path.
> - **Rule 7.** `rustix` also enables `fs`, for the keeper, staging and
>   collection.
> - **Rule 15.** R02b also takes the input's origin (the store, access
>   checks and the pinned revision) and the rest of the manifest's binding
>   (actor and generation, artifact, revision, job and fence).
> - **Rule 14.** The R02af row names the staging, the keeper, the
>   collection, its tests and its mutations.
> - **Rule 17.** The R02af point is settled by this amendment.

> **Amendment, 2026-11-13 (crashes).** Rule 17 left to R02ag to read the
> runner kernel's coredump code before the slice relies on it. 0042 asks
> for crashes after `fork` and `exec`, a weakened limit, pipe and socket
> routes with counters, and a routing change that stops admission. It does
> not say where a core would land, how the test looks there, or how a change
> is noticed during an attempt. Temporary branches `probe/r02ag` and
> `probe/r02ag-src` (deleted afterwards and never merged) measured this on
> `ubuntu-24.04` with shell commands, Bubblewrap 0.9.0 and the archive's
> source packages, in workflow runs 43400815963 (commit `8fb1312`),
> 43400994555 (commit `5838e1c`), 43403602393 (commit `14ce0e3`) and
> 43403783210 (commit `eacdf01`). No payload of this repository ran. The
> shell's `ulimit -c` counts blocks of 512 bytes. The findings:
>
> - **The kernel's source.** The hosted runner boots either an `-aws` or an
>   `-azure` build of Ubuntu's 7.0 kernel; the probes saw `7.0.0-1014-aws`
>   and `7.0.0-1017-azure` (`Ubuntu 7.0.0-1017.17~24.04.1-azure 7.0.14`).
>   The source packages of `linux-azure-7.0` (`7.0.0-1017.17~24.04.1`) and
>   `linux-aws-7.0` (`7.0.0-1017.18~24.04.1`) are in source format 1.0. In
>   both, `fs/coredump.c` and `fs/binfmt_elf.c` of the original tarball have
>   the SHA-256 of upstream tag `v7.0`, and Ubuntu's diff, which touches
>   5,832 files, touches neither. So the code read is the code those kernels
>   build from these two files.
> - **File routing.** `coredump_file` skips the dump when the core limit
>   is below the format's `min_coredump`, which is `ELF_EXEC_PAGESIZE`
>   (4,096 bytes) for ELF. With `kernel.core_pattern` set to `core`, limits
>   of 0 and 2,048 bytes left no file after `SIGSEGV` or `SIGABRT`; 4,096
>   bytes left a 4,002-byte `core`, and 512 KiB a 475,136-byte one. A child
>   of a shell with limit 0 left no file either, and a child could not raise
>   a hard limit of 0 (`EPERM`).
> - **Where a file core lands.** The kernel opens the name through the
>   crashing task's own root and working directory, with `O_CREAT`,
>   `O_EXCL` and `O_NOFOLLOW`. It then requires a regular file with one
>   link, owned by the task's file-system user and with mode `0600`. Inside
>   Bubblewrap, with a tmpfs at `/tmp` as the working directory, both `core`
>   and `/tmp/core-abs` landed in that tmpfs and nowhere on the host. With
>   the read-only `/` as the working directory, `core` left nothing. A
>   process with no capabilities, under its own user, still dumped.
> - **Pipe routing.** With `|<collector> %p %c`, a crash with limit 0 ran
>   the collector, which read 466,944 bytes and was given the limit 0. The
>   kernel sets the limit to infinity for the pipe. Only the limit of
>   exactly one byte, the kernel's guard against a crashing collector,
>   stopped it. A first counter in `/tmp` stayed empty: the kernel's
>   protection of files in sticky directories refused the collector's write.
> - **Socket routing.** With `@/tmp/core.sock`, a listener received 475,136
>   bytes from a crash with limit 0, and the same with a limit of 512 KiB.
> - **The runner as found.** `kernel.core_pattern` pipes to
>   `systemd-coredump`, `fs.suid_dumpable` is 2 and `kernel.core_pipe_limit`
>   is 16. After a crash with limit 0, `coredumpctl` listed a stored core.
>   With `fs.suid_dumpable` at 2, the kernel logs a warning when a relative
>   pattern is written. It refuses relative names only for dumps in its
>   set-id safe mode, which the payload never enters.
> - **A unit's limit.** `systemd-run -p LimitCORE=1M` gave the unit's
>   process soft and hard limits of 1 MiB; `LimitCORE=0` gave 0.
>
> So 0042's admission holds on these kernels: file routing with both limits
> at zero writes nothing, while pipe and socket routing receive the memory
> whatever the limit. R02ag works as follows.
>
> - **The payload's working directory.** After it mounts the scratch, the
>   bootstrap makes the scratch root `/tmp` its working directory, and its
>   readback requires that. The payload inherits it in every scenario. A
>   core under file routing would land in the scratch, never on the host
>   and never in the read-only root. The filter is unchanged: `chdir`,
>   `fchdir` and `getpid` stay refused.
> - **The crash fixture.** New payload scenarios. The payload learns its own
>   PID from `/proc/self/stat`, since `getpid` is refused. It starts each
>   child with the payload in a child role, which forks and `exec`s in one
>   step. The parent knows the child's PID from that.
>   - `crash`: the payload prints its core limits from `/proc/self/limits`,
>     writes 8 MiB of a synthetic pattern into memory and starts a child.
>     The child prints its limits again, writes 8 MiB of its own and sends
>     itself `SIGSEGV` twice. Rust's runtime handles `SIGSEGV` to detect
>     stack overflows; for a signal that is not a fault in a guard page its
>     handler restores the default action and returns, so the second signal
>     takes the default action. The parent waits, prints the child's signal
>     and the kernel's core flag from the wait status, and then sends itself
>     `SIGABRT`, which the runtime does not handle.
>   - `crash-setter`: the same up to the children. One child, after `exec`,
>     asks `prlimit64` to set its core limit to 1 MiB. Another asks `prctl`
>     to make it dumpable. The filter kills each with `SIGSYS`, which dumps
>     core when a limit allows it. The parent prints both children's
>     signals and core flags, then sends itself `SIGSEGV` twice.
>   - The signals are sent with `kill`, since a fault needs `unsafe` code.
>     They take the same dump path as a fault. The test requires death by
>     the named signal from the wait status, so a handler that kept a child
>     alive fails the test.
> - **The test's view.** The keeper keeps its handle on the scratch beside
>   the output root, and drops both when the attempt ends, as R02af's
>   release of the mount says. A test-only step of the plan, like the
>   planted trees of R02af, lists the whole scratch through that handle after
>   the group is empty and before the keeper is dropped, whatever the
>   verdict. It reads names, types and sizes only, with R02af's resolve
>   flags and bounds. The attempt still yields no output, since its exit
>   status is not 0.
> - **Routing during the attempt.** The provider keeps the exact bytes that
>   admission read from `kernel.core_pattern` (not the report's text). It
>   reads the file again at each poll of the stub and once more after the
>   group is empty. Each read is fresh, so the one-second lifetime of
>   admission's observation does not apply. A read that fails is
>   `UnreadableCoreRouting`. Bytes that differ are `CoreRoutingChanged`,
>   whatever the new value is. In either case:
>   - the provider ends the group as at the wall deadline;
>   - it quarantines the provider in its process, so every later launch in
>     that process is refused;
>   - the attempt yields no output and is refused with that reason.
>     The in-process quarantine is not 0042 rule 6's. That one refuses
>     admission for the data directory across restarts and reports that
>     operator action is required; it is the persisted quarantine that is
>     R02ah's, and R02ah takes this refusal into it. Polling cannot close the
>     interval between a change and a crash in it; 0042 already says so, and
>     the operator drains execution before changing the routing.
> - **The counters.** The preparation gains two routings, each with a
>   counter in a root-owned directory outside `/tmp`, which only root can
>   write:
>   - a pipe to a collector of the job's own
>     (`/opt/artifree-enforcement/collector`, root-owned), which adds a line
>     to its count file for each run;
>   - a socket that a listener started by `run.sh` serves, which adds a line
>     for each connection.
> - **The tests.** These run in the job, in a unit whose core limit is 1 MiB
>   (`-p LimitCORE=1M`), so a missing guard cannot pass because the parent
>   already had a limit of zero. Each test first reads its own limit and
>   requires 1 MiB.
>   - Crashes: the `crash` and `crash-setter` scenarios under file routing
>     (`core`). The test checks, in this order: a scratch listing with no
>     entry but the output root, which is empty; the core flag clear for
>     every child; no new entry under `/var/lib/systemd/coredump`; the
>     expected signals; limits of 0 before and after `exec`; the child's
>     line after `exec`; and cleanup observed. A broken limit guard thus
>     fails on the core file itself.
>   - The counters' routes: under each of the pipe and the socket routing,
>     a launch with the `crash` scenario is refused as `PipeCoreRouting` or
>     `SocketCoreRouting` before any group exists, and the counter stays at
>     zero. With the refusal missing, the payload would crash with limit 0
>     and the counter would move. `run.sh` then makes one trusted crash with
>     limit 0 and requires the counter to be one, so the counter is shown
>     to work, and to receive a dump that a limit of zero did not stop.
>   - A routing that cannot be admitted: an absolute pattern
>     (`/var/tmp/core`) is refused as `UnknownCoreRouting` before any group
>     exists.
>   - A change: a scenario `linger` sleeps 15 seconds and exits 0. While it
>     sleeps, `run.sh` sets the pipe routing, once it sees the attempt group.
>     The attempt is refused as `CoreRoutingChanged` and its group is gone.
>     The test then writes a marker, `run.sh` sets the routing back to
>     `core` and answers with another, and the test's second launch in the
>     same process is refused as quarantined. So the second refusal can come
>     only from the quarantine.
>   - An unreadable routing has unit tests through a failing reader. A real
>     `/proc` entry cannot be made unreadable for the job.
> - **Unit tests.** These run without the job: the comparison during the
>   attempt (same, changed, unreadable) through a fake reader, the
>   bootstrap's working-directory check, and the parser of the payload's
>   crash lines.
> - **The mutations.** Each must fail its test in the job:
>   - The core limit guard removed: the bootstrap neither sets nor checks
>     the core limit, and the provider's check of it is bypassed. A core
>     appears in the scratch and the core flag is set.
>   - The pipe refusal removed. The pipe launch is not refused, and the
>     counter moves.
>   - The socket refusal removed. The socket launch is not refused, and the
>     counter moves.
>   - The check during the attempt removed. The `linger` attempt passes.
>   - The quarantine on a change removed. The second launch is admitted.
>   - `prlimit64` allowed as a setter. The setter child is not killed by
>     `SIGSYS` (its hard limit of zero still refuses the raise).
>
> The rules, reworded:
>
> - **Rule 3.** After the scratch, the bootstrap makes `/tmp` its working
>   directory, and its readback requires it.
> - **Rule 5.** The provider reads the routing again at each poll and after
>   the group is empty. A change or a failed read ends the group,
>   quarantines the provider in its process and refuses the attempt; the
>   persisted quarantine stays R02ah's.
> - **Rule 9.** The preparation also sets the job's pipe and socket
>   routings with their counters, and an absolute pattern, each taken back
>   afterwards.
> - **Rule 13.** The environment record also holds the pipe and socket
>   counters, before and after the trusted crash, and the job fails
>   without them.
> - **Rule 14.** The R02ag row names the working directory, the crash
>   fixture, the counters, the change during an attempt, its tests and its
>   mutations.
> - **Rule 17.** The R02ag point is settled by this amendment, for the
>   `-aws` and `-azure` 7.0 kernels whose source was read.

> **Amendment, 2026-11-13 (the guardian).** The R02ah row names the
> guardian, the death of the supervisor at each phase, the attempt registry
> and the quarantine, and renewals against a stand-in for the store. It does
> not say what the guardian is, how it learns that the supervisor is gone,
> what a renewal carries, how a test kills a supervisor without the service
> manager cleaning up for it, or how a phase is reached. That is too much for
> one PR, so this amendment splits the row: R02ah is the guardian, its
> renewals and the supervisor's death; the new row R02ai is the attempt
> registry, its reconciliation and the persisted quarantine. The temporary
> branch `probe/r02ah` (deleted afterwards and never merged) measured the
> following on `ubuntu-24.04` (`7.0.0-1017-azure`) with shell commands,
> Python's standard library and Bubblewrap 0.9.0, in workflow run 43440918474
> (commit `1cfe1cd`). No payload of this repository ran.
>
> - **A unit whose main process is killed.** A unit with `Delegate=yes`
>   started a main shell that made `supervisor` and `attempt-1` beneath its
>   group, moved itself into `supervisor`, put a `sleep` into `attempt-1` and
>   left a background shell, the guardian's stand-in, in `supervisor`. With
>   `KillMode=process`, a `SIGKILL` of the main process left the other two
>   running. The stand-in saw its parent gone, wrote `1` to the attempt's
>   `cgroup.kill`, read `populated 0` at its first look and removed the
>   group. With the default `KillMode=control-group` the service manager
>   killed the stand-in and the `sleep` itself, so the stand-in never acted.
> - **The launcher's parent.** A Bubblewrap with `--unshare-user`,
>   `--unshare-pid` and `--die-with-parent`, running two `sleep`s, left none
>   of them after a `SIGKILL` of the shell that started it.
> - **A thread that started the launcher.** A Bubblewrap started from a Python
>   thread was still running, with its `sleep`, two seconds after that thread
>   had ended while the process lived. That disagrees with the parent-death
>   signal as `prctl(2)` describes it, and the cause was not examined. The
>   comment in `launcher.rs` that keeps the spawning thread alive until the
>   child is reaped stays; nothing below relies on a thread's exit either way.
>
> So a guardian that outlives the supervisor can end the attempt, and a test
> must run the supervisor in a unit with `KillMode=process`, or the service
> manager does the guardian's work and the test proves nothing. R02ah works as
> follows.
>
> - **The guardian.** A new mode `guardian` of the fixture program in the
>   tool root runs a function of the provider, as the stub does (rule 4). The
>   provider starts it after the attempt group exists and before the stub. It
>   stays in `supervisor`, outside the attempt group, and is the provider's
>   child. Its arguments are the attempt group's name, its absolute deadline
>   as milliseconds of `CLOCK_MONOTONIC`, and the renewal window in
>   milliseconds. The deadline is fixed when it starts: that time plus the
>   readiness deadline, the wall deadline and 2 seconds, so it falls after
>   every deadline of the provider and an earlier test keeps its own refusal.
>   The guardian accepts only a name of the form `attempt-<digits>-<digits>`,
>   the provider's form, opens the parent of its own group (from
>   `/proc/self/cgroup`) and that name beneath it with `openat2` and
>   `RESOLVE_BENEATH`, `RESOLVE_NO_SYMLINKS` and `RESOLVE_NO_MAGICLINKS`, and
>   then writes one ready byte on its socket. The provider waits for that byte
>   within the readiness deadline before it starts the stub. A guardian that
>   does not report ready refuses the attempt as
>   `NotStarted(GuardianNotStarted)`, and the group is ended as before. It
>   never receives a credential, the input or the output.
> - **What it watches.** Its standard input is one end of a Unix stream pair
>   whose other end only the provider holds; both ends close on `exec`. Its
>   standard output is null. The provider sends `L` at the release, `R` for
>   each renewal, `S` once it stops polling the stub (the stub's exit seen or
>   the group being ended), and `D` once its group is gone. End of file
>   without `D`, from the supervisor's death or a closed socket, ends the
>   attempt at any time, and so does the absolute deadline. Between `L` and
>   `S`, the renewal window passing without an `R` ends it too; before the
>   release the readiness deadline bounds the attempt, and after `S` the
>   provider's teardown, both with the absolute deadline behind them. No
>   renewal, record or live supervisor extends the deadline. The guardian ends
>   the attempt by writing `1` to `cgroup.kill` and reads `cgroup.events` for
>   `populated 0` for at most five seconds. It removes nothing: the provider's
>   teardown removes the group as before, and a group left by a dead
>   supervisor stays for R02ai's reconciliation. It then writes one line,
>   `guardian <reason> emptied=<0|1>` with the reason `supervisor-gone`,
>   `renewal-expired` or `deadline`, first on the standard error it inherits
>   from the provider, so that the line survives a dead supervisor, and then
>   on the socket, ignoring a failure of either write, and exits. After `D`
>   it exits without a line.
> - **Renewals.** The plan gains the job's lease, and the renewal window is a
>   third of it (0042 rule 6). The provider asks the store for a heartbeat at
>   the release and then at the first poll after half a window has passed
>   since the last heartbeat it asked, and sends `R` only after an answer that
>   confirms the current fence, no cancel, the pinned actor generation and
>   continuing authority. Every other answer stops the renewals, ends the
>   group as at the wall deadline and refuses the attempt as `LeaseLost` with
>   the part that failed. A heartbeat that fails because the store is
>   unavailable sends no `R` and is asked again half a window later, as 0030
>   rule 12 retries a renewal at its next checkpoint; the guardian ends the
>   attempt when the window passes. The store is a stand-in in the test build:
>   a function of the plan that answers each heartbeat by its number, at
>   once. The fixture plan's stand-in confirms every heartbeat. The real
>   store's heartbeat, which must answer within a bound shorter than half a
>   window, is R02b's.
> - **The provider's view.** After its group is gone the provider sends `D`,
>   reads the guardian's line from the socket if there is one, and waits for
>   the guardian to exit, for at most the cleanup observation deadline. A
>   guardian still running then is killed and reaped, and the attempt is
>   refused as `CleanupNotObserved`, since a process outside its group would
>   otherwise occupy `supervisor` at the next admission. An attempt that the
>   guardian ended is refused as `Guardian` with the reason, even when the
>   stub's exit was also seen; a cleanup that was not observed keeps
>   `CleanupNotObserved` first, and a routing change keeps `Routing`, as
>   R02ag ranks them.
> - **Phases, for the tests.** A test-only step of the plan stops the
>   supervisor at a named phase: it writes a marker named after the phase and
>   then either waits until killed (`hold`) or waits for a given time and goes
>   on (`stall`). The phases are `guarded` (the group and the guardian exist,
>   the stub does not), `started` (the stub runs, before the readiness report
>   is read), `ready` (the report was read, before the release), `running`
>   (after the release) and `collecting` (after a passing verdict and `D`,
>   before collection). A test-only switch also leaves the guardian out, so
>   the launcher's own parent-death signal can be shown alone, and another
>   makes the provider send `R` after every heartbeat whatever its answer and
>   never end the group on it: the faulty supervisor of 0042's acceptance.
> - **The death of the supervisor.** `run.sh` runs a supervisor test in a unit
>   of its own with `Delegate=yes` and `KillMode=process`, waits for the
>   phase's marker and sends `SIGKILL` to the unit's main process. Within ten
>   seconds it requires that no payload, probe or guardian process is left.
>   At `guarded`, `started`, `ready` and `running` it also requires an attempt
>   group beneath the unit's group that is empty (`populated 0`) and the
>   guardian's line naming `supervisor-gone` and `emptied=1` in the unit's
>   output. At `collecting` it requires no attempt group and no guardian line,
>   since the group was gone and the guardian had exited before the phase.
>   At `running`, without the guardian, it requires that no payload process
>   is left, through the launcher's `--die-with-parent`. `run.sh` then removes
>   the empty group that is left, since only R02ai's reconciliation would
>   remove it, and stops the unit.
> - **The tests in one process.** These run in the job, each with its own
>   execution directory, a lease of 3 seconds and so a window of 1 second:
>   - A stand-in that confirms twice and then reports the fence revoked: the
>     attempt of the `linger` scenario is refused as `LeaseLost(Fence)` within
>     3 seconds of the release, no group is left, and the guardian ended with
>     `D`. The same for a cancel, a changed generation and lost authority.
>   - A stand-in that is unavailable from the first heartbeat: the attempt is
>     refused as `Guardian(RenewalExpired)` within 3 seconds of the release.
>   - A supervisor stalled at `running` for 4 seconds: the stand-in confirms,
>     but no heartbeat is asked while the supervisor stalls, so the guardian
>     ends the attempt and the provider, on going on, refuses it as
>     `Guardian(RenewalExpired)`. Local liveness alone keeps nothing alive.
>   - The two cases of 0042's acceptance, each with the provider's own
>     deadline check switched off by a test-only switch and the `hang`
>     scenario: a faulty supervisor that keeps sending `R` after the stand-in
>     reports the fence revoked, and a stand-in that confirms every heartbeat.
>     Each attempt is refused as `Guardian(Deadline)` after at least 120 and
>     less than 135 seconds from the release.
>   - The fixture plan still yields its output, and every earlier test passes
>     with the guardian present.
> - **Unit tests.** The guardian's name check and its decision (end of file,
>   `L`, `S`, window, deadline, `D`) as a pure function of the times, the
>   parser of its line, and the provider's renewal decision over a table of
>   answers and times.
> - **The mutations.** Each must fail its test in the job:
>   - The guardian not started: the stalled supervisor and both 120-second
>     tests pass the guardian's deadline.
>   - The guardian's deadline check removed: both 120-second tests.
>   - Its window check removed: the unavailable store and the stalled
>     supervisor.
>   - End of file ignored: the death of the supervisor at `guarded`, where no
>     launcher exists yet.
>   - The provider's refusal on a non-confirming answer removed, so that it
>     keeps polling and renewing: the revoked fence, whose `linger` attempt
>     then exits 0.
>   - `--die-with-parent` removed, with the guardian left out: the death of
>     the supervisor at `running`.
>
> R02ai is the attempt registry of 0042 rule 6 under the execution directory,
> its reconciliation before a new admission (including a changed boot), the
> persisted quarantine that refuses admission for the data directory and
> reports that operator action is required, and the death of the supervisor
> at the registry and group-creation phases, which leave a record or an empty
> group with no guardian. The earlier amendments' "the persisted quarantine
> is R02ah's" now reads R02ai's. Its own amendment plans it before its code.
>
> The rules, reworded:
>
> - **Rule 5.** After the attempt group exists, the provider starts the
>   guardian and waits for its ready byte before the stub; it renews the
>   guardian only after a confirming heartbeat, and sends `D` after the group
>   is gone.
> - **Rule 9.** The job also runs the supervisor tests in a unit with
>   `KillMode=process` and kills its main process at each phase.
> - **Rule 14.** The R02ah row is reworded and the R02ai row added, as below.
> - **Rule 15.** R02ai adds the ownership record of 0042 rule 6.
> - **Rule 17.** A new R02ah point is settled by this amendment.

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

> **Note, 2026-11-13 (R02ac).** The launch now has the shape of the
> amendment of 2026-11-12: the launcher makes the scratch's mount point,
> remounts the root and `/dev` read-only and grants the two capabilities;
> the bootstrap mounts the scratch in its own mount namespace (`nosuid`,
> `nodev`, `noexec`, mode 700, 128 MiB, 4,096 inodes) before the limits; and
> the provider reads the bootstrap's mount table before the release, beside
> the facts it read before. The payload fixture now takes one scenario from a
> file of the tool root, which the job's preparation sets and the manifest
> covers. In the job, under that launch: threads and processes stop at the
> task limit; a memory hog is killed with its whole group and reported as
> out of memory; the scratch takes 128 MiB and 4,095 files and nothing
> outside it is writable; a log flood ends at the output budget; a hang ends
> at the wall deadline after the release; four spinning threads are
> throttled to one CPU; and a double-forked child in its own session, a
> background writer and a child that ignores `SIGTERM` leave no process
> behind after a normal exit or the deadline. Each test also reads `/proc`
> after the launch, while the job's unit still runs, so the absence of
> survivors does not rest on systemd ending the unit.
>
> What the slice settled: the filter is two programs. The first answers
> `clone3` with `ENOSYS`, so the C library falls back to `clone`, whose
> flags a filter can read; the allowlist, installed last because installing
> needs calls it refuses, admits `clone` only without namespace flags.
> The launcher's init takes its PID namespace with it, but the launcher can
> exit before the kernel has ended the rest of that namespace: in every
> measured run of the slice the group's member list was empty when the
> stub's exit was observed, and then, on the first run on `main`, a process
> bomb's children were still listed. So the provider reads the list again
> until it is empty, for a grace of two seconds from its first read and
> never past the wall deadline; a list that is still non-empty then counts as survivors and fails the attempt.
> `populated` is not used, since it stays set for some milliseconds after an
> ordinary exit. The grace is a margin, not a signal that the teardown has
> ended: a teardown slower than two seconds would fail a correct attempt as
> survivors, a known limit. The task limit and the CPU quota are reported as
> counters of the evidence, not as failures: a payload that meets the task
> limit sees `EAGAIN` and may still finish, and throttling slows it. Every
> outcome carries the group's counters when the group was emptied. Limits:
> the payload syscalls added to the allowlist are what the fixture's
> scenarios measured on the runner, and the public mapping stays R02ad's; the
> three new facilities are not probed at admission, as the amendment says;
> the persisted quarantine stays R02ah's.

> **Note, 2026-11-13 (R02ad).** The public mapping exists as
> `docs/linux-syscall-mapping.md`, kept in step with the probe table by a
> unit test. The test-support program generates 94 probe programs, which the
> job's preparation installs under `probes/` in the tool root and the
> manifest covers. In the job: one launch of the scenario `probes` runs every
> probe after a `fork` and an `exec`, and the test requires exactly the
> expected seccomp records in the kernel log window, with the four allowed
> controls succeeding and leaving none; the `clone3` probe as the payload
> exits with `ENOSYS`, and the same probe outside the sandbox gets `EINVAL`;
> a thread started after `exec` and a `prlimit64` on the payload's own child
> are refused with their records; a lower and a restore of the soft
> descriptor limit is refused; descriptors stop at 256 in the payload and in
> a child after `exec`; and `/proc` shows the boundary in every task. The
> preparation sets the two kernel-log settings, requires that no audit
> daemon runs, and records both in the run's environment. The settings are
> not restored, because the runner is disposable.
>
> One correction to the amendment: the thread's and the descendant's
> refusals do not run in the probes' launch. The filter kills the payload
> on each, so each has a launch and a test of its own, with the same
> attribution rule. As the amendment says, the x32 class fails this
> kernel's acceptance and stays an owner decision. The mutations ran on a
> temporary branch, never in the product; the mapping records their
> results.

> **Note, 2026-11-13 (R02ae).** The fixture profile's filter allows the
> local networking of the amendment: `socket` only in ten forms (Unix
> stream and datagram with protocol 0; IPv4 and IPv6 stream with protocol 0
> or TCP and datagram with protocol 0 or UDP; the close-on-exec and
> non-blocking flags allowed), `setsockopt` only for `SO_REUSEADDR` at the
> socket level, and `bind`, `listen`, `accept4`, `connect`, `sendto`,
> `recvfrom`, `getsockname` and `shutdown`. The mapping has a `network`
> class of 19 refused probes and six allowed controls. The attempt's
> arguments mount an empty read-only tmpfs over `/dev/pts` in the
> launcher's mount namespace, which the bootstrap's inherits; the mount
> checks of the bootstrap and of the provider require it as the last
> `/dev/pts` mount, with its size and mode bounds. The preparation
> writes the runner's IPv4 address into the tool root under the manifest.
> Six job tests run in the job's part 8: real host listeners on TCP, UDP, a
> Unix path and an abstract name keep their counters through a launch whose
> payload tries them, and the host reads the payload's namespace from
> outside; canary files in `/var/tmp` and the runner's home and a canary
> process stay intact while the payload reads, creates, sweeps every PID
> with signal 0 and sends `kill(-1)`; `/proc/sys` writes fail and
> `/dev/ptmx` and both masters through `/proc/1/root` give `ENOENT`; a
> real leak of a directory, a connection and a pipe is refused before READY; a live preload canary, built in the job,
> logs the test binary and neither the stub nor the launcher, and the payload
> sees no variable (inside the sandbox neither the canary nor the host's
> `/tmp` is mounted, so the log cannot show the bootstrap or the payload); and a marker written in one attempt is absent in the
> next. The tests append the listener counters and the canaries'
> observations to the environment record, and the job fails without them.
> The payload fixture's `kill(-1)` uses nix's `signal` feature, enabled
> only for the provider's test targets. The mutations ran on a temporary
> branch, never in the product; the mapping records their results.

> **Note, 2026-11-13 (R02af).** The input and output of the amendment exist,
> compiled only into the test build. The provider checks a `FilePackage`
> against its listing, stages it read-only in its execution directory and
> passes the path to the stub, which binds it at `/input`. The bootstrap
> makes its mount tree private and remounts `/input` with `noexec`, and both
> mount checks require that. Between the observation and the release the
> provider opens the keeper through `/proc/<bootstrap>/root/tmp`, checks its
> filesystem and mount id, reads the group's members again and makes
> `/tmp/out`. After a passing verdict it collects beneath that handle and
> computes the manifest under `artifree-output-manifest-v1`; on every other
> path it drops the handles unread. An empty directory is checked as a
> package path too, so a hidden or case-colliding directory name is refused
> although it holds no file. Unit tests cover the listing, staging and its
> removal, the mount-id comparison, the collector's bounds and refusals, a
> device through `/dev/null` and the two races. Fifteen job tests run in the
> job's part 9 with scenarios of the payload fixture: the copy with its
> manifest and `Shmem` after 64 MiB, the input's `EROFS` and `EACCES` both
> directly and through `/proc/1/root`, ten payload-made hostile trees, a
> planted link, hard link and FIFO, a failed attempt and a fresh one after a
> refusal. The output root takes one of the scratch's 4,096 inodes, so
> R02ac's inode test now expects 4,094 files beside the root and `out`.
> The amendment expected the `--bind` mutation with the mount
> checks kept to be refused before the release. The bootstrap's remount
> makes `/input` read-only in its own namespace whatever the launcher
> binds, so both checks pass; the input test fails instead, because a
> creation through `/proc/1/root/input` gives `EACCES` from the `0555`
> directory rather than `EROFS`. Under the depth mutation the deep empty
> directories are not admitted either: the directory-name check refuses them
> as `InvalidPath`, since their placeholder path has more than eight
> segments, so the deep test still fails. The ten mutations ran on the
> temporary branch `mutation/r02af` (run 43394408071, deleted afterwards and
> never merged), never in the product, and each failed its test: following
> links, the link count, the type check and the entry bound through the
> planted and bomb trees; the depth bound as above; both `--bind` mutations
> through the input test (the unchecked one already at a direct creation);
> the keeper dropped early (no output); the handles kept (`Shmem` grew by
> 65,456 KiB); and collection after a nonzero exit.

> **Note, 2026-11-13 (R02ag).** The crash acceptance of the amendment exists,
> compiled only into the test build. After the scratch the bootstrap makes
> `/tmp` its working directory and reads it back. The report keeps the exact
> routing bytes that admission read, and the attempt compares a fresh read
> with them at each poll and, once the group was observed gone, after it; a
> change or a failed read is refused as `Routing`, sets the in-process
> quarantine and yields no output. A group that was not observed gone keeps
> `CleanupNotObserved` as its reason, since it is not empty. The keeper keeps
> the scratch handle, and the plan's test-only listing reaches the test in the
> evidence, so the crash tests require an attempt that yields evidence. The
> listener's socket lies in the preparation's root-only directory: in v7.0's
> `net/unix/af_unix.c` the kernel looks a coredump socket up from init's root
> with its own credentials and checks no write permission, and the trusted
> crash reached it. The job's part 10 ran on `7.0.0-1017-azure` (run
> 43435830055): no core in the scratch, both children's core flags clear,
> SIGSYS for both setters, the pipe, socket and absolute routings refused
> before any group with both counters at 0 and 1 after the trusted crash, and
> the change refused as `CoreRoutingChanged`, then as quarantined. The six
> mutations ran on the temporary branch `mutation/r02ag` (run 43436517883,
> deleted afterwards and never merged), never in the product, and each
> failed its test: without the limit guard the scratch held a `core` of
> 1,175,552 bytes; without the pipe or the socket refusal the attempt ran and
> its collector received two dumps of about 8.9 MB; without the watch the
> `linger` attempt exited 0; without the quarantine the second launch ran;
> and with `prlimit64` allowed as a setter the setter child was not killed.
>
> **Note, 2026-11-14 (R02ah).** The guardian of the amendment exists,
> compiled only into the test build, with the provider's side, the stand-in
> store, the five phases and the test-only switches. Its deadline argument
> is milliseconds from the guardian's own start, read with a monotonic clock
> in the guardian, rather than absolute milliseconds of `CLOCK_MONOTONIC`:
> both name the same instant, and the relative form needs no clock
> conversion. The guardian reads at most one chunk of records before it
> decides by the time, so a peer that keeps writing cannot starve the
> deadline, and it reads a `cgroup.events` that is gone as an empty group,
> since the kernel removes only an empty group. The provider sends `S` once
> it no longer polls the stub, before the teardown grace, and sends `D` only
> when it removed the group; otherwise it closes its side of the socket, so
> that the guardian ends the attempt itself, and waits for the line one
> second past the cleanup observation deadline. In the job (run
> 43456839758, `ubuntu-24.04` image `20261111.370`) the revoked fence, the
> cancel, the changed generation and the lost authority were refused as
> `LeaseLost` with their part; the unavailable store and the stalled
> supervisor as `Guardian(RenewalExpired)`; the faulty supervisor and the
> confirmed renewals as `Guardian(Deadline)` after 132.6 seconds from the
> launch; and the fixture plan yielded its output. At each of the five
> phases, and at `running` without the guardian, no payload, probe or
> guardian process was left, and with the guardian its line named
> `supervisor-gone` and `emptied=1`. No attempt group was left at any phase:
> once its main process is gone the unit stops, and the service manager
> trims the empty groups of its delegated subtree, which the first run of
> the job showed at `guarded` (run 43454875474). So `run.sh` requires that
> every group that is left is empty and accepts a removed one, rather than
> one empty group. The six mutations ran as ten cases on the temporary
> branch `mutation/r02ah` (run 43456891505, deleted afterwards and never
> merged), with both unmutated baselines passing, and each failed its test:
> without the guardian the stalled supervisor's `linger` attempt exited 0,
> and both 120-second attempts were still running when their tests gave up
> after 150 seconds; without the guardian's deadline check both 120-second
> attempts were still running then too; without its window check the
> unavailable store's and the stalled supervisor's attempts exited 0; with
> end of file ignored a guardian outlived the supervisor's death at
> `guarded`; with the refusal on a lost lease removed the revoked fence's
> attempt exited 0; and without `--die-with-parent` and the guardian a
> payload process outlived the supervisor's death at `running`.

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
   cleared environment, read-only binds of the tool root and the staged input
   (the input at `/input`, from the attempt's staging directory, whose path
   comes from the provider; the amendment of 2026-11-13, input and output),
   the mount point of the scratch, a private `/proc` and a minimal `/dev`
   with an empty tmpfs over its `/dev/pts` (the amendment of 2026-11-13, the
   launcher's `/dev/pts`), the sandbox's root, `/dev` and `/dev/pts`
   remounted read-only, further user namespaces disabled inside, and the two
   capabilities the bootstrap needs for the
   scratch (`CAP_SYS_ADMIN` and `CAP_SETPCAP`, held in the innermost user
   namespace, which owns none of the launcher's mounts). The launcher keeps its own reaping init
   as the first process of the PID namespace; the payload is never that
   process. Nothing a request carries reaches the argument list.
3. **A trusted bootstrap runs inside the namespaces before the payload.** It is
   the launcher's direct child, under the launcher's init. In this order it:
   makes its own mount namespace, mounts the scratch there as one tmpfs with
   the flags of 0042, rule 4 and the size and the inode count of 0042, rule 5,
   and reads it back from its mount table with the launcher's empty
   read-only tmpfs over `/dev/pts` (the amendments of 2026-11-13, files,
   descriptors and network, and the launcher's `/dev/pts`); makes its mount
   tree private and remounts `/input` read-only with `nosuid`, `nodev` and
   `noexec`, and requires those flags in the same readback (the amendment
   of 2026-11-13, input and output);
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
   after the provider's release, which the provider gives only after it has
   read the bootstrap's state itself, its mount table included. Between that
   read and the release it opens the keeper of 0042, rule 7 on the scratch,
   checks it and makes the output root; it collects through the keeper only
   after a passing verdict, and drops it on every path (the amendment of
   2026-11-13, input and output). An attempt that never reported, or whose
   `exec` failed, counts as not started and as failed, never as a run.
6. **The tool root.** One directory per version holds the launcher, the
   bootstrap with the loader and libraries it needs to start, the payload's
   runtime and its libraries, with a manifest of every file's length and
   SHA-256 and the licences. The provider verifies the manifest before
   admission, refuses a root in which a file differs, and binds that directory
   read-only. In R02a the only payload is the repository's fixture and the
   tool root is assembled by the test harness at a fixed path, with the
   fixture's generated probe programs (the amendment of 2026-11-13); packaging a
   tool root for operators is R03's, when a real runtime exists. Until then no
   host is called supported outside the enforcement job.
7. **Three dependencies, for the system calls.** The bootstrap and the provider
   use `rustix` (resource limits, `prctl`, capabilities, descriptor handling,
   mounting, and with its `fs` feature the keeper, staging and collection of
   the amendment of 2026-11-13, input and output), `seccompiler` (compiling and installing the filter) and `nix`
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
      file name; let the runner's account read the kernel log and lift its
      message rate limit, with no audit daemon holding the audit channel (the
      amendment of 2026-11-13); then run the matrix inside a delegated unit as
      the runner's account.
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
    observation, the listener counters and the canaries' observations (the
    amendment of 2026-11-13, files, descriptors and network), as an artifact
    of the job.

### Slices

14. R02a is delivered in this order. Each slice passes `npm run check`,
    `npm run build` and the enforcement job, adds no job kind, no API and no
    caller command, and states in its PR which rows of 0042's matrix it now
    proves and which stay open. No slice runs a payload without the limits,
    the privilege drop and a default-deny filter of rule 3.

| Slice | What it adds                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       | 0042                  |
| ----- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | --------------------- |
| R02aa | The enforcement job with its three parts; the provider's admission: namespaces actually made, controllers written and read back, a group killed and seen empty, the routing policy, the kernel; every refusal of rule 10, for the tool root only a launcher or test-support program that is missing or not a regular executable file (the manifest's refusal is R02ab's). No payload.                                                                                                                                                                                                                                                                                                                                                                              | 1, 5 (routing), 8     |
| R02ab | The first launch: the tool root and its manifest, the stub and the handshake, the group's limits, the bootstrap with limits, privilege drop and a default-deny filter, the wall deadline, kill and the empty group, one attempt at a time by a crash-released lock. The fixture only exits.                                                                                                                                                                                                                                                                                                                                                                                                                                                                        | 3 (tools), 4, 5, 6, 8 |
| R02ac | The launch as amended on 2026-11-12 (read-only root and `/dev`, the scratch mounted and bounded by the bootstrap and read back by the provider), with its checks: file creation outside the scratch fails, and a launch without a read-only remount, without the inode count or without the provider's read of the mount table fails its test. Resources and cleanup under that launch: fork and thread bomb, memory, scratch bytes and inodes, log flood, hang, CPU; double fork, `setsid`, ignored signals, held pipes, a background writer.                                                                                                                                                                                                                     | 4 (scratch), 5, 6, 8  |
| R02ad | The public mapping from each system-call restriction to its filter entry, probe, independent observation and mutation, and the descriptor ceiling with its inheritance, setter and guard checks (the real-leak case of an inherited descriptor is R02ae's).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        | 4, 8                  |
| R02ae | Files, descriptors and network as amended on 2026-11-13: local networking in the fixture profile (with the mapping's network class, its guard mutations and the R02ad mutations run again), real host listeners whose counters do not change during the launch with the attempt's namespace read from outside and a mutation without the network namespace, host canaries and signals with a mutation exposing a host directory, `/proc` and the closed `/dev/pts` (the launcher's cover, required by the mount check, with `ENOENT` for `/dev/ptmx` and both paths through `/proc/1/root`, and a mutation without the cover with and without the check), a real descriptor leak refused by the bootstrap, and configuration injection with a live preload canary. | 4, 8                  |
| R02af | Input and output as amended on 2026-11-13: a pinned input checked against its listing and bound read-only at `/input` from a fresh staging directory; the keeper opened through `/proc` before the release with its mount id checked; collection after a passing verdict, beneath the output root, with the bounds and refusals of the amendment and a bound manifest; the private mount tree with a `noexec` input; payload-made and planted hostile trees, the verdicts, the freed tmpfs, and the ten mutations.                                                                                                                                                                                                                                                 | 3 (input), 7, 8       |
| R02ag | The crash acceptance as amended on 2026-11-13 (crashes): the scratch root as the payload's working directory, the `crash` and `crash-setter` fixtures under file routing with a 1 MiB parent limit, limits before and after `exec`, a scratch with no core and no stored core; the pipe and socket routes refused with their counters at zero and then shown live; an absolute pattern refused; a routing change during an attempt that ends it and quarantines the provider; and the six mutations.                                                                                                                                                                                                                                                               | 5 (cores), 8          |
| R02ah | The guardian as amended on 2026-11-13 (the guardian): a fixture mode in the tool root that stays in `supervisor`, is started before the stub and ends the group on its socket's end of file, an expired renewal window or the absolute deadline; renewals only after a heartbeat that confirms fence, cancel, generation and authority, against a stand-in store (revoked fence, cancel, generation, authority, unavailable, a stalled supervisor, 120 seconds of confirmations); the death of the supervisor at five phases in a unit with `KillMode=process`, and once without the guardian.                                                                                                                                                                     | 6, 8                  |
| R02ai | The attempt registry of 0042 rule 6 under the execution directory, its reconciliation before a new admission and after a changed boot, the persisted quarantine with its operator report, and the death of the supervisor at the registry and group-creation phases; planned by its own amendment.                                                                                                                                                                                                                                                                                                                                                                                                                                                                 | 6, 8                  |

15. **What R02a leaves to R02b.** R02ab to R02ag launch without the ownership
    record of 0042, rule 6; R02ai adds it. Everything that needs the job store
    is R02b's, including the input's origin (the store, access checks and the
    pinned revision) and the rest of the binding of R02af's manifest (actor
    and generation, artifact, revision, job and fence; the amendment of
    2026-11-13, input and output): the pinned actor generation, lease loss, cleanup before reclaim
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
    - R02ad: how a refusal is attributed to the filter rather than to the
      kernel. Settled by the amendment of 2026-11-13 (the syscall mapping).
    - R02ae: whether the private `/proc` and the launcher's `/dev/pts` reach
      beyond the attempt (the amendment of 2026-11-12). Settled by the
      amendments of 2026-11-13 (files, descriptors and network; the
      launcher's `/dev/pts`).
    - R02af: how the keeper of 0042, rule 7 holds the scratch until
      collection ends, now that the mount is made inside the sandbox and
      lives in the bootstrap's mount namespace. Settled by the amendment of
      2026-11-13 (input and output).
    - R02ag: that the coredump behaviour 0042 cites for kernel 6.18 holds for
      the kernel of the runner, read from that kernel's source before the
      slice relies on it. Settled by the amendment of 2026-11-13 (crashes).
    - R02ah: how the guardian learns that the supervisor is gone and how a
      test kills a supervisor without the service manager's cleanup.
      Settled by the amendment of 2026-11-13 (the guardian).

## Alternatives considered

- **A first-party `unsafe` block around the few system calls.** Rejected: the
  repository forbids it, and a reviewed crate with a safe interface gives the
  same calls.
- **Bounding the scratch without a third dependency.** Three ways were
  weighed after the measurement of 2026-11-12. `rustix`'s safe `unshare` is
  deprecated, and using it means silencing a lint. Its unsafe form is a
  first-party `unsafe` block, which rule 7 excludes. Leaving out the
  launcher's switch that disables further user namespaces would let the
  bootstrap mount without `unshare`, since it would then own the mount
  namespace, and would leave the filter as the only thing between a payload
  and a new user namespace; the switch stays.
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
- Three dependencies enter the provider and the server binary and must be
  reviewed when they are bumped.
- A host is supported only when an operator prepared it. The product still
  asks for no privilege at run time, but the preparation is real work that the
  documentation of R03 has to describe.
- WSL and containers stay unverified (0042, rule 1); the contributor host of
  this repository is refused by its crash routing.
