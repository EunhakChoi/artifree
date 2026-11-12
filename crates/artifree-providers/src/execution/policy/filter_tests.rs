//! Behavioral tests for the two seccomp programs of the fixture profile: the namespace rule of
//! `clone`, the refusal of `clone3` and the installation order (decisions 0042 rule 4 and 0056
//! rule 3). The programs are compared with copies built here from their specification, so a
//! change to either side makes a test fail. No filter is installed in this process.

use std::collections::BTreeMap;

use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch,
};

use super::super::report::sha256_hex;
use super::{
    ALLOWED, NAMESPACE_FLAGS, PolicyError, allowlist, clone3_refusal, fixture_filters,
    policy_sha256,
};

/// The `clone` system call number on `x86_64`.
const CLONE: i64 = 56;
/// The `clone3` system call number on `x86_64`.
const CLONE3: i64 = 435;
/// The `prlimit64` system call number on `x86_64`, the one call the allowlist admits as a query.
const PRLIMIT64: i64 = 302;
/// The eight namespace flags as one literal, so that the test does not read the constant it checks.
const NAMESPACE_MASK: u64 = 0x7E02_0080;
/// `CLONE_NEWUSER`, one of the eight namespace flags.
const CLONE_NEWUSER: u64 = 0x1000_0000;
/// `ENOSYS`, the error that makes libc fall back from `clone3` to `clone`.
const ENOSYS: u32 = 38;

/// The rule that matches a call only when its first argument, masked with `mask`, is zero.
fn flags_rule(mask: u64) -> Result<SeccompRule, PolicyError> {
    let condition =
        SeccompCondition::new(0, SeccompCmpArgLen::Qword, SeccompCmpOp::MaskedEq(mask), 0)
            .map_err(|_| PolicyError)?;
    SeccompRule::new(vec![condition]).map_err(|_| PolicyError)
}

/// The rule of `prlimit64` that matches only when its third argument, the new limit, is null.
fn query_rule() -> Result<SeccompRule, PolicyError> {
    let condition = SeccompCondition::new(2, SeccompCmpArgLen::Qword, SeccompCmpOp::Eq, 0)
        .map_err(|_| PolicyError)?;
    SeccompRule::new(vec![condition]).map_err(|_| PolicyError)
}

/// Builds the allowlist from its specification: every table entry without a condition,
/// `prlimit64` as a query, and `clone` with the given rules. `None` leaves `clone` out, and an
/// empty rule vector allows it whatever its arguments. A call without a rule kills the process.
fn allowlist_with_clone(clone_rules: Option<Vec<SeccompRule>>) -> Result<BpfProgram, PolicyError> {
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    for (_, number) in ALLOWED {
        rules.insert(number, Vec::new());
    }
    rules.insert(PRLIMIT64, vec![query_rule()?]);
    if let Some(clone_rules) = clone_rules {
        rules.insert(CLONE, clone_rules);
    }
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::KillProcess,
        SeccompAction::Allow,
        TargetArch::x86_64,
    )
    .map_err(|_| PolicyError)?;
    BpfProgram::try_from(filter).map_err(|_| PolicyError)
}

/// Builds the `clone3` refusal from its specification: `clone3` takes `matched`, and every other
/// call takes `mismatched`.
fn clone3_program(
    mismatched: SeccompAction,
    matched: SeccompAction,
) -> Result<BpfProgram, PolicyError> {
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    // An empty rule vector matches `clone3` whatever its arguments.
    rules.insert(CLONE3, Vec::new());
    let filter = SeccompFilter::new(rules, mismatched, matched, TargetArch::x86_64)
        .map_err(|_| PolicyError)?;
    BpfProgram::try_from(filter).map_err(|_| PolicyError)
}

/// The bytes that `policy_sha256` covers for `programs`, in order: each instruction's code as a
/// little-endian `u16`, its two jump offsets as one byte each, and its constant as a
/// little-endian `u32`.
fn serialized(programs: &[BpfProgram]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for program in programs {
        for instruction in program {
            bytes.extend_from_slice(&instruction.code.to_le_bytes());
            bytes.push(instruction.jt);
            bytes.push(instruction.jf);
            bytes.extend_from_slice(&instruction.k.to_le_bytes());
        }
    }
    bytes
}

#[test]
fn namespace_mask_is_every_namespace_flag() {
    assert_eq!(
        NAMESPACE_FLAGS, NAMESPACE_MASK,
        "the namespace mask must be the eight CLONE_NEW* flags, got {NAMESPACE_FLAGS:#X}"
    );
}

#[test]
fn allowlist_allows_clone_only_without_namespace_flags() -> Result<(), PolicyError> {
    let expected = allowlist_with_clone(Some(vec![flags_rule(NAMESPACE_MASK)?]))?;
    assert_eq!(
        allowlist()?,
        expected,
        "clone must be allowed only when its flags masked with the namespace mask are zero"
    );
    let variants = [
        (
            "clone is allowed whatever its flags",
            allowlist_with_clone(Some(Vec::new()))?,
        ),
        ("clone is left out", allowlist_with_clone(None)?),
        (
            "CLONE_NEWUSER is left out of the mask",
            allowlist_with_clone(Some(vec![flags_rule(NAMESPACE_MASK & !CLONE_NEWUSER)?]))?,
        ),
    ];
    for (case, variant) in variants {
        assert_ne!(
            allowlist()?,
            variant,
            "the allowlist must differ from the specification when {case}"
        );
    }
    Ok(())
}

#[test]
fn clone3_refusal_answers_enosys_and_allows_the_rest() -> Result<(), PolicyError> {
    let refusal = clone3_refusal()?;
    assert_eq!(
        refusal,
        clone3_program(SeccompAction::Allow, SeccompAction::Errno(ENOSYS))?,
        "clone3 must fail with ENOSYS and every other call must be allowed by this program"
    );
    assert_ne!(
        refusal,
        clone3_program(SeccompAction::Allow, SeccompAction::Errno(1))?,
        "the refusal must return ENOSYS, not another error number"
    );
    assert_ne!(
        refusal,
        clone3_program(SeccompAction::Allow, SeccompAction::KillProcess)?,
        "the refusal must not kill the process on clone3"
    );
    assert_ne!(
        refusal,
        clone3_program(SeccompAction::KillProcess, SeccompAction::Errno(ENOSYS))?,
        "the refusal must allow every other call, not kill it"
    );
    Ok(())
}

#[test]
fn programs_install_the_refusal_first_and_the_allowlist_last() -> Result<(), PolicyError> {
    assert_eq!(
        fixture_filters()?,
        [clone3_refusal()?, allowlist()?],
        "the refusal must be installed first and the allowlist last"
    );
    assert_ne!(
        fixture_filters()?,
        [allowlist()?, clone3_refusal()?],
        "installing the allowlist first would kill the process on the next apply_filter"
    );
    Ok(())
}

#[test]
fn digest_covers_both_programs_in_order() -> Result<(), PolicyError> {
    let digest = policy_sha256()?;
    assert_eq!(
        digest,
        sha256_hex(&serialized(&[clone3_refusal()?, allowlist()?])),
        "the digest must cover the refusal, then the allowlist"
    );
    assert_ne!(
        digest,
        sha256_hex(&serialized(&[allowlist()?])),
        "the digest must cover the refusal as well as the allowlist"
    );
    assert_ne!(
        digest,
        sha256_hex(&serialized(&[allowlist()?, clone3_refusal()?])),
        "the digest must follow the installation order"
    );
    Ok(())
}
