//! The `react-v1` profile's filter (decision 0058 rule 3, slice R03bcbb): the fixture's programs
//! with the bundler's measured and vDSO calls added, and nothing else.

use std::collections::BTreeSet;

use super::{
    ALLOWED, CLONE, FilterSet, PolicyError, QUERY_ONLY, REACT_V1_MEASURED, REACT_V1_UNMEASURED,
    SETSOCKOPT, SOCKET, allowlist_rules, clone3_refusal, compile, filters, react_v1_rules,
    refused_numbers,
};

/// The calls of the R03bcba artifact `bundler-calls.txt`, as the job printed them.
const ARTIFACT: [(&str, i64); 9] = [
    ("ioctl", 16),
    ("nanosleep", 35),
    ("getpid", 39),
    ("getcwd", 79),
    ("epoll_ctl", 233),
    ("tgkill", 234),
    ("epoll_pwait", 281),
    ("eventfd2", 290),
    ("epoll_create1", 291),
];

/// Calls that no profile may allow, as the fixture's table test lists them.
const NEVER: [i64; 8] = [160, 101, 272, 308, 165, 321, 162, 155];

#[test]
fn the_added_calls_are_the_measured_ones_and_the_vdso_ones() {
    assert_eq!(REACT_V1_MEASURED, ARTIFACT, "the measured calls are pinned");
    assert_eq!(
        REACT_V1_UNMEASURED,
        [("gettimeofday", 96), ("time", 201), ("getcpu", 309)]
    );
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
    for number in &expected {
        assert_eq!(rules.get(number), Some(&Vec::new()), "{number} is allowed");
    }
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
    // One added call less is another program, so each one is in the compiled allowlist.
    for (name, number) in REACT_V1_MEASURED.iter().chain(&REACT_V1_UNMEASURED) {
        let mut fewer = react_v1_rules()?;
        fewer.remove(number);
        assert_ne!(allowlist, compile(fewer)?, "{name} is in the program");
    }
    Ok(())
}
