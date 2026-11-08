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
use artifree_domain::package::{FileChange, FilePackage, PackageError, PackagePath};
use artifree_domain::{AppDataVersion, ArtifactKind, MetadataRevision, SourceRevision};

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
    pub source: Option<CopySource<'a>>,
}

/// The artifact a copy is made from, and what besides its files comes along.
#[derive(Debug, Clone, Copy)]
pub struct CopySource<'a> {
    /// The source artifact.
    pub artifact: &'a ArtifactId,
    /// Whether the source's application data (decision 0031) is copied into
    /// the new artifact's own data space, for the actor making the copy, in
    /// the same transaction as the create. The copy's data starts at version 1
    /// when the source's space has any member, and is not written otherwise.
    pub copy_data: bool,
    /// Whether the actor's entire current asset space of the source
    /// (decision 0045) is copied into the new artifact's own asset space, in
    /// the same transaction as the create. Associations keep their asset IDs,
    /// types, sizes, digests and creation times, and are charged in full to
    /// the copy's space and the actor's quota; the create is refused as
    /// [`StoreError::AssetLimitExceeded`] when they do not fit. The copy's
    /// asset version starts at 1 when anything is copied, and is 0 otherwise.
    /// Other actors' spaces and upload receipts are never copied.
    pub copy_assets: bool,
}

/// What a file must be like in the artifact's current revision for an edit
/// of it to apply (decision 0035).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileExpectation {
    /// The path is not a file of the current revision.
    Absent,
    /// The path is a file of the current revision with this SHA-256 digest.
    Sha256([u8; 32]),
}

/// One change to a file and the condition on that file alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEdit {
    /// The change; it names the path.
    pub change: FileChange,
    /// What the file at that path must be in the current revision.
    pub expect: FileExpectation,
}

/// The upper bound on the mismatches a [`StoreError::FilesChanged`] lists.
pub const MAX_LISTED_MISMATCHES: usize = 32;

/// A file whose current state does not meet the expectation of its edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMismatch {
    /// The edited path.
    pub path: PackagePath,
    /// The digest of the file in the current revision, or `None` when the
    /// path is not a file of it.
    pub current: Option<[u8; 32]>,
}

/// The mismatches of `edits` against the files of the current revision, in
/// path order; empty when every expectation holds. A path of `edits` that
/// repeats is judged once per edit.
#[must_use]
pub fn unmet_expectations(edits: &[FileEdit], files: &[FileEntry]) -> Vec<FileMismatch> {
    let mut unmet: Vec<FileMismatch> = edits
        .iter()
        .filter_map(|edit| {
            let path = edit.change.path();
            let current = files
                .iter()
                .find(|entry| &entry.path == path)
                .map(|entry| entry.sha256);
            let holds = match (edit.expect, current) {
                (FileExpectation::Absent, None) => true,
                (FileExpectation::Sha256(expected), Some(actual)) => expected == actual,
                _ => false,
            };
            (!holds).then(|| FileMismatch {
                path: path.clone(),
                current,
            })
        })
        .collect();
    unmet.sort_by(|left, right| left.path.cmp(&right.path));
    unmet
}

/// A request to append a revision that changes some files, when each of them
/// is in the state its edit expects. Files that no edit names are carried
/// over from the current revision, whatever revision that is.
#[derive(Debug, Clone, Copy)]
pub struct ConditionalCommit<'a> {
    /// Retry identity of the committing actor.
    pub request: RequestIdentity<'a>,
    /// Artifact to change.
    pub artifact: &'a ArtifactId,
    /// The edits, each to a different path; at least one.
    pub edits: &'a [FileEdit],
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

/// A request to remove an artifact in the trash for good (decision 0029).
#[derive(Debug, Clone, Copy)]
pub struct PurgeRequest<'a> {
    /// Retry identity of the purging actor.
    pub request: RequestIdentity<'a>,
    /// Artifact to remove. It must be in the trash.
    pub artifact: &'a ArtifactId,
    /// Metadata revision the purge was prepared against: the trash state the
    /// caller saw.
    pub base: MetadataRevision,
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

/// The durable result of a purge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurgeReceipt {
    /// The removed artifact. The identifier is never used again.
    pub artifact: ArtifactId,
    /// The metadata revision the purge itself recorded.
    pub metadata_revision: MetadataRevision,
    /// When the artifact was removed.
    pub purged_at: UnixMillis,
    /// Whether this call removed it or replayed an earlier request.
    pub delivery: Delivery,
}

/// The identity of a set of unreferenced contents: SHA-256 over the sorted
/// digests of its members, under a fixed prefix. A cleanup is conditional on
/// the set the caller reviewed (decision 0029).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ContentSetDigest(pub [u8; 32]);

impl fmt::Debug for ContentSetDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ContentSetDigest({self})")
    }
}

impl fmt::Display for ContentSetDigest {
    /// 64 lowercase hexadecimal digits.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0
            .iter()
            .try_for_each(|byte| write!(formatter, "{byte:02x}"))
    }
}

