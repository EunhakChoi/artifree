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
fn names_and_identities_are_unique_and_the_stub_names_its_own_identity() {
    for (index, profile) in PROFILES.iter().enumerate() {
        for other in PROFILES.iter().skip(index + 1) {
            assert_ne!(profile.name, other.name);
            assert_ne!(profile.inspection.identity(), other.inspection.identity());
        }
        assert_eq!(
            profile.stub_arguments,
            &["stub", profile.inspection.identity()],
            "{}",
            profile.name
        );
    }
}
