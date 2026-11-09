//! Source declarations for the narrowly supported runtime contract.

use std::collections::BTreeSet;

use serde::Deserialize;

pub(crate) const MANIFEST_PATH: &str = "artifree-runtime.json";
pub(crate) const SDK_PIN: &str = "artifree.app.v1";
pub(crate) const ASSETS_READ: &str = "assets.read";
pub(crate) const METADATA_READ: &str = "metadata.read";
pub(crate) const SOURCE_READ: &str = "source.read";
pub(crate) const SOURCE_DOWNLOAD: &str = "source.download";
pub(crate) const SOURCE_WRITE: &str = "source.write";
const MAX_MANIFEST_BYTES: usize = 16 * 1024;
const MAX_CAPABILITIES: usize = 32;
const MAX_CAPABILITY_BYTES: usize = 64;

/// A capability this server can grant. Declaring any other name is allowed
/// and grants nothing (decision 0020).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Capability {
    /// Listing an artifact's assets and reading their metadata and bytes
    /// (decision 0045, rule 12). The asset list and read frame routes check it.
    /// Its own consent: it neither implies nor is implied by any `source.*`
    /// capability.
    AssetsRead,
    MetadataRead,
    SourceDownload,
    SourceRead,
    /// Saving a file as a new revision; the only capability that changes the
    /// artifact (decision 0033).
    SourceWrite,
}

impl Capability {
    /// Every supported capability, in name order.
    pub(crate) const ALL: [Self; 5] = [
        Self::AssetsRead,
        Self::MetadataRead,
        Self::SourceDownload,
        Self::SourceRead,
        Self::SourceWrite,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::AssetsRead => ASSETS_READ,
            Self::MetadataRead => METADATA_READ,
            Self::SourceDownload => SOURCE_DOWNLOAD,
            Self::SourceRead => SOURCE_READ,
            Self::SourceWrite => SOURCE_WRITE,
        }
    }

    fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|known| known.name() == name)
    }

    fn bit(self) -> u8 {
        match self {
            Self::AssetsRead => 16,
            Self::MetadataRead => 1,
            Self::SourceDownload => 2,
            Self::SourceRead => 4,
            Self::SourceWrite => 8,
        }
    }
}

/// A set of granted capabilities. Each capability is its own consent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CapabilitySet(u8);

impl CapabilitySet {
    pub(crate) fn contains(self, capability: Capability) -> bool {
        self.0 & capability.bit() != 0
    }

    /// The names in the order of [`Capability::ALL`].
    pub(crate) fn names(self) -> Vec<String> {
        Capability::ALL
            .into_iter()
            .filter(|capability| self.contains(*capability))
            .map(|capability| capability.name().to_owned())
            .collect()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    sdk: String,
    #[serde(default)]
    capabilities: Vec<String>,
}

/// A complete, validated declaration for one source revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimePolicy {
    declared: BTreeSet<String>,
}

impl RuntimePolicy {
    /// A marker without a manifest preserves the original metadata opt-in.
    pub(crate) fn from_source(enabled: bool, bytes: Option<&[u8]>) -> Result<Self, &'static str> {
        if !enabled {
            return Ok(Self {
                declared: BTreeSet::new(),
            });
        }
        let Some(bytes) = bytes else {
            return Ok(Self {
                declared: BTreeSet::from([METADATA_READ.to_owned()]),
            });
        };
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err("runtime declaration is too large");
        }
        let manifest: Manifest =
            serde_json::from_slice(bytes).map_err(|_| "invalid runtime declaration")?;
        if manifest.sdk != SDK_PIN {
            return Err("unsupported runtime SDK pin");
        }
        if manifest.capabilities.len() > MAX_CAPABILITIES {
            return Err("too many declared capabilities");
        }
        let mut declared = BTreeSet::new();
        for name in manifest.capabilities {
            if !valid_capability(&name) || !declared.insert(name) {
                return Err("invalid or duplicate capability name");
            }
        }
        Ok(Self { declared })
    }

    pub(crate) fn declared(&self) -> Vec<String> {
        self.declared.iter().cloned().collect()
    }

    pub(crate) fn supported() -> Vec<String> {
        CapabilitySet(Capability::ALL.iter().fold(0, |bits, c| bits | c.bit())).names()
    }

    /// The set a user may grant, or `None` when `requested` names a
    /// capability that is undeclared, unsupported or repeated. An empty
    /// request is the empty set, which clears the grant.
    pub(crate) fn grant_of(&self, requested: &[String]) -> Option<CapabilitySet> {
        let mut set = CapabilitySet::default();
        for name in requested {
            let capability = Capability::parse(name).filter(|_| self.declared.contains(name))?;
            if set.contains(capability) {
                return None;
            }
            set.0 |= capability.bit();
        }
        Some(set)
    }
}

