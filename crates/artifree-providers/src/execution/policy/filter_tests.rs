//! Behavioral tests for the two seccomp programs of the fixture profile: the namespace rule of
//! `clone`, the refusal of `clone3`, the local rules of `socket` and `setsockopt`, the pinned
//! digest and the installation order (decisions 0042 rule 4 and 0056 rule 3, and the amendment of
//! 2026-11-13, "files, descriptors and network"). The programs are compared with copies built here
//! from their specification, so a change to either side makes a test fail. No filter is installed
//! in this process.

use std::collections::BTreeMap;

use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch,
};

use super::super::report::sha256_hex;
use super::{
    ALLOWED, FilterSet, NAMESPACE_FLAGS, PolicyError, allowlist, clone3_refusal, fixture_filters,
    policy_sha256,
};

/// The `clone` system call number on `x86_64`.
const CLONE: i64 = 56;
/// The `clone3` system call number on `x86_64`.
const CLONE3: i64 = 435;
/// The `prlimit64` system call number on `x86_64`, the one call the allowlist admits as a query.
const PRLIMIT64: i64 = 302;
/// The `socket` system call number on `x86_64`.
const SOCKET: i64 = 41;
/// The `setsockopt` system call number on `x86_64`.
const SETSOCKOPT: i64 = 54;
/// The eight namespace flags as one literal, so that the test does not read the constant it checks.
const NAMESPACE_MASK: u64 = 0x7E02_0080;
/// `CLONE_NEWUSER`, one of the eight namespace flags.
const CLONE_NEWUSER: u64 = 0x1000_0000;
/// `ENOSYS`, the error that makes libc fall back from `clone3` to `clone`.
const ENOSYS: u32 = 38;
/// The argument index of `socket`'s domain.
const DOMAIN_ARGUMENT: u8 = 0;
/// The argument index of `socket`'s type, whose base type `SOCK_TYPE_MASK` selects.
const TYPE_ARGUMENT: u8 = 1;
/// The argument index of `socket`'s protocol.
const PROTOCOL_ARGUMENT: u8 = 2;
/// The argument index of `setsockopt`'s level.
const LEVEL_ARGUMENT: u8 = 1;
/// The argument index of `setsockopt`'s option name.
const OPTION_ARGUMENT: u8 = 2;
/// The base type of `socket`'s type argument: the bits under this mask, so that `SOCK_CLOEXEC` and
/// `SOCK_NONBLOCK`, which lie outside it, do not change the type.
const SOCK_TYPE_MASK: u64 = 0xF;
/// `SOL_SOCKET`, the one `setsockopt` level the allowlist admits.
const SOL_SOCKET: u64 = 1;
/// `SO_REUSEADDR`, the one `setsockopt` option the allowlist admits.
const SO_REUSEADDR: u64 = 2;

/// One `(domain, type, protocol)` form of `socket`.
type SocketForm = (u64, u64, u64);

/// One condition of a rule: the argument index, the comparison and the value it is compared with.
type Check = (u8, SeccompCmpOp, u64);

/// One case that the allowlist must refuse: a description, the `socket` rules and the `setsockopt`
/// rules, where `None` leaves the call out.
type Case = (String, Option<Vec<SeccompRule>>, Option<Vec<SeccompRule>>);

/// The ten `(domain, type, protocol)` forms of the specification, in the order of the reviewed
/// program: `AF_UNIX` (1) stream (1) and datagram (2) with protocol 0; `AF_INET` (2) and `AF_INET6`
/// (10) stream and datagram with protocol 0; and stream with `IPPROTO_TCP` (6) and datagram with
/// `IPPROTO_UDP` (17) for both families.
const SOCKET_FORMS: [SocketForm; 10] = [
    (1, 1, 0),
    (1, 2, 0),
    (2, 1, 0),
    (2, 1, 6),
    (2, 2, 0),
    (2, 2, 17),
    (10, 1, 0),
    (10, 1, 6),
    (10, 2, 0),
    (10, 2, 17),
];

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

/// One `Dword` condition: the low four bytes of argument `argument` compared with `value`.
fn dword(
    argument: u8,
    operator: SeccompCmpOp,
    value: u64,
) -> Result<SeccompCondition, PolicyError> {
    SeccompCondition::new(argument, SeccompCmpArgLen::Dword, operator, value)
        .map_err(|_| PolicyError)
}

