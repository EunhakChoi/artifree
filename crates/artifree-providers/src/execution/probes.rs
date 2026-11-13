//! Generated probe programs of the Linux enforcement job (decision 0056, amendment of
//! 2026-11-13, "the syscall mapping").
//!
//! Each probe is a static x86-64 program of a few instructions, generated as bytes by the
//! test-support program of rule 4, without an assembler and without first-party `unsafe`. It
//! sets the arguments, enters the kernel once by `syscall` or by `int 0x80`, and exits with the
//! call's error number. The probes are part of the fixture (rule 6).
//!
//! The table is the one in `docs/linux-syscall-mapping.md`, and a unit test keeps the two in
//! step. Nothing in this module runs a probe. The writer exists for the test-support program
//! only, and the programs are of use only under the launch.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// The audit architecture value that the kernel prints for a native x86-64 entry.
pub(super) const AUDIT_ARCH_X86_64: u32 = 0xC000_003E;
/// The audit architecture value that the kernel prints for an `int 0x80` entry of a 64-bit
/// process.
pub(super) const AUDIT_ARCH_I386: u32 = 0x4000_0003;

/// The virtual address of the one loadable segment, which starts at the file's first byte.
const LOAD_ADDRESS: u64 = 0x40_0000;
/// The entry point: the code starts after the file header and the one program header.
const ENTRY_ADDRESS: u32 = 0x40_0078;
/// The size of an ELF64 file header.
const ELF_HEADER_BYTES: u16 = 64;
/// The size of an ELF64 program header.
const PROGRAM_HEADER_BYTES: u16 = 56;
/// The largest forward displacement of a short jump, which is a signed byte.
const MAX_SHORT_JUMP: u8 = 0x7F;
/// The size of the `clone_args` structure that `clone3` takes.
const CLONE_ARGS_BYTES: u32 = 64;
/// `CLONE_NEWUSER`, the namespace flag that the `clone3` structure carries.
const CLONE_NEWUSER: u64 = 0x1000_0000;
/// An invalid signal, so the kernel rejects the structure with `EINVAL` before creating anything.
const CLONE3_EXIT_SIGNAL: u64 = 0x100;
/// `mov edi, eax; neg edi; mov eax, 231; syscall`: `exit_group` with the negated result, so the
/// error number becomes the exit status.
const EXIT_WITH_RESULT: [u8; 11] = [
    0x89, 0xC7, 0xF7, 0xDF, 0xB8, 0xE7, 0x00, 0x00, 0x00, 0x0F, 0x05,
];
/// `xor edi, edi; mov eax, 231; syscall`: `exit_group(0)`.
const EXIT_ZERO: [u8; 9] = [0x31, 0xFF, 0xB8, 0xE7, 0x00, 0x00, 0x00, 0x0F, 0x05];

/// How a probe enters the kernel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Entry {
    /// The 64-bit `syscall` instruction.
    Native,
    /// The `int 0x80` entry, which a 64-bit process uses for i386 calls.
    I386,
}

/// What the enforcement job must observe for a probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Expect {
    /// The filter refuses the call: the probe is killed with `SIGSYS` and one kernel record
    /// names it with this architecture and number.
    Refused {
        /// The audit architecture the filter must see.
        arch: u32,
        /// The system-call number the filter must see.
        number: u32,
    },
    /// The call runs and the probe exits on its own, with a code and no signal. It leaves no kernel
    /// record.
    Allowed,
    /// The filter answers the call with `ENOSYS`. The probe exits on its own, with a code and no
    /// signal, and leaves no kernel record.
    Enosys,
}

/// The instructions that a probe runs around its call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Body {
    /// The call, then the exit with its result.
    Single,
    /// A `clone` without namespace flags, which is a fork. The child makes the call and exits
    /// with its result. The parent waits for any child and exits 0 whatever the child does.
    ForkedChild,
    /// A `clone3` whose structure the kernel rejects, then the exit with its result.
    Clone3Rejected,
}

/// One generated probe: the call it makes and what the enforcement job must observe.
#[derive(Clone, Copy, Debug)]
pub(super) struct Probe {
    /// The file name under the probes directory: ASCII `[a-z0-9_]`.
    pub(super) name: &'static str,
    /// The mapping class of the probe, which is a section heading of the mapping document.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "read only by the probe table's unit tests and the enforcement job's record checks"
        )
    )]
    pub(super) class: &'static str,
    /// How the call enters the kernel.
    pub(super) entry: Entry,
    /// The system-call number, which the probe loads into `eax`.
    pub(super) number: u32,
    /// The argument values in register order. At most six are used.
    pub(super) arguments: &'static [u32],
    /// The instructions that run around the call.
    pub(super) body: Body,
    /// What the enforcement job must observe for the probe.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "read only by the probe table's unit tests and the enforcement job's record checks"
        )
    )]
    pub(super) expect: Expect,
}

