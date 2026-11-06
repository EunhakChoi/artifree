//! Partial, read-only inspection for proposed untrusted execution (decision 0042).
//!
//! This module cannot launch a process or admit a host. Even a matching routing
//! observation ends in a typed refusal: containment and reviewed tools are absent.

use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::time::{Duration, Instant};

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
            Self::IncompleteEnforcement => "untrusted execution enforcement is not implemented",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for AdmissionRefusal {}

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
        if identity != FIXTURE.identity || manifest != FIXTURE.manifest {
            return Err(AdmissionRefusal::UnknownProfile);
        }
        Ok(FIXTURE)
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
