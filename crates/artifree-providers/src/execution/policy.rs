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
//! cannot raise or lower its limits. On other architectures `fixture_filters` returns
//! `PolicyError`, and its caller must refuse launch.

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

/// The syscalls the allowlist allows unconditionally, as `(name, number)` pairs sorted by number.
/// The names serve review and the table test; the numbers are what the filter uses. `clone3` is
/// listed too, so that the refusal's `ENOSYS` is its result and not a kill (see `clone3_refusal`).
#[cfg(target_arch = "x86_64")]
pub(super) const ALLOWED: [(&str, i64); 40] = [
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
/// query-only rule of `prlimit64`, and the rule of `clone` that admits only calls without a
/// namespace flag.
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
    Ok(rules)
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

/// The SHA-256 of the fixture profile's two programs, the refusal first and then the allowlist.
/// Each instruction contributes its code (little-endian `u16`), its two jump offsets (one byte
/// each) and its constant (little-endian `u32`), in program order.
///
/// # Errors
/// Returns `PolicyError` when the fixture filters cannot be built.
#[cfg(test)]
pub(super) fn policy_sha256() -> Result<String, PolicyError> {
    let programs = fixture_filters()?;
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
