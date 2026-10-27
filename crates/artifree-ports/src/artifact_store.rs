//! Durable storage for artifact metadata, immutable source revisions, and
//! request-key results.
//!
//! An implementation must make each commit atomic: the conditional revision
//! check, revision files, metadata update, and request-key record either all
//! become durable or none do. A returned commit is durable.

use std::fmt;

use artifree_domain::identity::{ActorId, ArtifactId, RequestKey, Title, UnixMillis};
use artifree_domain::package::{FilePackage, PackagePath};
use artifree_domain::{ArtifactKind, SourceRevision};

/// Stored metadata of an artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRecord {
    /// Artifact identifier.
    pub id: ArtifactId,
    /// Actor that owns the artifact.
    pub owner: ActorId,
    /// Source model family.
    pub kind: ArtifactKind,
    /// Display title.
    pub title: Title,
    /// Latest committed source revision.
    pub current_revision: SourceRevision,
    /// Creation time.
    pub created_at: UnixMillis,
    /// Time of the latest committed revision.
    pub updated_at: UnixMillis,
}

/// A file listed in a revision manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// Package path.
    pub path: PackagePath,
    /// Content size in bytes.
    pub size: u64,
}

/// Stored metadata of one immutable revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionRecord {
    /// Revision number, starting at 1 for each artifact.
    pub revision: SourceRevision,
    /// Actor that committed the revision.
    pub created_by: ActorId,
    /// Commit time.
    pub created_at: UnixMillis,
    /// Files in path order.
    pub files: Vec<FileEntry>,
}

/// A digest that identifies the complete content of a mutating request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestFingerprint(pub [u8; 32]);

/// The retry identity of a mutating request.
#[derive(Debug, Clone, Copy)]
pub struct RequestIdentity<'a> {
    /// Actor that owns the key's scope.
    pub actor: &'a ActorId,
    /// Client-chosen key.
    pub key: &'a RequestKey,
    /// Digest of the request content bound to the key.
    pub fingerprint: RequestFingerprint,
}

/// A request to create an artifact with revision 1.
#[derive(Debug, Clone, Copy)]
pub struct NewArtifact<'a> {
    /// Retry identity; its actor becomes the owner.
    pub request: RequestIdentity<'a>,
    /// Source model family.
    pub kind: ArtifactKind,
    /// Display title.
    pub title: &'a Title,
    /// Initial source.
    pub package: &'a FilePackage,
}

/// A request to append a revision when the artifact is still at `base`.
#[derive(Debug, Clone, Copy)]
pub struct RevisionCommit<'a> {
    /// Retry identity of the committing actor.
    pub request: RequestIdentity<'a>,
    /// Artifact to change.
    pub artifact: &'a ArtifactId,
    /// Revision the change was prepared against.
    pub base: SourceRevision,
    /// Complete new source.
    pub package: &'a FilePackage,
}

/// Whether a commit was performed now or is the stored result of a retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// This call committed the revision.
    Committed,
    /// The same request was committed earlier; nothing new was written.
    Replayed,
}

/// The durable result of a create or revision commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitReceipt {
    /// Artifact metadata after the commit identified by the request.
    pub artifact: ArtifactRecord,
    /// The revision the request created.
    pub revision: RevisionRecord,
    /// Whether this call wrote it.
    pub delivery: Delivery,
}

/// A storage failure or a rejected conditional write.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum StoreError {
    /// The artifact does not exist.
    NotFound,
    /// The artifact's current revision is not the requested base.
    RevisionConflict {
        /// The revision that is current now.
        current: SourceRevision,
    },
    /// The actor already used the key for a request with different content.
    RequestKeyReused,
    /// Storage could not complete the operation; the outcome is not committed
    /// unless a later retry with the same key reports it.
    Unavailable(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => formatter.write_str("artifact not found"),
            Self::RevisionConflict { current } => {
                write!(formatter, "artifact is at revision {}", current.0)
            }
            Self::RequestKeyReused => {
                formatter.write_str("request key was already used for a different request")
            }
            Self::Unavailable(reason) => write!(formatter, "storage unavailable: {reason}"),
        }
    }
}

impl std::error::Error for StoreError {}

/// Durable artifact storage.
///
/// Reads of a revision are stable: a committed revision never changes. Methods
/// do not check authorization; the application core does.
pub trait ArtifactStore {
    /// Creates an artifact at revision 1, or returns the stored receipt when
    /// the actor already committed the same request key and fingerprint.
    ///
    /// # Errors
    ///
    /// [`StoreError::RequestKeyReused`] when the key was used with another
    /// fingerprint, or [`StoreError::Unavailable`].
    fn create_artifact(&self, request: NewArtifact<'_>) -> Result<CommitReceipt, StoreError>;

    /// Appends the next revision if the artifact is still at `base`, or returns
    /// the stored receipt of an identical earlier request.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`], [`StoreError::RevisionConflict`],
    /// [`StoreError::RequestKeyReused`], or [`StoreError::Unavailable`].
    fn commit_revision(&self, request: RevisionCommit<'_>) -> Result<CommitReceipt, StoreError>;

    /// Metadata of one artifact.
    ///
    /// # Errors
    ///
    /// [`StoreError::Unavailable`].
    fn artifact(&self, id: &ArtifactId) -> Result<Option<ArtifactRecord>, StoreError>;

    /// Artifacts owned by an actor, most recently updated first.
    ///
    /// # Errors
    ///
    /// [`StoreError::Unavailable`].
    fn artifacts_owned_by(&self, owner: &ActorId) -> Result<Vec<ArtifactRecord>, StoreError>;

    /// Revision metadata, newest first.
    ///
    /// # Errors
    ///
    /// [`StoreError::Unavailable`].
    fn revisions(&self, id: &ArtifactId) -> Result<Vec<RevisionRecord>, StoreError>;

    /// The complete source of one revision.
    ///
    /// # Errors
    ///
    /// [`StoreError::Unavailable`], including when stored files no longer
    /// form a valid package.
    fn revision_package(
        &self,
        id: &ArtifactId,
        revision: SourceRevision,
    ) -> Result<Option<FilePackage>, StoreError>;

    /// One file of one revision.
    ///
    /// # Errors
    ///
    /// [`StoreError::Unavailable`].
    fn revision_file(
        &self,
        id: &ArtifactId,
        revision: SourceRevision,
        path: &PackagePath,
    ) -> Result<Option<Vec<u8>>, StoreError>;
}
