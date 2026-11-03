//! HTML file-package artifacts: create, conditional revision commits and
//! restores, duplicates, conditional metadata updates, lists, and
//! revision-pinned reads.
//!
//! Every operation checks the actor's authority against current stored
//! metadata. An artifact in the trash is missing to every request but its
//! recovery and the replay of a request made before the delete (decision
//! 0013). A purged artifact (decision 0029) is missing to every request,
//! the replay of an earlier one included, except the retry of its purge.
//! Commit success means durable acceptance. Source history is kept
//! separate from application data (decision 0031, [`crate::data`]) and access
//! policy. A duplicate copies files, and the data space only when asked;
//! artifacts have no uploaded assets yet. Publishing is not implemented.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use artifree_domain::AppDataVersion;
use artifree_domain::change::ChangeSequence;
use artifree_domain::data::DataError;
use artifree_domain::identity::{ActorId, ArtifactId, RequestKey, Title};
use artifree_domain::job::JobState;
use artifree_domain::package::{FileChange, FilePackage, PackageError, PackagePath};
use artifree_domain::{ArtifactKind, MetadataRevision, SourceRevision};
use artifree_ports::artifact_store::{
    ArtifactListing, ArtifactRecord, ArtifactStore, ChangePage, CleanupReceipt, CleanupRequest,
    CommitReceipt, ConditionalCommit, ContentSetDigest, CopySource, Delivery, FileEdit, FileEntry,
    FileExpectation, FileMismatch, MetadataReceipt, MetadataUpdate, NewArtifact, PurgeReceipt,
    PurgeRequest, RequestFingerprint, RequestIdentity, RevisionCommit, RevisionRecord, StoreError,
    UnreferencedContent,
};
use sha2::{Digest, Sha256};

use crate::listing::{self, ArtifactPage, ListQuery};
use crate::parts::{self, Boundary, ByteRange, FilePart, RangeError};
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
    /// The job does not exist, or its artifact is not accessible.
    Job,
}

/// What a source export holds; see [`ArtifactService::export`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionExport {
    /// The artifact when exported. Its title is not part of the revision.
    pub artifact: ArtifactRecord,
    /// The exported revision, with its files listed in path order.
    pub revision: RevisionRecord,
    /// The revision's files.
    pub files: FilePackage,
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
    /// An edited file is not in the state its edit expects; nothing was
    /// written (decision 0035). The strict [`Self::RevisionConflict`] is a
    /// different answer to a different command.
    FilesChanged {
        /// The current revision.
        current: SourceRevision,
        /// The first mismatches in path order, a bounded number of them.
        files: Vec<FileMismatch>,
        /// How many edits mismatch, listed or not.
        total: usize,
    },
    /// The edit committed but the stored revision is not what was asked for.
    /// The revision exists; the history shows it (decision 0035).
    EditNotAsRequested,
    /// The edit committed but the store could not be read to check it. The
    /// revision exists; a retry with the same key returns its receipt
    /// (decision 0035).
    EditUnverified,
    /// The metadata change was prepared against an older metadata revision;
    /// nothing was written.
    MetadataConflict {
        /// The current metadata revision.
        current: MetadataRevision,
    },
    /// The page token is malformed or belongs to another order or filters.
    InvalidPageToken,
    /// The range cannot be read from the file; see [`RangeError`].
    InvalidRange(RangeError),
    /// The artifact is not in the trash, so it cannot be purged.
    NotInTrash,
    /// The unreferenced content is not the set the caller reviewed; run the
    /// dry run again.
    ContentChanged,
    /// The actor already used this request key for different content.
    RequestKeyReused,
    /// The job is already succeeded, failed, or cancelled, so it cannot be
    /// cancelled (decision 0030).
    JobEnded {
        /// The state the job is in.
        state: JobState,
    },
    /// The actor already has the largest number of jobs waiting or running
    /// (decision 0030).
    TooManyJobs,
    /// The application data was prepared against an older version; nothing
    /// was written (decision 0031).
    DataConflict {
        /// The version the data is at.
        current: AppDataVersion,
    },
    /// A data path, value, or change breaks the rules of a data space
    /// (decision 0031).
    Data(DataError),
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
            Self::NotFound(Missing::Job) => formatter.write_str("job not found"),
            Self::RevisionConflict { current } => write!(
                formatter,
                "artifact changed; current revision is {}",
                current.0
            ),
            Self::FilesChanged { current, total, .. } => write!(
                formatter,
                "{total} edited files are not as expected; current revision is {}",
                current.0
            ),
            Self::EditNotAsRequested => formatter.write_str(
                "the edit was saved but the stored revision is not what was asked for; check the history",
            ),
            Self::EditUnverified => formatter.write_str(
                "the edit was saved but could not be checked; retry with the same key to get its receipt",
            ),
            Self::MetadataConflict { current } => write!(
                formatter,
                "artifact metadata changed; current metadata revision is {}",
                current.0
            ),
            Self::InvalidPageToken => formatter
                .write_str("page token is malformed or belongs to another order or filters"),
            Self::InvalidRange(error) => write!(formatter, "{error}"),
            Self::NotInTrash => formatter.write_str("artifact is not in the trash"),
            Self::ContentChanged => {
                formatter.write_str("the unreferenced content is not the set that was reviewed")
            }
            Self::RequestKeyReused => {
                formatter.write_str("request key was already used for a different request")
            }
            Self::JobEnded { state } => {
                write!(formatter, "the job is already {}", state.as_str())
            }
            Self::TooManyJobs => formatter
                .write_str("too many jobs are waiting or running; wait for one or cancel one"),
            Self::DataConflict { current } => write!(
                formatter,
                "the data changed; the current data version is {}",
                current.0
            ),
            Self::Data(error) => write!(formatter, "{error}"),
            Self::Unavailable(reason) => write!(formatter, "storage unavailable: {reason}"),
        }
    }
}

