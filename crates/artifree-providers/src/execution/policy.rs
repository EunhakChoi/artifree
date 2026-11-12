//! The fixture profile's seccomp filter: a default-deny allowlist for `x86_64` (decisions 0042
//! rule 4 and 0056 rule 3). The public syscall mapping of 0042 rule 8 belongs to a later slice.
//!
//! Every syscall that is not listed kills the process. `prlimit64` is allowed only as a query,
//! so the payload cannot raise or lower its limits. On other architectures `fixture_filter`
//! returns `PolicyError`, and its caller must refuse launch.

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

/// The syscalls every fixture filter allows unconditionally, as `(name, number)` pairs sorted
/// by number. The names serve review and the table test; the numbers are what the filter uses.
#[cfg(target_arch = "x86_64")]
pub(super) const ALLOWED: [(&str, i64); 27] = [
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
    ("pread64", 17),
    ("access", 21),
    ("execve", 59),
    ("exit", 60),
    ("sigaltstack", 131),
    ("arch_prctl", 158),
    ("gettid", 186),
    ("futex", 202),
    ("sched_getaffinity", 204),
    ("set_tid_address", 218),
    ("exit_group", 231),
    ("openat", 257),
    ("newfstatat", 262),
    ("set_robust_list", 273),
    ("getrandom", 318),
    ("rseq", 334),
];

/// The one syscall allowed only as a query: `prlimit64` may read a limit but never set one.
#[cfg(target_arch = "x86_64")]
pub(super) const QUERY_ONLY: (&str, i64) = ("prlimit64", 302);

/// The argument index of `prlimit64`'s new-limit pointer. Zero there means the call only reads.
#[cfg(target_arch = "x86_64")]
const NEW_LIMIT_ARGUMENT: u8 = 2;

/// Build the fixture profile's seccomp program, the one the bootstrap installs.
///
/// # Errors
/// Returns `PolicyError` when the architecture is not `x86_64` or the compiler rejects the rules.
pub(super) fn fixture_filter() -> Result<BpfProgram, PolicyError> {
    #[cfg(target_arch = "x86_64")]
    {
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
        compile(rules)
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        Err(PolicyError)
    }
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

/// The SHA-256 of the fixture filter's instructions, each as its code (little-endian `u16`),
/// its two jump offsets (one byte each) and its constant (little-endian `u32`), in program order.
///
/// # Errors
/// Returns `PolicyError` when the fixture filter cannot be built.
#[cfg(test)]
pub(super) fn policy_sha256() -> Result<String, PolicyError> {
    let program = fixture_filter()?;
    let mut bytes = Vec::with_capacity(program.len() * 8);
    for instruction in &program {
        bytes.extend_from_slice(&instruction.code.to_le_bytes());
        bytes.push(instruction.jt);
        bytes.push(instruction.jf);
        bytes.extend_from_slice(&instruction.k.to_le_bytes());
    }
    Ok(super::report::sha256_hex(&bytes))
}

#[cfg(test)]
mod tests;