/// A single-call probe of the native entry that the filter must refuse.
const fn native(
    name: &'static str,
    class: &'static str,
    number: u32,
    arguments: &'static [u32],
) -> Probe {
    Probe {
        name,
        class,
        entry: Entry::Native,
        number,
        arguments,
        body: Body::Single,
        expect: Expect::Refused {
            arch: AUDIT_ARCH_X86_64,
            number,
        },
    }
}

/// A single-call probe of an i386 call through `int 0x80` that the filter must refuse.
const fn i386(name: &'static str, number: u32, arguments: &'static [u32]) -> Probe {
    Probe {
        name,
        class: "abi",
        entry: Entry::I386,
        number,
        arguments,
        body: Body::Single,
        expect: Expect::Refused {
            arch: AUDIT_ARCH_I386,
            number,
        },
    }
}

/// A probe of a call that the filter must allow. It runs, exits on its own, and leaves no kernel
/// record.
const fn control(name: &'static str, number: u32, arguments: &'static [u32], body: Body) -> Probe {
    Probe {
        name,
        class: "controls",
        entry: Entry::Native,
        number,
        arguments,
        body,
        expect: Expect::Allowed,
    }
}

// One probe per line, so that each row reads as one row of the mapping document.
/// Every probe, grouped by class. Each class is a section heading of
/// `docs/linux-syscall-mapping.md`.
#[rustfmt::skip]
pub(super) const PROBES: [Probe; 119] = [
    // Controls: calls the filter must allow.
    control("ok_gettid", 186, &[], Body::Single),
    control("ok_prlimit_query", 302, &[0, 7, 0, 0], Body::Single),
    control("ok_clone_plain", 56, &[17, 0, 0, 0, 0], Body::Single),
    control("ok_fork_child_gettid", 186, &[], Body::ForkedChild),
    // Local networking that the fixture profile allows (0056, amendment of 2026-11-13, files,
    // descriptors and network). Descriptor 0 is not a socket, so setsockopt answers ENOTSOCK; the
    // point is that no record is left.
    control("ok_socket_inet_stream", 41, &[2, 1, 0], Body::Single),
    control("ok_socket_inet6_dgram", 41, &[10, 0x0008_0002, 0], Body::Single),
    control("ok_socket_unix_stream", 41, &[1, 0x801, 0], Body::Single),
    control("ok_socket_inet_tcp", 41, &[2, 1, 6], Body::Single),
    control("ok_socket_inet6_udp", 41, &[10, 2, 17], Body::Single),
    control("ok_setsockopt_reuseaddr", 54, &[0, 1, 2, 0, 0], Body::Single),

    // Default denial: numbers that the policy does not name.
    native("default_uname", "default", 63, &[0]),
    native("default_getpid", "default", 39, &[]),
    native("default_unknown_1000", "default", 1000, &[]),

    // Namespaces: the namespace entry points, and each namespace flag on clone.
    native("ns_unshare", "namespaces", 272, &[0x0002_0000]),
    native("ns_setns", "namespaces", 308, &[0, 0]),
    native("ns_fork", "namespaces", 57, &[]),
    native("ns_vfork", "namespaces", 58, &[]),
    native("ns_clone_newns", "namespaces", 56, &[0x0002_0011, 0, 0, 0, 0]),
    native("ns_clone_newcgroup", "namespaces", 56, &[0x0200_0011, 0, 0, 0, 0]),
    native("ns_clone_newuts", "namespaces", 56, &[0x0400_0011, 0, 0, 0, 0]),
    native("ns_clone_newipc", "namespaces", 56, &[0x0800_0011, 0, 0, 0, 0]),
    native("ns_clone_newuser", "namespaces", 56, &[0x1000_0011, 0, 0, 0, 0]),
    native("ns_clone_newpid", "namespaces", 56, &[0x2000_0011, 0, 0, 0, 0]),
    native("ns_clone_newnet", "namespaces", 56, &[0x4000_0011, 0, 0, 0, 0]),
    native("ns_clone_newtime", "namespaces", 56, &[0x91, 0, 0, 0, 0]),
    Probe {
        name: "ns_fork_child_unshare",
        class: "namespaces",
        entry: Entry::Native,
        number: 272,
        arguments: &[0x0002_0000],
        body: Body::ForkedChild,
        expect: Expect::Refused {
            arch: AUDIT_ARCH_X86_64,
            number: 272,
        },
    },
    // The namespace flags sit in a clone_args structure, which the kernel would reject with EINVAL.
    // The filter answers ENOSYS before the kernel reads the structure, and leaves no record.
    Probe {
        name: "ns_clone3",
        class: "namespaces",
        entry: Entry::Native,
        number: 435,
        arguments: &[],
        body: Body::Clone3Rejected,
        expect: Expect::Enosys,
    },

    // Mount: the mount table, the new mount API and the root.
    native("mount_mount", "mount", 165, &[0, 0, 0, 0, 0]),
    native("mount_umount2", "mount", 166, &[0, 0]),
    native("mount_pivot_root", "mount", 155, &[0, 0]),
    native("mount_chroot", "mount", 161, &[0]),
    native("mount_open_tree", "mount", 428, &[0, 0, 0]),
    native("mount_move_mount", "mount", 429, &[0, 0, 0, 0, 0]),
    native("mount_fsopen", "mount", 430, &[0, 0]),
    native("mount_fsconfig", "mount", 431, &[0, 0, 0, 0, 0]),
    native("mount_fsmount", "mount", 432, &[0, 0, 0]),
    native("mount_setattr", "mount", 442, &[0, 0, 0, 0, 0]),

    // Privilege: each call would change a state the launch depends on.
    native("priv_setuid", "privilege", 105, &[0]),
    native("priv_setgid", "privilege", 106, &[0]),
    native("priv_setreuid", "privilege", 113, &[0, 0]),
    native("priv_setregid", "privilege", 114, &[0, 0]),
    native("priv_setresuid", "privilege", 117, &[0, 0, 0]),
    native("priv_setresgid", "privilege", 119, &[0, 0, 0]),
    native("priv_setgroups", "privilege", 116, &[0, 0]),
    native("priv_setfsuid", "privilege", 122, &[0]),
    native("priv_setfsgid", "privilege", 123, &[0]),
    // Undoing the boundary: the attempt to undo the empty capability sets.
    native("priv_capset", "privilege", 126, &[0, 0]),
    // Undoing the boundary: the attempt to undo no_new_privs.
    native("priv_prctl_nnp_off", "privilege", 157, &[38, 0, 0, 0, 0]),
    // Undoing the boundary: the attempt to undo the filter.
    native("priv_seccomp", "privilege", 317, &[0, 0, 0]),

    // Tracing: process inspection and control across processes.
    native("trace_ptrace", "tracing", 101, &[0, 0, 0, 0]),
    native("trace_process_vm_readv", "tracing", 310, &[1, 0, 0, 0, 0, 0]),
    native("trace_process_vm_writev", "tracing", 311, &[1, 0, 0, 0, 0, 0]),
    native("trace_kcmp", "tracing", 312, &[1, 1, 0, 0, 0]),
    native("trace_pidfd_open", "tracing", 434, &[1, 0]),
    native("trace_pidfd_getfd", "tracing", 438, &[0, 0, 0]),
    native("trace_process_madvise", "tracing", 440, &[0, 0, 0, 0, 0]),

    // Kernel: modules and BPF.
    native("module_init", "kernel", 175, &[0, 0, 0]),
    native("module_finit", "kernel", 313, &[0, 0, 0]),
    native("module_delete", "kernel", 176, &[0, 0]),
    native("bpf_bpf", "kernel", 321, &[0, 0, 0]),
    native("bpf_perf_event_open", "kernel", 298, &[0, 0, 0, 0, 0]),
    // Port and descriptor-table access to hardware.
    native("kernel_iopl", "kernel", 172, &[0]),
    native("kernel_ioperm", "kernel", 173, &[0, 0, 0]),
    native("kernel_modify_ldt", "kernel", 154, &[0, 0, 0]),

    // Host-global: state shared by every process on the host.
    native("global_settimeofday", "host-global", 164, &[0, 0]),
    native("global_clock_settime", "host-global", 227, &[0, 0]),
    native("global_clock_adjtime", "host-global", 305, &[0, 0]),
    native("global_adjtimex", "host-global", 159, &[0]),
    native("global_sethostname", "host-global", 170, &[0, 0]),
    native("global_setdomainname", "host-global", 171, &[0, 0]),
    native("global_reboot", "host-global", 169, &[0, 0, 0, 0]),
    native("global_swapon", "host-global", 167, &[0, 0]),
    native("global_swapoff", "host-global", 168, &[0]),
    native("global_acct", "host-global", 163, &[0]),
    native("global_kexec_load", "host-global", 246, &[0, 0, 0, 0]),
    native("global_syslog", "host-global", 103, &[0, 0, 0]),
    native("global_kexec_file_load", "host-global", 320, &[0, 0, 0, 0, 0]),
    native("global_quotactl", "host-global", 179, &[0, 0, 0, 0]),
    // A handle names a host file outside the mount namespace's view.
    native("global_name_to_handle_at", "host-global", 303, &[0, 0, 0, 0, 0]),
    native("global_open_by_handle_at", "host-global", 304, &[0, 0, 0]),

    // Keyrings: kernel key storage.
    native("key_add_key", "keyrings", 248, &[0, 0, 0, 0, 0]),
    native("key_request_key", "keyrings", 249, &[0, 0, 0, 0]),
    native("key_keyctl", "keyrings", 250, &[0, 0, 0, 0, 0]),

    // Async: the asynchronous and multiplexed interfaces.
    native("async_io_uring_setup", "async", 425, &[0, 0]),
    native("async_io_uring_enter", "async", 426, &[0, 0, 0, 0, 0, 0]),
    native("async_io_uring_register", "async", 427, &[0, 0, 0, 0]),
    native("async_io_setup", "async", 206, &[0, 0]),
    native("async_io_submit", "async", 209, &[0, 0, 0]),
    native("async_userfaultfd", "async", 323, &[0]),

    // Limits: the setters that would change the descriptor ceiling.
    native("limit_setrlimit", "limits", 160, &[7, 0]),
    // The new-limit pointer is non-null, so the call would set a limit.
    native("limit_prlimit_set", "limits", 302, &[0, 7, 0x40_0000, 0]),

    // Network: socket forms, socket options and message calls outside the local networking that
    // the fixture profile allows (0056, amendment of 2026-11-13, files, descriptors and network).
    native("net_socket_netlink", "network", 41, &[16, 2, 0]),
    native("net_socket_packet", "network", 41, &[17, 3, 0x300]),
    native("net_socket_raw", "network", 41, &[2, 3, 1]),
    native("net_socket_inet_seqpacket", "network", 41, &[2, 5, 0]),
    native("net_socket_unix_seqpacket", "network", 41, &[1, 5, 0]),
    native("net_socket_inet_sctp", "network", 41, &[2, 1, 132]),
    native("net_socket_inet6_mptcp", "network", 41, &[10, 1, 262]),
    native("net_socket_inet_l2tp", "network", 41, &[2, 2, 115]),
    native("net_socket_unix_protocol", "network", 41, &[1, 1, 1]),
    native("net_setsockopt_sndbuf", "network", 54, &[0, 1, 7, 0, 0]),
    native("net_setsockopt_ip_level", "network", 54, &[0, 0, 1, 0, 0]),
    native("net_accept", "network", 43, &[0, 0, 0]),
    native("net_sendmsg", "network", 46, &[0, 0, 0]),
    native("net_recvmsg", "network", 47, &[0, 0, 0]),
    native("net_getpeername", "network", 52, &[0, 0, 0]),
    native("net_socketpair", "network", 53, &[1, 1, 0, 0]),
    native("net_getsockopt", "network", 55, &[0, 0, 0, 0, 0]),
    native("net_recvmmsg", "network", 299, &[0, 0, 0, 0, 0]),
    native("net_sendmmsg", "network", 307, &[0, 0, 0, 0]),

    // Alternate ABIs: the x32 entries and the i386 entries through `int 0x80`.
    // The x32 class FAILS this host's acceptance (0056, amendment of 2026-11-13): the reviewed
    // kernel has no x32 entry point, so no control exists. Its records are still required by the
    // enforcement job, but they are not counted as evidence.
    native("abi_x32_gettid", "abi", 0x4000_00BA, &[]),
    native("abi_x32_getpid", "abi", 0x4000_0027, &[]),
    i386("abi_i386_exit", 1, &[0]),
    i386("abi_i386_getpid", 20, &[]),
    i386("abi_i386_setrlimit", 75, &[7, 0]),
    i386("abi_i386_ugetrlimit", 191, &[7, 0]),
    i386("abi_i386_prlimit64", 340, &[0, 7, 0x40_0000, 0]),
    i386("abi_i386_socketcall", 102, &[1, 0]),
    i386("abi_i386_ipc", 117, &[0, 0, 0, 0, 0, 0]),
];