impl ArtifactError {
    /// Whether a write that failed with this error had already committed a
    /// revision, so that followers of the change feed must still learn of it.
    /// Only an edit that could not be verified after its commit does.
    #[must_use]
    pub fn committed_a_revision(&self) -> bool {
        matches!(self, Self::EditNotAsRequested | Self::EditUnverified)
    }
}

impl std::error::Error for ArtifactError {}

impl From<StoreError> for ArtifactError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::NotFound => Self::NotFound(Missing::Artifact),
            StoreError::RevisionConflict { current } => Self::RevisionConflict { current },
            StoreError::FilesChanged {
                current,
                mismatches,
                total,
            } => Self::FilesChanged {
                current,
                files: mismatches,
                total,
            },
            StoreError::Package(error) => Self::InvalidPackage(error),
            StoreError::MetadataConflict { current } => Self::MetadataConflict { current },
            StoreError::NotInTrash => Self::NotInTrash,
            StoreError::ContentChanged => Self::ContentChanged,
            StoreError::RequestKeyReused => Self::RequestKeyReused,
            StoreError::Unchanged => Self::InvalidPackage(PackageError::Unchanged),
            StoreError::Unavailable(reason) => Self::Unavailable(reason),
            other => Self::Unavailable(other.to_string()),
        }
    }
}

/// A change to an artifact's title, pin, trash state, or several of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataChange {
    title: Option<Title>,
    pinned: Option<bool>,
    deleted: Option<bool>,
}

impl MetadataChange {
    /// A change of the given fields, or `None` when it names none. `deleted`
    /// moves the artifact to the trash (`true`) or out of it (`false`).
    #[must_use]
    pub fn new(title: Option<Title>, pinned: Option<bool>, deleted: Option<bool>) -> Option<Self> {
        (title.is_some() || pinned.is_some() || deleted.is_some()).then_some(Self {
            title,
            pinned,
            deleted,
        })
    }
}

/// Application commands for file-package artifacts. HTTP, MCP, and CLI
/// adapters call these methods instead of the store.
#[derive(Debug)]
pub struct ArtifactService<S> {
    pub(crate) store: S,
}

impl<S: ArtifactStore> ArtifactService<S> {
    /// Uses `store` for all durable state.
    pub fn new(store: S) -> Self {
        Self { store }
    }

    /// Creates an app artifact owned by `actor` at revision 1.
    ///
    /// Retrying with the same key and content returns the original receipt,
    /// unless the artifact it created was purged since (decision 0029).
    ///
    /// # Errors
    ///
    /// [`ArtifactError::NotFound`] for the retry of a create whose artifact
    /// was purged, [`ArtifactError::RequestKeyReused`], or
    /// [`ArtifactError::Unavailable`].
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
            source: None,
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
        let record = self.owned(actor, id)?;
        let mut hasher = Fingerprint::new("save");
        hasher.field(id.as_str().as_bytes());
        hasher.field(&base.0.to_be_bytes());
        // Hash in path order: a retry that lists the same changes in another
        // order is the same request. Paths must be distinct, so the order is
        // total for every change set that can commit.
        let mut ordered: Vec<&FileChange> = changes.iter().collect();
        ordered.sort_by(|left, right| left.path().cmp(right.path()));
        for change in ordered {
            match change {
                FileChange::Put { path, content } => hasher.put(path, content),
                FileChange::Delete { path } => {
                    hasher.field(b"delete");
                    hasher.field(path.as_str().as_bytes());
                }
            }
        }
        let fingerprint = hasher.finish();
        let package = checked(&record, self.saved_package(&record, base, changes))?;
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

    /// Commits the files of an earlier `revision` as the next revision, only
    /// if `base` is still current. Every revision stays as it was, and the
    /// title and pin are not touched.
    ///
    /// Retrying with the same key returns the original receipt even after
    /// later revisions were committed.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::NotFound`] for a missing or inaccessible artifact or
    /// revision, [`ArtifactError::RevisionConflict`] when `base` is not
    /// current, [`ArtifactError::InvalidPackage`] when the current revision
    /// `base` already has those files, [`ArtifactError::RequestKeyReused`], or
    /// [`ArtifactError::Unavailable`].
    pub fn restore(
        &self,
        actor: &ActorId,
        key: &RequestKey,
        id: &ArtifactId,
        base: SourceRevision,
        revision: SourceRevision,
    ) -> Result<CommitReceipt, ArtifactError> {
        let record = self.owned(actor, id)?;
        let mut hasher = Fingerprint::new("restore");
        hasher.field(id.as_str().as_bytes());
        hasher.field(&base.0.to_be_bytes());
        hasher.field(&revision.0.to_be_bytes());
        // The store decides in one step whether this is a replay, a stale
        // base, or files the base already has.
        let package = checked(&record, self.restored_package(&record, base, revision))?;
        let receipt = self.store.commit_revision(RevisionCommit {
            request: RequestIdentity {
                actor,
                key,
                fingerprint: hasher.finish(),
            },
            artifact: id,
            base,
            package: &package,
        })?;
        Ok(receipt)
    }

