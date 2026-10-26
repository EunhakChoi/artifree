//! HTML file-package artifacts: create, conditional revision commits, and
//! revision-pinned reads.
//!
//! Every operation checks the actor's authority against current stored
//! metadata. Commit success means durable acceptance. Source history is kept
//! separate from future application data and access policy. Publish, restore,
//! and fork are not implemented.

use std::fmt;

use artifree_domain::identity::{ActorId, ArtifactId, RequestKey, Title};
use artifree_domain::package::{FileChange, FilePackage, PackageError, PackagePath};
use artifree_domain::{ArtifactKind, SourceRevision};
use artifree_ports::artifact_store::{
    ArtifactRecord, ArtifactStore, CommitReceipt, NewArtifact, RequestFingerprint, RequestIdentity,
    RevisionCommit, RevisionRecord, StoreError,
};
use sha2::{Digest, Sha256};

use crate::policy;

/// What could not be found. Inaccessible artifacts are reported as missing so
/// that callers cannot probe for identifiers they may not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    /// The artifact does not exist or the actor may not access it.
    Artifact,
    /// The revision does not exist.
    Revision,
    /// The file is not part of the revision.
    File,
}

/// Why an artifact operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ArtifactError {
    /// The submitted files or changes are invalid.
    InvalidPackage(PackageError),
    /// The target is missing or inaccessible.
    NotFound(Missing),
    /// The change was prepared against an older revision; nothing was written.
    RevisionConflict {
        /// The current revision.
        current: SourceRevision,
    },
    /// The actor already used this request key for different content.
    RequestKeyReused,
    /// Storage failed. The request may be retried with the same key.
    Unavailable(String),
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPackage(error) => write!(formatter, "invalid package: {error}"),
            Self::NotFound(Missing::Artifact) => formatter.write_str("artifact not found"),
            Self::NotFound(Missing::Revision) => formatter.write_str("revision not found"),
            Self::NotFound(Missing::File) => formatter.write_str("file not found in revision"),
            Self::RevisionConflict { current } => write!(
                formatter,
                "artifact changed; current revision is {}",
                current.0
            ),
            Self::RequestKeyReused => {
                formatter.write_str("request key was already used for a different request")
            }
            Self::Unavailable(reason) => write!(formatter, "storage unavailable: {reason}"),
        }
    }
}

impl std::error::Error for ArtifactError {}

impl From<StoreError> for ArtifactError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::NotFound => Self::NotFound(Missing::Artifact),
            StoreError::RevisionConflict { current } => Self::RevisionConflict { current },
            StoreError::RequestKeyReused => Self::RequestKeyReused,
            StoreError::Unavailable(reason) => Self::Unavailable(reason),
            other => Self::Unavailable(other.to_string()),
        }
    }
}

/// Application commands for file-package artifacts. HTTP, MCP, and CLI
/// adapters call these methods instead of the store.
#[derive(Debug)]
pub struct ArtifactService<S> {
    store: S,
}

impl<S: ArtifactStore> ArtifactService<S> {
    /// Uses `store` for all durable state.
    pub fn new(store: S) -> Self {
        Self { store }
    }

    /// Creates an app artifact owned by `actor` at revision 1.
    ///
    /// Retrying with the same key and content returns the original receipt.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::RequestKeyReused`] or [`ArtifactError::Unavailable`].
    pub fn create(
        &self,
        actor: &ActorId,
        key: &RequestKey,
        title: &Title,
        package: &FilePackage,
    ) -> Result<CommitReceipt, ArtifactError> {
        let kind = ArtifactKind::App;
        let mut hasher = Fingerprint::new("create");
        hasher.field(kind.as_str().as_bytes());
        hasher.field(title.as_str().as_bytes());
        for (path, content) in package.files() {
            hasher.put(path, content);
        }
        let receipt = self.store.create_artifact(NewArtifact {
            request: RequestIdentity {
                actor,
                key,
                fingerprint: hasher.finish(),
            },
            kind,
            title,
            package,
        })?;
        Ok(receipt)
    }

