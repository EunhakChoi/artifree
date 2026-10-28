//! Durable storage for artifact metadata, immutable source revisions, and
//! request-key results.
//!
//! An implementation must make each commit atomic: the conditional revision
//! check, revision files, metadata update, request-key record, and change
//! feed entry either all become durable or none do. A returned commit is
//! durable. The same holds for a metadata update and its conditional check.

use std::fmt;

use artifree_domain::change::{ChangeSequence, FeedId};
use artifree_domain::identity::{ActorId, ArtifactId, RequestKey, Title, UnixMillis};
use artifree_domain::package::{FilePackage, PackagePath};
use artifree_domain::{ArtifactKind, MetadataRevision, SourceRevision};

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
    /// Whether the owner pinned the artifact.
    pub pinned: bool,
    /// Latest committed source revision.
    pub current_revision: SourceRevision,
    /// Latest metadata revision: the version of the title, pin, and trash
    /// state.
    pub metadata_revision: MetadataRevision,
    /// When the artifact was moved to the trash, or `None` outside it.
    pub deleted_at: Option<UnixMillis>,
    /// Creation time.
    pub created_at: UnixMillis,
    /// Time of the latest committed source revision; metadata updates do
    /// not change it.
    pub updated_at: UnixMillis,
}

/// A file listed in a revision manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// Package path.
    pub path: PackagePath,
    /// Content size in bytes.
    pub size: u64,
    /// SHA-256 digest of the content. Equal digests mean equal content.
    pub sha256: [u8; 32],
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
    /// The artifact a copy is made from. The create is refused as
    /// [`StoreError::NotFound`] unless it is outside the trash when the
    /// create commits.
    pub source: Option<&'a ArtifactId>,
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

/// A request to change an artifact's title, pin, or trash state when its
/// metadata is still at `base`. A field that is `None` stays as it is.
#[derive(Debug, Clone, Copy)]
pub struct MetadataUpdate<'a> {
    /// Retry identity of the updating actor.
    pub request: RequestIdentity<'a>,
    /// Artifact to change.
    pub artifact: &'a ArtifactId,
    /// Metadata revision the change was prepared against.
    pub base: MetadataRevision,
    /// New title.
    pub title: Option<&'a Title>,
    /// New pin.
    pub pinned: Option<bool>,
    /// Whether the artifact is in the trash.
    pub deleted: Option<bool>,
}

impl MetadataUpdate<'_> {
    /// Whether the update takes the artifact out of the trash, the only
    /// change an artifact in the trash accepts.
    #[must_use]
    pub fn recovers(&self) -> bool {
        self.deleted == Some(false)
    }
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

/// The durable result of a metadata update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataReceipt {
    /// Artifact metadata as of the update the request identifies. An update
    /// that changed nothing reports the metadata it found.
    pub artifact: ArtifactRecord,
    /// Whether this call handled the request or replayed an earlier one.
    pub delivery: Delivery,
}

/// The order of an artifact list. Ties are broken by identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListOrder {
    /// The most recently updated source first.
    Updated,
    /// The most recently created first.
    Created,
}

/// Where a list page continues: after the artifact with this sort time and
/// identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListPosition {
    /// The sort time of the last artifact already listed.
    pub at: UnixMillis,
    /// Its identifier.
    pub id: ArtifactId,
}

/// One page of an owner's artifacts.
#[derive(Debug, Clone, Copy)]
pub struct ArtifactListing<'a> {
    /// The owner whose artifacts are listed.
    pub owner: &'a ActorId,
    /// Only titles containing this text, ignoring ASCII case.
    pub search: Option<&'a str>,
    /// Only artifacts with this pin.
    pub pinned: Option<bool>,
    /// List the trash instead of the artifacts outside it.
    pub deleted: bool,
    /// The order.
    pub order: ListOrder,
    /// Continue after this position.
    pub after: Option<&'a ListPosition>,
    /// At most this many artifacts.
    pub limit: usize,
}

/// What a commit did to its artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChangeKind {
    /// The artifact was created at revision 1.
    Created,
    /// A revision was appended.
    Revised,
    /// The title or pin changed; no source revision was written.
    MetadataUpdated,
    /// The artifact was moved to the trash.
    Deleted,
    /// The artifact was taken out of the trash.
    Recovered,
}

/// One commit as recorded in the change feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeRecord {
    /// Position of the commit in the feed.
    pub sequence: ChangeSequence,
    /// The changed artifact.
    pub artifact: ArtifactId,
    /// What the commit did.
    pub kind: ChangeKind,
    /// The source revision the commit created, or that was current when the
    /// metadata changed.
    pub revision: SourceRevision,
    /// The metadata revision after the commit.
    pub metadata_revision: MetadataRevision,
    /// Commit time.
    pub committed_at: UnixMillis,
}