    /// Commits `edits` as the next revision, each applied to the file of the
    /// **current** revision that its expectation describes, and nothing
    /// otherwise (decision 0035). The strict commands, [`Self::save`] and
    /// [`Self::restore`], are not changed by this one.
    ///
    /// `base` is the revision the caller read the files from. It is not a
    /// condition: a head that moved since is no conflict by itself, and the
    /// files that no edit names are carried over from the current revision
    /// even if they changed meanwhile. It must be a revision the artifact has,
    /// and it is part of the request's identity. An edit applies only if its
    /// own file is as expected, and one that is not rejects all of them.
    ///
    /// Retrying with the same key and content returns the original receipt
    /// even after later revisions were committed. A rejected request does not
    /// use its key. After a new commit, the stored revision is read back and
    /// compared with the edits.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::NotFound`] for a missing or inaccessible artifact or a
    /// `base` it has no revision for, [`ArtifactError::InvalidPackage`] for no
    /// edit, a path edited twice, a result that is no valid package, or one
    /// that equals the current revision, [`ArtifactError::FilesChanged`],
    /// [`ArtifactError::RequestKeyReused`],
    /// [`ArtifactError::EditNotAsRequested`] or
    /// [`ArtifactError::EditUnverified`] (the revision exists, see
    /// [`ArtifactError::committed_a_revision`]), or
    /// [`ArtifactError::Unavailable`].
    pub fn edit_files(
        &self,
        actor: &ActorId,
        key: &RequestKey,
        id: &ArtifactId,
        base: SourceRevision,
        edits: &[FileEdit],
    ) -> Result<CommitReceipt, ArtifactError> {
        let record = self.owned(actor, id)?;
        let mut hasher = Fingerprint::new("edit_files");
        hasher.field(id.as_str().as_bytes());
        hasher.field(&base.0.to_be_bytes());
        // Paths are distinct, so path order is total: a retry that lists the
        // same edits in another order is the same request.
        let mut ordered: Vec<&FileEdit> = edits.iter().collect();
        ordered.sort_by(|left, right| left.change.path().cmp(right.change.path()));
        for edit in ordered {
            hasher.edit(edit);
        }
        let fingerprint = hasher.finish();
        checked(&record, edits_are_possible(&record, base, edits))?;
        let receipt = self.store.commit_edits(ConditionalCommit {
            request: RequestIdentity {
                actor,
                key,
                fingerprint,
            },
            artifact: id,
            edits,
        })?;
        if receipt.delivery == Delivery::Committed {
            self.read_back(id, &receipt.revision, edits)
                .map_err(|error| match error {
                    ArtifactError::Unavailable(_) => ArtifactError::EditUnverified,
                    other => other,
                })?;
        }
        Ok(receipt)
    }

    /// Reads the revision `committed` created and compares it with `edits`:
    /// the previous revision's files, changed only as the edits say, and the
    /// bytes of every file put.
    fn read_back(
        &self,
        id: &ArtifactId,
        committed: &RevisionRecord,
        edits: &[FileEdit],
    ) -> Result<(), ArtifactError> {
        let missing = || ArtifactError::NotFound(Missing::Artifact);
        let previous = SourceRevision(committed.revision.0.saturating_sub(1));
        let after = self
            .store
            .revision(id, committed.revision)?
            .ok_or_else(missing)?;
        let before = self.store.revision(id, previous)?.ok_or_else(missing)?;
        if !listing_is_as_edited(&before.files, &after.files, edits) {
            return Err(ArtifactError::EditNotAsRequested);
        }
        for edit in edits {
            if let FileChange::Put { path, content } = &edit.change {
                let stored = self.store.revision_file(id, committed.revision, path)?;
                if stored.as_deref() != Some(content.as_slice()) {
                    return Err(ArtifactError::EditNotAsRequested);
                }
            }
        }
        Ok(())
    }

    /// Creates an artifact owned by `actor` whose revision 1 has the files
    /// of `revision` of `id`, byte for byte, with its own history. The title
    /// is `title`, or the source's current title followed by " (Copy)"; the
    /// copy is not pinned. The files are copied; the source's application data
    /// (decision 0031) is copied only with `copy_data`, as the copy's own
    /// space, in the same transaction, from the source as it is when the copy
    /// commits and whatever `revision` is: data has no revisions. The source
    /// is not touched.
    ///
    /// Retrying with the same key returns the original receipt even after
    /// the source was renamed, revised, or purged; the copy has its own
    /// history. A retry whose copy was purged is not found.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::NotFound`] for a missing or inaccessible artifact or
    /// revision, [`ArtifactError::RequestKeyReused`], or
    /// [`ArtifactError::Unavailable`].
    pub fn duplicate(
        &self,
        actor: &ActorId,
        key: &RequestKey,
        id: &ArtifactId,
        revision: SourceRevision,
        title: Option<&Title>,
        copy_data: bool,
    ) -> Result<CommitReceipt, ArtifactError> {
        let mut hasher = Fingerprint::new("duplicate");
        hasher.field(id.as_str().as_bytes());
        hasher.field(&revision.0.to_be_bytes());
        // The default title is hashed as such, not as the title it gives, so
        // a retry after a rename of the source is still the same request.
        match title {
            Some(title) => {
                hasher.field(b"title");
                hasher.field(title.as_str().as_bytes());
            }
            None => hasher.field(b"copy title"),
        }
        // Only a copy that takes the data says so: the keys of copies made
        // before application data existed keep their fingerprints.
        if copy_data {
            hasher.field(b"copy data");
        }
        let request = RequestIdentity {
            actor,
            key,
            fingerprint: hasher.finish(),
        };
        let record = match self.owned(actor, id) {
            Ok(record) => record,
            // The source may have been purged since the copy was made. The
            // key is the actor's own, so its copy is replayed; anything else
            // is a source that is missing.
            Err(error @ ArtifactError::NotFound(_)) => {
                return self.store.replay_create(request)?.ok_or(error);
            }
            Err(error) => return Err(error),
        };
        let package = checked(&record, self.revision_of(&record, revision))?;
        let title = title.cloned().unwrap_or_else(|| record.title.copy_of());
        let receipt = self.store.create_artifact(NewArtifact {
            request,
            kind: record.kind,
            title: &title,
            package: &package,
            // The store refuses the copy unless its source is still outside
            // the trash when the copy commits.
            source: Some(CopySource {
                artifact: id,
                copy_data,
            }),
        })?;
        Ok(receipt)
    }

