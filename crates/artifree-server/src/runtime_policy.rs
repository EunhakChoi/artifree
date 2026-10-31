//! Source declarations for the narrowly supported runtime contract.

use std::collections::BTreeSet;

use serde::Deserialize;

pub(crate) const MANIFEST_PATH: &str = "artifree-runtime.json";
pub(crate) const SDK_PIN: &str = "artifree.app.v1";
pub(crate) const METADATA_READ: &str = "metadata.read";
const MAX_MANIFEST_BYTES: usize = 16 * 1024;
const MAX_CAPABILITIES: usize = 32;
const MAX_CAPABILITY_BYTES: usize = 64;

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
        vec![METADATA_READ.to_owned()]
    }

    pub(crate) fn supports_grant(&self, requested: &[String]) -> bool {
        match requested {
            [] => true,
            [name] => name == METADATA_READ && self.declared.contains(name),
            _ => false,
        }
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
    use super::{METADATA_READ, RuntimePolicy};

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
        assert!(subset.supports_grant(&[METADATA_READ.to_owned()]));
        assert!(!subset.supports_grant(&[METADATA_READ.to_owned(), METADATA_READ.to_owned()]));
        assert!(!subset.supports_grant(&[METADATA_READ.to_owned(), "future.write".into()]));
        assert!(!subset.supports_grant(&["future.write".into()]));
        assert!(!omitted.supports_grant(&[METADATA_READ.to_owned()]));
        assert!(subset.supports_grant(&[]));
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
