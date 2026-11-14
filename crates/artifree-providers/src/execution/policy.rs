//! The fixture profile's seccomp programs for `x86_64` (decisions 0042 rule 4 and 0056 rule 3).
//! The public syscall mapping of 0042 rule 8 belongs to a later slice.
//!
//! The bootstrap installs two programs, in this order. The first is the refusal: it makes
//! `clone3` fail with `ENOSYS`, so libc falls back to `clone`. The second is the allowlist: it
//! reads the flags of `clone`, which `clone3` passes behind a pointer that no filter can read.
//! The allowlist is installed last, because `seccompiler` sets `no_new_privs` and installs its
//! program through `prctl` and `seccomp`, and the allowlist kills the process on those calls.
//!
//! The allowlist kills the process on any syscall it does not list. `clone` is allowed only when
//! its flags carry no namespace flag, and `prlimit64` is allowed only as a query, so the payload
//! cannot raise or lower its limits. `socket` is allowed only for the local forms of the fixture
//! profile: `AF_UNIX` stream or datagram with protocol 0; `AF_INET` and `AF_INET6` stream with
//! protocol 0 or TCP, and datagram with protocol 0 or UDP. The close-on-exec and non-blocking
//! flags may be added to the type. `setsockopt` is allowed only for `SO_REUSEADDR` at the socket
//! level (decision 0056, amendment of 2026-11-13, "files, descriptors and network"). On other
//! architectures `fixture_filters` returns `PolicyError`, and its caller must refuse launch.

#[cfg(target_arch = "x86_64")]
use std::collections::BTreeMap;

use seccompiler::BpfProgram;
#[cfg(target_arch = "x86_64")]
use seccompiler::{
    SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter, SeccompRule,
    TargetArch,
};

/// The filter could not be built, because the architecture has no reviewed table or the
/// compiler rejected the rules. Launch must refuse in that case (decision 0042 rule 4).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PolicyError;

/// The system call filter of an execution profile (decision 0058 rule 3). Each profile names
/// one, and the bootstrap installs only that one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FilterSet {
    /// The fixture profile's refusal and allowlist.
    Fixture,
    /// The bundler measurement's programs (decision 0058, note "R03bcb split"): the calls that
    /// the public syscall mapping refuses kill the process, the fixture's allowlist and the calls
    /// measured so far are allowed, and every other call is allowed and leaves a kernel record.
    /// Only the test build and the `enforcement-fixture` build know it.
    #[cfg(any(test, feature = "enforcement-fixture"))]
    Measure,
}

/// The two programs of `set`, in installation order. For `Measure`, no call is added to the
/// allowlist; see `filters_with`.
///
/// # Errors
/// Returns `PolicyError` when the architecture is not `x86_64` or the compiler rejects the rules.
#[cfg(any(test, feature = "enforcement-fixture"))]
pub(super) fn filters(set: FilterSet) -> Result<[BpfProgram; 2], PolicyError> {
    filters_with(set, &[])
}

/// The two programs of `set`, in installation order. `measured` lists the calls a measurement
/// has already seen; only `Measure` allows them, so that each round records only calls it has
/// not seen before. The other sets ignore it.
///
/// # Errors
/// Returns `PolicyError` when the architecture is not `x86_64` or the compiler rejects the rules.
pub(super) fn filters_with(
    set: FilterSet,
    measured: &[i64],
) -> Result<[BpfProgram; 2], PolicyError> {
    match set {
        FilterSet::Fixture => {
            let _ = measured;
            fixture_filters()
        }
        #[cfg(any(test, feature = "enforcement-fixture"))]
        FilterSet::Measure => measure_filters(measured),
    }
}

