//! Atomic private upload storage and durable metadata-only receipts (0045).

use std::fmt;

use crate::artifact_store::{Delivery, RequestIdentity, StoreError};
use artifree_domain::SourceRevision;
use artifree_domain::assets::{
    AssetId, AssetMediaType, AssetMetadata, AssetRejection, AssetVersion,
};
use artifree_domain::identity::{ActorId, ArtifactId, UnixMillis};

/// One snapshot of an asset space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetSnapshot {
    /// Independent state version, zero before the first upload.
    pub version: AssetVersion,
    /// Metadata ordered by opaque ID.
    pub assets: Vec<AssetMetadata>,
    /// Logical association bytes, including equal-byte uploads.
    pub bytes: u64,
}
/// Immutable metadata and bytes read under the same live scoped authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetContent {
    /// Current version of the space.
    pub version: AssetVersion,
    /// Immutable metadata.
    pub metadata: AssetMetadata,
    /// Exact uploaded bytes.
    pub bytes: Vec<u8>,
}
/// Frozen identity and conditional state shared by preflight and settlement.
#[derive(Debug, Clone, Copy)]
pub struct AssetWrite<'a> {
    /// Actor-wide persistent retry identity.
    pub request: RequestIdentity<'a>,
    /// Live owned artifact.
    pub artifact: &'a ArtifactId,
    /// Observed independent asset version.
    pub base: AssetVersion,
    /// Optional runtime-only source fence, checked before replay.
    pub expected_source: Option<SourceRevision>,
    /// Server clock.
    pub now: UnixMillis,
}
/// A metadata-only operation receipt; it describes the original result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetReceipt {
    /// Original resulting version.
    pub version: AssetVersion,
    /// Uploaded metadata, or none for deletion.
    pub uploaded: Option<AssetMetadata>,
    /// Uploaded or requested deletion ID.
    pub id: AssetId,
    /// Whether deletion removed an association.
    pub deleted: bool,
    /// Whether this invocation committed or replayed.
    pub delivery: Delivery,
}
/// Storage or conditional rejection without a new key reservation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AssetStoreError {
    /// Missing, foreign, trashed or purged artifact or association.
    NotFound,
    /// The actor-wide key belongs to another request.
    RequestKeyReused,
    /// The independent state changed.
    Conflict {
        /// Current independent version.
        current: AssetVersion,
    },
    /// A runtime binding's source head changed before settlement.
    SourceConflict {
        /// Current source revision.
        current: SourceRevision,
    },
    /// Admission or logical quota refusal.
    Rejected(AssetRejection),
    /// Storage failed; the outcome can be unknown.
    Unavailable(String),
}
impl fmt::Display for AssetStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => formatter.write_str("asset or artifact not found"),
            Self::RequestKeyReused => {
                formatter.write_str("request key was already used for another request")
            }
            Self::Conflict { current } => write!(
                formatter,
                "asset state changed; current version is {}",
                current.0
            ),
            Self::SourceConflict { current } => write!(
                formatter,
                "source changed; current revision is {}",
                current.0
            ),
            Self::Rejected(reason) => write!(formatter, "{reason}"),
            Self::Unavailable(reason) => write!(formatter, "storage unavailable: {reason}"),
        }
    }
}
impl std::error::Error for AssetStoreError {}
impl From<StoreError> for AssetStoreError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::NotFound => Self::NotFound,
            StoreError::RequestKeyReused => Self::RequestKeyReused,
            StoreError::AssetLimitExceeded => Self::Rejected(AssetRejection::LimitExceeded),
            other => Self::Unavailable(other.to_string()),
        }
    }
}
/// Synchronous asset persistence. Every method checks live ownership in its snapshot.
pub trait AssetStore {
    /// Lists bounded metadata and usage in one snapshot.
    /// # Errors
    /// Missing authority or storage failure.
    fn assets(
        &self,
        actor: &ActorId,
        artifact: &ArtifactId,
    ) -> Result<AssetSnapshot, AssetStoreError>;
    /// Reads exact bytes under current scoped authority.
    /// # Errors
    /// Missing authority/association or storage failure.
    fn asset_content(
        &self,
        actor: &ActorId,
        artifact: &ArtifactId,
        id: &AssetId,
    ) -> Result<AssetContent, AssetStoreError>;
    /// Checks authority, optional source fence, key, and base in a short read snapshot.
    /// # Errors
    /// Authority, key, base or storage rejection.
    fn preflight_asset(
        &self,
        write: AssetWrite<'_>,
    ) -> Result<Option<AssetReceipt>, AssetStoreError>;
    /// Rechecks preflight order in one immediate transaction, then settles validation and quota.
    /// A matching concurrent receipt wins even over a validation rejection.
    /// # Errors
    /// Authority, key, base, saved validation, quota or storage rejection.
    fn upload_asset(
        &self,
        write: AssetWrite<'_>,
        media_type: &AssetMediaType,
        bytes: &[u8],
        validation: Result<(), AssetRejection>,
    ) -> Result<AssetReceipt, AssetStoreError>;
    /// Deletes one scoped association, retaining original replay metadata.
    /// # Errors
    /// Authority, key, base or storage rejection.
    fn delete_asset(
        &self,
        write: AssetWrite<'_>,
        id: &AssetId,
    ) -> Result<AssetReceipt, AssetStoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_limit_store_error_is_a_limit_rejection() {
        assert_eq!(
            AssetStoreError::from(StoreError::AssetLimitExceeded),
            AssetStoreError::Rejected(AssetRejection::LimitExceeded),
            "an asset quota refusal must not read as unavailable storage"
        );
    }
}