    /// Changes the title, pin, or trash state of an accessible artifact, only
    /// if its metadata is still at `base`. Source revisions are not touched.
    /// A change to the values already stored writes nothing but the key. In
    /// the trash, only a change that takes the artifact out is accepted.
    ///
    /// Retrying with the same key and content returns the original receipt
    /// even after later changes.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::NotFound`] for a missing or inaccessible artifact, or
    /// one in the trash that the change does not take out,
    /// [`ArtifactError::MetadataConflict`] when `base` is not current,
    /// [`ArtifactError::RequestKeyReused`], or [`ArtifactError::Unavailable`].
    pub fn update(
        &self,
        actor: &ActorId,
        key: &RequestKey,
        id: &ArtifactId,
        base: MetadataRevision,
        change: &MetadataChange,
    ) -> Result<MetadataReceipt, ArtifactError> {
        // The store decides the trash: only a recovery reaches it.
        self.owned(actor, id)?;
        let mut hasher = Fingerprint::new("update");
        hasher.field(id.as_str().as_bytes());
        hasher.field(&base.0.to_be_bytes());
        match &change.title {
            Some(title) => {
                hasher.field(b"title");
                hasher.field(title.as_str().as_bytes());
            }
            None => hasher.field(b"keep title"),
        }
        hasher.field(match change.pinned {
            Some(true) => b"pin",
            Some(false) => b"unpin",
            None => b"keep pin",
        });
        hasher.field(match change.deleted {
            Some(true) => b"delete",
            Some(false) => b"recover",
            None => b"keep trash state",
        });
        let receipt = self.store.update_metadata(MetadataUpdate {
            request: RequestIdentity {
                actor,
                key,
                fingerprint: hasher.finish(),
            },
            artifact: id,
            base,
            title: change.title.as_ref(),
            pinned: change.pinned,
            deleted: change.deleted,
        })?;
        Ok(receipt)
    }

    /// Removes an artifact in the trash for good, only if its metadata is
    /// still at `base` (decision 0029). Its files and titles are deleted; the
    /// identifier, request keys, and change feed entries remain, and the copies
    /// made from it are untouched. The purge is not undone.
    ///
    /// Retrying with the same key and content returns the original receipt.
    /// An earlier request that named the artifact is not found afterwards.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::NotFound`] for a missing, inaccessible, or purged
    /// artifact, [`ArtifactError::NotInTrash`] for one outside the trash,
    /// [`ArtifactError::MetadataConflict`] when `base` is not current,
    /// [`ArtifactError::RequestKeyReused`], or [`ArtifactError::Unavailable`].
    pub fn purge(
        &self,
        actor: &ActorId,
        key: &RequestKey,
        id: &ArtifactId,
        base: MetadataRevision,
    ) -> Result<PurgeReceipt, ArtifactError> {
        // A purged artifact is not stored as one, so only an artifact that
        // still exists can belong to someone else. Anything else goes to the
        // store, which finds the actor's own purge by its key or reports the
        // artifact missing.
        if let Some(record) = self.store.artifact(id)?
            && !policy::may_access(actor, &record)
        {
            return Err(ArtifactError::NotFound(Missing::Artifact));
        }
        let mut hasher = Fingerprint::new("purge");
        hasher.field(id.as_str().as_bytes());
        hasher.field(&base.0.to_be_bytes());
        Ok(self.store.purge_artifact(PurgeRequest {
            request: RequestIdentity {
                actor,
                key,
                fingerprint: hasher.finish(),
            },
            artifact: id,
            base,
        })?)
    }

    /// What content no revision file refers to: the dry run of
    /// [`Self::remove_unreferenced_content`]. Content is shared by all
    /// owners' revisions and is not owned by anyone; with one owner nothing is
    /// revealed that the owner could not see.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::Unavailable`].
    pub fn unreferenced_content(&self) -> Result<UnreferencedContent, ArtifactError> {
        Ok(self.store.unreferenced_content()?)
    }