impl ContentSetDigest {
    /// The digest written by [`fmt::Display`]: exactly 64 lowercase
    /// hexadecimal digits, nothing else.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        parse_sha256(text).map(Self)
    }
}

/// A SHA-256 digest written as exactly 64 lowercase hexadecimal digits,
/// nothing else.
#[must_use]
pub fn parse_sha256(text: &str) -> Option<[u8; 32]> {
    let bytes = text.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let mut digest = [0_u8; 32];
    for (slot, pair) in digest.iter_mut().zip(bytes.chunks_exact(2)) {
        let high = lowercase_hex(pair[0])?;
        let low = lowercase_hex(pair[1])?;
        *slot = (high << 4) | low;
    }
    Some(digest)
}

fn lowercase_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

/// File content that no revision file of any artifact refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnreferencedContent {
    /// How many distinct contents.
    pub blobs: u64,
    /// Their total size in bytes.
    pub bytes: u64,
    /// The identity of this set, to give back as the base of the cleanup.
    pub digest: ContentSetDigest,
}

/// A cleanup of unreferenced content: conditional on the set the caller
/// reviewed, and retryable by key.
#[derive(Debug, Clone, Copy)]
pub struct CleanupRequest<'a> {
    /// The actor, key, and fingerprint of the request.
    pub request: RequestIdentity<'a>,
    /// The set the caller reviewed; the cleanup deletes only this set.
    pub base: ContentSetDigest,
}

/// The durable result of a cleanup: what its first execution deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CleanupReceipt {
    /// How many distinct contents were deleted.
    pub blobs: u64,
    /// Their total size in bytes.
    pub bytes: u64,
    /// Whether this call deleted them or replayed an earlier request.
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
    /// The artifact was removed from the trash for good. Reading it is not
    /// found, as in the trash, and it never returns.
    Purged,
    /// A data write changed the space of the actor whose feed holds the row
    /// (decision 0040). The artifact's own source and metadata did not change.
    DataChanged,
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
    /// metadata or the data changed.
    pub revision: SourceRevision,
    /// The metadata revision after the commit, or the one that was current
    /// when the data changed.
    pub metadata_revision: MetadataRevision,
    /// The version the write left the data space at: present exactly for
    /// [`ChangeKind::DataChanged`], never in `revision`.
    pub data_version: Option<AppDataVersion>,
    /// Commit time.
    pub committed_at: UnixMillis,
}

/// Part of an owner's changes and the owner's feed state, read at one point in
/// time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangePage {
    /// The feed's identity.
    pub feed: FeedId,
    /// The owner's changes after the requested position, in feed order.
    pub changes: Vec<ChangeRecord>,
    /// The newest position in the owner's feed. Each owner numbers its own
    /// changes (decision 0036), so the head says nothing about other owners.
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
    /// The artifact exists but is not in the trash, so it cannot be purged.
    NotInTrash,
    /// The unreferenced content is not the set the caller reviewed.
    ContentChanged,
    /// The actor already used the key for a request with different content.
    RequestKeyReused,
    /// The files are those of the base revision; nothing was written.
    Unchanged,
    /// An edit's file is not in the state it expects; nothing was written.
    FilesChanged {
        /// The revision that is current now.
        current: SourceRevision,
        /// The first mismatches in path order, at most
        /// [`MAX_LISTED_MISMATCHES`].
        mismatches: Vec<FileMismatch>,
        /// How many edits mismatch, listed or not.
        total: usize,
    },
    /// The edits do not make a valid package; nothing was written.
    Package(PackageError),
    /// A copy's assets would exceed a space or actor asset limit (decision
    /// 0045); nothing was written and no key was recorded.
    AssetLimitExceeded,
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
            Self::NotInTrash => formatter.write_str("artifact is not in the trash"),
            Self::ContentChanged => {
                formatter.write_str("the unreferenced content is not the set that was reviewed")
            }
            Self::RequestKeyReused => {
                formatter.write_str("request key was already used for a different request")
            }
            Self::Unchanged => formatter.write_str("the files are those of the base revision"),
            Self::FilesChanged { current, total, .. } => write!(
                formatter,
                "{total} edited files are not as expected in revision {}",
                current.0
            ),
            Self::Package(error) => write!(formatter, "invalid package: {error}"),
            Self::AssetLimitExceeded => formatter.write_str("asset limit exceeded"),
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
///
/// A purged artifact (decision 0029) is gone: reads find nothing, a write
/// that names it is [`StoreError::NotFound`] even for a retry of an earlier
/// request, and only the retry of the purge itself replays. Its request keys
/// stay reserved.
pub trait ArtifactStore {
    /// Creates an artifact at revision 1, or returns the stored receipt when
    /// the actor already committed the same request key and fingerprint.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] when the copy's source is missing or in the
    /// trash, [`StoreError::AssetLimitExceeded`] when the copied assets do not
    /// fit, [`StoreError::RequestKeyReused`] when the key was used with
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