/// The `mov` opcodes that load each native argument register, in argument order. The four
/// bytes of the immediate follow each one.
const NATIVE_REGISTERS: &[&[u8]] = &[
    &[0xBF],       // edi
    &[0xBE],       // esi
    &[0xBA],       // edx
    &[0x41, 0xBA], // r10d
    &[0x41, 0xB8], // r8d
    &[0x41, 0xB9], // r9d
];

/// The `mov` opcodes that load each i386 argument register, in argument order. The four bytes
/// of the immediate follow each one.
const I386_REGISTERS: &[&[u8]] = &[
    &[0xBB], // ebx
    &[0xB9], // ecx
    &[0xBA], // edx
    &[0xBE], // esi
    &[0xBF], // edi
    &[0xBD], // ebp
];

/// The argument-register opcodes of an entry.
fn registers(entry: Entry) -> &'static [&'static [u8]] {
    match entry {
        Entry::Native => NATIVE_REGISTERS,
        Entry::I386 => I386_REGISTERS,
    }
}

/// The instruction that enters the kernel through an entry.
fn enter(entry: Entry) -> &'static [u8] {
    match entry {
        Entry::Native => &[0x0F, 0x05],
        Entry::I386 => &[0xCD, 0x80],
    }
}

/// The instructions that load `arguments` into the entry's argument registers, load the call
/// number into `eax`, and enter the kernel. Arguments past the sixth are ignored.
fn call(entry: Entry, number: u32, arguments: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for (prefix, argument) in registers(entry).iter().zip(arguments) {
        bytes.extend_from_slice(prefix);
        bytes.extend_from_slice(&argument.to_le_bytes());
    }
    bytes.push(0xB8); // mov eax, imm32
    bytes.extend_from_slice(&number.to_le_bytes());
    bytes.extend_from_slice(enter(entry));
    bytes
}