/// The syscalls the allowlist allows unconditionally, as `(name, number)` pairs sorted by number.
/// The names serve review and the table test; the numbers are what the filter uses. `clone3` is
/// listed too, so that the refusal's `ENOSYS` is its result and not a kill (see `clone3_refusal`).
#[cfg(target_arch = "x86_64")]
pub(super) const ALLOWED: [(&str, i64); 48] = [
    ("read", 0),
    ("write", 1),
    ("close", 3),
    ("fstat", 5),
    ("poll", 7),
    ("mmap", 9),
    ("mprotect", 10),
    ("munmap", 11),
    ("brk", 12),
    ("rt_sigaction", 13),
    ("rt_sigprocmask", 14),
    ("rt_sigreturn", 15),
    ("pread64", 17),
    ("access", 21),
    ("madvise", 28),
    // The fixture profile allows communication inside the attempt (decision 0056, amendment of
    // 2026-11-13, "files, descriptors and network"). The attempt's network namespace, not the
    // filter, keeps it inside. `socket` and `setsockopt` are allowed only in the forms of
    // `socket_rules` and `setsockopt_rule`, so they are not listed here.
    ("connect", 42),
    ("sendto", 44),
    ("recvfrom", 45),
    ("shutdown", 48),
    ("bind", 49),
    ("listen", 50),
    ("getsockname", 51),
    ("execve", 59),
    ("exit", 60),
    ("wait4", 61),
    // The survivors scenario signals its own child. The payload runs in its own PID namespace, so
    // a signal reaches only processes of its attempt.
    ("kill", 62),
    ("fcntl", 72),
    ("mkdir", 83),
    ("unlink", 87),
    ("setsid", 112),
    ("sigaltstack", 131),
    ("arch_prctl", 158),
    ("gettid", 186),
    ("futex", 202),
    ("sched_getaffinity", 204),
    ("getdents64", 217),
    ("set_tid_address", 218),
    // Read-only clock; the vDSO usually answers it, but a host whose clock source the vDSO
    // cannot use makes it a real system call (the PR #190 review).
    ("clock_gettime", 228),
    ("clock_nanosleep", 230),
    ("exit_group", 231),
    ("openat", 257),
    ("newfstatat", 262),
    ("set_robust_list", 273),
    // Local networking, as above.
    ("accept4", 288),
    ("getrandom", 318),
    ("statx", 332),
    ("rseq", 334),
    CLONE3,
];

/// The one syscall allowed only as a query: `prlimit64` may read a limit but never set one.
#[cfg(target_arch = "x86_64")]
pub(super) const QUERY_ONLY: (&str, i64) = ("prlimit64", 302);

/// The `clone` system call. The allowlist allows it only when its flags carry no namespace flag
/// (see `NAMESPACE_FLAGS`), so it is not in `ALLOWED`.
#[cfg(target_arch = "x86_64")]
pub(super) const CLONE: (&str, i64) = ("clone", 56);

/// The `clone3` system call. The refusal answers it with `ENOSYS`; it is also in `ALLOWED`.
#[cfg(target_arch = "x86_64")]
pub(super) const CLONE3: (&str, i64) = ("clone3", 435);

/// The `socket` system call. It is allowed only for the forms of `SOCKET_FORMS`, so it is not in
/// `ALLOWED`.
#[cfg(target_arch = "x86_64")]
pub(super) const SOCKET: (&str, i64) = ("socket", 41);

/// The `setsockopt` system call. It is allowed only for `SO_REUSEADDR` at the socket level, which
/// std's `TcpListener::bind` sets, so it is not in `ALLOWED`.
#[cfg(target_arch = "x86_64")]
pub(super) const SETSOCKOPT: (&str, i64) = ("setsockopt", 54);

/// The argument index of `prlimit64`'s new-limit pointer. Zero there means the call only reads.
#[cfg(target_arch = "x86_64")]
const NEW_LIMIT_ARGUMENT: u8 = 2;

/// The argument index of `clone`'s flags, which carry the namespace bits.
#[cfg(target_arch = "x86_64")]
const CLONE_FLAGS_ARGUMENT: u8 = 0;

/// The error number the refusal returns for `clone3`. libc reads `ENOSYS` as "no `clone3` here"
/// and falls back to `clone`.
#[cfg(target_arch = "x86_64")]
const ENOSYS: u32 = 38;

/// `CLONE_NEWTIME`: a new time namespace.
#[cfg(target_arch = "x86_64")]
const CLONE_NEWTIME: u64 = 0x80;

