//! Host inspection for proposed untrusted execution (decisions 0042 and 0056).
//!
//! Implemented: the read-only core-routing inspection and, on Linux only, a host
//! admission that checks facilities in a fixed order, verifies the tool root against its
//! manifest, runs one trusted namespace check through the tool root's launcher and one
//! trusted probe child in a probe cgroup. `inspect_host` gives every caller a refusal, at best
//! `IncompleteEnforcement` (0056 rule 16), and no host is called supported.
//! `run_stub`, `run_stub_for`, `run_bootstrap` and `run_guardian` serve the test-support program
//! and the server's `execution` entry point (0057 rule 4). `write_probe_programs` serves the
//! test-support program only (0056 rule 4); the probe programs are of use only under the launch.
//! The launch (0056 rule 14, R02ab and R02ac) is compiled into this crate's tests and, on Linux,
//! into a build with the `enforcement-fixture` feature (0057 rule 2). That build serves the
//! fixture profile only, which `known_profiles` lists; a release build knows no profile and has
//! no launch. The test payload runs the fixed resource and cleanup scenarios of R02ac and the
//! syscall-mapping and descriptor scenarios of R02ad. Outside the tests, only the fixture profile
//! of the feature build can start an attempt.
//! Its input staging and output collection (R02af) and the routing check during an attempt
//! (R02ag, `routing_unchanged`) are compiled into the same builds.
//!
//! Admission order; the first missing facility is reported (0056 rule 10):
//! 1. Linux (`UnsupportedOperatingSystem`).
//! 2. Kernel facilities: a readable kernel release (`UnsupportedKernel`), the
//!    process's cgroup v2 group from the `0::` line of `/proc/self/cgroup`
//!    (`CgroupV2Unavailable`), and `cgroup.kill` in that group (`MissingKillSupport`).
//! 3. Core routing (the routing refusals above).
//! 4. Tool root: the launcher, then the fixture (`MissingLauncher`, `MissingFixture`), then the
//!    owner and modes of the root's ancestors (`ToolRootUnprotected`), then every entry against
//!    the root's `MANIFEST` (`MissingToolManifest`, `ToolRootMismatch`, `ToolRootUnprotected`).
//! 5. Delegation: the group, its `cgroup.procs` and `cgroup.subtree_control` are
//!    owned by the provider's account (`UndelegatedCgroup`).
//! 6. Controllers memory, pids, cpu, in that order (`MissingController`).
//! 7. The provider is the group's only process (`OccupiedCgroup`). The provider
//!    then moves into the child group `supervisor` and writes `+memory +pids +cpu`
//!    to the group's `cgroup.subtree_control`; `EBUSY` there is facility 7, any
//!    other write failure facility 5, a controller missing on read-back facility 6.
//! 8. Seven namespaces made by the launcher (`NamespacesUnavailable`).
//! 9. Probe limits written and read back exactly (`LimitNotEnforced`), then the
//!    probe child started (`ProbeNotStarted`) and placed in its group
//!    (`ProbeNotPlaced`).
//! 10. Probe group killed, seen empty within five seconds and its child reaped
//!     (`CleanupNotObserved`); the group is then removed. A failed attempt leaves
//!     its probe group in place (0042, rule 6).

use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::time::{Duration, Instant};

use artifree_domain::job::ProfileName;