/// The probe's call followed by the exit with its result.
fn probe_part(probe: &Probe) -> Vec<u8> {
    let mut bytes = call(probe.entry, probe.number, probe.arguments);
    bytes.extend_from_slice(&EXIT_WITH_RESULT);
    bytes
}

/// A `clone` without namespace flags, then the probe's call in the child and a wait in the
/// parent.
fn forked_child(probe: &Probe) -> io::Result<Vec<u8>> {
    let child = probe_part(probe);
    let jump = u8::try_from(child.len())
        .ok()
        .filter(|jump| *jump <= MAX_SHORT_JUMP)
        .ok_or_else(|| invalid("a forked child is too long for a short jump"))?;
    let mut bytes = call(Entry::Native, 56, &[17, 0, 0, 0, 0]); // clone(SIGCHLD): a fork
    bytes.extend_from_slice(&[0x85, 0xC0]); // test eax, eax
    // The displacement is measured from the end of this jnz, which is where the child part
    // starts, so it skips exactly the child part and lands on the parent part.
    bytes.extend_from_slice(&[0x75, jump]); // jnz parent
    bytes.extend_from_slice(&child);
    bytes.extend_from_slice(&call(Entry::Native, 61, &[0xFFFF_FFFF, 0, 0, 0])); // wait4(-1)
    bytes.extend_from_slice(&EXIT_ZERO);
    Ok(bytes)
}