/// `CLONE_NEWNS`: a new mount namespace.
#[cfg(target_arch = "x86_64")]
const CLONE_NEWNS: u64 = 0x0002_0000;

/// `CLONE_NEWCGROUP`: a new cgroup namespace.
#[cfg(target_arch = "x86_64")]
const CLONE_NEWCGROUP: u64 = 0x0200_0000;

/// `CLONE_NEWUTS`: a new UTS namespace.
#[cfg(target_arch = "x86_64")]
const CLONE_NEWUTS: u64 = 0x0400_0000;

/// `CLONE_NEWIPC`: a new IPC namespace.
#[cfg(target_arch = "x86_64")]
const CLONE_NEWIPC: u64 = 0x0800_0000;

/// `CLONE_NEWUSER`: a new user namespace.
#[cfg(target_arch = "x86_64")]
const CLONE_NEWUSER: u64 = 0x1000_0000;

/// `CLONE_NEWPID`: a new PID namespace.
#[cfg(target_arch = "x86_64")]
const CLONE_NEWPID: u64 = 0x2000_0000;

/// `CLONE_NEWNET`: a new network namespace.
#[cfg(target_arch = "x86_64")]
const CLONE_NEWNET: u64 = 0x4000_0000;

/// Every namespace flag of `clone`. A `clone` that sets any of them creates a namespace, which
/// the fixture profile never allows.
#[cfg(target_arch = "x86_64")]
pub(super) const NAMESPACE_FLAGS: u64 = CLONE_NEWTIME
    | CLONE_NEWNS
    | CLONE_NEWCGROUP
    | CLONE_NEWUTS
    | CLONE_NEWIPC
    | CLONE_NEWUSER
    | CLONE_NEWPID
    | CLONE_NEWNET;

/// `AF_UNIX`: the local (Unix domain) socket family.
#[cfg(target_arch = "x86_64")]
const AF_UNIX: u64 = 1;

/// `AF_INET`: the `IPv4` socket family.
#[cfg(target_arch = "x86_64")]
const AF_INET: u64 = 2;

/// `AF_INET6`: the IPv6 socket family.
#[cfg(target_arch = "x86_64")]
const AF_INET6: u64 = 10;

/// `SOCK_STREAM`: a connection-oriented byte stream.
#[cfg(target_arch = "x86_64")]
const SOCK_STREAM: u64 = 1;

/// `SOCK_DGRAM`: a datagram socket.
#[cfg(target_arch = "x86_64")]
const SOCK_DGRAM: u64 = 2;

/// `IPPROTO_TCP`: the TCP protocol number.
#[cfg(target_arch = "x86_64")]
const IPPROTO_TCP: u64 = 6;

/// `IPPROTO_UDP`: the UDP protocol number.
#[cfg(target_arch = "x86_64")]
const IPPROTO_UDP: u64 = 17;

/// The type bits of `socket`'s second argument. `SOCK_CLOEXEC` (0x80000) and `SOCK_NONBLOCK`
/// (0x800) lie outside the mask, so a type that carries either flag still matches its base type.
#[cfg(target_arch = "x86_64")]
const SOCK_TYPE_MASK: u64 = 0xF;

/// `SOL_SOCKET`: the socket level of `setsockopt`.
#[cfg(target_arch = "x86_64")]
const SOL_SOCKET: u64 = 1;

/// `SO_REUSEADDR`: the socket option that lets a listener rebind an address in `TIME_WAIT`.
#[cfg(target_arch = "x86_64")]
const SO_REUSEADDR: u64 = 2;

/// The argument index of `socket`'s domain.
#[cfg(target_arch = "x86_64")]
const SOCKET_DOMAIN_ARGUMENT: u8 = 0;

/// The argument index of `socket`'s type, whose type bits `SOCK_TYPE_MASK` selects.
#[cfg(target_arch = "x86_64")]
const SOCKET_TYPE_ARGUMENT: u8 = 1;

