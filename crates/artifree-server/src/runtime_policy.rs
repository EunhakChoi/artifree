//! Source declarations for the narrowly supported runtime contract.

use std::collections::BTreeSet;

use serde::Deserialize;

pub(crate) const MANIFEST_PATH: &str = "artifree-runtime.json";
pub(crate) const SDK_PIN: &str = "artifree.app.v1";
/// Capability names the runtime can grant (decisions 0020 and 0032).
pub(crate) const METADATA_READ: &str = "metadata.read";
const SOURCE_READ: &str = "source.read";
const SOURCE_WRITE: &str = "source.write";
const SOURCE_DOWNLOAD: &str = "source.download";
const MAX_MANIFEST_BYTES: usize = 16 * 1024;
const MAX_CAPABILITIES: usize = 32;
const MAX_CAPABILITY_BYTES: usize = 64;

/// One capability this server can grant to a frame. A declaration may name
/// other capabilities; only these can be granted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Capability {
    /// Read the artifact's own title and revision numbers (0019).
    MetadataRead,
    /// Read the pinned revision's source files (0032).
    SourceRead,
    /// Save changed source files as a new revision (0032). Distinct from
    /// reading: a frame that may read cannot write.
    SourceWrite,
    /// Ask the user to download the pinned revision's file or archive (0032).
    SourceDownload,
}

impl Capability {
    /// Every capability, in the order a grant lists them.
    pub(crate) const ALL: [Self; 4] = [
        Self::MetadataRead,
        Self::SourceRead,
        Self::SourceWrite,
        Self::SourceDownload,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::MetadataRead => METADATA_READ,
            Self::SourceRead => SOURCE_READ,
            Self::SourceWrite => SOURCE_WRITE,
            Self::SourceDownload => SOURCE_DOWNLOAD,
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|capability| capability.name() == name)
    }

    const fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

/// A small set of granted capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct CapabilitySet(u8);

impl CapabilitySet {
    pub(crate) fn contains(self, capability: Capability) -> bool {
        self.0 & capability.bit() != 0
    }

    fn with(self, capability: Capability) -> Self {
        Self(self.0 | capability.bit())
    }

    /// The granted names, in the fixed order of [`Capability::ALL`].
    pub(crate) fn names(self) -> Vec<String> {
        Capability::ALL
            .into_iter()
            .filter(|capability| self.contains(*capability))
            .map(|capability| capability.name().to_owned())
            .collect()
    }
}

impl FromIterator<Capability> for CapabilitySet {
    fn from_iter<I: IntoIterator<Item = Capability>>(iter: I) -> Self {
        Self(
            iter.into_iter()
                .fold(0, |bits, capability| bits | capability.bit()),
        )
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
        CapabilitySet::from_iter(Capability::ALL).names()
    }

    /// The grant a request asks for, or `None` unless it is a list of
    /// distinct capabilities that are both supported and declared by this
    /// revision. An empty list clears the grant.
    pub(crate) fn grant(&self, requested: &[String]) -> Option<CapabilitySet> {
        let mut granted = CapabilitySet::default();
        for name in requested {
            let capability = Capability::from_name(name)?;
            if !self.declared.contains(name) || granted.contains(capability) {
                return None;
            }
            granted = granted.with(capability);
        }
        Some(granted)
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
    use super::{Capability, CapabilitySet, METADATA_READ, RuntimePolicy};

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
        let names = |list: &[&str]| {
            list.iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            subset
                .grant(&names(&["metadata.read"]))
                .map(CapabilitySet::names),
            Some(names(&["metadata.read"]))
        );
        // Duplicates, a declared name this server does not support, and an
        // undeclared supported name are all refused.
        assert!(
            subset
                .grant(&names(&["metadata.read", "metadata.read"]))
                .is_none()
        );
        assert!(
            subset
                .grant(&names(&["metadata.read", "future.write"]))
                .is_none()
        );
        assert!(subset.grant(&names(&["future.write"])).is_none());
        assert!(subset.grant(&names(&["source.read"])).is_none());
        assert!(omitted.grant(&names(&["metadata.read"])).is_none());
        assert_eq!(subset.grant(&[]), Some(CapabilitySet::default()));
        Ok(())
    }

    #[test]
    fn source_capabilities_are_separate_declared_and_supported_names()
    -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(
            RuntimePolicy::supported(),
            [
                "metadata.read",
                "source.read",
                "source.write",
                "source.download"
            ]
        );
        let policy = RuntimePolicy::from_source(
            true,
            Some(
                br#"{"sdk":"artifree.app.v1","capabilities":["source.read","source.write","source.download"]}"#,
            ),
        )?;
        let names = |list: &[&str]| {
            list.iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>()
        };
        let read = policy
            .grant(&names(&["source.read"]))
            .ok_or("read must be grantable")?;
        // Holding the read capability says nothing about the others.
        assert!(read.contains(Capability::SourceRead));
        assert!(!read.contains(Capability::SourceWrite));
        assert!(!read.contains(Capability::SourceDownload));
        assert!(!read.contains(Capability::MetadataRead));
        let both = policy
            .grant(&names(&["source.write", "source.read"]))
            .ok_or("a declared subset must be grantable")?;
        assert_eq!(both.names(), names(&["source.read", "source.write"]));
        // A source capability is never implied by the metadata opt-in marker.
        let legacy = RuntimePolicy::from_source(true, None)?;
        assert!(legacy.grant(&names(&["source.write"])).is_none());
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
}