#[cfg(target_os = "linux")]
mod admission;
// The launch serves the fixture profile of the test build and of the `enforcement-fixture`
// feature (decision 0057 rule 2); a release build has none (0056 rule 16, 0042 rule 8).
#[cfg(all(target_os = "linux", any(test, feature = "enforcement-fixture")))]
mod attempt;
#[cfg(target_os = "linux")]
mod bootstrap;
#[cfg(target_os = "linux")]
mod cgroup;
// Output collection serves only the launch, which exists in the test build and in the
// `enforcement-fixture` feature (decision 0057 rule 2; 0056 rule 16; amendment of 2026-11-13,
// input and output).
#[cfg(all(target_os = "linux", any(test, feature = "enforcement-fixture")))]
mod collection;
// The guardian of one attempt (decision 0056, amendment of 2026-11-13, the guardian). Its entry
// points are the test-support program and the server's `execution guardian` (0056 rule 4;
// 0057 rule 4).
#[cfg(target_os = "linux")]
mod guardian;
#[cfg(target_os = "linux")]
mod launcher;
// The admission lock serves only the launch, which exists in the test build and in the
// `enforcement-fixture` feature (decision 0057 rule 2; 0056 rule 16).
#[cfg(all(target_os = "linux", any(test, feature = "enforcement-fixture")))]
mod lock;
#[cfg(target_os = "linux")]
mod manifest;
#[cfg(target_os = "linux")]
mod policy;
#[cfg(target_os = "linux")]
mod probe_group;
// The execution profiles (decision 0058 rules 1 and 3, R03ba).
#[cfg(target_os = "linux")]
mod profile;
// The generated probe programs of the enforcement job (decision 0056, amendment of 2026-11-13).
#[cfg(target_os = "linux")]
mod probes;
#[cfg(target_os = "linux")]
mod readiness;
// The attempt registry, its reconciliation and the persisted quarantine serve only the launch,
// which exists in the test build and in the `enforcement-fixture` feature (decision 0057 rule 2;
// 0056 rule 16; amendment of 2026-11-14, the attempt registry).
#[cfg(all(target_os = "linux", any(test, feature = "enforcement-fixture")))]
mod registry;
mod report;
// The routing check during an attempt serves the launch of the test build and of the
// `enforcement-fixture` feature (0056 rule 16, amendment of 2026-11-13, crashes). Its tests sit
// beside it and run in the test build only.
#[cfg(test)]
mod routing_tests;
// The input staging serves only the launch, which exists in the test build and in the
// `enforcement-fixture` feature (decision 0057 rule 2; 0056 rule 16; amendment of 2026-11-13,
// input and output).
#[cfg(all(target_os = "linux", any(test, feature = "enforcement-fixture")))]
mod staging;
// The attempt group's counters serve only the launch, which exists in the test build and in the
// `enforcement-fixture` feature (decision 0057 rule 2; 0056 rule 16).
#[cfg(all(target_os = "linux", any(test, feature = "enforcement-fixture")))]
mod usage;
// The worker's adapter of the execution port for the fixture profile (decision 0057 rules 2 and 15
// to 19). Only a Linux build with the `enforcement-fixture` feature has it; a release build has none.
#[cfg(all(target_os = "linux", feature = "enforcement-fixture"))]
mod worker_port;

#[cfg(target_os = "linux")]
pub use bootstrap::{BootstrapFailure, run_bootstrap, run_stub, run_stub_for};
#[cfg(target_os = "linux")]
pub use guardian::{GuardianFailure, run_guardian};
#[cfg(target_os = "linux")]
pub use launcher::EntryProgram;
#[cfg(target_os = "linux")]
pub use probes::write_probe_programs;
pub use report::HostReport;
#[cfg(all(target_os = "linux", feature = "enforcement-fixture"))]
pub use worker_port::{ExecutionStartRefusal, FixtureExecution};

const CORE_PATTERN: &str = "/proc/sys/kernel/core_pattern";
const MAX_PATTERN_BYTES: u64 = 4096;
const OBSERVATION_LIFETIME: Duration = Duration::from_secs(1);