/// The argument index of `socket`'s protocol.
#[cfg(target_arch = "x86_64")]
const SOCKET_PROTOCOL_ARGUMENT: u8 = 2;

/// The argument index of `setsockopt`'s level.
#[cfg(target_arch = "x86_64")]
const SETSOCKOPT_LEVEL_ARGUMENT: u8 = 1;

/// The argument index of `setsockopt`'s option name.
#[cfg(target_arch = "x86_64")]
const SETSOCKOPT_OPTION_ARGUMENT: u8 = 2;

/// The `(domain, type, protocol)` forms that `socket` may take (decision 0056, amendment of
/// 2026-11-13). Keep the order: the compiled program, and so the policy digest, depends on it.
#[cfg(target_arch = "x86_64")]
const SOCKET_FORMS: [(u64, u64, u64); 10] = [
    (AF_UNIX, SOCK_STREAM, 0),
    (AF_UNIX, SOCK_DGRAM, 0),
    (AF_INET, SOCK_STREAM, 0),
    (AF_INET, SOCK_STREAM, IPPROTO_TCP),
    (AF_INET, SOCK_DGRAM, 0),
    (AF_INET, SOCK_DGRAM, IPPROTO_UDP),
    (AF_INET6, SOCK_STREAM, 0),
    (AF_INET6, SOCK_STREAM, IPPROTO_TCP),
    (AF_INET6, SOCK_DGRAM, 0),
    (AF_INET6, SOCK_DGRAM, IPPROTO_UDP),
];

/// Build the fixture profile's two seccomp programs in installation order: the `clone3` refusal
/// first, then the allowlist.
///
/// The order is the installation order, and it matters. The allowlist must be installed last,
/// because `seccompiler`'s `apply_filter` calls `prctl` and `seccomp`, which the allowlist kills
/// (measured on the Linux runner on 2026-11-12).
///
/// # Errors
/// Returns `PolicyError` when the architecture is not `x86_64` or the compiler rejects the rules.
pub(super) fn fixture_filters() -> Result<[BpfProgram; 2], PolicyError> {
    #[cfg(target_arch = "x86_64")]
    {
        Ok([clone3_refusal()?, allowlist()?])
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        Err(PolicyError)
    }
}

/// The bundler measurement's two programs: the mapping's refusals, then the logging allowlist.
/// The refusals come first for the same reason the fixture's do: the second program's default
/// action is `LOG`, which would let `prctl` and `seccomp` through, but the first kills them.
///
/// # Errors
/// Returns `PolicyError` when the architecture is not `x86_64` or the compiler rejects the rules.
#[cfg(any(test, feature = "enforcement-fixture"))]
fn measure_filters(measured: &[i64]) -> Result<[BpfProgram; 2], PolicyError> {
    #[cfg(target_arch = "x86_64")]
    {
        let allowed = allowlist_rules()?;
        let mut refused: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
        for number in refused_numbers(&allowed) {
            refused.insert(number, Vec::new());
        }
        let refusal = SeccompFilter::new(
            refused,
            SeccompAction::Allow,
            SeccompAction::KillProcess,
            TargetArch::x86_64,
        )
        .map_err(|_| PolicyError)?;
        let mut logged = allowed;
        for &number in measured {
            // A measured call that the allowlist restricts by its arguments keeps its rules, so
            // a refused form of it is still recorded.
            logged.entry(number).or_default();
        }
        let logging = SeccompFilter::new(
            logged,
            SeccompAction::Log,
            SeccompAction::Allow,
            TargetArch::x86_64,
        )
        .map_err(|_| PolicyError)?;
        Ok([
            BpfProgram::try_from(refusal).map_err(|_| PolicyError)?,
            BpfProgram::try_from(logging).map_err(|_| PolicyError)?,
        ])
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = measured;
        Err(PolicyError)
    }
}

