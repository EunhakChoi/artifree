//! Private uploads share the artifact application boundary (0045).

use crate::artifacts::{ArtifactService, Fingerprint};
use artifree_domain::SourceRevision;
use artifree_domain::access::Space;
use artifree_domain::assets::{AssetId, AssetMediaType, AssetRejection, AssetVersion};
use artifree_domain::identity::{ActorId, ArtifactId, RequestKey, UnixMillis};
use artifree_ports::access_store::AccessStore;
use artifree_ports::artifact_store::{ArtifactStore, RequestIdentity};
use artifree_ports::asset_store::{
    AssetContent, AssetReceipt, AssetSnapshot, AssetStore, AssetStoreError, AssetWrite,
};

/// A frozen bounded upload. MIME syntax and decoded size belong to the stable envelope.
#[derive(Debug, Clone, Copy)]
pub struct AssetUpload<'a> {
    /// The space written: the actor's own, or the artifact's declared shared
    /// space (decision 0060 rule 10).
    pub space: Space,
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
    /// The space the association is deleted from.
    pub space: Space,
    /// Independent base, unrelated to source revisions.
    pub base: AssetVersion,
    /// Association to delete from the artifact.
    pub id: &'a AssetId,
    /// Server time.
    pub now: UnixMillis,
}

impl<S: ArtifactStore + AccessStore + AssetStore> ArtifactService<S> {
    /// Reads one scoped asset snapshot under current authority.
    /// # Errors
    /// Missing authority or storage failure.
    pub fn assets(
        &self,
        actor: &ActorId,
        artifact: &ArtifactId,
        space: Space,
    ) -> Result<AssetSnapshot, AssetStoreError> {
        self.store.assets(actor, artifact, space)
    }
    /// Checks, before a write's key and content are read, that `actor` may
    /// write `space` of the artifact (decision 0060 rule 4): access, then for
    /// the shared space its declaration and the role it names. The write
    /// checks again where it commits (rule 5).
    /// # Errors
    /// `NotFound`, `Forbidden` or `Unavailable`.
    pub fn admit_asset_write(
        &self,
        actor: &ActorId,
        artifact: &ArtifactId,
        space: Space,
    ) -> Result<(), AssetStoreError> {
        self.store.admit_asset_write(actor, artifact, space)
    }
    /// Reads exact immutable content under current authority.
    /// # Errors
    /// Missing authority/association or storage failure.
    pub fn asset_content(
        &self,
        actor: &ActorId,
        artifact: &ArtifactId,
        space: Space,
        id: &AssetId,
    ) -> Result<AssetContent, AssetStoreError> {
        self.store.asset_content(actor, artifact, space, id)
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
        shared_field(&mut fingerprint, upload.space);
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
            space: upload.space,
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
        (artifact, space): (&ArtifactId, Space),
        base: AssetVersion,
        id: &AssetId,
        now: UnixMillis,
    ) -> Result<AssetReceipt, AssetStoreError> {
        self.delete_fenced(
            actor,
            key,
            artifact,
            AssetDeletion {
                space,
                base,
                id,
                now,
            },
            None,
        )
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
        let AssetDeletion {
            space,
            base,
            id,
            now,
        } = deletion;
        let mut fingerprint = Fingerprint::new("delete_asset");
        fingerprint.field(artifact.as_str().as_bytes());
        shared_field(&mut fingerprint, space);
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
                space,
                base,
                expected_source,
                now,
            },
            id,
        )
    }
}

/// Names the shared space in a request's fingerprint, so a key sent to one
/// space never replays a write of the other; an own write hashes as before the
/// shared space existed (decision 0060 rule 10).
fn shared_field(fingerprint: &mut Fingerprint, space: Space) {
    if space == Space::Shared {
        fingerprint.field(space.as_str().as_bytes());
    }
}
