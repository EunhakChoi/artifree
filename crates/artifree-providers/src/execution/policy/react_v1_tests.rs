//! The `react-v1` profile's filter (decision 0058 rule 3, slice R03bcbb): the fixture's programs
//! with the bundler's measured and vDSO calls added, `ioctl` only for the terminal queries, and
//! nothing else.

use std::collections::BTreeSet;
use std::fs;
use std::io;

use seccompiler::{SeccompCmpOp, SeccompRule};

use super::{
    ALLOWED, CLONE, FilterSet, IOCTL, PolicyError, QUERY_ONLY, REACT_V1_MEASURED,
    REACT_V1_UNMEASURED, SETSOCKOPT, SOCKET, TERMINAL_QUERIES, allowlist_rules, clone3_refusal,
    compile, dword, filters, react_v1_rules, refused_numbers,
};

/// Calls that no profile may allow, as the fixture's table test lists them.
const NEVER: [i64; 8] = [160, 101, 272, 308, 165, 321, 162, 155];

#[test]
fn the_added_calls_are_new_and_never_dangerous() {
    let added: Vec<i64> = REACT_V1_MEASURED
        .iter()
        .chain(&REACT_V1_UNMEASURED)
        .map(|&(_, number)| number)
        .collect();
    let unique: BTreeSet<i64> = added.iter().copied().collect();
    assert_eq!(unique.len(), added.len(), "no call is added twice");
    let fixture: BTreeSet<i64> = ALLOWED
        .iter()
        .chain(&[QUERY_ONLY, CLONE, SOCKET, SETSOCKOPT])
        .map(|&(_, number)| number)
        .collect();
    for number in &added {
        assert!(!fixture.contains(number), "{number} is not the fixture's");
        assert!(!NEVER.contains(number), "{number} is never allowed");
    }
}

#[test]
fn no_added_call_is_one_the_public_mapping_refuses() -> Result<(), PolicyError> {
    let refused = refused_numbers(&allowlist_rules()?);
    for (name, number) in REACT_V1_MEASURED.iter().chain(&REACT_V1_UNMEASURED) {
        assert!(
            !refused.contains(number),
            "{name} is refused by the mapping"
        );
    }
    Ok(())
}

#[test]
fn the_allowlist_is_the_fixtures_with_exactly_the_added_calls() -> Result<(), PolicyError> {
    let rules = react_v1_rules()?;
    let fixture = allowlist_rules()?;
    let added: BTreeSet<i64> = rules
        .keys()
        .filter(|number| !fixture.contains_key(number))
        .copied()
        .collect();
    let expected: BTreeSet<i64> = REACT_V1_MEASURED
        .iter()
        .chain(&REACT_V1_UNMEASURED)
        .map(|&(_, number)| number)
        .collect();
    assert_eq!(added, expected);
    for (number, fixture_rules) in &fixture {
        assert_eq!(
            rules.get(number),
            Some(fixture_rules),
            "the fixture's rules for {number} are kept as they are"
        );
    }
    let (_, ioctl_number) = IOCTL;
    assert!(REACT_V1_MEASURED.contains(&IOCTL));
    for number in expected.iter().filter(|number| **number != ioctl_number) {
        assert_eq!(rules.get(number), Some(&Vec::new()), "{number} is allowed");
    }
    let mut queries = Vec::new();
    for request in TERMINAL_QUERIES {
        let condition = dword(1, SeccompCmpOp::Eq, request)?;
        queries.push(SeccompRule::new(vec![condition]).map_err(|_| PolicyError)?);
    }
    assert_eq!(
        rules.get(&ioctl_number),
        Some(&queries),
        "ioctl is allowed only for the terminal queries"
    );
    Ok(())
}

#[test]
fn the_programs_are_the_refusal_and_the_compiled_allowlist() -> Result<(), PolicyError> {
    let [refusal, allowlist] = filters(FilterSet::ReactV1)?;
    assert_eq!(refusal, clone3_refusal()?);
    assert_eq!(allowlist, compile(react_v1_rules()?)?);
    let [_, fixture_allowlist] = filters(FilterSet::Fixture)?;
    assert_ne!(
        allowlist, fixture_allowlist,
        "react-v1 has its own allowlist"
    );
    let mut any_ioctl = react_v1_rules()?;
    any_ioctl.insert(IOCTL.1, Vec::new());
    assert_ne!(
        allowlist,
        compile(any_ioctl)?,
        "ioctl is restricted to its requests"
    );
    // One added call less is another program, so each one is in the compiled allowlist.
    for (name, number) in REACT_V1_MEASURED.iter().chain(&REACT_V1_UNMEASURED) {
        let mut fewer = react_v1_rules()?;
        fewer.remove(number);
        assert_ne!(allowlist, compile(fewer)?, "{name} is in the program");
    }
    Ok(())
}

/// Compares the terminal queries with the kernel's own request numbers. Only the Linux
/// enforcement job runs it, for the reason of the syscall table's test: it reads the runner's
/// kernel headers.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_terminal_queries_match_the_kernel_headers() -> io::Result<()> {
    const HEADER: &str = "/usr/include/asm-generic/ioctls.h";
    let text = fs::read_to_string(HEADER)?;
    let request = |name: &str| {
        text.lines().find_map(
            |line| match line.split_whitespace().collect::<Vec<_>>().as_slice() {
                ["#define", key, value] if *key == name => value
                    .strip_prefix("0x")
                    .and_then(|hex| u64::from_str_radix(hex, 16).ok()),
                _ => None,
            },
        )
    };
    let headers = [request("TCGETS"), request("TIOCGWINSZ")];
    assert_eq!(
        headers,
        TERMINAL_QUERIES.map(Some),
        "the terminal queries disagree with {HEADER}"
    );
    Ok(())
}