fn valid_capability(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_CAPABILITY_BYTES
        && name.contains('.')
        && name.split('.').all(|part| !part.is_empty())
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
        })
}

#[cfg(test)]
mod tests {
    use super::{
        ASSETS_READ, Capability, CapabilitySet, METADATA_READ, RuntimePolicy, SOURCE_DOWNLOAD,
        SOURCE_READ, SOURCE_WRITE,
    };

    #[test]
    fn snapshot_omission_subset_and_clear_are_distinct_from_legacy_opt_in()
    -> Result<(), Box<dyn std::error::Error>> {
        let legacy = RuntimePolicy::from_source(true, None)?;
        assert_eq!(legacy.declared(), [METADATA_READ]);
        let absent = RuntimePolicy::from_source(false, None)?;
        assert!(absent.declared().is_empty());
        let omitted = RuntimePolicy::from_source(true, Some(br#"{"sdk":"artifree.app.v1"}"#))?;
        let clear = RuntimePolicy::from_source(
            true,
            Some(br#"{"sdk":"artifree.app.v1","capabilities":[]}"#),
        )?;
        assert!(omitted.declared().is_empty());
        assert!(clear.declared().is_empty());
        let subset = RuntimePolicy::from_source(
            true,
            Some(br#"{"sdk":"artifree.app.v1","capabilities":["metadata.read","future.write"]}"#),
        )?;
        assert!(subset.grant_of(&[METADATA_READ.to_owned()]).is_some());
        assert!(
            subset
                .grant_of(&[METADATA_READ.to_owned(), METADATA_READ.to_owned()])
                .is_none()
        );
        assert!(
            subset
                .grant_of(&[METADATA_READ.to_owned(), "future.write".into()])
                .is_none()
        );
        assert!(subset.grant_of(&["future.write".into()]).is_none());
        assert!(omitted.grant_of(&[METADATA_READ.to_owned()]).is_none());
        assert_eq!(subset.grant_of(&[]).map(CapabilitySet::names), Some(vec![]));
        Ok(())
    }

    #[test]
    fn each_source_capability_is_declared_and_granted_on_its_own()
    -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(
            RuntimePolicy::supported(),
            [
                ASSETS_READ,
                METADATA_READ,
                SOURCE_DOWNLOAD,
                SOURCE_READ,
                SOURCE_WRITE
            ]
        );
        let read_only = RuntimePolicy::from_source(
            true,
            Some(br#"{"sdk":"artifree.app.v1","capabilities":["source.read","source.download"]}"#),
        )?;
        let one = read_only
            .grant_of(&[SOURCE_READ.to_owned()])
            .ok_or("a declared capability must be grantable")?;
        assert!(one.contains(Capability::SourceRead));
        assert!(!one.contains(Capability::SourceDownload));
        assert!(!one.contains(Capability::MetadataRead));
        let both = read_only
            .grant_of(&[SOURCE_DOWNLOAD.to_owned(), SOURCE_READ.to_owned()])
            .ok_or("two declared capabilities must be grantable")?;
        assert_eq!(both.names(), [SOURCE_DOWNLOAD, SOURCE_READ]);
        // A capability the entry never declared cannot be granted.
        assert!(read_only.grant_of(&[METADATA_READ.to_owned()]).is_none());
        let legacy = RuntimePolicy::from_source(true, None)?;
        assert!(legacy.grant_of(&[SOURCE_READ.to_owned()]).is_none());
        Ok(())
    }

    #[test]
    fn writing_is_granted_on_its_own_and_never_implied_by_reading()
    -> Result<(), Box<dyn std::error::Error>> {
        let policy = RuntimePolicy::from_source(
            true,
            Some(
                br#"{"sdk":"artifree.app.v1","capabilities":["source.read","source.download","source.write"]}"#,
            ),
        )?;
        for others in [
            vec![SOURCE_READ, SOURCE_DOWNLOAD],
            vec![SOURCE_READ],
            vec![SOURCE_DOWNLOAD],
            vec![],
        ] {
            let granted = policy
                .grant_of(
                    &others
                        .iter()
                        .map(|name| (*name).to_owned())
                        .collect::<Vec<_>>(),
                )
                .ok_or("a declared capability must be grantable")?;
            assert!(
                !granted.contains(Capability::SourceWrite),
                "{others:?} must not grant writing"
            );
        }
        let write = policy
            .grant_of(&[SOURCE_WRITE.to_owned()])
            .ok_or("a declared capability must be grantable")?;
        assert!(write.contains(Capability::SourceWrite));
        assert!(!write.contains(Capability::SourceRead));
        assert!(!write.contains(Capability::SourceDownload));
        // Declaring reading does not declare writing.
        let read_only = RuntimePolicy::from_source(
            true,
            Some(br#"{"sdk":"artifree.app.v1","capabilities":["source.read"]}"#),
        )?;
        assert!(read_only.grant_of(&[SOURCE_WRITE.to_owned()]).is_none());
        Ok(())
    }

    #[test]
    fn unsupported_pin_invalid_and_duplicate_declarations_are_refused() {
        for source in [
            br#"{"sdk":"artifree.app.v2"}"#.as_slice(),
            br#"{"capabilities":[]}"#,
            br#"{"sdk":"artifree.app.v1","capabilities":["metadata.read","metadata.read"]}"#,
            br#"{"sdk":"artifree.app.v1","capabilities":["Not-valid"]}"#,
            br#"{"sdk":"artifree.app.v1","capabilities":["metadata..read"]}"#,
            br#"{"sdk":"artifree.app.v1","unknown":true}"#,
        ] {
            assert!(RuntimePolicy::from_source(true, Some(source)).is_err());
        }
    }

    #[test]
    fn assets_read_is_declared_only_by_its_own_name() -> Result<(), Box<dyn std::error::Error>> {
        let sources_only = RuntimePolicy::from_source(
            true,
            Some(
                br#"{"sdk":"artifree.app.v1","capabilities":["source.download","source.read","source.write"]}"#,
            ),
        )?;
        assert_eq!(
            sources_only.declared(),
            [SOURCE_DOWNLOAD, SOURCE_READ, SOURCE_WRITE]
        );
        assert!(
            sources_only.grant_of(&[ASSETS_READ.to_owned()]).is_none(),
            "source declarations must not declare assets.read"
        );
        let assets_only = RuntimePolicy::from_source(
            true,
            Some(br#"{"sdk":"artifree.app.v1","capabilities":["assets.read"]}"#),
        )?;
        assert_eq!(assets_only.declared(), [ASSETS_READ]);
        for source in [SOURCE_DOWNLOAD, SOURCE_READ, SOURCE_WRITE] {
            assert!(
                assets_only.grant_of(&[source.to_owned()]).is_none(),
                "{source} is undeclared beside assets.read and must not be grantable"
            );
        }
        let legacy = RuntimePolicy::from_source(true, None)?;
        assert!(legacy.grant_of(&[ASSETS_READ.to_owned()]).is_none());
        Ok(())
    }

    #[test]
    fn assets_read_is_granted_and_withdrawn_independently_of_source_reads()
    -> Result<(), Box<dyn std::error::Error>> {
        let policy = RuntimePolicy::from_source(
            true,
            Some(
                br#"{"sdk":"artifree.app.v1","capabilities":["assets.read","source.download","source.read","source.write"]}"#,
            ),
        )?;
        let assets = policy
            .grant_of(&[ASSETS_READ.to_owned()])
            .ok_or("a declared capability must be grantable")?;
        assert_eq!(
            assets.names(),
            [ASSETS_READ],
            "assets.read alone must grant only itself"
        );
        for others in [
            vec![SOURCE_DOWNLOAD],
            vec![SOURCE_READ],
            vec![SOURCE_WRITE],
            vec![SOURCE_DOWNLOAD, SOURCE_READ, SOURCE_WRITE],
        ] {
            let granted = policy
                .grant_of(
                    &others
                        .iter()
                        .map(|name| (*name).to_owned())
                        .collect::<Vec<_>>(),
                )
                .ok_or("a declared capability must be grantable")?;
            assert!(
                !granted.contains(Capability::AssetsRead),
                "{others:?} must not grant assets.read"
            );
        }
        // Withdrawing either one from both leaves the other held.
        let both = policy
            .grant_of(&[SOURCE_READ.to_owned(), ASSETS_READ.to_owned()])
            .ok_or("two declared capabilities must be grantable")?;
        assert_eq!(both.names(), [ASSETS_READ, SOURCE_READ]);
        let without_assets = policy
            .grant_of(&[SOURCE_READ.to_owned()])
            .ok_or("a declared capability must be grantable")?;
        assert_eq!(
            without_assets.names(),
            [SOURCE_READ],
            "withdrawing assets.read must leave exactly source.read"
        );
        Ok(())
    }

    #[test]
    fn assets_read_round_trips_through_its_name_and_its_bit()
    -> Result<(), Box<dyn std::error::Error>> {
        let policy = RuntimePolicy::from_source(
            true,
            Some(br#"{"sdk":"artifree.app.v1","capabilities":["assets.read"]}"#),
        )?;
        let granted = policy
            .grant_of(&[ASSETS_READ.to_owned()])
            .ok_or("a declared capability must be grantable")?;
        assert_eq!(granted.names(), [ASSETS_READ]);
        assert_eq!(Capability::parse(ASSETS_READ), Some(Capability::AssetsRead));
        for capability in Capability::ALL {
            assert_eq!(
                CapabilitySet(capability.bit()).names(),
                [capability.name()],
                "the bit of {capability:?} must name only that capability"
            );
            assert_eq!(Capability::parse(capability.name()), Some(capability));
        }
        Ok(())
    }

    #[test]
    fn near_assets_names_name_no_capability_and_grant_nothing()
    -> Result<(), Box<dyn std::error::Error>> {
        for near in [
            "assets",
            "assets.write",
            "assets.download",
            "assets.reads",
            "asset.read",
            "Assets.read",
            "assets.read.v2",
        ] {
            assert_eq!(
                Capability::parse(near),
                None,
                "{near:?} must not name a capability"
            );
        }
        // A well-formed but unknown name may be declared (decision 0020) and still grants nothing.
        let policy = RuntimePolicy::from_source(
            true,
            Some(br#"{"sdk":"artifree.app.v1","capabilities":["assets.download","assets.write"]}"#),
        )?;
        assert_eq!(policy.declared(), ["assets.download", "assets.write"]);
        for near in ["assets.download", "assets.write"] {
            assert!(
                policy.grant_of(&[near.to_owned()]).is_none(),
                "{near} is not a capability and must not be grantable"
            );
        }
        Ok(())
    }

    #[test]
    fn bare_or_malformed_assets_declarations_are_refused() {
        for source in [
            br#"{"sdk":"artifree.app.v1","capabilities":["assets"]}"#.as_slice(),
            br#"{"sdk":"artifree.app.v1","capabilities":["Assets.read"]}"#,
            br#"{"sdk":"artifree.app.v1","capabilities":["assets.read "]}"#,
        ] {
            assert_eq!(
                RuntimePolicy::from_source(true, Some(source)),
                Err("invalid or duplicate capability name"),
                "declaration {} must be refused",
                String::from_utf8_lossy(source)
            );
        }
    }
}
