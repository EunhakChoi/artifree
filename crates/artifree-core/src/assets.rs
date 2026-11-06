//! Private uploads share the artifact application boundary (0045).

use crate::artifacts::{ArtifactService, Fingerprint};
use artifree_domain::assets::{AssetId, AssetMediaType, AssetRejection, AssetVersion};
use artifree_domain::identity::{ActorId, ArtifactId, RequestKey, UnixMillis};
use artifree_ports::artifact_store::{ArtifactStore, RequestIdentity};
use artifree_ports::asset_store::{
    AssetContent, AssetReceipt, AssetSnapshot, AssetStore, AssetStoreError, AssetWrite,
};

/// A frozen bounded upload. MIME syntax and decoded size belong to the stable envelope.
#[derive(Debug, Clone, Copy)]
pub struct AssetUpload<'a> {
    /// Independent base, unrelated to source revisions.
    pub base: AssetVersion,
    /// Original declared MIME spelling.
    pub content_type: &'a AssetMediaType,
    /// Original exact decoded bytes.
    pub bytes: &'a [u8],
    /// Server time.
    pub now: UnixMillis,
}
impl<S: ArtifactStore + AssetStore> ArtifactService<S> {
    /// Reads one scoped asset snapshot under current authority.
    /// # Errors
    /// Missing authority or storage failure.
    pub fn assets(
        &self,
        actor: &ActorId,
        artifact: &ArtifactId,
    ) -> Result<AssetSnapshot, AssetStoreError> {
        self.store.assets(actor, artifact)
    }
    /// Reads exact immutable content under current authority.
    /// # Errors
    /// Missing authority/association or storage failure.
    pub fn asset_content(
        &self,
        actor: &ActorId,
        artifact: &ArtifactId,
        id: &AssetId,
    ) -> Result<AssetContent, AssetStoreError> {
        self.store.asset_content(actor, artifact, id)
    }
    /// Uploads with replay preceding the current content validator.
    /// # Errors
    /// Authority, base, key, content, quota or storage rejection.
    pub fn upload_asset(
        &self,
        actor: &ActorId,
        key: &RequestKey,
        artifact: &ArtifactId,
        upload: AssetUpload<'_>,
    ) -> Result<AssetReceipt, AssetStoreError> {
        self.upload_asset_with_validator(
            actor,
            key,
            artifact,
            upload,
            crate::asset_validation::validate,
        )
    }
    /// Runs the current policy outside all storage snapshots and locks.
    ///
    /// The validator is injectable to verify policy retirement and concurrent
    /// settlement; it cannot authorize a write or bypass provider quotas.
    /// # Errors
    /// The same errors as `upload_asset`.
    pub fn upload_asset_with_validator(
        &self,
        actor: &ActorId,
        key: &RequestKey,
        artifact: &ArtifactId,
        upload: AssetUpload<'_>,
        validate: impl FnOnce(&AssetMediaType, &[u8]) -> Result<(), AssetRejection>,
    ) -> Result<AssetReceipt, AssetStoreError> {
        if upload.bytes.len() > artifree_domain::assets::MAX_ASSET_BYTES {
            return Err(AssetStoreError::Rejected(AssetRejection::LimitExceeded));
        }
        let mut fingerprint = Fingerprint::new("upload_asset");
        fingerprint.field(artifact.as_str().as_bytes());
        fingerprint.field(&upload.base.0.to_be_bytes());
        fingerprint.field(upload.content_type.as_str().as_bytes());
        fingerprint.field(upload.bytes);
        let write = AssetWrite {
            request: RequestIdentity {
                actor,
                key,
                fingerprint: fingerprint.finish(),
            },
            artifact,
            base: upload.base,
            expected_source: None,
            now: upload.now,
        };
        if let Some(receipt) = self.store.preflight_asset(write)? {
            return Ok(receipt);
        }
        let validation = validate(upload.content_type, upload.bytes);
        self.store
            .upload_asset(write, upload.content_type, upload.bytes, validation)
    }
    /// Deletes a scoped association with actor-wide durable replay.
    /// # Errors
    /// Authority, base, key or storage rejection.
    pub fn delete_asset(
        &self,
        actor: &ActorId,
        key: &RequestKey,
        artifact: &ArtifactId,
        base: AssetVersion,
        id: &AssetId,
        now: UnixMillis,
    ) -> Result<AssetReceipt, AssetStoreError> {
        let mut fingerprint = Fingerprint::new("delete_asset");
        fingerprint.field(artifact.as_str().as_bytes());
        fingerprint.field(&base.0.to_be_bytes());
        fingerprint.field(id.as_str().as_bytes());
        self.store.delete_asset(
            AssetWrite {
                request: RequestIdentity {
                    actor,
                    key,
                    fingerprint: fingerprint.finish(),
                },
                artifact,
                base,
                expected_source: None,
                now,
            },
            id,
        )
    }
}
