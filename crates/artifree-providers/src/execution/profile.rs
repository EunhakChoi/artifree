//! The execution profiles this provider knows (decision 0058 rules 1 and 3, slice R03ba).
//!
//! A profile fixes everything an attempt runs: its name in the store, its inspection identity and
//! manifest, the payload inside the tool root, the payload's arguments and environment, the stub's
//! arguments and the system call filter. Nothing a request carries selects or changes any of them
//! (decision 0042 rule 2): a job names a profile, and the provider looks the name up here.
//!
//! The fixture is the only profile, so this table changes no behavior yet.

use super::bootstrap::PAYLOAD;
use super::policy::FilterSet;
use super::{FIXTURE, FIXTURE_PROFILE, InspectionProfile};

/// One reviewed, fixed execution profile.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct ExecutionProfile {
    /// The profile's name in the store (`jobs.profile`, decision 0057 rule 1).
    pub(super) name: &'static str,
    /// The identity the stub and the bootstrap are given, and the manifest bytes bound to the
    /// output.
    pub(super) inspection: InspectionProfile,
    /// The payload's path inside the sandbox, where the tool root is bound as `/tools`.
    pub(super) payload: &'static str,
    /// The payload's arguments after its path.
    pub(super) arguments: &'static [&'static str],
    /// The payload's whole environment; nothing else is passed.
    pub(super) environment: &'static [(&'static str, &'static str)],
    /// The stub's arguments before the input path: the mode word and the identity.
    pub(super) stub_arguments: &'static [&'static str],
    /// The system call filter the bootstrap installs.
    pub(super) filter: FilterSet,
}

/// The repository's fixture: the test-support payload with no arguments and an empty
/// environment, under the fixture's filter.
pub(super) const FIXTURE_EXECUTION: ExecutionProfile = ExecutionProfile {
    name: FIXTURE_PROFILE,
    inspection: FIXTURE,
    payload: PAYLOAD,
    arguments: &[],
    environment: &[],
    stub_arguments: &["stub", FIXTURE.identity],
    filter: FilterSet::Fixture,
};

/// Every profile, in no particular order. Names and identities are unique.
const PROFILES: &[ExecutionProfile] = &[FIXTURE_EXECUTION];

/// The profile whose inspection identity is exactly `identity`.
#[must_use]
pub(super) fn by_identity(identity: &str) -> Option<&'static ExecutionProfile> {
    PROFILES
        .iter()
        .find(|profile| profile.inspection.identity() == identity)
}

/// The filter of every profile, so that a capacity taken before any claim can show that each
/// one builds (decision 0042 rule 5).
#[cfg(any(test, feature = "enforcement-fixture"))]
pub(super) fn filter_sets() -> impl Iterator<Item = FilterSet> {
    PROFILES.iter().map(|profile| profile.filter)
}

/// The profile whose store name is exactly `name`.
#[cfg(any(test, feature = "enforcement-fixture"))]
#[must_use]
pub(super) fn by_name(name: &str) -> Option<&'static ExecutionProfile> {
    PROFILES.iter().find(|profile| profile.name == name)
}

#[cfg(test)]
mod tests;