    /// Appends the next revision with the edits applied to the files of the
    /// current revision if every edit's file is in the state it expects and
    /// the result differs from the current revision, or returns the stored
    /// receipt of an identical earlier request (decision 0035). The request
    /// key, the expectations, and the commit are decided in one atomic step
    /// against one current revision, so no writer can land between them. The
    /// key of a request that fails is not recorded.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`], [`StoreError::FilesChanged`],
    /// [`StoreError::Package`], [`StoreError::Unchanged`],
    /// [`StoreError::RequestKeyReused`], or [`StoreError::Unavailable`].
    fn commit_edits(&self, request: ConditionalCommit<'_>) -> Result<CommitReceipt, StoreError>;

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

    /// The receipt of an earlier create with this retry identity, without
    /// writing anything; `None` when there is none or its artifact was purged.
    /// It lets a copy's retry replay although its source is gone.
    ///
    /// # Errors
    ///
    /// [`StoreError::Unavailable`].
    fn replay_create(
        &self,
        request: RequestIdentity<'_>,
    ) -> Result<Option<CommitReceipt>, StoreError>;

    /// Removes an artifact in the trash for good if its metadata is still at
    /// `base`, or returns the stored receipt of an identical earlier purge.
    /// Its files, titles, and listing of files are deleted, and so is content
    /// no other revision file refers to. Revision and metadata headers, request
    /// keys, and change feed entries remain, and the identifier is never
    /// reused.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] for a missing or purged artifact,
    /// [`StoreError::NotInTrash`], [`StoreError::MetadataConflict`],
    /// [`StoreError::RequestKeyReused`], or [`StoreError::Unavailable`].
    fn purge_artifact(&self, request: PurgeRequest<'_>) -> Result<PurgeReceipt, StoreError>;

    /// The content that no revision file refers to, counted in one snapshot.
    ///
    /// # Errors
    ///
    /// [`StoreError::Unavailable`].
    fn unreferenced_content(&self) -> Result<UnreferencedContent, StoreError>;

    /// Deletes the content that no revision file refers to, if it is the set
    /// at `base`, and records what was deleted with the request key; or
    /// returns that record for a retry of the same request. What is deleted is
    /// decided in the write transaction, so a commit that refers to content
    /// either finishes first, which keeps it, or starts after, which stores it
    /// again. The key shares its space with the keys of artifact requests.
    ///
    /// # Errors
    ///
    /// [`StoreError::ContentChanged`] when the set differs from `base`,
    /// [`StoreError::RequestKeyReused`], or [`StoreError::Unavailable`].
    fn remove_unreferenced_content(
        &self,
        request: CleanupRequest<'_>,
    ) -> Result<CleanupReceipt, StoreError>;

    /// Metadata of one artifact. A purged artifact is `None`.
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
    /// Positions are the owner's own (decision 0036). A commit's change becomes
    /// visible with the commit, and changes become visible in sequence order: once a reader has seen a sequence, no
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

#[cfg(test)]
mod tests {
    use super::*;

    fn path(name: &str) -> PackagePath {
        let Ok(path) = name.parse() else {
            panic!("{name} is not a package path");
        };
        path
    }

    fn entry(name: &str, digest: u8) -> FileEntry {
        FileEntry {
            path: path(name),
            size: 1,
            sha256: [digest; 32],
        }
    }

    fn put(name: &str, expect: FileExpectation) -> FileEdit {
        FileEdit {
            change: FileChange::Put {
                path: path(name),
                content: Vec::new(),
            },
            expect,
        }
    }

    #[test]
    fn each_expectation_is_judged_against_its_own_file() {
        let files = [entry("a.txt", 1), entry("b.txt", 2)];
        let edits = [
            put("d.txt", FileExpectation::Sha256([3; 32])),
            put("b.txt", FileExpectation::Sha256([9; 32])),
            put("a.txt", FileExpectation::Sha256([1; 32])),
            put("c.txt", FileExpectation::Absent),
        ];
        assert_eq!(
            unmet_expectations(&edits, &files),
            [
                FileMismatch {
                    path: path("b.txt"),
                    current: Some([2; 32]),
                },
                FileMismatch {
                    path: path("d.txt"),
                    current: None,
                },
            ],
            "a held, c is absent as expected; b differs, d is missing; path order, not request order"
        );
        let taken = [put("a.txt", FileExpectation::Absent)];
        assert_eq!(
            unmet_expectations(&taken, &files),
            [FileMismatch {
                path: path("a.txt"),
                current: Some([1; 32]),
            }],
            "a create of an existing path"
        );
    }

    #[test]
    fn a_digest_is_exactly_64_lowercase_hexadecimal_digits() {
        let text = "0f".repeat(32);
        assert_eq!(parse_sha256(&text), Some([0x0f; 32]));
        for bad in [
            "0F".repeat(32),
            "0f".repeat(31),
            format!("{text}0"),
            "0g".repeat(32),
            String::new(),
            " ".repeat(64),
        ] {
            assert_eq!(parse_sha256(&bad), None, "{bad:?}");
        }
    }
}
