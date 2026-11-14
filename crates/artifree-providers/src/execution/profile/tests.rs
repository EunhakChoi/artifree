//! The profile table finds a profile only by its exact name or identity.

use super::super::policy::FilterSet;
use super::{
    FIXTURE_EXECUTION, MEASUREMENT, PROFILES, REACT_V1, REACT_V1_FILTER_CHECK, TEST_ENTRIES,
    by_identity, by_name, filter_sets,
};

#[test]
fn the_fixture_is_found_by_its_exact_name_and_identity() {
    assert_eq!(by_name("fixture"), Some(&FIXTURE_EXECUTION));
    assert_eq!(
        by_identity("artifree-fixture-inspection-v1"),
        Some(&FIXTURE_EXECUTION)
    );
}

#[test]
fn a_near_miss_finds_no_profile() {
    for name in [
        "",
        "Fixture",
        "fixture ",
        "fixture-v2",
        "React-v1",
        "react-v2",
        "react",
    ] {
        assert_eq!(by_name(name), None, "{name:?}");
    }
    for identity in [
        "",
        "artifree-fixture-inspection-v2",
        "artifree-fixture-inspection-v1 ",
        "fixture",
        "react-v1",
        "artifree-react-v2",
    ] {
        assert_eq!(by_identity(identity), None, "{identity:?}");
    }
}

#[test]
fn react_v1_is_found_by_its_exact_name_and_identity() {
    assert_eq!(by_name("react-v1"), Some(&REACT_V1));
    assert_eq!(by_identity("artifree-react-v1"), Some(&REACT_V1));
    assert_eq!(REACT_V1.payload, "/tools/bin/esbuild");
    assert_eq!(REACT_V1.filter, FilterSet::ReactV1);
    assert_eq!(REACT_V1.arguments, MEASUREMENT.arguments);
    assert_eq!(REACT_V1.environment, MEASUREMENT.environment);
}

#[test]
fn the_test_entries_are_found_by_identity_but_never_by_name() {
    assert_eq!(
        by_identity("artifree-bundler-measurement-v1"),
        Some(&MEASUREMENT)
    );
    assert_eq!(
        by_identity("artifree-react-v1-filter-check"),
        Some(&REACT_V1_FILTER_CHECK)
    );
    assert_eq!(REACT_V1_FILTER_CHECK.payload, FIXTURE_EXECUTION.payload);
    assert_eq!(REACT_V1_FILTER_CHECK.filter, FilterSet::ReactV1);
    for entry in TEST_ENTRIES {
        assert_eq!(entry.name, "");
        assert_eq!(by_name(entry.name), None);
    }
}

#[test]
fn every_filter_is_offered_once() {
    let sets: Vec<FilterSet> = filter_sets().collect();
    assert_eq!(
        sets,
        [FilterSet::Fixture, FilterSet::ReactV1, FilterSet::Measure]
    );
}

#[test]
fn names_and_identities_are_unique() {
    let entries: Vec<_> = PROFILES.iter().chain(TEST_ENTRIES).collect();
    for (index, profile) in entries.iter().enumerate() {
        for other in entries.iter().skip(index + 1) {
            if !profile.name.is_empty() {
                assert_ne!(profile.name, other.name);
            }
            assert_ne!(profile.inspection.identity(), other.inspection.identity());
        }
    }
}

#[cfg(feature = "enforcement-fixture")]
#[test]
fn a_feature_build_offers_every_profile_in_the_table_with_its_claims() {
    let known = super::super::known_profiles();
    let offered: Vec<&str> = known
        .iter()
        .map(artifree_domain::job::ProfileName::as_str)
        .collect();
    let table: Vec<&str> = PROFILES.iter().map(|profile| profile.name).collect();
    assert_eq!(offered, table);
}
