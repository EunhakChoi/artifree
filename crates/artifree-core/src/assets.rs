//! Private uploads share the artifact application boundary (0045).

use crate::artifacts::{ArtifactService, Fingerprint};
use artifree_domain::SourceRevision;
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

/// One scoped asset association to delete. Like `AssetUpload`, it carries the
/// base and server time.
#[derive(Debug, Clone, Copy)]
pub struct AssetDeletion<'a> {
    /// Independent base, unrelated to source revisions.
    pub base: AssetVersion,
    /// Association to delete from the artifact.
    pub id: &'a AssetId,
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
        self.upload_fenced(actor, key, artifact, upload, None, validate)
    }
    /// Uploads one runtime frame's asset, fenced to the source revision its
    /// binding pinned. The store checks that head before key replay, so a
    /// changed source refuses the call instead of replaying an old receipt
    /// (0045 rule 12).
    /// # Errors
    /// The same errors as `upload_asset`, plus
    /// `AssetStoreError::SourceConflict` when the source head moved.
    pub fn runtime_upload_asset(
        &self,
        actor: &ActorId,
        key: &RequestKey,
        artifact: &ArtifactId,
        upload: AssetUpload<'_>,
        expected_source: SourceRevision,
    ) -> Result<AssetReceipt, AssetStoreError> {
        self.upload_fenced(
            actor,
            key,
            artifact,
            upload,
            Some(expected_source),
            crate::asset_validation::validate,
        )
    }
    /// Shared upload path. `expected_source` is the runtime source fence, or
    /// `None` for a caller that holds no frame.
    fn upload_fenced(
        &self,
        actor: &ActorId,
        key: &RequestKey,
        artifact: &ArtifactId,
        upload: AssetUpload<'_>,
        expected_source: Option<SourceRevision>,
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
            expected_source,
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
        self.delete_fenced(actor, key, artifact, AssetDeletion { base, id, now }, None)
    }
    /// Deletes one runtime frame's asset association, fenced to the source
    /// revision its binding pinned. The store checks that head before key
    /// replay, as for uploads (0045 rule 12).
    /// # Errors
    /// The same errors as `delete_asset`, plus
    /// `AssetStoreError::SourceConflict` when the source head moved.
    pub fn runtime_delete_asset(
        &self,
        actor: &ActorId,
        key: &RequestKey,
        artifact: &ArtifactId,
        deletion: AssetDeletion<'_>,
        expected_source: SourceRevision,
    ) -> Result<AssetReceipt, AssetStoreError> {
        self.delete_fenced(actor, key, artifact, deletion, Some(expected_source))
    }
    /// Shared delete path. `expected_source` is the runtime source fence, or
    /// `None` for a caller that holds no frame.
    fn delete_fenced(
        &self,
        actor: &ActorId,
        key: &RequestKey,
        artifact: &ArtifactId,
        deletion: AssetDeletion<'_>,
        expected_source: Option<SourceRevision>,
    ) -> Result<AssetReceipt, AssetStoreError> {
        let AssetDeletion { base, id, now } = deletion;
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
                expected_source,
                now,
            },
            id,
        )
    }
}
