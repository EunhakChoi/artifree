//! The execution profiles this provider knows (decision 0058 rules 1 and 3, slice R03ba).
//!
//! A profile fixes everything an attempt runs: its name in the store, its inspection identity and
//! manifest, the payload inside the tool root, the payload's arguments and environment, and the
//! system call filter; the stub is given `stub` and the identity. Nothing a request carries selects or changes any of them
//! (decision 0042 rule 2): a job names a profile, and the provider looks the name up here.
//!
//! The fixture is the only profile, so this table changes no behavior yet.

use super::policy::FilterSet;
use super::{FIXTURE, InspectionProfile};

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
    /// The system call filter the bootstrap installs.
    pub(super) filter: FilterSet,
}

/// The name of the fixture execution profile (decision 0057 rule 2).
const FIXTURE_PROFILE: &str = "fixture";

/// The fixture's payload executable inside the tool root, as the bootstrap sees it.
const PAYLOAD: &str = "/tools/bin/artifree-execution-payload";

/// The repository's fixture: the test-support payload with no arguments and an empty
/// environment, under the fixture's filter.
pub(super) const FIXTURE_EXECUTION: ExecutionProfile = ExecutionProfile {
    name: FIXTURE_PROFILE,
    inspection: FIXTURE,
    payload: PAYLOAD,
    arguments: &[],
    environment: &[],
    filter: FilterSet::Fixture,
};

/// Every profile, in no particular order. Names and identities are unique.
const PROFILES: &[ExecutionProfile] = &[FIXTURE_EXECUTION];

/// The bundler measurement (decision 0058, note "R03bcb split"): the root's bundler with the
/// argv and environment of rule 1, under the measuring filter. It is no profile: it has no store
/// name, so no job can name it, and only the enforcement job's attempt tests launch it. Only the
/// test build and the `enforcement-fixture` build know it.
#[cfg(any(test, feature = "enforcement-fixture"))]
pub(super) const MEASUREMENT: ExecutionProfile = ExecutionProfile {
    name: "",
    inspection: InspectionProfile {
        identity: "artifree-bundler-measurement-v1",
        manifest: b"payload=esbuild\nfilter=measure\nenforcement=incomplete\n",
    },
    payload: "/tools/bin/esbuild",
    arguments: BUNDLER_ARGUMENTS,
    environment: BUNDLER_ENVIRONMENT,
    filter: FilterSet::Measure,
};

/// The bundler's arguments after its path (decision 0058 rule 1): bundle the entry module into
/// `/tmp/out` as `app.js` and, when the source imports CSS, `app.css`; ES modules for the
/// browser, the automatic JSX runtime, minified, without source maps, colour or a metafile, and
/// at most 20 messages.
#[cfg(any(test, feature = "enforcement-fixture"))]
const BUNDLER_ARGUMENTS: &[&str] = &[
    "/input/src/main.tsx",
    "--bundle",
    "--outdir=/tmp/out",
    "--entry-names=app",
    "--format=esm",
    "--platform=browser",
    "--jsx=automatic",
    "--minify",
    "--sourcemap=false",
    "--color=false",
    "--log-limit=20",
];

/// The bundler's whole environment (decision 0058 rule 1): the module directory of the root's
/// preinstalled packages, and one processor for the Go runtime, so that it stays within one CPU
/// and 64 tasks.
#[cfg(any(test, feature = "enforcement-fixture"))]
const BUNDLER_ENVIRONMENT: &[(&str, &str)] =
    &[("NODE_PATH", "/tools/modules"), ("GOMAXPROCS", "1")];

/// The profile whose inspection identity is exactly `identity`, or the bundler measurement in
/// the builds that know it.
#[must_use]
pub(super) fn by_identity(identity: &str) -> Option<&'static ExecutionProfile> {
    let found = PROFILES
        .iter()
        .find(|profile| profile.inspection.identity() == identity);
    #[cfg(any(test, feature = "enforcement-fixture"))]
    let found =
        found.or_else(|| (MEASUREMENT.inspection.identity() == identity).then_some(&MEASUREMENT));
    found
}

/// The filter of every profile and of the bundler measurement, so that a capacity taken before
/// any claim can show that each one builds (decision 0042 rule 5).
#[cfg(any(test, feature = "enforcement-fixture"))]
pub(super) fn filter_sets() -> impl Iterator<Item = FilterSet> {
    PROFILES
        .iter()
        .map(|profile| profile.filter)
        .chain([MEASUREMENT.filter])
}

/// The store name of every profile, for the list a worker passes with each claim.
#[cfg(feature = "enforcement-fixture")]
pub(super) fn names() -> impl Iterator<Item = &'static str> {
    PROFILES.iter().map(|profile| profile.name)
}

/// The profile whose store name is exactly `name`.
#[cfg(any(test, feature = "enforcement-fixture"))]
#[must_use]
pub(super) fn by_name(name: &str) -> Option<&'static ExecutionProfile> {
    PROFILES.iter().find(|profile| profile.name == name)
}

#[cfg(test)]
mod tests;