    /// Deletes the content no revision file of any artifact, in the trash or
    /// not, refers to, if it is the set `base` that a dry run reported, and
    /// reports what it deleted. A retry with the same `key` and `base` returns
    /// the report of the first run (delivered as a replay) whatever the
    /// set is by then; the same key with another `base`, or used for another
    /// request, is [`ArtifactError::RequestKeyReused`]. The key is kept for
    /// the life of the database, like the keys of artifact requests.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::ContentChanged`] when the set is not `base`,
    /// [`ArtifactError::RequestKeyReused`], or [`ArtifactError::Unavailable`].
    pub fn remove_unreferenced_content(
        &self,
        actor: &ActorId,
        key: &RequestKey,
        base: ContentSetDigest,
    ) -> Result<CleanupReceipt, ArtifactError> {
        let mut hasher = Fingerprint::new("cleanup");
        hasher.field(&base.0);
        Ok(self.store.remove_unreferenced_content(CleanupRequest {
            request: RequestIdentity {
                actor,
                key,
                fingerprint: hasher.finish(),
            },
            base,
        })?)
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

    /// One page of the artifacts the actor owns that match `query`, after
    /// the position `page_token` names, if any.
    ///
    /// A list follows its order as artifacts change: one updated while a
    /// caller pages through the list by update time can appear twice or
    /// not at all.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::InvalidPageToken`] or [`ArtifactError::Unavailable`].
    pub fn list(
        &self,
        actor: &ActorId,
        query: &ListQuery,
        page_token: Option<&str>,
    ) -> Result<ArtifactPage, ArtifactError> {
        let after = page_token
            .map(|token| listing::position(token, query).ok_or(ArtifactError::InvalidPageToken))
            .transpose()?;
        let limit = query.limit.get();
        // One more than a page tells whether another page follows.
        let mut artifacts = self.store.list_artifacts(ArtifactListing {
            owner: actor,
            search: query.search.as_ref().map(listing::SearchText::as_str),
            pinned: query.pinned,
            deleted: query.deleted,
            order: query.order,
            after: after.as_ref(),
            limit: limit + 1,
        })?;
        let next_page_token = if artifacts.len() > limit {
            artifacts.truncate(limit);
            artifacts
                .last()
                .map(|last| listing::page_token(query, last))
        } else {
            None
        };
        Ok(ArtifactPage {
            artifacts,
            next_page_token,
        })
    }

    /// Up to `limit` committed changes to the actor's artifacts after
    /// `after`, with the feed's identity and head. Only an artifact's owner
    /// learns of its changes; the head counts every actor's commits.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::Unavailable`].
    pub fn changes(
        &self,
        actor: &ActorId,
        after: ChangeSequence,
        limit: usize,
    ) -> Result<ChangePage, ArtifactError> {
        Ok(self.store.changes(actor, after, limit)?)
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
        let revisions = self.store.revisions(id)?;
        // An artifact outside the trash has a revision, so an empty list means
        // a purge landed after the check (decision 0029).
        if revisions.is_empty() {
            return Err(ArtifactError::NotFound(Missing::Artifact));
        }
        Ok(revisions)
    }

    /// One revision of an accessible artifact.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::NotFound`] or [`ArtifactError::Unavailable`].
    pub fn revision(
        &self,
        actor: &ActorId,
        id: &ArtifactId,
        revision: SourceRevision,
    ) -> Result<RevisionRecord, ArtifactError> {
        let artifact = self.authorized(actor, id)?;
        self.listing_of(&artifact, revision)
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

    /// Part of one file of a pinned revision (decision 0016). The file is
    /// read as [`Self::read_file`] reads it, with the same access and the same
    /// answers for a missing artifact, revision, or file, and then cut; see
    /// [`parts::part`].
    ///
    /// # Errors
    ///
    /// As [`Self::read_file`], and [`ArtifactError::InvalidRange`] for a
    /// range that starts past the end of the file or, read between
    /// characters, cannot be cut there.
    pub fn read_file_part(
        &self,
        actor: &ActorId,
        id: &ArtifactId,
        revision: SourceRevision,
        path: &PackagePath,
        range: ByteRange,
        boundary: Boundary,
    ) -> Result<FilePart, ArtifactError> {
        let file = self.read_file(actor, id, revision, path)?;
        parts::part(file, range, boundary).map_err(ArtifactError::InvalidRange)
    }

    /// Everything a source export of `revision` holds (decision 0015): the
    /// artifact as it is now, the revision's listing, and its files. A read
    /// like [`Self::read_file`], with the same access and the same answers
    /// for a missing artifact or revision.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::NotFound`] or [`ArtifactError::Unavailable`].
    pub fn export(
        &self,
        actor: &ActorId,
        id: &ArtifactId,
        revision: SourceRevision,
    ) -> Result<RevisionExport, ArtifactError> {
        let artifact = self.authorized(actor, id)?;
        let listing = self.listing_of(&artifact, revision)?;
        // The listing exists, so a package that is gone was purged meanwhile.
        let files = self.package(id, revision).map_err(|error| match error {
            ArtifactError::NotFound(_) => ArtifactError::NotFound(Missing::Artifact),
            other => other,
        })?;
        Ok(RevisionExport {
            artifact,
            revision: listing,
            files,
        })
    }

    /// The files a save commits: `changes` applied to `base`.
    fn saved_package(
        &self,
        record: &ArtifactRecord,
        base: SourceRevision,
        changes: Vec<FileChange>,
    ) -> Result<FilePackage, ArtifactError> {
        current_candidate(record, base)?;
        let base_package = self.package(&record.id, base)?;
        match base_package.apply(changes) {
            Ok(package) => Ok(package),
            // Whether the files change is decided with the commit, against
            // the base as it is then: a stale base is a conflict, and a retry
            // of a committed save a replay.
            Err(PackageError::Unchanged) => Ok(base_package),
            Err(error) => Err(ArtifactError::InvalidPackage(error)),
        }
    }

    /// The files a restore of `revision` onto `base` commits.
    fn restored_package(
        &self,
        record: &ArtifactRecord,
        base: SourceRevision,
        revision: SourceRevision,
    ) -> Result<FilePackage, ArtifactError> {
        current_candidate(record, base)?;
        self.revision_of(record, revision)
    }

    /// The files of an existing revision. Its number is checked first, as for
    /// a read, so that one past every revision is missing rather than a
    /// storage failure.
    fn revision_of(
        &self,
        record: &ArtifactRecord,
        revision: SourceRevision,
    ) -> Result<FilePackage, ArtifactError> {
        if revision.0 == 0 || revision > record.current_revision {
            return Err(ArtifactError::NotFound(Missing::Revision));
        }
        self.package(&record.id, revision)
    }

    /// The listing of an existing revision, checked as in
    /// [`Self::revision_of`].
    fn listing_of(
        &self,
        record: &ArtifactRecord,
        revision: SourceRevision,
    ) -> Result<RevisionRecord, ArtifactError> {
        if revision.0 == 0 || revision > record.current_revision {
            return Err(ArtifactError::NotFound(Missing::Revision));
        }
        // The revision is within the artifact's history, so only a purge that
        // landed after the check can make it missing (decision 0029).
        self.store
            .revision(&record.id, revision)?
            .ok_or(ArtifactError::NotFound(Missing::Artifact))
    }

    fn package(
        &self,
        id: &ArtifactId,
        revision: SourceRevision,
    ) -> Result<FilePackage, ArtifactError> {
        self.store
            .revision_package(id, revision)?
            .ok_or(ArtifactError::NotFound(Missing::Revision))
    }

    /// An artifact the actor may read: one it owns, outside the trash.
    pub(crate) fn authorized(
        &self,
        actor: &ActorId,
        id: &ArtifactId,
    ) -> Result<ArtifactRecord, ArtifactError> {
        let record = self.owned(actor, id)?;
        if record.deleted_at.is_some() {
            return Err(ArtifactError::NotFound(Missing::Artifact));
        }
        Ok(record)
    }

    /// An artifact the actor may change, also in the trash: a write there
    /// is answered by [`checked`] and the store (decision 0013).
    fn owned(&self, actor: &ActorId, id: &ArtifactId) -> Result<ArtifactRecord, ArtifactError> {
        match self.store.artifact(id)? {
            Some(record) if policy::may_access(actor, &record) => Ok(record),
            _ => Err(ArtifactError::NotFound(Missing::Artifact)),
        }
    }
}

/// The checks of an edit request that the store cannot make cheaper or more
/// precisely: it names an edit, no path twice, and a revision the artifact
/// has. Whether the files are as expected is the store's, with the commit.
fn edits_are_possible(
    record: &ArtifactRecord,
    base: SourceRevision,
    edits: &[FileEdit],
) -> Result<(), ArtifactError> {
    if edits.is_empty() {
        return Err(ArtifactError::InvalidPackage(PackageError::EmptyChange));
    }
    let mut seen = BTreeSet::new();
    for edit in edits {
        if !seen.insert(edit.change.path()) {
            return Err(ArtifactError::InvalidPackage(PackageError::DuplicatePath(
                edit.change.path().clone(),
            )));
        }
    }
    if base.0 == 0 || base > record.current_revision {
        return Err(ArtifactError::NotFound(Missing::Revision));
    }
    Ok(())
}

/// Whether `after` lists exactly the files of `before` with `edits` applied:
/// the same paths, and for each its size and digest.
fn listing_is_as_edited(before: &[FileEntry], after: &[FileEntry], edits: &[FileEdit]) -> bool {
    let mut expected: BTreeMap<&PackagePath, (u64, [u8; 32])> = before
        .iter()
        .map(|entry| (&entry.path, (entry.size, entry.sha256)))
        .collect();
    for edit in edits {
        match &edit.change {
            FileChange::Put { path, content } => {
                let size = u64::try_from(content.len()).unwrap_or(u64::MAX);
                expected.insert(path, (size, Sha256::digest(content).into()));
            }
            FileChange::Delete { path } => {
                expected.remove(path);
            }
        }
    }
    let listed: BTreeMap<&PackagePath, (u64, [u8; 32])> = after
        .iter()
        .map(|entry| (&entry.path, (entry.size, entry.sha256)))
        .collect();
    listed == expected && after.len() == listed.len()
}

/// Refuses a base that cannot be current: revision 0 never exists, and a
/// future one is not current either. Whether a past one still is, the store
/// decides with the commit.
fn current_candidate(record: &ArtifactRecord, base: SourceRevision) -> Result<(), ArtifactError> {
    if base.0 == 0 || base > record.current_revision {
        return Err(ArtifactError::RevisionConflict {
            current: record.current_revision,
        });
    }
    Ok(())
}

/// The outcome of a write's checks before the store (decision 0013, item 2).
/// In the trash only the retry of a request that committed before the delete
/// can succeed, and such a retry passes these checks: its base and revision
/// existed, and its changes formed a package. A failed check there is
/// therefore answered as for an unknown artifact. A storage failure stays
/// one, so that the request is retried.
fn checked<T>(
    record: &ArtifactRecord,
    prepared: Result<T, ArtifactError>,
) -> Result<T, ArtifactError> {
    match prepared {
        Err(error @ ArtifactError::Unavailable(_)) => Err(error),
        Err(_) if record.deleted_at.is_some() => Err(ArtifactError::NotFound(Missing::Artifact)),
        prepared => prepared,
    }
}

/// Length-prefixed SHA-256 over a request's operation and content, so that
/// field boundaries cannot be shifted to produce the same digest.
pub(crate) struct Fingerprint(Sha256);

impl Fingerprint {
    pub(crate) fn new(operation: &str) -> Self {
        let mut fingerprint = Self(Sha256::new());
        fingerprint.field(operation.as_bytes());
        fingerprint
    }

    pub(crate) fn field(&mut self, bytes: &[u8]) {
        self.0.update((bytes.len() as u64).to_be_bytes());
        self.0.update(bytes);
    }

    fn edit(&mut self, edit: &FileEdit) {
        match &edit.change {
            FileChange::Put { path, content } => self.put(path, content),
            FileChange::Delete { path } => {
                self.field(b"delete");
                self.field(path.as_str().as_bytes());
            }
        }
        match edit.expect {
            FileExpectation::Absent => self.field(b"absent"),
            FileExpectation::Sha256(digest) => {
                self.field(b"sha256");
                self.field(&digest);
            }
        }
    }

    fn put(&mut self, path: &PackagePath, content: &[u8]) {
        self.field(b"put");
        self.field(path.as_str().as_bytes());
        self.field(&Sha256::digest(content));
    }

    pub(crate) fn finish(self) -> RequestFingerprint {
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

    /// A store whose artifact is in place for the authorization check and gone
    /// for every read after it: what a reader sees when a purge commits
    /// between the two (decision 0029). Writes are not used.
    struct PurgedAfterTheCheck {
        record: ArtifactRecord,
    }

    impl ArtifactStore for PurgedAfterTheCheck {
        fn create_artifact(&self, _: NewArtifact<'_>) -> Result<CommitReceipt, StoreError> {
            Err(StoreError::Unavailable("unused".into()))
        }
        fn commit_revision(&self, _: RevisionCommit<'_>) -> Result<CommitReceipt, StoreError> {
            Err(StoreError::Unavailable("unused".into()))
        }
        fn commit_edits(&self, _: ConditionalCommit<'_>) -> Result<CommitReceipt, StoreError> {
            Err(StoreError::Unavailable("unused".into()))
        }
        fn update_metadata(&self, _: MetadataUpdate<'_>) -> Result<MetadataReceipt, StoreError> {
            Err(StoreError::Unavailable("unused".into()))
        }
        fn replay_create(
            &self,
            _: RequestIdentity<'_>,
        ) -> Result<Option<CommitReceipt>, StoreError> {
            Ok(None)
        }
        fn purge_artifact(&self, _: PurgeRequest<'_>) -> Result<PurgeReceipt, StoreError> {
            Err(StoreError::Unavailable("unused".into()))
        }
        fn unreferenced_content(&self) -> Result<UnreferencedContent, StoreError> {
            Err(StoreError::Unavailable("unused".into()))
        }
        fn remove_unreferenced_content(
            &self,
            _: CleanupRequest<'_>,
        ) -> Result<CleanupReceipt, StoreError> {
            Err(StoreError::Unavailable("unused".into()))
        }
        fn artifact(&self, _: &ArtifactId) -> Result<Option<ArtifactRecord>, StoreError> {
            Ok(Some(self.record.clone()))
        }
        fn list_artifacts(
            &self,
            _: ArtifactListing<'_>,
        ) -> Result<Vec<ArtifactRecord>, StoreError> {
            Ok(Vec::new())
        }
        fn revisions(&self, _: &ArtifactId) -> Result<Vec<RevisionRecord>, StoreError> {
            Ok(Vec::new())
        }
        fn revision(
            &self,
            _: &ArtifactId,
            _: SourceRevision,
        ) -> Result<Option<RevisionRecord>, StoreError> {
            Ok(None)
        }
        fn revision_package(
            &self,
            _: &ArtifactId,
            _: SourceRevision,
        ) -> Result<Option<FilePackage>, StoreError> {
            Ok(None)
        }
        fn revision_file(
            &self,
            _: &ArtifactId,
            _: SourceRevision,
            _: &PackagePath,
        ) -> Result<Option<Vec<u8>>, StoreError> {
            Ok(None)
        }
        fn changes(
            &self,
            _: &ActorId,
            _: ChangeSequence,
            _: usize,
        ) -> Result<ChangePage, StoreError> {
            Err(StoreError::Unavailable("unused".into()))
        }
    }

    #[test]
    fn reads_that_a_purge_overtakes_are_not_found_not_empty_or_unavailable()
    -> Result<(), Box<dyn std::error::Error>> {
        let owner: ActorId = "owner".parse()?;
        let id: ArtifactId = "art_0123456789abcdef0123456789abcdef".parse()?;
        let service = ArtifactService::new(PurgedAfterTheCheck {
            record: ArtifactRecord {
                id: id.clone(),
                owner: owner.clone(),
                kind: ArtifactKind::App,
                title: "Demo".parse()?,
                pinned: false,
                current_revision: SourceRevision(1),
                metadata_revision: MetadataRevision(1),
                deleted_at: None,
                created_at: artifree_domain::identity::UnixMillis(1),
                updated_at: artifree_domain::identity::UnixMillis(1),
            },
        });

        assert_eq!(
            service.revisions(&owner, &id).err(),
            Some(ArtifactError::NotFound(Missing::Artifact)),
            "an empty listing of a live artifact is a vanished artifact"
        );
        assert_eq!(
            service.revision(&owner, &id, SourceRevision(1)).err(),
            Some(ArtifactError::NotFound(Missing::Artifact))
        );
        assert_eq!(
            service.export(&owner, &id, SourceRevision(1)).err(),
            Some(ArtifactError::NotFound(Missing::Artifact))
        );
        Ok(())
    }

    fn path(name: &str) -> PackagePath {
        let Ok(path) = name.parse() else {
            panic!("{name} is not a package path");
        };
        path
    }

    fn listed(name: &str, content: &[u8]) -> FileEntry {
        FileEntry {
            path: path(name),
            size: content.len() as u64,
            sha256: Sha256::digest(content).into(),
        }
    }

    fn put_edit(name: &str, content: &[u8], expect: FileExpectation) -> FileEdit {
        FileEdit {
            change: FileChange::Put {
                path: path(name),
                content: content.to_vec(),
            },
            expect,
        }
    }

    #[test]
    fn the_listing_read_back_is_the_previous_one_with_only_the_edits_applied() {
        let before = [listed("a.txt", b"a"), listed("b.txt", b"b")];
        let edits = [
            put_edit("a.txt", b"new", FileExpectation::Sha256([0; 32])),
            put_edit("c.txt", b"c", FileExpectation::Absent),
            FileEdit {
                change: FileChange::Delete {
                    path: path("b.txt"),
                },
                expect: FileExpectation::Sha256([0; 32]),
            },
        ];
        let right = [listed("a.txt", b"new"), listed("c.txt", b"c")];
        assert!(listing_is_as_edited(&before, &right, &edits));

        let wrong: [(&str, [FileEntry; 2]); 4] = [
            (
                "a stale digest",
                [listed("a.txt", b"a"), listed("c.txt", b"c")],
            ),
            (
                "a size off by one",
                [
                    FileEntry {
                        size: 4,
                        ..listed("a.txt", b"new")
                    },
                    listed("c.txt", b"c"),
                ],
            ),
            (
                "the deleted file kept",
                [listed("a.txt", b"new"), listed("b.txt", b"b")],
            ),
            (
                "another file's digest",
                [listed("a.txt", b"new"), listed("c.txt", b"x")],
            ),
        ];
        for (what, after) in wrong {
            assert!(!listing_is_as_edited(&before, &after, &edits), "{what}");
        }
        let extra = [
            listed("a.txt", b"new"),
            listed("c.txt", b"c"),
            listed("z.txt", b"z"),
        ];
        assert!(
            !listing_is_as_edited(&before, &extra, &edits),
            "a file nobody asked for"
        );
        let doubled = [
            listed("a.txt", b"new"),
            listed("c.txt", b"c"),
            listed("c.txt", b"c"),
        ];
        assert!(
            !listing_is_as_edited(&before, &doubled, &edits),
            "a path listed twice"
        );
    }

    #[test]
    fn an_edit_request_names_an_edit_no_path_twice_and_a_revision_the_artifact_has()
    -> Result<(), Box<dyn std::error::Error>> {
        let record = ArtifactRecord {
            id: "art_0123456789abcdef0123456789abcdef".parse()?,
            owner: "owner".parse()?,
            kind: ArtifactKind::App,
            title: "Demo".parse()?,
            pinned: false,
            current_revision: SourceRevision(3),
            metadata_revision: MetadataRevision(1),
            deleted_at: None,
            created_at: artifree_domain::identity::UnixMillis(1),
            updated_at: artifree_domain::identity::UnixMillis(1),
        };
        let one = put_edit("a.txt", b"x", FileExpectation::Absent);
        let other = put_edit("b.txt", b"x", FileExpectation::Absent);
        let check =
            |base, edits: &[FileEdit]| edits_are_possible(&record, SourceRevision(base), edits);

        assert_eq!(check(1, &[one.clone(), other.clone()]), Ok(()));
        assert_eq!(
            check(3, std::slice::from_ref(&one)),
            Ok(()),
            "the current revision"
        );
        assert_eq!(
            check(1, &[]),
            Err(ArtifactError::InvalidPackage(PackageError::EmptyChange))
        );
        assert_eq!(
            check(1, &[one.clone(), other, one.clone()]),
            Err(ArtifactError::InvalidPackage(PackageError::DuplicatePath(
                "a.txt".parse()?
            )))
        );
        for base in [0, 4] {
            assert_eq!(
                check(base, std::slice::from_ref(&one)),
                Err(ArtifactError::NotFound(Missing::Revision)),
                "base {base}"
            );
        }
        Ok(())
    }

    #[test]
    fn the_fingerprint_of_an_edit_covers_its_change_and_its_condition() {
        let fingerprint = |edits: &[FileEdit]| {
            let mut hasher = Fingerprint::new("edit_files");
            for edit in edits {
                hasher.edit(edit);
            }
            hasher.finish()
        };
        let absent = put_edit("a.txt", b"x", FileExpectation::Absent);
        let expects = put_edit("a.txt", b"x", FileExpectation::Sha256([1; 32]));
        let other_digest = put_edit("a.txt", b"x", FileExpectation::Sha256([2; 32]));
        let other_bytes = put_edit("a.txt", b"y", FileExpectation::Absent);
        let delete = FileEdit {
            change: FileChange::Delete {
                path: path("a.txt"),
            },
            expect: FileExpectation::Sha256([1; 32]),
        };
        let all = [absent, expects, other_digest, other_bytes, delete];
        for (left, first) in all.iter().enumerate() {
            for (right, second) in all.iter().enumerate() {
                assert_eq!(
                    fingerprint(std::slice::from_ref(first))
                        == fingerprint(std::slice::from_ref(second)),
                    left == right,
                    "edits {left} and {right}"
                );
            }
        }
    }

    #[test]
    fn fingerprint_distinguishes_shifted_field_boundaries() {
        assert_ne!(digest(&[b"ab", b"c"]), digest(&[b"a", b"bc"]));
        assert_ne!(digest(&[b"abc"]), digest(&[b"abc", b""]));
        assert_eq!(digest(&[b"ab", b"c"]), digest(&[b"ab", b"c"]));
    }
}