    /// Applies `changes` to revision `base` and commits the result as the next
    /// revision, only if `base` is still current.
    ///
    /// Retrying with the same key and content returns the original receipt even
    /// after later revisions were committed.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::NotFound`] for a missing or inaccessible artifact,
    /// [`ArtifactError::RevisionConflict`] when `base` is not current,
    /// [`ArtifactError::InvalidPackage`], [`ArtifactError::RequestKeyReused`],
    /// or [`ArtifactError::Unavailable`].
    pub fn save(
        &self,
        actor: &ActorId,
        key: &RequestKey,
        id: &ArtifactId,
        base: SourceRevision,
        changes: Vec<FileChange>,
    ) -> Result<CommitReceipt, ArtifactError> {
        let record = self.authorized(actor, id)?;
        if base > record.current_revision {
            return Err(ArtifactError::RevisionConflict {
                current: record.current_revision,
            });
        }
        let mut hasher = Fingerprint::new("save");
        hasher.field(id.as_str().as_bytes());
        hasher.field(&base.0.to_be_bytes());
        for change in &changes {
            match change {
                FileChange::Put { path, content } => hasher.put(path, content),
                FileChange::Delete { path } => {
                    hasher.field(b"delete");
                    hasher.field(path.as_str().as_bytes());
                }
            }
        }
        let fingerprint = hasher.finish();
        let base_package = self
            .store
            .revision_package(id, base)?
            .ok_or(ArtifactError::NotFound(Missing::Revision))?;
        let package = base_package
            .apply(changes)
            .map_err(ArtifactError::InvalidPackage)?;
        let receipt = self.store.commit_revision(RevisionCommit {
            request: RequestIdentity {
                actor,
                key,
                fingerprint,
            },
            artifact: id,
            base,
            package: &package,
        })?;
        Ok(receipt)
    }

    /// Current metadata of an accessible artifact.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::NotFound`] or [`ArtifactError::Unavailable`].
    pub fn artifact(
        &self,
        actor: &ActorId,
        id: &ArtifactId,
    ) -> Result<ArtifactRecord, ArtifactError> {
        self.authorized(actor, id)
    }

    /// Artifacts the actor owns, most recently updated first.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::Unavailable`].
    pub fn list(&self, actor: &ActorId) -> Result<Vec<ArtifactRecord>, ArtifactError> {
        Ok(self.store.artifacts_owned_by(actor)?)
    }

    /// Revision history of an accessible artifact, newest first.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::NotFound`] or [`ArtifactError::Unavailable`].
    pub fn revisions(
        &self,
        actor: &ActorId,
        id: &ArtifactId,
    ) -> Result<Vec<RevisionRecord>, ArtifactError> {
        self.authorized(actor, id)?;
        Ok(self.store.revisions(id)?)
    }

    /// One file of a pinned revision. Authority is checked on every read, so a
    /// previously issued link stops working when access ends.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::NotFound`] or [`ArtifactError::Unavailable`].
    pub fn read_file(
        &self,
        actor: &ActorId,
        id: &ArtifactId,
        revision: SourceRevision,
        path: &PackagePath,
    ) -> Result<Vec<u8>, ArtifactError> {
        let record = self.authorized(actor, id)?;
        if revision.0 == 0 || revision > record.current_revision {
            return Err(ArtifactError::NotFound(Missing::Revision));
        }
        self.store
            .revision_file(id, revision, path)?
            .ok_or(ArtifactError::NotFound(Missing::File))
    }

    fn authorized(
        &self,
        actor: &ActorId,
        id: &ArtifactId,
    ) -> Result<ArtifactRecord, ArtifactError> {
        match self.store.artifact(id)? {
            Some(record) if policy::may_access(actor, &record) => Ok(record),
            _ => Err(ArtifactError::NotFound(Missing::Artifact)),
        }
    }
}

/// Length-prefixed SHA-256 over a request's operation and content, so that
/// field boundaries cannot be shifted to produce the same digest.
struct Fingerprint(Sha256);

impl Fingerprint {
    fn new(operation: &str) -> Self {
        let mut fingerprint = Self(Sha256::new());
        fingerprint.field(operation.as_bytes());
        fingerprint
    }

    fn field(&mut self, bytes: &[u8]) {
        self.0.update((bytes.len() as u64).to_be_bytes());
        self.0.update(bytes);
    }

    fn put(&mut self, path: &PackagePath, content: &[u8]) {
        self.field(b"put");
        self.field(path.as_str().as_bytes());
        self.field(&Sha256::digest(content));
    }

    fn finish(self) -> RequestFingerprint {
        RequestFingerprint(self.0.finalize().into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(fields: &[&[u8]]) -> RequestFingerprint {
        let mut fingerprint = Fingerprint::new("test");
        for field in fields {
            fingerprint.field(field);
        }
        fingerprint.finish()
    }

    #[test]
    fn fingerprint_distinguishes_shifted_field_boundaries() {
        assert_ne!(digest(&[b"ab", b"c"]), digest(&[b"a", b"bc"]));
        assert_ne!(digest(&[b"abc"]), digest(&[b"abc", b""]));
        assert_eq!(digest(&[b"ab", b"c"]), digest(&[b"ab", b"c"]));
    }
}