/// A `clone3` whose structure the kernel rejects with `EINVAL`, then the exit with its result.
///
/// The structure follows the code in the same segment, so its address is known before it is
/// written.
fn clone3_rejected() -> io::Result<Vec<u8>> {
    let head_length =
        call(Entry::Native, 435, &[0, CLONE_ARGS_BYTES]).len() + EXIT_WITH_RESULT.len();
    let head_length = u32::try_from(head_length)
        .map_err(|_| invalid("the clone3 head is too long for a 32-bit address"))?;
    let address = ENTRY_ADDRESS
        .checked_add(head_length)
        .ok_or_else(|| invalid("the clone3 structure address overflows"))?;
    let mut bytes = call(Entry::Native, 435, &[address, CLONE_ARGS_BYTES]);
    bytes.extend_from_slice(&EXIT_WITH_RESULT);
    // clone_args: flags, pidfd, child_tid, parent_tid, exit_signal, stack, stack_size, tls.
    for field in [CLONE_NEWUSER, 0, 0, 0, CLONE3_EXIT_SIGNAL, 0, 0, 0] {
        bytes.extend_from_slice(&field.to_le_bytes());
    }
    Ok(bytes)
}

/// The instructions that follow the headers: the probe's body.
fn code(probe: &Probe) -> io::Result<Vec<u8>> {
    match probe.body {
        Body::Single => Ok(probe_part(probe)),
        Body::ForkedChild => forked_child(probe),
        Body::Clone3Rejected => clone3_rejected(),
    }
}

