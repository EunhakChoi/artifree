//! The bundler measurement's filter (decision 0058, note "R03bcb split"): which calls it kills by
//! number, and that the calls a measurement has seen change only its logging program.

use super::{
    ALLOWED, FilterSet, PolicyError, allowlist_rules, filters, filters_with, refused_numbers,
};

#[test]
fn the_measurement_kills_the_mapped_refusals_but_not_what_the_fixture_allows()
-> Result<(), PolicyError> {
    let allowed = allowlist_rules()?;
    let refused = refused_numbers(&allowed);
    // unshare, setns, mount, pivot_root, setuid, capset and clone3: each refused outright by the
    // public mapping, so the measurement kills it by number.
    for number in [272, 308, 165, 155, 105, 126, 435] {
        assert!(refused.contains(&number), "{number} is killed");
    }
    // uname and getpid are refused only because the fixture does not list them; clone,
    // prlimit64, socket and setsockopt are allowed in some form. None is killed by number.
    for number in [63, 39, 56, 302, 41, 54] {
        assert!(
            !refused.contains(&number),
            "{number} is not killed by number"
        );
    }
    for (name, number) in ALLOWED {
        if name != "clone3" {
            assert!(
                !refused.contains(&number),
                "{name} is allowed by the fixture"
            );
        }
    }
    assert!(
        refused
            .windows(2)
            .all(|pair| matches!(pair, [a, b] if a < b))
    );
    Ok(())
}

#[test]
fn measured_calls_change_only_the_measurement_logging_program() -> Result<(), PolicyError> {
    let [refusal, logging] = filters(FilterSet::Measure)?;
    let [same_refusal, wider_logging] = filters_with(FilterSet::Measure, &[39, 63])?;
    assert_eq!(
        refusal, same_refusal,
        "the refusals do not depend on what was measured"
    );
    assert_ne!(
        logging, wider_logging,
        "a measured call is allowed without a record"
    );
    assert_eq!(
        filters_with(FilterSet::Fixture, &[39, 63])?,
        filters(FilterSet::Fixture)?,
        "the fixture ignores measured calls"
    );
    Ok(())
}
