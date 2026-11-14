//! The profile table finds a profile only by its exact name or identity.

use super::{FIXTURE_EXECUTION, PROFILES, by_identity, by_name};

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
    for name in ["", "Fixture", "fixture ", "fixture-v2", "react-v1"] {
        assert_eq!(by_name(name), None, "{name:?}");
    }
    for identity in [
        "",
        "artifree-fixture-inspection-v2",
        "artifree-fixture-inspection-v1 ",
        "fixture",
    ] {
        assert_eq!(by_identity(identity), None, "{identity:?}");
    }
}

#[test]
fn names_and_identities_are_unique() {
    for (index, profile) in PROFILES.iter().enumerate() {
        for other in PROFILES.iter().skip(index + 1) {
            assert_ne!(profile.name, other.name);
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