/// A static ELF64 x86-64 executable: one `PT_LOAD` segment with read and execute permission
/// that covers the whole file, and the code at the entry address.
fn elf(code: &[u8]) -> io::Result<Vec<u8>> {
    let headers = u64::from(ELF_HEADER_BYTES) + u64::from(PROGRAM_HEADER_BYTES);
    let file_length = u64::try_from(code.len())
        .ok()
        .and_then(|length| length.checked_add(headers))
        .ok_or_else(|| invalid("the probe program is too long"))?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&[0x7F, b'E', b'L', b'F']); // magic
    // ELFCLASS64, ELFDATA2LSB, EV_CURRENT, ELFOSABI_NONE, ABI version 0
    bytes.extend_from_slice(&[2, 1, 1, 0, 0]);
    bytes.extend_from_slice(&[0; 7]); // padding of e_ident
    bytes.extend_from_slice(&2_u16.to_le_bytes()); // e_type: ET_EXEC
    bytes.extend_from_slice(&0x3E_u16.to_le_bytes()); // e_machine: x86-64
    bytes.extend_from_slice(&1_u32.to_le_bytes()); // e_version
    bytes.extend_from_slice(&u64::from(ENTRY_ADDRESS).to_le_bytes()); // e_entry
    bytes.extend_from_slice(&u64::from(ELF_HEADER_BYTES).to_le_bytes()); // e_phoff
    bytes.extend_from_slice(&0_u64.to_le_bytes()); // e_shoff
    bytes.extend_from_slice(&0_u32.to_le_bytes()); // e_flags
    bytes.extend_from_slice(&ELF_HEADER_BYTES.to_le_bytes()); // e_ehsize
    bytes.extend_from_slice(&PROGRAM_HEADER_BYTES.to_le_bytes()); // e_phentsize
    bytes.extend_from_slice(&1_u16.to_le_bytes()); // e_phnum
    bytes.extend_from_slice(&[0; 6]); // e_shentsize, e_shnum, e_shstrndx: no sections
    bytes.extend_from_slice(&1_u32.to_le_bytes()); // p_type: PT_LOAD
    bytes.extend_from_slice(&5_u32.to_le_bytes()); // p_flags: PF_R | PF_X
    bytes.extend_from_slice(&0_u64.to_le_bytes()); // p_offset
    bytes.extend_from_slice(&LOAD_ADDRESS.to_le_bytes()); // p_vaddr
    bytes.extend_from_slice(&LOAD_ADDRESS.to_le_bytes()); // p_paddr
    bytes.extend_from_slice(&file_length.to_le_bytes()); // p_filesz
    bytes.extend_from_slice(&file_length.to_le_bytes()); // p_memsz
    bytes.extend_from_slice(&0x1000_u64.to_le_bytes()); // p_align
    bytes.extend_from_slice(code);
    Ok(bytes)
}

/// The complete program file of one probe.
fn program(probe: &Probe) -> io::Result<Vec<u8>> {
    elf(&code(probe)?)
}

/// An `InvalidInput` error for a probe that the byte builders cannot encode.
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// Writes every probe program into `directory` as `<directory>/<name>` with mode `0o755`.
///
/// This is test-support only (decision 0056 rule 4 and its amendment of 2026-11-13). The
/// enforcement job's `prepare.sh` calls it through the fixture's `write-probes` mode. It creates
/// the directory when it is missing, writes nothing else, and runs no probe.
///
/// # Errors
/// Returns the first I/O failure, or `InvalidInput` for a probe that cannot be encoded. Files
/// written before the failure are left in place.
pub fn write_probe_programs(directory: &Path) -> io::Result<()> {
    fs::create_dir_all(directory)?;
    for probe in &PROBES {
        let path = directory.join(probe.name);
        fs::write(&path, program(probe)?)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