/// Why this partial inspector refuses untrusted execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AdmissionRefusal {
    /// This operating system has no proposed execution backend.
    UnsupportedOperatingSystem,
    /// The supervisor could not read a bounded real-host routing value.
    UnreadableCoreRouting,
    /// A host-global pipe collector would bypass the proposed zero-core policy.
    PipeCoreRouting,
    /// A socket or request collector is outside the reviewed policy.
    SocketCoreRouting,
    /// The routing value is not in the deliberately narrow file-mode grammar.
    UnknownCoreRouting,
    /// Routing no longer matches the original exact bytes.
    CoreRoutingChanged,
    /// A monotonic observation expired or its supplied clock moved backwards.
    StaleObservation,
    /// A caller supplied a profile or manifest other than the fixed policy.
    UnknownProfile,
    /// The kernel release could not be read through the supervisor's proc view.
    UnsupportedKernel,
    /// No cgroup v2 group for this process was found under `/sys/fs/cgroup`.
    CgroupV2Unavailable,
    /// The provider's cgroup has no `cgroup.kill` file.
    MissingKillSupport,
    /// The tool root's launcher is missing or not a regular executable file.
    MissingLauncher,
    /// The tool root's fixture is missing or not a regular executable file.
    MissingFixture,
    /// The tool root has no manifest that could be read and parsed (decision 0056 rule 6).
    MissingToolManifest,
    /// A tool root file differs from its manifest, or the root holds a file the manifest does not list or lacks one it lists (0056 rule 6).
    ToolRootMismatch,
    /// The tool root, one of its entries or one of its ancestors could be changed by someone other
    /// than root: it is not owned by root, it has a group or other write bit (an ancestor may
    /// when its sticky bit is set), or it has a set-user-ID or set-group-ID bit (decision 0058
    /// rule 19, R03bca).
    ToolRootUnprotected,
    /// The provider's cgroup is not delegated to the provider's own account.
    UndelegatedCgroup,
    /// A required controller is not available to the provider's cgroup.
    MissingController(Controller),
    /// Another process shares the provider's cgroup.
    OccupiedCgroup,
    /// The launcher did not make every one of the seven namespaces.
    NamespacesUnavailable,
    /// A probe limit could not be written or did not read back exactly.
    LimitNotEnforced,
    /// The trusted probe child could not be started.
    ProbeNotStarted,
    /// The trusted probe child could not be placed in its group.
    ProbeNotPlaced,
    /// The probe group was not seen empty, reaped and removed before the deadline.
    CleanupNotObserved,
    /// Tool packaging, enforcement and dedicated-host acceptance are incomplete.
    IncompleteEnforcement,
}

impl fmt::Display for AdmissionRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::UnsupportedOperatingSystem => {
                "untrusted execution requires a reviewed Linux host"
            }
            Self::UnreadableCoreRouting => "host core routing could not be read",
            Self::PipeCoreRouting => "host pipe core routing is unsupported",
            Self::SocketCoreRouting => "host socket core routing is unsupported",
            Self::UnknownCoreRouting => "host core routing is unreviewed",
            Self::CoreRoutingChanged => "host core routing changed",
            Self::StaleObservation => "host observation is stale",
            Self::UnknownProfile => "execution profile identity is unknown",
            Self::UnsupportedKernel => "host kernel release could not be read",
            Self::CgroupV2Unavailable => "no cgroup v2 group was found for the provider",
            Self::MissingKillSupport => "the provider's cgroup has no kill support",
            Self::MissingLauncher => "the tool root's launcher is missing",
            Self::MissingFixture => "the tool root's fixture is missing",
            Self::MissingToolManifest => "the tool root has no readable manifest",
            Self::ToolRootMismatch => "the tool root does not match its manifest",
            Self::ToolRootUnprotected => {
                "the tool root could be changed by someone other than root"
            }
            Self::UndelegatedCgroup => "the provider's cgroup is not delegated to it",
            Self::MissingController(Controller::Memory) => "the memory controller is not delegated",
            Self::MissingController(Controller::Pids) => "the pids controller is not delegated",
            Self::MissingController(Controller::Cpu) => "the cpu controller is not delegated",
            Self::OccupiedCgroup => "another process occupies the provider's cgroup",
            Self::NamespacesUnavailable => "the launcher could not make every namespace",
            Self::LimitNotEnforced => "a probe limit did not read back exactly",
            Self::ProbeNotStarted => "the probe process could not be started",
            Self::ProbeNotPlaced => "the probe process could not be placed in its cgroup",
            Self::CleanupNotObserved => "the probe group was not seen empty",
            Self::IncompleteEnforcement => "untrusted execution enforcement is not implemented",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for AdmissionRefusal {}

/// A cgroup v2 controller the admission requires, in checking order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Controller {
    /// The `memory` controller.
    Memory,
    /// The `pids` controller.
    Pids,
    /// The `cpu` controller.
    Cpu,
}

impl Controller {
    /// Every required controller in the fixed checking order.
    pub const REQUIRED: [Self; 3] = [Self::Memory, Self::Pids, Self::Cpu];