/// The calls the measurement kills: every native call the public syscall mapping refuses
/// outright (the probes of each class but `default`, whose calls are refused only because the
/// fixture does not list them), and `clone3`. A call that the fixture's allowlist allows in some
/// form (`clone`, `prlimit64`, `socket`, `setsockopt`) is not killed by number; its refused forms
/// fall through to the logging program, and the enforcement job fails when such a record appears.
#[cfg(all(target_arch = "x86_64", any(test, feature = "enforcement-fixture")))]
pub(super) fn refused_numbers(allowed: &BTreeMap<i64, Vec<SeccompRule>>) -> Vec<i64> {
    let (_, clone3_number) = CLONE3;
    let mut numbers: Vec<i64> = super::probes::PROBES
        .iter()
        .filter(|probe| {
            probe.entry == super::probes::Entry::Native
                && probe.class != "default"
                && matches!(probe.expect, super::probes::Expect::Refused { .. })
        })
        .map(|probe| i64::from(probe.number))
        .filter(|number| !allowed.contains_key(number))
        .chain([clone3_number])
        .collect();
    numbers.sort_unstable();
    numbers.dedup();
    numbers
}

/// The refusal program: `clone3` fails with `ENOSYS`, and every other call is allowed by this
/// program. libc then falls back to `clone`, whose flags the allowlist can read.
///
/// The kernel applies the most restrictive action of all installed programs. The refusal's
/// `ERRNO` therefore wins over the allowlist's `ALLOW` for `clone3`, while a `KILL` from the
/// allowlist would win over the `ERRNO`; that is why `clone3` is in `ALLOWED`.
///
/// # Errors
/// Returns `PolicyError` when the compiler rejects the rules.
#[cfg(target_arch = "x86_64")]
pub(super) fn clone3_refusal() -> Result<BpfProgram, PolicyError> {
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    let (_, clone3_number) = CLONE3;
    // An empty rule vector matches the syscall whatever its arguments.
    rules.insert(clone3_number, Vec::new());
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(ENOSYS),
        TargetArch::x86_64,
    )
    .map_err(|_| PolicyError)?;
    BpfProgram::try_from(filter).map_err(|_| PolicyError)
}

/// The allowlist program: every syscall without a matching rule kills the process.
///
/// # Errors
/// Returns `PolicyError` when the compiler rejects the rules.
#[cfg(target_arch = "x86_64")]
pub(super) fn allowlist() -> Result<BpfProgram, PolicyError> {
    compile(allowlist_rules()?)
}

/// The allowlist's rules: every unconditional entry of `ALLOWED` with an empty rule vector, the
/// query-only rule of `prlimit64`, the rule of `clone` that admits only calls without a namespace
/// flag, the rule of `socket` with one rule per allowed form, and the rule of `setsockopt`.
///
/// # Errors
/// Returns `PolicyError` when the compiler rejects a condition or a rule.
#[cfg(target_arch = "x86_64")]
fn allowlist_rules() -> Result<BTreeMap<i64, Vec<SeccompRule>>, PolicyError> {
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    for (_, number) in ALLOWED {
        // An empty rule vector allows the syscall whatever its arguments.
        rules.insert(number, Vec::new());
    }
    let (_, query_number) = QUERY_ONLY;
    let new_limit_is_null = SeccompCondition::new(
        NEW_LIMIT_ARGUMENT,
        SeccompCmpArgLen::Qword,
        SeccompCmpOp::Eq,
        0,
    )
    .map_err(|_| PolicyError)?;
    let query_rule = SeccompRule::new(vec![new_limit_is_null]).map_err(|_| PolicyError)?;
    rules.insert(query_number, vec![query_rule]);
    let (_, clone_number) = CLONE;
    let no_namespace_flags = SeccompCondition::new(
        CLONE_FLAGS_ARGUMENT,
        SeccompCmpArgLen::Qword,
        SeccompCmpOp::MaskedEq(NAMESPACE_FLAGS),
        0,
    )
    .map_err(|_| PolicyError)?;
    let clone_rule = SeccompRule::new(vec![no_namespace_flags]).map_err(|_| PolicyError)?;
    rules.insert(clone_number, vec![clone_rule]);
    let (_, socket_number) = SOCKET;
    rules.insert(socket_number, socket_rules()?);
    let (_, setsockopt_number) = SETSOCKOPT;
    rules.insert(setsockopt_number, vec![setsockopt_rule()?]);
    Ok(rules)
}

