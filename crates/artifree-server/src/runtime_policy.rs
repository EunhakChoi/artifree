//! Source declarations for the narrowly supported runtime contract.

use std::collections::BTreeSet;

use serde::Deserialize;

pub(crate) const MANIFEST_PATH: &str = "artifree-runtime.json";
pub(crate) const SDK_PIN: &str = "artifree.app.v1";
pub(crate) const ASSETS_DOWNLOAD: &str = "assets.download";
pub(crate) const ASSETS_READ: &str = "assets.read";
pub(crate) const ASSETS_WRITE: &str = "assets.write";
pub(crate) const DATA_READ: &str = "data.read";
pub(crate) const DATA_WRITE: &str = "data.write";
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
    /// Saving one of an artifact's uploaded assets to the device after the user
    /// confirms each download (decision 0045, rule 12). The asset download
    /// frame routes check it. Its own consent: it neither implies nor is
    /// implied by `assets.read`, `assets.write` or any `source.*` capability.
    AssetsDownload,
    /// Listing an artifact's assets and reading their metadata and bytes
    /// (decision 0045, rule 12). The asset list and read frame routes check it.
    /// Its own consent: it neither implies nor is implied by any `source.*`
    /// capability.
    AssetsRead,
    /// Uploading and deleting an artifact's uploaded assets (decision 0045,
    /// rule 12). The asset upload and delete frame routes check it. Its own
    /// consent: it neither implies nor is implied by `assets.read` or any
    /// `source.*` capability.
    AssetsWrite,
    /// Reading the calling user's application data for the artifact a view is
    /// bound to (decision 0053, rule 1). The data read frame route checks it.
    /// Its own consent: it neither implies nor is implied by any `metadata`,
    /// `source` or `assets` capability.
    DataRead,
    /// Replacing, merging into and removing the calling user's application data
    /// for the artifact a view is bound to (decision 0053, rule 1). The data set,
    /// update and delete frame routes check it. Its own consent: it neither
    /// implies nor is implied by `data.read` or any `metadata`, `source` or
    /// `assets` capability.
    DataWrite,
    MetadataRead,
    SourceDownload,
    SourceRead,
    /// Saving a file as a new revision; the only capability that changes the
    /// artifact (decision 0033).
    SourceWrite,
}

impl Capability {
    /// Every supported capability, in name order.
    pub(crate) const ALL: [Self; 9] = [
        Self::AssetsDownload,
        Self::AssetsRead,
        Self::AssetsWrite,
        Self::DataRead,
        Self::DataWrite,
        Self::MetadataRead,
        Self::SourceDownload,
        Self::SourceRead,
        Self::SourceWrite,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::AssetsDownload => ASSETS_DOWNLOAD,
            Self::AssetsRead => ASSETS_READ,
            Self::AssetsWrite => ASSETS_WRITE,
            Self::DataRead => DATA_READ,
            Self::DataWrite => DATA_WRITE,
            Self::MetadataRead => METADATA_READ,
            Self::SourceDownload => SOURCE_DOWNLOAD,
            Self::SourceRead => SOURCE_READ,
            Self::SourceWrite => SOURCE_WRITE,
        }
    }

    fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|known| known.name() == name)
    }

    fn bit(self) -> u16 {
        match self {
            Self::AssetsDownload => 64,
            Self::AssetsRead => 16,
            Self::AssetsWrite => 32,
            Self::DataRead => 128,
            Self::DataWrite => 256,
            Self::MetadataRead => 1,
            Self::SourceDownload => 2,
            Self::SourceRead => 4,
            Self::SourceWrite => 8,
        }
    }
}