    /// The kernel's name for this controller.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Pids => "pids",
            Self::Cpu => "cpu",
        }
    }
}

/// Immutable identity of the first proposed fixture policy, not an executable.
///
/// No installed executable/tool manifest has been reviewed or shipped. These
/// exact bytes pin only this inspector's policy, never tool identity or safety.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InspectionProfile {
    identity: &'static str,
    manifest: &'static [u8],
}

const FIXTURE: InspectionProfile = InspectionProfile {
    identity: "artifree-fixture-inspection-v1",
    manifest: b"routing=file-basename-v1\ntools=unpackaged\nenforcement=incomplete\n",
};

impl InspectionProfile {
    /// The only repository-owned inspection policy. It cannot authorize launch.
    #[must_use]
    pub fn fixture() -> Self {
        FIXTURE
    }

    /// Check exact profile and manifest identity without interpreting paths,
    /// commands, environment variables, or caller-selected tool locations.
    ///
    /// # Errors
    /// Returns `UnknownProfile` for every mismatch, including extra bytes.
    pub fn verify_identity(identity: &str, manifest: &[u8]) -> Result<Self, AdmissionRefusal> {
        #[cfg(target_os = "linux")]
        let known = profile::by_identity(identity)
            .map(|profile| profile.inspection)
            .filter(|inspection| inspection.manifest == manifest);
        #[cfg(not(target_os = "linux"))]
        let known =
            (identity == FIXTURE.identity && manifest == FIXTURE.manifest).then_some(FIXTURE);
        known.ok_or(AdmissionRefusal::UnknownProfile)
    }

    /// The exact non-executable inspection manifest.
    #[must_use]
    pub fn manifest(self) -> &'static [u8] {
        self.manifest
    }

    /// Versioned identity of this non-executable inspection policy.
    #[must_use]
    pub fn identity(self) -> &'static str {
        self.identity
    }
}

/// An exact, short-lived observation, never a launch permission or support claim.
///
/// A host administrator can change routing immediately after any read. Rechecks
/// neither close that race nor detect changes that were reverted between reads.
#[derive(Debug)]
pub struct RoutingObservation {
    pattern: Vec<u8>,
    observed_at: Instant,
}

impl RoutingObservation {
    /// Read the real host routing through the trusted supervisor's proc view.
    ///
    /// Call only outside any payload mount/proc namespace. This read does not
    /// attest kernel behavior, namespace identity, or administrator coordination.
    ///
    /// # Errors
    /// Returns a typed refusal for non-Linux hosts, inaccessible routing or any
    /// value outside the narrow ordinary-file grammar.
    pub fn inspect(_profile: InspectionProfile) -> Result<Self, AdmissionRefusal> {
        if !cfg!(target_os = "linux") {
            return Err(AdmissionRefusal::UnsupportedOperatingSystem);
        }
        Self::from_reader(fs::File::open(CORE_PATTERN), Instant::now())
    }

    /// Re-read host routing and refuse execution even if the observation matches.
    ///
    /// # Errors
    /// Always returns a refusal. The most specific observed routing or freshness
    /// failure takes precedence over `IncompleteEnforcement`.
    pub fn refuse_execution(&self) -> Result<(), AdmissionRefusal> {
        if !cfg!(target_os = "linux") {
            return Err(AdmissionRefusal::UnsupportedOperatingSystem);
        }
        self.refuse_with_reader(fs::File::open(CORE_PATTERN), Instant::now)
    }

    fn refuse_with_reader(
        &self,
        reader: io::Result<impl Read>,
        clock: impl FnOnce() -> Instant,
    ) -> Result<(), AdmissionRefusal> {
        self.recheck(reader, clock)?;
        Err(AdmissionRefusal::IncompleteEnforcement)
    }

    fn from_reader(
        reader: io::Result<impl Read>,
        observed_at: Instant,
    ) -> Result<Self, AdmissionRefusal> {
        let pattern = read_pattern(reader)?;
        validate_pattern(&pattern)?;
        Ok(Self {
            pattern,
            observed_at,
        })
    }

