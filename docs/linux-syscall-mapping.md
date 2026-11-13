# Linux system-call mapping

The public acceptance mapping of decision 0042 (rule 4 and the acceptance section) for the Linux fixture profile, as planned by decision 0056 (amendment of 2026-11-13, the syscall mapping). **Status: R02ad, implemented; the x32 class fails acceptance on the reviewed kernel.** The job `Linux enforcement` runs the probes and the tests named below on every change.

## How a row is proved

- **Probe.** A static x86-64 program of a few instructions, generated as bytes by the test-support program (`execution::probes`), installed in the tool root under `probes/` and covered by its manifest. It sets the arguments, enters the kernel once (`syscall`, or `int 0x80` for i386), and exits with the call's error number. One launch of the scenario `probes` starts every probe as a child of the payload, so each runs after a `fork` and an `exec`.
- **Expected denial.** The filter kills the probe with `SIGSYS`. The evidence is the kernel's seccomp record (`audit: type=1326`) naming the probe's program (`exe=`), `sig=31`, the architecture and the number. The job test reads the kernel log from its end before the launch to its end after it and requires exactly the expected records: none missing, none repeated, no other seccomp record. Exit statuses and error numbers are not evidence.
- **Control (unfiltered).** What the kernel did with the same probe and no filter, measured on the runner (kernel 7.0.0-1017-azure) by a temporary job on 2026-11-13. Most calls fail in the kernel for another reason, which is why only the record counts. _not measured_ marks probes added after that job.
- **Mutation.** Run by a script on a temporary branch, never in the product: one patch at a time, then the named job test must fail. Every refused call is also allowed alone in turn; the results are attached to the pull request and rerun whenever the filter, this table or the bootstrap's limits change. Rows marked _measured_ were already observed by the planning job; the results of the R02ad run are at the end.

Beyond the probes, the job also proves: a refused call from a payload thread started after `exec` (record of the payload, `unshare`); a `prlimit64` that sets the limit of the payload's own child (record, `prlimit64`); a lower-and-restore of the soft descriptor limit within the hard limit is refused (the setter guard's case); the descriptor ceiling of 256 in the payload and in a child after `exec` (`EMFILE` with the highest descriptor 255); and, read from outside through `/proc`, `NoNewPrivs` 1, seccomp mode 2 with two filters, empty capability sets and both limits in every payload task. The descriptor guard's mutations: the ceiling raised to 512, and the initial limit guard removed so that the runner's own limit is inherited; both must fail the ceiling test. A descriptor inherited above the ceiling is refused by the bootstrap; `job_a_leaked_descriptor_is_refused` proves it with a real leak of a directory, a connection and a pipe (R02ae).

## controls

Allowed calls in the same launch; each must run, exit on its own with a code and leave no record, and `ok_prlimit_query` must exit 0. The exit codes of the others are a thread or child id, so they cannot tell an allowed call from one answered with an error number; no rule of this filter answers these calls with one.

| Probe                     | Call                                                    | Entry      | Expected                                         | Control (unfiltered) | Mutation |
| ------------------------- | ------------------------------------------------------- | ---------- | ------------------------------------------------ | -------------------- | -------- |
| `ok_gettid`               | gettid                                                  | x86-64 186 | allowed, no record                               | succeeded            | —        |
| `ok_prlimit_query`        | prlimit64, no new limit                                 | x86-64 302 | allowed, no record                               | exit 0 (0)           | —        |
| `ok_clone_plain`          | clone(SIGCHLD)                                          | x86-64 56  | allowed, no record                               | succeeded            | —        |
| `ok_fork_child_gettid`    | gettid in a forked child                                | x86-64 186 | allowed, no record                               | exit 0 (0)           | —        |
| `ok_socket_inet_stream`   | socket(AF_INET, SOCK_STREAM, 0)                         | x86-64 41  | allowed, no record                               | not measured         | —        |
| `ok_socket_inet6_dgram`   | socket(AF_INET6, SOCK_DGRAM \| SOCK_CLOEXEC, 0)         | x86-64 41  | allowed, no record                               | not measured         | —        |
| `ok_socket_unix_stream`   | socket(AF_UNIX, SOCK_STREAM \| SOCK_NONBLOCK, 0)        | x86-64 41  | allowed, no record                               | not measured         | —        |
| `ok_socket_inet_tcp`      | socket(AF_INET, SOCK_STREAM, IPPROTO_TCP)               | x86-64 41  | allowed, no record                               | not measured         | —        |
| `ok_socket_inet6_udp`     | socket(AF_INET6, SOCK_DGRAM, IPPROTO_UDP)               | x86-64 41  | allowed, no record                               | not measured         | —        |
| `ok_setsockopt_reuseaddr` | setsockopt(0, SOL_SOCKET, SO_REUSEADDR) on a non-socket | x86-64 54  | allowed, no record (the kernel answers ENOTSOCK) | not measured         | —        |

