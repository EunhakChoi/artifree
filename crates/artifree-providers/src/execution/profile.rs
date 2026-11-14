//! The execution profiles this provider knows (decision 0058 rules 1 and 3, slice R03ba).
//!
//! A profile fixes everything an attempt runs: its name in the store, its inspection identity and
//! manifest, the payload inside the tool root, the payload's arguments and environment, and the
//! system call filter; the stub is given `stub` and the identity. Nothing a request carries selects or changes any of them
//! (decision 0042 rule 2): a job names a profile, and the provider looks the name up here.
//!
//! The fixture is the only profile of a release build's table, which no release build uses yet.
//! The test build and the `enforcement-fixture` build also know `react-v1` (decision 0058 rule 22,
//! slice R03bcbb), and two entries with no store name that only the enforcement job's attempt
//! tests launch: the bundler measurement and the `react-v1` filter check.

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
    /// What the profile requires of its input beyond the package rules.
    pub(super) input: InputRule,
}

/// What a profile requires of its input before it is staged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum InputRule {
    /// Any verified package.
    Any,
    /// No file that would change how the bundler resolves an import (decision 0058 rule 2): the
    /// pinned packages of the tool root are the only ones a build can import. Only the profiles of
    /// the test build and the `enforcement-fixture` build use it.
    #[cfg(any(test, feature = "enforcement-fixture"))]
    FixedResolution,
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
    input: InputRule::Any,
};

/// The name of the React profile (decision 0058 rule 1).
#[cfg(any(test, feature = "enforcement-fixture"))]
const REACT_V1_PROFILE: &str = "react-v1";

/// The bundler's executable inside the tool root, as the bootstrap sees it.
#[cfg(any(test, feature = "enforcement-fixture"))]
const BUNDLER: &str = "/tools/bin/esbuild";

/// The React profile (decision 0058 rules 1 to 3): the root's bundler with the argv and
/// environment of rule 1, under its own filter. Only the test build and the
/// `enforcement-fixture` build know it until R03f (rule 22).
#[cfg(any(test, feature = "enforcement-fixture"))]
pub(super) const REACT_V1: ExecutionProfile = ExecutionProfile {
    name: REACT_V1_PROFILE,
    inspection: InspectionProfile {
        identity: "artifree-react-v1",
        manifest: b"profile=react-v1\npayload=esbuild\nfilter=react-v1\nenforcement=incomplete\n",
    },
    payload: BUNDLER,
    arguments: BUNDLER_ARGUMENTS,
    environment: BUNDLER_ENVIRONMENT,
    filter: FilterSet::ReactV1,
    input: InputRule::FixedResolution,
};

/// Every profile, in no particular order. Names and identities are unique.
#[cfg(not(any(test, feature = "enforcement-fixture")))]
const PROFILES: &[ExecutionProfile] = &[FIXTURE_EXECUTION];

/// Every profile, in no particular order. Names and identities are unique.
#[cfg(any(test, feature = "enforcement-fixture"))]
const PROFILES: &[ExecutionProfile] = &[FIXTURE_EXECUTION, REACT_V1];

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
    payload: BUNDLER,
    arguments: BUNDLER_ARGUMENTS,
    environment: BUNDLER_ENVIRONMENT,
    filter: FilterSet::Measure,
    input: InputRule::FixedResolution,
};

/// The `react-v1` filter check (slice R03bcbb): the fixture's payload under `react-v1`'s filter,
/// so that a payload scenario can make a call outside that filter and show that it is killed. The
/// bundler itself cannot be made to. Like the measurement, it has no store name, and only the
/// test build and the `enforcement-fixture` build know it.
#[cfg(any(test, feature = "enforcement-fixture"))]
pub(super) const REACT_V1_FILTER_CHECK: ExecutionProfile = ExecutionProfile {
    name: "",
    inspection: InspectionProfile {
        identity: "artifree-react-v1-filter-check",
        manifest: b"payload=fixture\nfilter=react-v1\nenforcement=incomplete\n",
    },
    payload: PAYLOAD,
    arguments: &[],
    environment: &[],
    filter: FilterSet::ReactV1,
    input: InputRule::Any,
};

/// The entries that are no profile: no job can name them.
#[cfg(any(test, feature = "enforcement-fixture"))]
const TEST_ENTRIES: [&ExecutionProfile; 2] = [&MEASUREMENT, &REACT_V1_FILTER_CHECK];

/// The bundler's arguments after its path (decision 0058 rule 1): bundle the entry module into
/// `/tmp/out` as `app.js` and, when the source imports CSS, `app.css`; ES modules for the
/// browser, the automatic JSX runtime, minified, without colour and with at most 20 messages.
/// No source map or metafile is asked for, and esbuild writes neither unless asked (its
/// `--sourcemap` takes no `false` value).
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
    "--color=false",
    "--log-limit=20",
];

/// The bundler's whole environment (decision 0058 rule 1): the module directory of the root's
/// preinstalled packages, and one processor for the Go runtime, so that it stays within one CPU
/// and 64 tasks.
#[cfg(any(test, feature = "enforcement-fixture"))]
const BUNDLER_ENVIRONMENT: &[(&str, &str)] =
    &[("NODE_PATH", "/tools/modules"), ("GOMAXPROCS", "1")];

/// The profile whose inspection identity is exactly `identity`, or, in the builds that know them,
/// the entry of `TEST_ENTRIES` with that identity.
#[must_use]
pub(super) fn by_identity(identity: &str) -> Option<&'static ExecutionProfile> {
    let found = PROFILES
        .iter()
        .find(|profile| profile.inspection.identity() == identity);
    #[cfg(any(test, feature = "enforcement-fixture"))]
    let found = found.or_else(|| {
        TEST_ENTRIES
            .into_iter()
            .find(|entry| entry.inspection.identity() == identity)
    });
    found
}

/// The filter of every profile and of every entry of `TEST_ENTRIES`, each once, so that a
/// capacity taken before any claim can show that each one builds (decision 0042 rule 5).
#[cfg(any(test, feature = "enforcement-fixture"))]
pub(super) fn filter_sets() -> impl Iterator<Item = FilterSet> {
    let mut sets: Vec<FilterSet> = Vec::new();
    for set in PROFILES
        .iter()
        .chain(TEST_ENTRIES)
        .map(|profile| profile.filter)
    {
        if !sets.contains(&set) {
            sets.push(set);
        }
    }
    sets.into_iter()
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