    fn recheck(
        &self,
        reader: io::Result<impl Read>,
        clock: impl FnOnce() -> Instant,
    ) -> Result<(), AdmissionRefusal> {
        let current = read_pattern(reader)?;
        let elapsed = clock()
            .checked_duration_since(self.observed_at)
            .ok_or(AdmissionRefusal::StaleObservation)?;
        if elapsed >= OBSERVATION_LIFETIME {
            return Err(AdmissionRefusal::StaleObservation);
        }
        if current != self.pattern {
            return Err(AdmissionRefusal::CoreRoutingChanged);
        }
        validate_pattern(&current)
    }
}

/// The answer of one host admission run, which is always a refusal (decision 0056 rule 16).
///
/// When every implemented facility passed, the refusal is `IncompleteEnforcement` and the
/// report holds the evidence. It never permits a launch.
#[derive(Debug)]
pub struct AdmissionOutcome {
    refusal: AdmissionRefusal,
    report: Option<HostReport>,
}

impl AdmissionOutcome {
    /// The refusal; `IncompleteEnforcement` when every implemented facility passed.
    #[must_use]
    pub fn refusal(&self) -> AdmissionRefusal {
        self.refusal
    }

    /// Evidence recorded only when every implemented facility passed.
    #[must_use]
    pub fn report(&self) -> Option<&HostReport> {
        self.report.as_ref()
    }
}

/// Checks this host's facilities in the documented order and runs the trusted probes.
///
/// On Linux it runs the tool root's launcher and fixture (no payload), moves the calling
/// process into its group's `supervisor` child, and writes only below the provider's own
/// delegated cgroup. Calls in one process run one at a time. Call it only from the
/// supervisor's own view. Other systems are
/// refused before any proc path is read.
#[must_use]
pub fn inspect_host(profile: InspectionProfile) -> AdmissionOutcome {
    #[cfg(target_os = "linux")]
    {
        match admission::admit(profile, &admission::production_roots()) {
            Ok(report) => AdmissionOutcome {
                refusal: AdmissionRefusal::IncompleteEnforcement,
                report: Some(report),
            },
            Err(refusal) => AdmissionOutcome {
                refusal,
                report: None,
            },
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = profile;
        AdmissionOutcome {
            refusal: AdmissionRefusal::UnsupportedOperatingSystem,
            report: None,
        }
    }
}

/// The execution profiles this build knows (decision 0057 rules 1 and 2): every profile in the
/// table in a Linux build with the `enforcement-fixture` feature, none otherwise. A worker passes
/// them with every claim, so a `run_profile` job of any other profile ends as
/// `execution_unavailable`.
#[must_use]
pub fn known_profiles() -> Vec<ProfileName> {
    #[cfg(all(target_os = "linux", feature = "enforcement-fixture"))]
    {
        // An invalid constant is left out of the list, which fails closed.
        profile::names()
            .filter_map(|name| name.parse().ok())
            .collect()
    }
    #[cfg(not(all(target_os = "linux", feature = "enforcement-fixture")))]
    {
        Vec::new()
    }
}

fn read_pattern(reader: io::Result<impl Read>) -> Result<Vec<u8>, AdmissionRefusal> {
    let mut reader = reader.map_err(|_| AdmissionRefusal::UnreadableCoreRouting)?;
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(MAX_PATTERN_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| AdmissionRefusal::UnreadableCoreRouting)?;
    if bytes.len() as u64 > MAX_PATTERN_BYTES {
        return Err(AdmissionRefusal::UnknownCoreRouting);
    }
    Ok(bytes)
}

/// Compares the routing read now with the exact bytes that admission read, as the attempt does
/// at each poll of the stub and once more after its group is empty (decision 0056 rule 5, as
/// reworded by the amendment of 2026-11-13, crashes). Each read is fresh, so the one-second
/// lifetime of admission's observation does not apply. The current bytes are compared, not
/// validated again: bytes that differ are `CoreRoutingChanged`, whatever the new value is.
///
/// # Errors
///
/// Returns `UnreadableCoreRouting` for a read that fails and `UnknownCoreRouting` for a value
/// over the bound (both from `read_pattern`), and `CoreRoutingChanged` for bytes that differ.
#[cfg(any(test, all(target_os = "linux", feature = "enforcement-fixture")))]
pub(super) fn routing_unchanged(
    admitted: &[u8],
    reader: io::Result<impl Read>,
) -> Result<(), AdmissionRefusal> {
    let current = read_pattern(reader)?;
    if current != admitted {
        return Err(AdmissionRefusal::CoreRoutingChanged);
    }
    Ok(())
}

// Intentionally only a literal portable basename, optionally followed by the
// kernel proc entry's one newline. Percent expansions and directories await
// reviewed kernel/tool acceptance. A syntactic match is never host admission.
fn validate_pattern(bytes: &[u8]) -> Result<(), AdmissionRefusal> {
    match bytes.first() {
        Some(b'|') => return Err(AdmissionRefusal::PipeCoreRouting),
        Some(b'@') => return Err(AdmissionRefusal::SocketCoreRouting),
        _ => {}
    }
    let name = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    if name.is_empty()
        || name == b"."
        || name == b".."
        || !name
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(byte))
    {
        return Err(AdmissionRefusal::UnknownCoreRouting);
    }
    Ok(())
}