/// The rules of `socket`: one rule per form in `SOCKET_FORMS`, each matching the domain, the type
/// under `SOCK_TYPE_MASK`, and the protocol. A call that matches none of them is refused.
///
/// # Errors
/// Returns `PolicyError` when the compiler rejects a condition or a rule.
#[cfg(target_arch = "x86_64")]
fn socket_rules() -> Result<Vec<SeccompRule>, PolicyError> {
    let mut rules = Vec::new();
    for (domain, kind, protocol) in SOCKET_FORMS {
        let conditions = vec![
            dword(SOCKET_DOMAIN_ARGUMENT, SeccompCmpOp::Eq, domain)?,
            dword(
                SOCKET_TYPE_ARGUMENT,
                SeccompCmpOp::MaskedEq(SOCK_TYPE_MASK),
                kind,
            )?,
            dword(SOCKET_PROTOCOL_ARGUMENT, SeccompCmpOp::Eq, protocol)?,
        ];
        rules.push(SeccompRule::new(conditions).map_err(|_| PolicyError)?);
    }
    Ok(rules)
}

/// The rule of `setsockopt`: the socket level and the option `SO_REUSEADDR` together.
///
/// # Errors
/// Returns `PolicyError` when the compiler rejects a condition or the rule.
#[cfg(target_arch = "x86_64")]
fn setsockopt_rule() -> Result<SeccompRule, PolicyError> {
    SeccompRule::new(vec![
        dword(SETSOCKOPT_LEVEL_ARGUMENT, SeccompCmpOp::Eq, SOL_SOCKET)?,
        dword(SETSOCKOPT_OPTION_ARGUMENT, SeccompCmpOp::Eq, SO_REUSEADDR)?,
    ])
    .map_err(|_| PolicyError)
}

/// One `Dword` condition: the low four bytes of argument `argument` compared with `value`.
///
/// # Errors
/// Returns `PolicyError` when the compiler rejects the condition.
#[cfg(target_arch = "x86_64")]
fn dword(
    argument: u8,
    operator: SeccompCmpOp,
    value: u64,
) -> Result<SeccompCondition, PolicyError> {
    SeccompCondition::new(argument, SeccompCmpArgLen::Dword, operator, value)
        .map_err(|_| PolicyError)
}

/// Compile the rules into a program that kills the process on any syscall without a matching rule.
#[cfg(target_arch = "x86_64")]
fn compile(rules: BTreeMap<i64, Vec<SeccompRule>>) -> Result<BpfProgram, PolicyError> {
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::KillProcess,
        SeccompAction::Allow,
        TargetArch::x86_64,
    )
    .map_err(|_| PolicyError)?;
    BpfProgram::try_from(filter).map_err(|_| PolicyError)
}

/// The SHA-256 of the two programs of `set`, the refusal first and then the allowlist.
/// Each instruction contributes its code (little-endian `u16`), its two jump offsets (one byte
/// each) and its constant (little-endian `u32`), in program order. Only the launch of the test
/// build and of the `enforcement-fixture` feature uses it (decision 0057 rule 2).
///
/// # Errors
/// Returns `PolicyError` when the filters cannot be built.
#[cfg(any(test, feature = "enforcement-fixture"))]
pub(super) fn policy_sha256(set: FilterSet) -> Result<String, PolicyError> {
    let programs = filters(set)?;
    let mut bytes = Vec::new();
    for program in &programs {
        for instruction in program {
            bytes.extend_from_slice(&instruction.code.to_le_bytes());
            bytes.push(instruction.jt);
            bytes.push(instruction.jf);
            bytes.extend_from_slice(&instruction.k.to_le_bytes());
        }
    }
    Ok(super::report::sha256_hex(&bytes))
}

#[cfg(test)]
mod tests;

#[cfg(all(test, target_arch = "x86_64"))]
mod filter_tests;

#[cfg(all(test, target_arch = "x86_64"))]
mod measure_tests;
