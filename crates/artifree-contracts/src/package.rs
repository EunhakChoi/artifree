//! The rules a package must meet, as the server enforces them (decision
//! 0017). Clients use them to check a change set before sending it; the
//! server checks every request again.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Limits and accepted file types of a package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PackageRules {
    /// The document a preview opens; every package has it.
    pub entry_path: String,
    /// Most files in one package.
    pub max_files: u64,
    /// Largest file in bytes.
    pub max_file_bytes: u64,
    /// Largest sum of every file's size in bytes.
    pub max_package_bytes: u64,
    /// Longest path in bytes.
    pub max_path_bytes: u64,
    /// Most segments in one path.
    pub max_path_segments: u64,
    /// Longest path segment in bytes.
    pub max_segment_bytes: u64,
    /// Every accepted file extension, in lowercase, with the `Content-Type`
    /// its files are served with. Extensions match without regard to ASCII
    /// case.
    pub media_types: BTreeMap<String, String>,
}