/// Read at most `limit` bytes of a small kernel file; a longer file is `InvalidData`.
#[cfg(target_os = "linux")]
fn read_bounded(path: &std::path::Path, limit: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if !u64::try_from(bytes.len()).is_ok_and(|length| length <= limit) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file exceeds its bound",
        ));
    }
    Ok(bytes)
}

/// Write a value to an existing cgroup control file; never creates the file.
#[cfg(target_os = "linux")]
fn write_control(path: &std::path::Path, value: &str) -> io::Result<()> {
    use std::io::Write as _;
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)?
        .write_all(value.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_collector_unknown_and_unreadable_routing() {
        for (pattern, refusal) in [
            (&b"|collector %p\n"[..], AdmissionRefusal::PipeCoreRouting),
            (&b"@collector\n"[..], AdmissionRefusal::SocketCoreRouting),
            (&b""[..], AdmissionRefusal::UnknownCoreRouting),
            (&b" core"[..], AdmissionRefusal::UnknownCoreRouting),
            (&b"core\n\n"[..], AdmissionRefusal::UnknownCoreRouting),
            (&b"core.%p"[..], AdmissionRefusal::UnknownCoreRouting),
            (&b"/tmp/core"[..], AdmissionRefusal::UnknownCoreRouting),
            (&b".."[..], AdmissionRefusal::UnknownCoreRouting),
            (&b"core\0"[..], AdmissionRefusal::UnknownCoreRouting),
            (&b"\xff"[..], AdmissionRefusal::UnknownCoreRouting),
        ] {
            assert_eq!(validate_pattern(pattern), Err(refusal), "{pattern:?}");
        }
        let unreadable: io::Result<&[u8]> = Err(io::Error::from(io::ErrorKind::PermissionDenied));
        assert_eq!(
            read_pattern(unreadable),
            Err(AdmissionRefusal::UnreadableCoreRouting)
        );
        assert_eq!(
            read_pattern(Ok(&vec![b'a'; 4097][..])),
            Err(AdmissionRefusal::UnknownCoreRouting)
        );
    }

    #[test]
    fn exact_recheck_rejects_policy_changes_even_between_file_modes() -> Result<(), AdmissionRefusal>
    {
        let now = Instant::now();
        let observation = RoutingObservation::from_reader(Ok(&b"core\n"[..]), now)?;
        assert_eq!(observation.recheck(Ok(&b"core\n"[..]), || now), Ok(()));
        for changed in [
            &b"other\n"[..],
            &b"core"[..],
            &b"|collector"[..],
            &b"@socket"[..],
        ] {
            assert_eq!(
                observation.recheck(Ok(changed), || now),
                Err(AdmissionRefusal::CoreRoutingChanged)
            );
        }
        let unreadable: io::Result<&[u8]> = Err(io::Error::from(io::ErrorKind::NotFound));
        assert_eq!(
            observation.recheck(unreadable, || now),
            Err(AdmissionRefusal::UnreadableCoreRouting)
        );
        Ok(())
    }

    #[test]
    fn monotonic_freshness_has_an_exact_boundary_and_rejects_reversal()
    -> Result<(), AdmissionRefusal> {
        let start = Instant::now();
        let observation = RoutingObservation::from_reader(Ok(&b"core"[..]), start)?;
        assert_eq!(
            observation.recheck(Ok(&b"core"[..]), || start
                + Duration::from_nanos(999_999_999)),
            Ok(())
        );
        assert_eq!(
            observation.recheck(Ok(&b"core"[..]), || start + OBSERVATION_LIFETIME),
            Err(AdmissionRefusal::StaleObservation)
        );
        let later =
            RoutingObservation::from_reader(Ok(&b"core"[..]), start + Duration::from_secs(1))?;
        assert_eq!(
            later.recheck(Ok(&b"core"[..]), || start),
            Err(AdmissionRefusal::StaleObservation)
        );
        Ok(())
    }

    #[test]
    fn freshness_includes_time_spent_reading() -> Result<(), AdmissionRefusal> {
        use std::cell::Cell;

        struct AdvancingReader<'clock> {
            bytes: &'static [u8],
            clock: &'clock Cell<Instant>,
            after_read: Instant,
        }

        impl Read for AdvancingReader<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                self.clock.set(self.after_read);
                self.bytes.read(buffer)
            }
        }

        let start = Instant::now();
        let observation = RoutingObservation::from_reader(Ok(&b"core"[..]), start)?;
        let clock = Cell::new(start);
        let reader = AdvancingReader {
            bytes: b"core",
            clock: &clock,
            after_read: start + OBSERVATION_LIFETIME,
        };
        assert_eq!(
            observation.refuse_with_reader(Ok(reader), || clock.get()),
            Err(AdmissionRefusal::StaleObservation)
        );
        Ok(())
    }

    #[test]
    fn profile_identity_is_exact_and_does_not_accept_commands_or_paths() {
        let profile = InspectionProfile::fixture();
        assert_eq!(
            InspectionProfile::verify_identity(profile.identity(), profile.manifest()),
            Ok(profile)
        );
        for identity in [
            "fixture",
            "/tmp/profile",
            "$(command)",
            "artifree-fixture-inspection-v1 ",
        ] {
            assert_eq!(
                InspectionProfile::verify_identity(identity, profile.manifest()),
                Err(AdmissionRefusal::UnknownProfile)
            );
        }
        for manifest in [
            &b"tools=/tmp/payload\n"[..],
            &b""[..],
            &b"routing=file-basename-v1\ntools=unpackaged\nenforcement=incomplete\n\n"[..],
        ] {
            assert_eq!(
                InspectionProfile::verify_identity(profile.identity(), manifest),
                Err(AdmissionRefusal::UnknownProfile)
            );
        }
    }

    #[test]
    fn matching_observation_still_refuses_execution() -> Result<(), AdmissionRefusal> {
        let now = Instant::now();
        let observation = RoutingObservation::from_reader(Ok(&b"core"[..]), now)?;
        assert_eq!(
            observation.refuse_with_reader(Ok(&b"core"[..]), || now),
            Err(AdmissionRefusal::IncompleteEnforcement)
        );
        Ok(())
    }

    #[test]
    fn actual_host_inspection_never_grants_execution() {
        // Exercise the real trusted proc read without changing routing or
        // launching anything. The host's value is deliberately not assumed.
        let refusal = match RoutingObservation::inspect(InspectionProfile::fixture()) {
            Ok(observation) => match observation.refuse_execution() {
                Err(refusal) => refusal,
                Ok(()) => panic!("partial inspector must never admit execution"),
            },
            Err(refusal) => refusal,
        };
        assert!(matches!(
            refusal,
            AdmissionRefusal::UnsupportedOperatingSystem
                | AdmissionRefusal::UnreadableCoreRouting
                | AdmissionRefusal::PipeCoreRouting
                | AdmissionRefusal::SocketCoreRouting
                | AdmissionRefusal::UnknownCoreRouting
                | AdmissionRefusal::CoreRoutingChanged
                | AdmissionRefusal::StaleObservation
                | AdmissionRefusal::IncompleteEnforcement
        ));
    }
}