/// A set of granted capabilities. Each capability is its own consent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CapabilitySet(u16);

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
        ASSETS_DOWNLOAD, ASSETS_READ, ASSETS_WRITE, Capability, CapabilitySet, DATA_READ,
        DATA_WRITE, METADATA_READ, RuntimePolicy, SOURCE_DOWNLOAD, SOURCE_READ, SOURCE_WRITE,
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
                ASSETS_DOWNLOAD,
                ASSETS_READ,
                ASSETS_WRITE,
                DATA_READ,
                DATA_WRITE,
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

    // Grants exactly `names`; a name that is undeclared, unknown or repeated
    // refuses the whole request.
    fn grant(policy: &RuntimePolicy, names: &[&str]) -> Result<CapabilitySet, &'static str> {
        policy
            .grant_of(
                &names
                    .iter()
                    .map(|name| (*name).to_owned())
                    .collect::<Vec<_>>(),
            )
            .ok_or("a declared capability must be grantable")
    }

    #[test]
    fn assets_write_is_declared_only_by_its_own_name() -> Result<(), Box<dyn std::error::Error>> {
        let others = RuntimePolicy::from_source(
            true,
            Some(
                br#"{"sdk":"artifree.app.v1","capabilities":["assets.read","source.download","source.read","source.write"]}"#,
            ),
        )?;
        assert_eq!(
            others.declared(),
            [ASSETS_READ, SOURCE_DOWNLOAD, SOURCE_READ, SOURCE_WRITE]
        );
        assert!(
            others.grant_of(&[ASSETS_WRITE.to_owned()]).is_none(),
            "declaring assets.read and source.* must not declare assets.write"
        );
        let own = RuntimePolicy::from_source(
            true,
            Some(br#"{"sdk":"artifree.app.v1","capabilities":["assets.write"]}"#),
        )?;
        assert_eq!(own.declared(), [ASSETS_WRITE]);
        for other in [ASSETS_READ, SOURCE_DOWNLOAD, SOURCE_READ, SOURCE_WRITE] {
            assert!(
                own.grant_of(&[other.to_owned()]).is_none(),
                "{other} is undeclared beside assets.write and must not be grantable"
            );
        }
        let legacy = RuntimePolicy::from_source(true, None)?;
        assert!(legacy.grant_of(&[ASSETS_WRITE.to_owned()]).is_none());
        Ok(())
    }

    #[test]
    fn assets_write_is_granted_and_withdrawn_independently_of_assets_read_and_sources()
    -> Result<(), Box<dyn std::error::Error>> {
        let policy = RuntimePolicy::from_source(
            true,
            Some(
                br#"{"sdk":"artifree.app.v1","capabilities":["assets.read","assets.write","source.download","source.read","source.write"]}"#,
            ),
        )?;
        let write = grant(&policy, &[ASSETS_WRITE])?;
        assert_eq!(
            write.names(),
            [ASSETS_WRITE],
            "assets.write alone must grant only itself"
        );
        for (name, capability) in [
            (ASSETS_READ, Capability::AssetsRead),
            (SOURCE_DOWNLOAD, Capability::SourceDownload),
            (SOURCE_READ, Capability::SourceRead),
            (SOURCE_WRITE, Capability::SourceWrite),
        ] {
            // Withdrawing assets.write leaves exactly this capability, which
            // does not grant writing.
            let alone = grant(&policy, &[name])?;
            assert_eq!(alone.names(), [name], "{name} alone must grant only itself");
            assert!(
                !alone.contains(Capability::AssetsWrite),
                "{name} must not grant assets.write"
            );
            // Granted together, each keeps its own consent and implies nothing
            // else. Withdrawing either one from the pair leaves the other alone,
            // as checked above and below.
            let both = grant(&policy, &[ASSETS_WRITE, name])?;
            assert!(
                both.contains(Capability::AssetsWrite) && both.contains(capability),
                "assets.write beside {name} must grant both, got {:?}",
                both.names()
            );
            assert_eq!(
                both.names().len(),
                2,
                "assets.write beside {name} must grant exactly two, got {:?}",
                both.names()
            );
        }
        let others = grant(
            &policy,
            &[ASSETS_READ, SOURCE_DOWNLOAD, SOURCE_READ, SOURCE_WRITE],
        )?;
        assert!(
            !others.contains(Capability::AssetsWrite),
            "all the other capabilities together must not grant assets.write"
        );
        Ok(())
    }

    #[test]
    fn assets_write_round_trips_through_its_name_and_its_bit()
    -> Result<(), Box<dyn std::error::Error>> {
        let policy = RuntimePolicy::from_source(
            true,
            Some(br#"{"sdk":"artifree.app.v1","capabilities":["assets.write"]}"#),
        )?;
        let granted = grant(&policy, &[ASSETS_WRITE])?;
        assert_eq!(granted.names(), [ASSETS_WRITE]);
        assert_eq!(
            policy.grant_of(&granted.names()),
            Some(granted),
            "the names of a grant must grant the same set again"
        );
        assert_eq!(
            Capability::parse(ASSETS_WRITE),
            Some(Capability::AssetsWrite)
        );
        assert_eq!(
            CapabilitySet(Capability::AssetsWrite.bit()).names(),
            [ASSETS_WRITE],
            "the bit of assets.write must name only assets.write"
        );
        Ok(())
    }

    #[test]
    fn assets_download_is_declared_only_by_its_own_name() -> Result<(), Box<dyn std::error::Error>>
    {
        let others = RuntimePolicy::from_source(
            true,
            Some(
                br#"{"sdk":"artifree.app.v1","capabilities":["assets.read","assets.write","source.download","source.read","source.write"]}"#,
            ),
        )?;
        assert_eq!(
            others.declared(),
            [
                ASSETS_READ,
                ASSETS_WRITE,
                SOURCE_DOWNLOAD,
                SOURCE_READ,
                SOURCE_WRITE
            ]
        );
        assert!(
            others.grant_of(&[ASSETS_DOWNLOAD.to_owned()]).is_none(),
            "declaring assets.read, assets.write and source.* must not declare assets.download"
        );
        let own = RuntimePolicy::from_source(
            true,
            Some(br#"{"sdk":"artifree.app.v1","capabilities":["assets.download"]}"#),
        )?;
        assert_eq!(own.declared(), [ASSETS_DOWNLOAD]);
        for other in [
            ASSETS_READ,
            ASSETS_WRITE,
            DATA_READ,
            METADATA_READ,
            SOURCE_DOWNLOAD,
            SOURCE_READ,
            SOURCE_WRITE,
        ] {
            assert!(
                own.grant_of(&[other.to_owned()]).is_none(),
                "{other} is undeclared beside assets.download and must not be grantable"
            );
        }
        let legacy = RuntimePolicy::from_source(true, None)?;
        assert!(legacy.grant_of(&[ASSETS_DOWNLOAD.to_owned()]).is_none());
        Ok(())
    }

    #[test]
    fn assets_download_is_granted_and_withdrawn_independently_of_every_other_capability()
    -> Result<(), Box<dyn std::error::Error>> {
        let policy = RuntimePolicy::from_source(
            true,
            Some(
                br#"{"sdk":"artifree.app.v1","capabilities":["assets.download","assets.read","assets.write","data.read","metadata.read","source.download","source.read","source.write"]}"#,
            ),
        )?;
        let download = grant(&policy, &[ASSETS_DOWNLOAD])?;
        assert_eq!(
            download.names(),
            [ASSETS_DOWNLOAD],
            "assets.download alone must grant only itself"
        );
        for (name, capability) in [
            (ASSETS_READ, Capability::AssetsRead),
            (ASSETS_WRITE, Capability::AssetsWrite),
            (DATA_READ, Capability::DataRead),
            (METADATA_READ, Capability::MetadataRead),
            (SOURCE_DOWNLOAD, Capability::SourceDownload),
            (SOURCE_READ, Capability::SourceRead),
            (SOURCE_WRITE, Capability::SourceWrite),
        ] {
            // Without assets.download, this capability alone grants only itself.
            let alone = grant(&policy, &[name])?;
            assert_eq!(alone.names(), [name], "{name} alone must grant only itself");
            assert!(
                !alone.contains(Capability::AssetsDownload),
                "{name} must not grant assets.download"
            );
            // Granted together, each keeps its own consent and implies nothing
            // else. Withdrawing either one from the pair leaves the other alone.
            let both = grant(&policy, &[ASSETS_DOWNLOAD, name])?;
            assert!(
                both.contains(Capability::AssetsDownload) && both.contains(capability),
                "assets.download beside {name} must grant both, got {:?}",
                both.names()
            );
            assert_eq!(
                both.names().len(),
                2,
                "assets.download beside {name} must grant exactly two, got {:?}",
                both.names()
            );
            assert_eq!(
                grant(&policy, &[ASSETS_DOWNLOAD])?.names(),
                [ASSETS_DOWNLOAD],
                "withdrawing {name} must leave assets.download"
            );
        }
        let others = grant(
            &policy,
            &[
                ASSETS_READ,
                ASSETS_WRITE,
                DATA_READ,
                METADATA_READ,
                SOURCE_DOWNLOAD,
                SOURCE_READ,
                SOURCE_WRITE,
            ],
        )?;
        assert_eq!(
            others.names(),
            [
                ASSETS_READ,
                ASSETS_WRITE,
                DATA_READ,
                METADATA_READ,
                SOURCE_DOWNLOAD,
                SOURCE_READ,
                SOURCE_WRITE
            ],
            "withdrawing assets.download must leave every other capability"
        );
        Ok(())
    }

    #[test]
    fn assets_download_round_trips_through_its_name_and_its_bit()
    -> Result<(), Box<dyn std::error::Error>> {
        let policy = RuntimePolicy::from_source(
            true,
            Some(br#"{"sdk":"artifree.app.v1","capabilities":["assets.download"]}"#),
        )?;
        let granted = grant(&policy, &[ASSETS_DOWNLOAD])?;
        assert_eq!(granted.names(), [ASSETS_DOWNLOAD]);
        assert_eq!(
            policy.grant_of(&granted.names()),
            Some(granted),
            "the names of a grant must grant the same set again"
        );
        assert_eq!(
            Capability::parse(ASSETS_DOWNLOAD),
            Some(Capability::AssetsDownload)
        );
        assert_eq!(
            Capability::AssetsDownload.bit(),
            64,
            "assets.download must be bit 64"
        );
        assert_eq!(
            CapabilitySet(64).names(),
            [ASSETS_DOWNLOAD],
            "bit 64 must name only assets.download"
        );
        Ok(())
    }

    #[test]
    fn near_assets_names_name_no_capability_and_grant_nothing()
    -> Result<(), Box<dyn std::error::Error>> {
        for near in [
            "assets",
            "assets.downloads",
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
            Some(br#"{"sdk":"artifree.app.v1","capabilities":["assets.downloads"]}"#),
        )?;
        assert_eq!(policy.declared(), ["assets.downloads"]);
        assert!(
            policy.grant_of(&["assets.downloads".to_owned()]).is_none(),
            "assets.downloads is not a capability and must not be grantable"
        );
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