/// Part of an owner's changes and the feed's state, read at one point in
/// time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangePage {
    /// The feed's identity.
    pub feed: FeedId,
    /// The owner's changes after the requested position, in feed order.
    pub changes: Vec<ChangeRecord>,
    /// The newest position in the feed, across all owners.
    pub head: ChangeSequence,
}

/// A storage failure or a rejected conditional write.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum StoreError {
    /// The artifact does not exist, or the request reached it in the trash.
    NotFound,
    /// The artifact's current revision is not the requested base.
    RevisionConflict {
        /// The revision that is current now.
        current: SourceRevision,
    },
    /// The artifact's current metadata revision is not the requested base.
    MetadataConflict {
        /// The metadata revision that is current now.
        current: MetadataRevision,
    },
    /// The actor already used the key for a request with different content.
    RequestKeyReused,
    /// The files are those of the base revision; nothing was written.
    Unchanged,
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
            Self::MetadataConflict { current } => {
                write!(formatter, "artifact metadata is at revision {}", current.0)
            }
            Self::RequestKeyReused => {
                formatter.write_str("request key was already used for a different request")
            }
            Self::Unchanged => formatter.write_str("the files are those of the base revision"),
            Self::Unavailable(reason) => write!(formatter, "storage unavailable: {reason}"),
        }
    }
}

impl std::error::Error for StoreError {}

/// Durable artifact storage.
///
/// Reads of a revision are stable: a committed revision never changes. Methods
/// do not check authorization; the application core does.
///
/// A write to an artifact in the trash is answered with the replay of the
/// same request or [`StoreError::NotFound`], before any other check, unless
/// it recovers the artifact (decision 0013). Reads return artifacts in the
/// trash as stored; the core decides who may see them.
pub trait ArtifactStore {
    /// Creates an artifact at revision 1, or returns the stored receipt when
    /// the actor already committed the same request key and fingerprint.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] when the copy's source is missing or in the
    /// trash, [`StoreError::RequestKeyReused`] when the key was used with
    /// another fingerprint, or [`StoreError::Unavailable`].
    fn create_artifact(&self, request: NewArtifact<'_>) -> Result<CommitReceipt, StoreError>;

    /// Appends the next revision if the artifact is still at `base` and the
    /// package differs from it, or returns the stored receipt of an identical
    /// earlier request. All three are decided in one atomic step.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`], [`StoreError::RevisionConflict`],
    /// [`StoreError::Unchanged`], [`StoreError::RequestKeyReused`], or
    /// [`StoreError::Unavailable`].
    fn commit_revision(&self, request: RevisionCommit<'_>) -> Result<CommitReceipt, StoreError>;

    /// Changes the title, pin, or trash state if the metadata is still at
    /// `base`, or returns the stored receipt of an identical earlier request.
    /// A change to the values already stored writes no metadata revision and
    /// no change feed entry, but its key is recorded.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`], [`StoreError::MetadataConflict`],
    /// [`StoreError::RequestKeyReused`], or [`StoreError::Unavailable`].
    fn update_metadata(&self, request: MetadataUpdate<'_>) -> Result<MetadataReceipt, StoreError>;

    /// Metadata of one artifact.
    ///
    /// # Errors
    ///
    /// [`StoreError::Unavailable`].
    fn artifact(&self, id: &ArtifactId) -> Result<Option<ArtifactRecord>, StoreError>;

    /// Up to `listing.limit` of an owner's artifacts that match the listing,
    /// in its order, after its position: those in the trash or those outside
    /// it.
    ///
    /// # Errors
    ///
    /// [`StoreError::Unavailable`].
    fn list_artifacts(
        &self,
        listing: ArtifactListing<'_>,
    ) -> Result<Vec<ArtifactRecord>, StoreError>;

    /// Revision metadata, newest first.
    ///
    /// # Errors
    ///
    /// [`StoreError::Unavailable`].
    fn revisions(&self, id: &ArtifactId) -> Result<Vec<RevisionRecord>, StoreError>;

    /// Metadata of one revision, as [`Self::revisions`] lists it, read
    /// without the others.
    ///
    /// # Errors
    ///
    /// [`StoreError::Unavailable`].
    fn revision(
        &self,
        id: &ArtifactId,
        revision: SourceRevision,
    ) -> Result<Option<RevisionRecord>, StoreError>;

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

    /// Up to `limit` changes to artifacts `owner` owns, after `after`.
    ///
    /// A commit's change becomes visible with the commit, and changes become
    /// visible in sequence order: once a reader has seen a sequence, no
    /// smaller one appears later. When fewer than `limit` changes are
    /// returned, the owner has none between them and `head`.
    ///
    /// # Errors
    ///
    /// [`StoreError::Unavailable`].
    fn changes(
        &self,
        owner: &ActorId,
        after: ChangeSequence,
        limit: usize,
    ) -> Result<ChangePage, StoreError>;
}