/// A rule that matches only when every check holds, each check being a `Dword` condition in order.
fn rule(checks: Vec<Check>) -> Result<SeccompRule, PolicyError> {
    let conditions = checks
        .into_iter()
        .map(|(argument, operator, value)| dword(argument, operator, value))
        .collect::<Result<Vec<SeccompCondition>, PolicyError>>()?;
    SeccompRule::new(conditions).map_err(|_| PolicyError)
}

/// The `socket` rules for `forms`, in order: each rule checks the domain with `Eq`, the type with
/// `type_operator` and the protocol with `Eq`. The argument `omitted` names gets no check.
fn socket_rules(
    forms: &[SocketForm],
    type_operator: &SeccompCmpOp,
    omitted: Option<u8>,
) -> Result<Vec<SeccompRule>, PolicyError> {
    forms
        .iter()
        .map(|&(domain, socket_type, protocol)| {
            let mut checks: Vec<Check> = vec![
                (DOMAIN_ARGUMENT, SeccompCmpOp::Eq, domain),
                (TYPE_ARGUMENT, type_operator.clone(), socket_type),
                (PROTOCOL_ARGUMENT, SeccompCmpOp::Eq, protocol),
            ];
            checks.retain(|(argument, _, _)| Some(*argument) != omitted);
            rule(checks)
        })
        .collect()
}

/// The `setsockopt` rule: `SOL_SOCKET` at the level and `SO_REUSEADDR` as the option, each checked
/// with `Eq`. The argument `omitted` names gets no check.
fn setsockopt_rule(omitted: Option<u8>) -> Result<SeccompRule, PolicyError> {
    let mut checks: Vec<Check> = vec![
        (LEVEL_ARGUMENT, SeccompCmpOp::Eq, SOL_SOCKET),
        (OPTION_ARGUMENT, SeccompCmpOp::Eq, SO_REUSEADDR),
    ];
    checks.retain(|(argument, _, _)| Some(*argument) != omitted);
    rule(checks)
}

/// The specification's `socket` rules: one per form of `SOCKET_FORMS`, with the type under
/// `MaskedEq` of `SOCK_TYPE_MASK`.
fn spec_socket_rules() -> Result<Vec<SeccompRule>, PolicyError> {
    socket_rules(&SOCKET_FORMS, &SeccompCmpOp::MaskedEq(SOCK_TYPE_MASK), None)
}

/// The specification's `setsockopt` rule, which admits only `SO_REUSEADDR` at `SOL_SOCKET`.
fn spec_setsockopt_rules() -> Result<Vec<SeccompRule>, PolicyError> {
    Ok(vec![setsockopt_rule(None)?])
}