## default

Calls outside the reviewed allowlist that are not named dangerous: default denial.

| Probe                  | Call                 | Entry       | Expected                      | Control (unfiltered) | Mutation      |
| ---------------------- | -------------------- | ----------- | ----------------------------- | -------------------- | ------------- |
| `default_uname`        | uname                | x86-64 63   | record `sig=31 arch=c000003e` | exit 14 (EFAULT)     | allowed alone |
| `default_getpid`       | getpid               | x86-64 39   | record `sig=31 arch=c000003e` | succeeded            | allowed alone |
| `default_unknown_1000` | an unassigned number | x86-64 1000 | record `sig=31 arch=c000003e` | exit 38 (ENOSYS)     | allowed alone |

## namespaces

Namespace changes and process creation with namespace flags (0042 rule 4: permitted creation must not create namespaces).

| Probe                   | Call                                                                                | Entry      | Expected                                 | Control (unfiltered) | Mutation                                                             |
| ----------------------- | ----------------------------------------------------------------------------------- | ---------- | ---------------------------------------- | -------------------- | -------------------------------------------------------------------- |
| `ns_unshare`            | unshare(CLONE_NEWNS)                                                                | x86-64 272 | record `sig=31 arch=c000003e`            | exit 1 (EPERM)       | allowed alone                                                        |
| `ns_setns`              | setns                                                                               | x86-64 308 | record `sig=31 arch=c000003e`            | exit 22 (EINVAL)     | allowed alone                                                        |
| `ns_fork`               | fork                                                                                | x86-64 57  | record `sig=31 arch=c000003e`            | succeeded            | allowed alone                                                        |
| `ns_vfork`              | vfork                                                                               | x86-64 58  | record `sig=31 arch=c000003e`            | succeeded            | allowed alone                                                        |
| `ns_clone_newns`        | clone(CLONE_NEWNS)                                                                  | x86-64 56  | record `sig=31 arch=c000003e`            | exit 1 (EPERM)       | namespace mask removed: EPERM from the kernel, no record (measured)  |
| `ns_clone_newcgroup`    | clone(CLONE_NEWCGROUP)                                                              | x86-64 56  | record `sig=31 arch=c000003e`            | not measured         | its flag removed from the mask                                       |
| `ns_clone_newuts`       | clone(CLONE_NEWUTS)                                                                 | x86-64 56  | record `sig=31 arch=c000003e`            | not measured         | its flag removed from the mask                                       |
| `ns_clone_newipc`       | clone(CLONE_NEWIPC)                                                                 | x86-64 56  | record `sig=31 arch=c000003e`            | not measured         | its flag removed from the mask                                       |
| `ns_clone_newuser`      | clone(CLONE_NEWUSER)                                                                | x86-64 56  | record `sig=31 arch=c000003e`            | exit 28 (ENOSPC)     | namespace mask removed: ENOSPC from the kernel, no record (measured) |
| `ns_clone_newpid`       | clone(CLONE_NEWPID)                                                                 | x86-64 56  | record `sig=31 arch=c000003e`            | not measured         | its flag removed from the mask                                       |
| `ns_clone_newnet`       | clone(CLONE_NEWNET)                                                                 | x86-64 56  | record `sig=31 arch=c000003e`            | not measured         | its flag removed from the mask                                       |
| `ns_clone_newtime`      | clone with bit 0x80 (CLONE_NEWTIME's bit, read by clone as part of the exit signal) | x86-64 56  | record `sig=31 arch=c000003e`            | not measured         | its flag removed from the mask                                       |
| `ns_fork_child_unshare` | unshare in a forked child                                                           | x86-64 272 | record `sig=31 arch=c000003e`            | exit 0 (0)           | allowed alone                                                        |
| `ns_clone3`             | clone3 with CLONE_NEWUSER and an invalid exit signal                                | x86-64 435 | ENOSYS from the first program, no record | exit 22 (EINVAL)     | first program removed: EINVAL (measured)                             |

## mount

Mount changes.

| Probe              | Call          | Entry      | Expected                      | Control (unfiltered) | Mutation      |
| ------------------ | ------------- | ---------- | ----------------------------- | -------------------- | ------------- |
| `mount_mount`      | mount         | x86-64 165 | record `sig=31 arch=c000003e` | exit 14 (EFAULT)     | allowed alone |
| `mount_umount2`    | umount2       | x86-64 166 | record `sig=31 arch=c000003e` | exit 14 (EFAULT)     | allowed alone |
| `mount_pivot_root` | pivot_root    | x86-64 155 | record `sig=31 arch=c000003e` | exit 14 (EFAULT)     | allowed alone |
| `mount_chroot`     | chroot        | x86-64 161 | record `sig=31 arch=c000003e` | exit 14 (EFAULT)     | allowed alone |
| `mount_open_tree`  | open_tree     | x86-64 428 | record `sig=31 arch=c000003e` | exit 14 (EFAULT)     | allowed alone |
| `mount_move_mount` | move_mount    | x86-64 429 | record `sig=31 arch=c000003e` | exit 1 (EPERM)       | allowed alone |
| `mount_fsopen`     | fsopen        | x86-64 430 | record `sig=31 arch=c000003e` | exit 1 (EPERM)       | allowed alone |
| `mount_fsconfig`   | fsconfig      | x86-64 431 | record `sig=31 arch=c000003e` | not measured         | allowed alone |
| `mount_fsmount`    | fsmount       | x86-64 432 | record `sig=31 arch=c000003e` | not measured         | allowed alone |
| `mount_setattr`    | mount_setattr | x86-64 442 | record `sig=31 arch=c000003e` | exit 22 (EINVAL)     | allowed alone |

## privilege

Credential, capability and privilege changes, and attempts to undo the boundary.

| Probe                | Call                                            | Entry      | Expected                      | Control (unfiltered)             | Mutation      |
| -------------------- | ----------------------------------------------- | ---------- | ----------------------------- | -------------------------------- | ------------- |
| `priv_setuid`        | setuid                                          | x86-64 105 | record `sig=31 arch=c000003e` | exit 22 (EINVAL)                 | allowed alone |
| `priv_setgid`        | setgid                                          | x86-64 106 | record `sig=31 arch=c000003e` | exit 22 (EINVAL)                 | allowed alone |
| `priv_setreuid`      | setreuid                                        | x86-64 113 | record `sig=31 arch=c000003e` | not measured                     | allowed alone |
| `priv_setregid`      | setregid                                        | x86-64 114 | record `sig=31 arch=c000003e` | not measured                     | allowed alone |
| `priv_setresuid`     | setresuid                                       | x86-64 117 | record `sig=31 arch=c000003e` | exit 22 (EINVAL)                 | allowed alone |
| `priv_setresgid`     | setresgid                                       | x86-64 119 | record `sig=31 arch=c000003e` | not measured                     | allowed alone |
| `priv_setgroups`     | setgroups                                       | x86-64 116 | record `sig=31 arch=c000003e` | exit 1 (EPERM)                   | allowed alone |
| `priv_setfsuid`      | setfsuid                                        | x86-64 122 | record `sig=31 arch=c000003e` | returned the old fsuid           | allowed alone |
| `priv_setfsgid`      | setfsgid                                        | x86-64 123 | record `sig=31 arch=c000003e` | not measured                     | allowed alone |
| `priv_capset`        | capset (undo: capabilities)                     | x86-64 126 | record `sig=31 arch=c000003e` | exit 14 (EFAULT)                 | allowed alone |
| `priv_prctl_nnp_off` | prctl(PR_SET_NO_NEW_PRIVS) (undo: no_new_privs) | x86-64 157 | record `sig=31 arch=c000003e` | exit 22 (EINVAL)                 | allowed alone |
| `priv_seccomp`       | seccomp (undo: the filter)                      | x86-64 317 | record `sig=31 arch=c000003e` | exit 137 (SIGKILL (strict mode)) | allowed alone |

## tracing

Tracing and interference with other processes.

| Probe                     | Call                   | Entry      | Expected                      | Control (unfiltered) | Mutation      |
| ------------------------- | ---------------------- | ---------- | ----------------------------- | -------------------- | ------------- |
| `trace_ptrace`            | ptrace(PTRACE_TRACEME) | x86-64 101 | record `sig=31 arch=c000003e` | exit 0 (0)           | allowed alone |
| `trace_process_vm_readv`  | process_vm_readv       | x86-64 310 | record `sig=31 arch=c000003e` | exit 0 (0)           | allowed alone |
| `trace_process_vm_writev` | process_vm_writev      | x86-64 311 | record `sig=31 arch=c000003e` | not measured         | allowed alone |
| `trace_kcmp`              | kcmp                   | x86-64 312 | record `sig=31 arch=c000003e` | exit 0 (0)           | allowed alone |
| `trace_pidfd_open`        | pidfd_open             | x86-64 434 | record `sig=31 arch=c000003e` | succeeded            | allowed alone |
| `trace_pidfd_getfd`       | pidfd_getfd            | x86-64 438 | record `sig=31 arch=c000003e` | exit 9 (EBADF)       | allowed alone |
| `trace_process_madvise`   | process_madvise        | x86-64 440 | record `sig=31 arch=c000003e` | not measured         | allowed alone |

## kernel

Kernel modules, BPF, and hardware port and descriptor-table access.

| Probe                 | Call            | Entry      | Expected                      | Control (unfiltered) | Mutation      |
| --------------------- | --------------- | ---------- | ----------------------------- | -------------------- | ------------- |
| `module_init`         | init_module     | x86-64 175 | record `sig=31 arch=c000003e` | exit 1 (EPERM)       | allowed alone |
| `module_finit`        | finit_module    | x86-64 313 | record `sig=31 arch=c000003e` | exit 1 (EPERM)       | allowed alone |
| `module_delete`       | delete_module   | x86-64 176 | record `sig=31 arch=c000003e` | exit 1 (EPERM)       | allowed alone |
| `bpf_bpf`             | bpf             | x86-64 321 | record `sig=31 arch=c000003e` | exit 22 (EINVAL)     | allowed alone |
| `bpf_perf_event_open` | perf_event_open | x86-64 298 | record `sig=31 arch=c000003e` | exit 13 (EACCES)     | allowed alone |
| `kernel_iopl`         | iopl            | x86-64 172 | record `sig=31 arch=c000003e` | not measured         | allowed alone |
| `kernel_ioperm`       | ioperm          | x86-64 173 | record `sig=31 arch=c000003e` | not measured         | allowed alone |
| `kernel_modify_ldt`   | modify_ldt      | x86-64 154 | record `sig=31 arch=c000003e` | not measured         | allowed alone |

## host-global

Host-global state, clocks included.

| Probe                      | Call              | Entry      | Expected                      | Control (unfiltered) | Mutation      |
| -------------------------- | ----------------- | ---------- | ----------------------------- | -------------------- | ------------- |
| `global_settimeofday`      | settimeofday      | x86-64 164 | record `sig=31 arch=c000003e` | exit 1 (EPERM)       | allowed alone |
| `global_clock_settime`     | clock_settime     | x86-64 227 | record `sig=31 arch=c000003e` | exit 14 (EFAULT)     | allowed alone |
| `global_clock_adjtime`     | clock_adjtime     | x86-64 305 | record `sig=31 arch=c000003e` | exit 14 (EFAULT)     | allowed alone |
| `global_adjtimex`          | adjtimex          | x86-64 159 | record `sig=31 arch=c000003e` | exit 14 (EFAULT)     | allowed alone |
| `global_sethostname`       | sethostname       | x86-64 170 | record `sig=31 arch=c000003e` | exit 1 (EPERM)       | allowed alone |
| `global_setdomainname`     | setdomainname     | x86-64 171 | record `sig=31 arch=c000003e` | not measured         | allowed alone |
| `global_reboot`            | reboot            | x86-64 169 | record `sig=31 arch=c000003e` | exit 1 (EPERM)       | allowed alone |
| `global_swapon`            | swapon            | x86-64 167 | record `sig=31 arch=c000003e` | exit 1 (EPERM)       | allowed alone |
| `global_swapoff`           | swapoff           | x86-64 168 | record `sig=31 arch=c000003e` | not measured         | allowed alone |
| `global_acct`              | acct              | x86-64 163 | record `sig=31 arch=c000003e` | exit 1 (EPERM)       | allowed alone |
| `global_kexec_load`        | kexec_load        | x86-64 246 | record `sig=31 arch=c000003e` | exit 1 (EPERM)       | allowed alone |
| `global_syslog`            | syslog            | x86-64 103 | record `sig=31 arch=c000003e` | not measured         | allowed alone |
| `global_kexec_file_load`   | kexec_file_load   | x86-64 320 | record `sig=31 arch=c000003e` | not measured         | allowed alone |
| `global_quotactl`          | quotactl          | x86-64 179 | record `sig=31 arch=c000003e` | not measured         | allowed alone |
| `global_name_to_handle_at` | name_to_handle_at | x86-64 303 | record `sig=31 arch=c000003e` | not measured         | allowed alone |
| `global_open_by_handle_at` | open_by_handle_at | x86-64 304 | record `sig=31 arch=c000003e` | not measured         | allowed alone |

## keyrings

Keyrings.

| Probe             | Call        | Entry      | Expected                      | Control (unfiltered) | Mutation      |
| ----------------- | ----------- | ---------- | ----------------------------- | -------------------- | ------------- |
| `key_add_key`     | add_key     | x86-64 248 | record `sig=31 arch=c000003e` | exit 14 (EFAULT)     | allowed alone |
| `key_request_key` | request_key | x86-64 249 | record `sig=31 arch=c000003e` | exit 14 (EFAULT)     | allowed alone |
| `key_keyctl`      | keyctl      | x86-64 250 | record `sig=31 arch=c000003e` | exit 22 (EINVAL)     | allowed alone |

## async

Unreviewed asynchronous and multiplexed paths.

| Probe                     | Call              | Entry      | Expected                      | Control (unfiltered) | Mutation      |
| ------------------------- | ----------------- | ---------- | ----------------------------- | -------------------- | ------------- |
| `async_io_uring_setup`    | io_uring_setup    | x86-64 425 | record `sig=31 arch=c000003e` | exit 14 (EFAULT)     | allowed alone |
| `async_io_uring_enter`    | io_uring_enter    | x86-64 426 | record `sig=31 arch=c000003e` | not measured         | allowed alone |
| `async_io_uring_register` | io_uring_register | x86-64 427 | record `sig=31 arch=c000003e` | not measured         | allowed alone |
| `async_io_setup`          | io_setup          | x86-64 206 | record `sig=31 arch=c000003e` | exit 14 (EFAULT)     | allowed alone |
| `async_io_submit`         | io_submit         | x86-64 209 | record `sig=31 arch=c000003e` | not measured         | allowed alone |
| `async_userfaultfd`       | userfaultfd       | x86-64 323 | record `sig=31 arch=c000003e` | exit 1 (EPERM)       | allowed alone |

## limits

Resource-limit setters (0042 rule 5: the descriptor and core limits cannot be changed).

| Probe               | Call                       | Entry      | Expected                      | Control (unfiltered) | Mutation                                           |
| ------------------- | -------------------------- | ---------- | ----------------------------- | -------------------- | -------------------------------------------------- |
| `limit_setrlimit`   | setrlimit                  | x86-64 160 | record `sig=31 arch=c000003e` | exit 14 (EFAULT)     | allowed alone                                      |
| `limit_prlimit_set` | prlimit64 with a new limit | x86-64 302 | record `sig=31 arch=c000003e` | exit 22 (EINVAL)     | setter guard removed: EINVAL, no record (measured) |

## network

Sockets outside the forms the fixture profile allows for communication inside the attempt (0042 rule 4; 0056, amendment of 2026-11-13, files, descriptors and network). The profile allows `socket` for `AF_UNIX` stream or datagram with protocol 0, and for `AF_INET` and `AF_INET6` stream with protocol 0 or TCP and datagram with protocol 0 or UDP, the close-on-exec and non-blocking flags included; `setsockopt` only for `SO_REUSEADDR` at the socket level; and `bind`, `listen`, `accept4`, `connect`, `sendto`, `recvfrom`, `getsockname` and `shutdown`. That the attempt's network namespace keeps such sockets inside is proved by `job_the_network_reaches_no_host_listener` with real host listeners, not by this table.

| Probe                       | Call                                         | Entry      | Expected                      | Control (unfiltered) | Mutation                   |
| --------------------------- | -------------------------------------------- | ---------- | ----------------------------- | -------------------- | -------------------------- |
| `net_socket_netlink`        | socket(AF_NETLINK, SOCK_DGRAM, 0)            | x86-64 41  | record `sig=31 arch=c000003e` | not measured         | domain condition removed   |
| `net_socket_packet`         | socket(AF_PACKET, SOCK_RAW, ETH_P_ALL)       | x86-64 41  | record `sig=31 arch=c000003e` | exit 1 (EPERM)       | socket unconditional       |
| `net_socket_raw`            | socket(AF_INET, SOCK_RAW, IPPROTO_ICMP)      | x86-64 41  | record `sig=31 arch=c000003e` | exit 1 (EPERM)       | socket unconditional       |
| `net_socket_inet_seqpacket` | socket(AF_INET, SOCK_SEQPACKET, 0)           | x86-64 41  | record `sig=31 arch=c000003e` | not measured         | type condition removed     |
| `net_socket_unix_seqpacket` | socket(AF_UNIX, SOCK_SEQPACKET, 0)           | x86-64 41  | record `sig=31 arch=c000003e` | not measured         | type condition removed     |
| `net_socket_inet_sctp`      | socket(AF_INET, SOCK_STREAM, IPPROTO_SCTP)   | x86-64 41  | record `sig=31 arch=c000003e` | not measured         | protocol condition removed |
| `net_socket_inet6_mptcp`    | socket(AF_INET6, SOCK_STREAM, IPPROTO_MPTCP) | x86-64 41  | record `sig=31 arch=c000003e` | not measured         | protocol condition removed |
| `net_socket_inet_l2tp`      | socket(AF_INET, SOCK_DGRAM, IPPROTO_L2TP)    | x86-64 41  | record `sig=31 arch=c000003e` | not measured         | protocol condition removed |
| `net_socket_unix_protocol`  | socket(AF_UNIX, SOCK_STREAM, 1)              | x86-64 41  | record `sig=31 arch=c000003e` | not measured         | protocol condition removed |
| `net_setsockopt_sndbuf`     | setsockopt(SOL_SOCKET, SO_SNDBUF)            | x86-64 54  | record `sig=31 arch=c000003e` | not measured         | option condition removed   |
| `net_setsockopt_ip_level`   | setsockopt(IPPROTO_IP, IP_TTL)               | x86-64 54  | record `sig=31 arch=c000003e` | not measured         | level condition removed    |
| `net_accept`                | accept                                       | x86-64 43  | record `sig=31 arch=c000003e` | not measured         | allowed alone              |
| `net_sendmsg`               | sendmsg (descriptor passing)                 | x86-64 46  | record `sig=31 arch=c000003e` | not measured         | allowed alone              |
| `net_recvmsg`               | recvmsg (descriptor passing)                 | x86-64 47  | record `sig=31 arch=c000003e` | not measured         | allowed alone              |
| `net_getpeername`           | getpeername                                  | x86-64 52  | record `sig=31 arch=c000003e` | not measured         | allowed alone              |
| `net_socketpair`            | socketpair                                   | x86-64 53  | record `sig=31 arch=c000003e` | not measured         | allowed alone              |
| `net_getsockopt`            | getsockopt                                   | x86-64 55  | record `sig=31 arch=c000003e` | not measured         | allowed alone              |
| `net_recvmmsg`              | recvmmsg                                     | x86-64 299 | record `sig=31 arch=c000003e` | not measured         | allowed alone              |
| `net_sendmmsg`              | sendmmsg                                     | x86-64 307 | record `sig=31 arch=c000003e` | not measured         | allowed alone              |

## abi

Alternate system-call ABIs. **x32 FAILS acceptance on the reviewed kernel**: it has no x32 entry point (`CONFIG_X86_X32_ABI` unset), so no control exists; its records are required but not counted (0056, amendment of 2026-11-13). The i386 entries (`int 0x80`) exist on this kernel and are refused by the architecture check.

| Probe                 | Call                    | Entry             | Expected                      | Control (unfiltered) | Mutation                                      |
| --------------------- | ----------------------- | ----------------- | ----------------------------- | -------------------- | --------------------------------------------- |
| `abi_x32_gettid`      | gettid with the x32 bit | x86-64 0x400000BA | record `sig=31 arch=c000003e` | exit 38 (ENOSYS)     | none possible on this kernel                  |
| `abi_x32_getpid`      | getpid with the x32 bit | x86-64 0x40000027 | record `sig=31 arch=c000003e` | exit 38 (ENOSYS)     | none possible on this kernel                  |
| `abi_i386_exit`       | exit                    | i386 1            | record `sig=31 arch=40000003` | exit 0 (0)           | architecture check opened: exits 0 (measured) |
| `abi_i386_getpid`     | getpid                  | i386 20           | record `sig=31 arch=40000003` | succeeded            | architecture check opened                     |
| `abi_i386_setrlimit`  | setrlimit               | i386 75           | record `sig=31 arch=40000003` | exit 14 (EFAULT)     | architecture check opened                     |
| `abi_i386_ugetrlimit` | ugetrlimit              | i386 191          | record `sig=31 arch=40000003` | not measured         | architecture check opened                     |
| `abi_i386_prlimit64`  | prlimit64               | i386 340          | record `sig=31 arch=40000003` | exit 22 (EINVAL)     | architecture check opened                     |
| `abi_i386_socketcall` | socketcall              | i386 102          | record `sig=31 arch=40000003` | exit 14 (EFAULT)     | architecture check opened                     |
| `abi_i386_ipc`        | ipc                     | i386 117          | record `sig=31 arch=40000003` | exit 38 (ENOSYS)     | architecture check opened                     |

## Mutation results

Run on 2026-11-13 by `scripts/enforcement/mutate.py` on the temporary branch `mutation/r02ad` (never merged), workflow run 43327325321 on `ubuntu-24.04`: one job per mutation, each applying one patch, rebuilding, and running only the named job test. The unmutated baseline passed. All 71 mutations failed their test:

- `job_probes_are_refused_with_their_records`: the architecture check opened in both programs (the first missing record was `abi_i386_exit`); the default action changed from kill to `EPERM` (`default_uname`); the whole namespace mask removed (`ns_clone_newns`); each of the eight namespace flags removed from the mask alone (its `ns_clone_*` probe); the setter guard removed (`limit_prlimit_set`); and each of the 54 refused x86-64 numbers allowed alone (the probes with that number).
- `job_clone3_is_refused_with_enosys`: the first program no longer answers `clone3` (the payload did not exit with `ENOSYS`).
- `job_lowering_the_soft_limit_is_refused` and `job_a_descendants_limit_cannot_be_set`: the setter guard removed (the payload was not killed with `SIGSYS`).
- `job_descriptors_stop_at_the_ceiling`: the ceiling raised to 512, and the initial limit guard removed with the readiness check of that limit bypassed, so the payload kept the unit's limit (the payload's soft descriptor limit differed in both).

No mutation is possible for the x32 rows on this kernel.