/// Builds the allowlist from its specification with the given rules for `clone`, `socket` and
/// `setsockopt`. A call whose rules are `None` is left out, so the program kills it; an empty rule
/// vector allows it whatever its arguments.
fn allowlist_with(
    clone_rules: Option<Vec<SeccompRule>>,
    socket: Option<Vec<SeccompRule>>,
    setsockopt: Option<Vec<SeccompRule>>,
) -> Result<BpfProgram, PolicyError> {
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    for (_, number) in ALLOWED {
        rules.insert(number, Vec::new());
    }
    rules.insert(PRLIMIT64, vec![query_rule()?]);
    if let Some(clone_rules) = clone_rules {
        rules.insert(CLONE, clone_rules);
    }
    if let Some(socket) = socket {
        rules.insert(SOCKET, socket);
    }
    if let Some(setsockopt) = setsockopt {
        rules.insert(SETSOCKOPT, setsockopt);
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

/// Builds the allowlist from its specification: every table entry without a condition,
/// `prlimit64` as a query, `clone` with the given rules, and the specification's `socket` and
/// `setsockopt` rules. `None` leaves `clone` out, and an empty rule vector allows it whatever its
/// arguments. A call without a rule kills the process.
fn allowlist_with_clone(clone_rules: Option<Vec<SeccompRule>>) -> Result<BpfProgram, PolicyError> {
    allowlist_with(
        clone_rules,
        Some(spec_socket_rules()?),
        Some(spec_setsockopt_rules()?),
    )
}

/// Builds the allowlist with the specification's `clone` rule and the given `socket` and
/// `setsockopt` rules, so that a variant differs from the specification only in those two.
fn allowlist_with_socket(
    socket: Option<Vec<SeccompRule>>,
    setsockopt: Option<Vec<SeccompRule>>,
) -> Result<BpfProgram, PolicyError> {
    allowlist_with(Some(vec![flags_rule(NAMESPACE_MASK)?]), socket, setsockopt)
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

/// The cases that the allowlist must refuse. Each differs from the specification in one way: a
/// `socket` rule without one of its checks, with the type under `Eq` instead of `MaskedEq`, with
/// one of the ten forms left out or one more form added, allowed whatever its arguments or absent;
/// and a `setsockopt` rule allowed whatever its arguments, with one of its checks only, or absent.
fn refused_cases() -> Result<Vec<Case>, PolicyError> {
    let masked = SeccompCmpOp::MaskedEq(SOCK_TYPE_MASK);
    let mut cases: Vec<Case> = vec![
        (
            "the socket rules do not check the domain".to_owned(),
            Some(socket_rules(&SOCKET_FORMS, &masked, Some(DOMAIN_ARGUMENT))?),
            Some(spec_setsockopt_rules()?),
        ),
        (
            "the socket rules do not check the type".to_owned(),
            Some(socket_rules(&SOCKET_FORMS, &masked, Some(TYPE_ARGUMENT))?),
            Some(spec_setsockopt_rules()?),
        ),
        (
            "the socket rules do not check the protocol".to_owned(),
            Some(socket_rules(
                &SOCKET_FORMS,
                &masked,
                Some(PROTOCOL_ARGUMENT),
            )?),
            Some(spec_setsockopt_rules()?),
        ),
        (
            "the type is compared with Eq, so the flags of socket are refused".to_owned(),
            Some(socket_rules(&SOCKET_FORMS, &SeccompCmpOp::Eq, None)?),
            Some(spec_setsockopt_rules()?),
        ),
        (
            "socket is allowed whatever its arguments".to_owned(),
            Some(Vec::new()),
            Some(spec_setsockopt_rules()?),
        ),
        (
            "socket is left out".to_owned(),
            None,
            Some(spec_setsockopt_rules()?),
        ),
        (
            "setsockopt is allowed whatever its arguments".to_owned(),
            Some(spec_socket_rules()?),
            Some(Vec::new()),
        ),
        (
            "setsockopt checks only the level".to_owned(),
            Some(spec_socket_rules()?),
            Some(vec![setsockopt_rule(Some(OPTION_ARGUMENT))?]),
        ),
        (
            "setsockopt checks only the option".to_owned(),
            Some(spec_socket_rules()?),
            Some(vec![setsockopt_rule(Some(LEVEL_ARGUMENT))?]),
        ),
        (
            "setsockopt is left out".to_owned(),
            Some(spec_socket_rules()?),
            None,
        ),
    ];
    for left_out in SOCKET_FORMS {
        let forms: Vec<SocketForm> = SOCKET_FORMS
            .into_iter()
            .filter(|form| *form != left_out)
            .collect();
        cases.push((
            format!("the form {left_out:?} is left out"),
            Some(socket_rules(&forms, &masked, None)?),
            Some(spec_setsockopt_rules()?),
        ));
    }
    let mut with_extra_form = SOCKET_FORMS.to_vec();
    with_extra_form.push((1, 5, 0));
    cases.push((
        "the AF_UNIX seqpacket form (1, 5, 0) is added".to_owned(),
        Some(socket_rules(&with_extra_form, &masked, None)?),
        Some(spec_setsockopt_rules()?),
    ));
    Ok(cases)
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
fn allowlist_allows_only_the_local_socket_forms() -> Result<(), PolicyError> {
    let actual = allowlist()?;
    let expected =
        allowlist_with_socket(Some(spec_socket_rules()?), Some(spec_setsockopt_rules()?))?;
    assert_eq!(
        actual, expected,
        "socket must be allowed only for the ten local forms and setsockopt only for SO_REUSEADDR"
    );
    for (case, socket, setsockopt) in refused_cases()? {
        let variant = allowlist_with_socket(socket, setsockopt)?;
        assert_ne!(
            actual, variant,
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
    let digest = policy_sha256(FilterSet::Fixture)?;
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

/// The SHA-256 of the reviewed fixture programs is pinned, so that a change to the filter cannot
/// pass unnoticed. A reviewed change must update this value and the mapping's mutation run too:
/// because the filter changes, the whole mutation matrix of R02ad is run again (decision 0056,
/// amendment of 2026-11-13).
#[test]
fn policy_digest_is_pinned() -> Result<(), PolicyError> {
    assert_eq!(
        policy_sha256(FilterSet::Fixture)?,
        "1a54f4826f1cbfe1889b758deec25f374d3c4c848592bc5f872b07645c829a35",
        "the policy digest must be the pinned value of the reviewed programs"
    );
    Ok(())
}
