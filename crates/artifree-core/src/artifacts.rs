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
//! policy. A duplicate copies files, the data space only when asked, and the
//! uploaded asset space only when asked, independently of the data (0045).
//! Publishing is not implemented.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use artifree_domain::AppDataVersion;
use artifree_domain::artifact_type::TypeId;
use artifree_domain::change::ChangeSequence;
use artifree_domain::data::DataError;
use artifree_domain::identity::{ActorId, ArtifactId, RequestKey, Title, UnixMillis, UserLabel};
use artifree_domain::job::JobState;
use artifree_domain::package::{FileChange, FilePackage, FileSet, PackageError, PackagePath};
use artifree_domain::{ArtifactKind, MetadataRevision, SourceRevision};
use artifree_ports::access_store::{
    AccessChange, AccessReceipt, AccessRequest, AccessStore, AccessStoreError,
};
use artifree_ports::artifact_store::{
    ArtifactListing, ArtifactRecord, ArtifactStore, ChangePage, CleanupReceipt, CleanupRequest,
    CommitReceipt, ConditionalCommit, ContentSetDigest, CopySource, Delivery, FileEdit, FileEntry,
    FileExpectation, FileMismatch, MetadataReceipt, MetadataUpdate, NewArtifact, PurgeReceipt,
    PurgeRequest, RequestFingerprint, RequestIdentity, RevisionCommit, RevisionRecord, StoreError,
    UnreferencedContent,
};
use sha2::{Digest, Sha256};

use crate::catalog::{Catalog, Release};
use crate::listing::{self, ArtifactPage, ListQuery, PageArtifact};
use crate::parts::{self, Boundary, ByteRange, FilePart, RangeError};
use crate::policy;
use artifree_domain::access::{Access, AccessGeneration, AccessVersion, Need, Role};

/// An artifact's access list as one caller may see it (decision 0060 rules 6
/// and 8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessView {
    /// The artifact's owner.
    pub owner: ActorId,
    /// The owner's label.
    pub owner_label: UserLabel,
    /// The members, ordered by user id.
    pub members: Vec<MemberView>,
    /// The artifact's access version.
    pub version: AccessVersion,
}

/// One member of an access list as one caller may see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberView {
    /// The member.
    pub user: ActorId,
    /// The member's label.
    pub label: UserLabel,
    /// The member's role.
    pub role: Role,
    /// Whether the member's user is disabled, told to the owner only:
    /// `None` for anyone else (decision 0060 rules 8 and 18).
    pub disabled: Option<bool>,
}

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
    /// The revision has no build for the profile (decision 0058 rule 14).
    Build,
    /// The user to revoke is not a member of the artifact (decision 0060
    /// rule 6).
    Member,
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
    /// The actor is a member of the artifact whose role does not allow the
    /// operation (decision 0060 rule 4); nothing was written.
    Forbidden,
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
    /// The edit committed but checking it could not be completed: the store
    /// could not be read, or a revision or file to compare was not there (the
    /// artifact may have been purged meanwhile). The revision was committed;
    /// a retry with the same key returns its receipt while the artifact
    /// exists (decision 0035).
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
    /// The profile is not one this build can build for the public (decision
    /// 0058 rule 17); nothing was enqueued.
    ProfileUnknown,
    /// An opt-in data write no longer matches its admitted authority.
    OutboxBindingChanged,
    /// The application data was prepared against an older version; nothing
    /// was written (decision 0031).
    DataConflict {
        /// The version the data is at.
        current: AppDataVersion,
    },
    /// A data path, value, or change breaks the rules of a data space
    /// (decision 0031).
    Data(DataError),
    /// A duplicate's copied assets would exceed the copy's space limits or the
    /// actor's asset limits (decision 0045); nothing was written.
    AssetLimitExceeded,
    /// The create names a type the catalog does not have (decision 0059
    /// rule 5); nothing was written.
    TypeUnknown,
    /// The revision lacks paths its type's release requires (decision 0059
    /// rules 5 and 6); nothing was written.
    TypeRequirements {
        /// The missing paths, in path order.
        missing: Vec<PackagePath>,
    },
    /// The artifact's type or release is not one this server knows, so it
    /// takes no new commit and no new duplicate (decision 0059 rule 7);
    /// nothing was written.
    TypeReleaseUnknown,
    /// The change of access was prepared against an older access version;
    /// nothing was written (decision 0060 rule 6).
    AccessConflict {
        /// The access version the artifact is at.
        current: AccessVersion,
    },
    /// A grant to a user who is not registered, is disabled, or is the
    /// owner: one answer for all three, so it does not tell them apart
    /// (decision 0060 rule 6); nothing was written.
    NotGrantable,
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
            Self::NotFound(Missing::Build) => formatter.write_str("revision has no build"),
            Self::NotFound(Missing::Member) => formatter.write_str("the user is not a member"),
            Self::AccessConflict { current } => write!(
                formatter,
                "access changed; current access version is {}",
                current.0
            ),
            Self::NotGrantable => formatter.write_str(
                "the user cannot be granted access: not a registered, enabled user other than the owner",
            ),
            Self::Forbidden => formatter.write_str("the actor's role does not allow this"),
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
                "the edit was committed but could not be verified; retry with the same key to get its receipt if the artifact still exists",
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
            Self::ProfileUnknown => formatter.write_str("this server cannot build that profile"),
            Self::OutboxBindingChanged => formatter.write_str("outbox binding changed"),
            Self::DataConflict { current } => write!(
                formatter,
                "the data changed; the current data version is {}",
                current.0
            ),
            Self::Data(error) => write!(formatter, "{error}"),
            Self::AssetLimitExceeded => formatter.write_str("asset limit exceeded"),
            Self::TypeUnknown => formatter.write_str("the catalog has no such type"),
            Self::TypeRequirements { missing } => {
                formatter.write_str("the revision lacks paths its type requires:")?;
                for path in missing {
                    write!(formatter, " {path}")?;
                }
                Ok(())
            }
            Self::TypeReleaseUnknown => formatter.write_str(
                "this server does not know the artifact's type release, so it takes no new commit or copy",
            ),
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

impl From<AccessStoreError> for ArtifactError {
    fn from(error: AccessStoreError) -> Self {
        match error {
            AccessStoreError::NotFound => Self::NotFound(Missing::Artifact),
            AccessStoreError::RequestKeyReused => Self::RequestKeyReused,
            AccessStoreError::VersionConflict { current } => Self::AccessConflict { current },
            AccessStoreError::NotGrantable => Self::NotGrantable,
            AccessStoreError::NotMember => Self::NotFound(Missing::Member),
            AccessStoreError::Unavailable(reason) => Self::Unavailable(reason),
        }
    }
}

impl From<StoreError> for ArtifactError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::NotFound => Self::NotFound(Missing::Artifact),
            StoreError::Forbidden => Self::Forbidden,
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
            StoreError::AssetLimitExceeded => Self::AssetLimitExceeded,
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

/// What a duplicate copies besides the files of the selected revision. Each
/// part is copied only when asked; both are `false` by default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CopyParts {
    /// The source's application data (decision 0031), as it is when the copy
    /// commits.
    pub data: bool,
    /// The actor's entire current asset space of the source (decision 0045),
    /// as it is when the copy commits.
    pub assets: bool,
}

/// Application commands for file-package artifacts. HTTP, MCP, and CLI
/// adapters call these methods instead of the store.
#[derive(Debug)]
pub struct ArtifactService<S> {
    pub(crate) store: S,
    catalog: Catalog,
}

impl<S: ArtifactStore + AccessStore> ArtifactService<S> {
    /// Uses `store` for all durable state, with an empty type catalog; see
    /// [`Self::with_catalog`].
    pub fn new(store: S) -> Self {
        Self {
            store,
            catalog: Catalog::empty(),
        }
    }

    /// The same service with `catalog` as its types (decision 0059). A server
    /// passes [`Catalog::builtin`].
    #[must_use]
    pub fn with_catalog(self, catalog: Catalog) -> Self {
        Self { catalog, ..self }
    }

    /// The types this service creates from and checks commits against.
    #[must_use]
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// Creates an artifact of the type `type_id` owned by `actor` at revision
    /// 1 (decision 0059 rule 5). Revision 1 is `files` when given, whole, and
    /// otherwise the current release's starter; either way it must hold the
    /// release's required paths, `index.html` among them when the release
    /// requires it, so `files` is a set that may lack it. The artifact gets the release's kind and
    /// records the type and release for good.
    ///
    /// The request's identity covers the type id and the files, or that there
    /// were none, but not the release: retrying with the same key after the
    /// catalog gained a release returns the original receipt, which names
    /// the release it was created with. A retry is replayed before the type
    /// and its requirements are checked.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::TypeUnknown`], [`ArtifactError::TypeRequirements`],
    /// [`ArtifactError::InvalidPackage`] when `files` lack an `index.html`
    /// the release does not require, [`ArtifactError::NotFound`] for the
    /// retry of a create whose artifact was purged,
    /// [`ArtifactError::RequestKeyReused`], or [`ArtifactError::Unavailable`].
    pub fn create_from_type(
        &self,
        actor: &ActorId,
        key: &RequestKey,
        title: &Title,
        type_id: &TypeId,
        files: Option<&FileSet>,
    ) -> Result<CommitReceipt, ArtifactError> {
        let mut hasher = Fingerprint::new("create");
        hasher.field(b"type");
        hasher.field(type_id.as_str().as_bytes());
        hasher.field(title.as_str().as_bytes());
        match files {
            Some(files) => {
                hasher.field(b"files");
                for (path, content) in files.files() {
                    hasher.put(path, content);
                }
            }
            None => hasher.field(b"no files"),
        }
        let request = RequestIdentity {
            actor,
            key,
            fingerprint: hasher.finish(),
        };
        let prepared = self
            .catalog
            .current(type_id)
            .ok_or(ArtifactError::TypeUnknown)
            .and_then(|release| typed_package(release, files).map(|package| (release, package)));
        let (release, package) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => return self.store.replay_receipt(request)?.ok_or(error),
        };
        let artifact_type = release.artifact_type();
        let receipt = self.store.create_artifact(NewArtifact {
            request,
            kind: release.kind,
            artifact_type: Some(&artifact_type),
            title,
            package: package.as_ref().unwrap_or(&release.starter),
            source: None,
        })?;
        Ok(receipt)
    }

    /// The release whose rules a new commit to or copy of `record` follows:
    /// `None` for an untyped artifact. An artifact whose release this server
    /// does not know takes none (decision 0059 rule 7); its error is
    /// [`ArtifactError::TypeReleaseUnknown`], or not found in the trash, as
    /// [`checked`] answers.
    fn pinned_release(&self, record: &ArtifactRecord) -> Result<Option<&Release>, ArtifactError> {
        match &record.artifact_type {
            None => Ok(None),
            Some(pinned) => checked(
                record,
                self.catalog
                    .release(&pinned.id, pinned.release)
                    .map(Some)
                    .ok_or(ArtifactError::TypeReleaseUnknown),
            ),
        }
    }

    /// The release of `record` as [`Self::pinned_release`] finds it, or, when
    /// there is none to follow, the stored receipt of `request` if it has one:
    /// a retry learns what happened before the refusal (decision 0059 rule 7).
    fn release_or_replay(
        &self,
        record: &ArtifactRecord,
        request: RequestIdentity<'_>,
    ) -> Result<Result<Option<&Release>, CommitReceipt>, ArtifactError> {
        match self.pinned_release(record) {
            Ok(release) => Ok(Ok(release)),
            Err(error) => match self.store.replay_receipt(request)? {
                Some(receipt) => Ok(Err(receipt)),
                None => Err(error),
            },
        }
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
            artifact_type: None,
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
    /// [`ArtifactError::InvalidPackage`], [`ArtifactError::TypeRequirements`]
    /// when the changes delete a path the artifact's release requires,
    /// whatever the base, [`ArtifactError::TypeReleaseUnknown`],
    /// [`ArtifactError::RequestKeyReused`], or [`ArtifactError::Unavailable`].
    pub fn save(
        &self,
        actor: &ActorId,
        key: &RequestKey,
        id: &ArtifactId,
        base: SourceRevision,
        changes: Vec<FileChange>,
    ) -> Result<CommitReceipt, ArtifactError> {
        let record = self.owned(actor, id, Need::Change)?;
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
        let request = RequestIdentity {
            actor,
            key,
            fingerprint: hasher.finish(),
        };
        let release = match self.release_or_replay(&record, request)? {
            Ok(release) => release,
            Err(replayed) => return Ok(replayed),
        };
        // A deleted required path is named before the package is built, which
        // would refuse a missing `index.html` as a package error instead.
        let package = checked(
            &record,
            keeps(release, changes.iter())
                .and_then(|()| self.saved_package(&record, base, changes))
                .and_then(|package| meets(release, &package).map(|()| package)),
        )?;
        let receipt = self.store.commit_revision(RevisionCommit {
            request,
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
    /// `base` already has those files, [`ArtifactError::TypeRequirements`],
    /// [`ArtifactError::TypeReleaseUnknown`], [`ArtifactError::RequestKeyReused`],
    /// or [`ArtifactError::Unavailable`].
    pub fn restore(
        &self,
        actor: &ActorId,
        key: &RequestKey,
        id: &ArtifactId,
        base: SourceRevision,
        revision: SourceRevision,
    ) -> Result<CommitReceipt, ArtifactError> {
        let record = self.owned(actor, id, Need::Change)?;
        let mut hasher = Fingerprint::new("restore");
        hasher.field(id.as_str().as_bytes());
        hasher.field(&base.0.to_be_bytes());
        hasher.field(&revision.0.to_be_bytes());
        // The store decides in one step whether this is a replay, a stale
        // base, or files the base already has.
        let request = RequestIdentity {
            actor,
            key,
            fingerprint: hasher.finish(),
        };
        let release = match self.release_or_replay(&record, request)? {
            Ok(release) => release,
            Err(replayed) => return Ok(replayed),
        };
        // A revision that passed its release's check once passes again; this
        // refuses only one the store holds against the rules.
        let package = checked(
            &record,
            self.restored_package(&record, base, revision)
                .and_then(|package| meets(release, &package).map(|()| package)),
        )?;
        let receipt = self.store.commit_revision(RevisionCommit {
            request,
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
    /// Before anything is written: [`ArtifactError::NotFound`] for a missing,
    /// inaccessible or trashed artifact, or a `base` it has no revision for
    /// (or, on a retry, an artifact purged since the commit);
    /// [`ArtifactError::InvalidPackage`] for no edit, a path edited twice, a
    /// result that is no valid package, or one that equals the current
    /// revision; [`ArtifactError::FilesChanged`];
    /// [`ArtifactError::TypeRequirements`] for an edit that deletes a path
    /// the artifact's release requires; [`ArtifactError::TypeReleaseUnknown`];
    /// [`ArtifactError::RequestKeyReused`]; and
    /// [`ArtifactError::Unavailable`] when the store fails in the commit
    /// itself, whose outcome is then unknown until a retry with the same key
    /// reports it.
    ///
    /// After the commit no error is a plain one: the revision exists, as
    /// [`ArtifactError::committed_a_revision`] says. A read-back that
    /// disagrees is [`ArtifactError::EditNotAsRequested`]; one that cannot be
    /// completed, whether for a store error or a revision or file that is not
    /// there to read, is [`ArtifactError::EditUnverified`].
    pub fn edit_files(
        &self,
        actor: &ActorId,
        key: &RequestKey,
        id: &ArtifactId,
        base: SourceRevision,
        edits: &[FileEdit],
    ) -> Result<CommitReceipt, ArtifactError> {
        let record = self.owned(actor, id, Need::Change)?;
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
        let request = RequestIdentity {
            actor,
            key,
            fingerprint: hasher.finish(),
        };
        let release = match self.release_or_replay(&record, request)? {
            Ok(release) => release,
            Err(replayed) => return Ok(replayed),
        };
        checked(
            &record,
            edits_are_possible(&record, base, edits)
                .and_then(|()| keeps(release, edits.iter().map(|edit| &edit.change))),
        )?;
        let receipt = self.store.commit_edits(ConditionalCommit {
            request,
            artifact: id,
            edits,
        })?;
        if receipt.delivery == Delivery::Committed {
            self.read_back(id, &receipt.revision, edits)?;
        }
        Ok(receipt)
    }

    /// Reads the revision `committed` created and compares it with `edits`:
    /// the previous revision's files, changed only as the edits say, and the
    /// bytes of every file put. The edit has committed, so no failure here is
    /// an ordinary one: a comparison that disagrees is
    /// [`ArtifactError::EditNotAsRequested`], and anything that stops the
    /// comparison, a store error or a revision or file that is not there to
    /// read (the artifact may have been purged meanwhile), is
    /// [`ArtifactError::EditUnverified`].
    fn read_back(
        &self,
        id: &ArtifactId,
        committed: &RevisionRecord,
        edits: &[FileEdit],
    ) -> Result<(), ArtifactError> {
        match self.matches_edits(id, committed, edits) {
            Ok(Some(true)) => Ok(()),
            Ok(Some(false)) => Err(ArtifactError::EditNotAsRequested),
            Ok(None) | Err(_) => Err(ArtifactError::EditUnverified),
        }
    }

    /// Whether the revision `committed` is as `edits` made it, or `None` when
    /// a revision or file needed for the comparison is not there to read.
    fn matches_edits(
        &self,
        id: &ArtifactId,
        committed: &RevisionRecord,
        edits: &[FileEdit],
    ) -> Result<Option<bool>, StoreError> {
        let previous = SourceRevision(committed.revision.0.saturating_sub(1));
        let Some(after) = self.store.revision(id, committed.revision)? else {
            return Ok(None);
        };
        let Some(before) = self.store.revision(id, previous)? else {
            return Ok(None);
        };
        if !listing_is_as_edited(&before.files, &after.files, edits) {
            return Ok(Some(false));
        }
        for edit in edits {
            if let FileChange::Put { path, content } = &edit.change {
                // The listing has the file, so a file that cannot be read is
                // a gap in what the store holds, not a different file.
                let Some(stored) = self.store.revision_file(id, committed.revision, path)? else {
                    return Ok(None);
                };
                if stored != *content {
                    return Ok(Some(false));
                }
            }
        }
        Ok(Some(true))
    }

    /// Creates an artifact owned by `actor` whose revision 1 has the files
    /// of `revision` of `id`, byte for byte, with its own history. The title
    /// is `title`, or the source's current title followed by " (Copy)"; the
    /// copy is not pinned. The files are copied; the source's application data
    /// (decision 0031) is copied only with `parts.data`, and the actor's
    /// uploaded asset space (decision 0045) only with `parts.assets`, each as
    /// the copy's own space, in the same transaction, from the source as it is
    /// when the copy commits and whatever `revision` is: neither has
    /// revisions. Copied assets keep their IDs, so references to them in the
    /// copied files and data still resolve. The source is not touched.
    ///
    /// Retrying with the same key returns the original receipt even after
    /// the source was renamed, revised, or purged; the copy has its own
    /// history. A retry whose copy was purged is not found.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::NotFound`] for a missing or inaccessible artifact or
    /// revision, [`ArtifactError::AssetLimitExceeded`] when the copied assets
    /// do not fit (no key is recorded), [`ArtifactError::TypeReleaseUnknown`],
    /// [`ArtifactError::RequestKeyReused`], or [`ArtifactError::Unavailable`].
    pub fn duplicate(
        &self,
        actor: &ActorId,
        key: &RequestKey,
        id: &ArtifactId,
        revision: SourceRevision,
        title: Option<&Title>,
        parts: CopyParts,
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
        if parts.data {
            hasher.field(b"copy data");
        }
        // The same holds for assets: a copy without them keeps the
        // fingerprint it had before assets could be copied.
        if parts.assets {
            hasher.field(b"copy assets");
        }
        let request = RequestIdentity {
            actor,
            key,
            fingerprint: hasher.finish(),
        };
        let record = match self.owned(actor, id, Need::Read) {
            Ok(record) => record,
            // The source may have been purged since the copy was made. The
            // key is the actor's own, so its copy is replayed; anything else
            // is a source that is missing.
            Err(error @ ArtifactError::NotFound(_)) => {
                return self.store.replay_receipt(request)?.ok_or(error);
            }
            Err(error) => return Err(error),
        };
        // The copy keeps the source's type and release (decision 0059 rule 6),
        // so a release this server does not know takes no new copy (rule 7).
        if let Err(replayed) = self.release_or_replay(&record, request)? {
            return Ok(replayed);
        }
        let package = checked(&record, self.revision_of(&record, revision))?;
        let title = title.cloned().unwrap_or_else(|| record.title.copy_of());
        let receipt = self.store.create_artifact(NewArtifact {
            request,
            kind: record.kind,
            artifact_type: record.artifact_type.as_ref(),
            title: &title,
            package: &package,
            // The store refuses the copy unless its source is still outside
            // the trash when the copy commits.
            source: Some(CopySource {
                artifact: id,
                copy_data: parts.data,
                copy_assets: parts.assets,
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
        // The store decides the trash: only a recovery reaches it. A move to
        // or from the trash is the owner's (decision 0060 rule 3).
        self.owned(actor, id, Need::of_metadata(change.deleted.is_some()))?;
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
        // Only the owner purges (decision 0060 rule 3): a member outside the
        // trash is refused for its role, and inside it reaches nothing (rule 4).
        if let Some(record) = self.store.artifact(id)? {
            self.admit(actor, &record, Need::Own)?;
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
    /// owners' revisions and is not owned by anyone, so this reports on the
    /// whole store and the server offers it to the administrator only
    /// (decision 0036).
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

    /// As [`Self::artifact`], with how `actor` stands towards it: owner, or
    /// member with a role (decision 0060 rule 6, which has read tools report
    /// the caller's role). Access is checked as every read checks it.
    ///
    /// # Errors
    ///
    /// As [`Self::artifact`].
    pub fn artifact_with_access(
        &self,
        actor: &ActorId,
        id: &ArtifactId,
    ) -> Result<(ArtifactRecord, Access), ArtifactError> {
        self.reachable_with_access(actor, id, Need::Read)
    }

    /// Grants `user` a role on the actor's artifact, changes it, or revokes
    /// it (decision 0060 rule 6), if the artifact is at `base`. Only the
    /// owner may: a member is refused for their role, and anyone else, or
    /// anyone while the artifact is in the trash, finds no artifact. The key
    /// is in the owner's key space; a retry with the same key and the same
    /// change returns the first receipt, delivered as a replay. Each change
    /// raises the access version by one; a grant of the role the member
    /// already has changes nothing. The store cancels the queued jobs a
    /// revocation or a downgrade takes (rule 15) and appends the change rows
    /// (rule 14) in the same transaction. What the user holds in memory is the
    /// caller's to remove once the receipt says the change committed.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::NotFound`] for a missing, inaccessible, trashed, or
    /// purged artifact, or ([`Missing::Member`]) a revocation of a user who
    /// is not a member; [`ArtifactError::Forbidden`] for a member;
    /// [`ArtifactError::AccessConflict`] when `base` is not current;
    /// [`ArtifactError::NotGrantable`]; [`ArtifactError::RequestKeyReused`];
    /// or [`ArtifactError::Unavailable`].
    #[expect(
        clippy::too_many_arguments,
        reason = "Each argument is one part of the change request; a struct would only rename them."
    )]
    pub fn change_access(
        &self,
        actor: &ActorId,
        key: &RequestKey,
        id: &ArtifactId,
        user: &ActorId,
        change: AccessChange,
        base: AccessVersion,
        now: UnixMillis,
    ) -> Result<AccessReceipt, ArtifactError> {
        // A member is refused for its role (rule 4) before the store, which
        // finds nothing that is not the actor's own.
        self.owned(actor, id, Need::Own)?;
        let mut hasher = Fingerprint::new("access");
        hasher.field(id.as_str().as_bytes());
        hasher.field(user.as_str().as_bytes());
        hasher.field(match change {
            AccessChange::Grant(role) => role.as_str().as_bytes(),
            AccessChange::Revoke => b"revoke".as_slice(),
        });
        hasher.field(&base.0.to_be_bytes());
        Ok(self.store.change_access(AccessRequest {
            request: RequestIdentity {
                actor,
                key,
                fingerprint: hasher.finish(),
            },
            artifact: id,
            user,
            change,
            base,
            now,
        })?)
    }

    /// The access list of an artifact the actor owns or is a member of
    /// (decision 0060 rule 6). Only the owner is told which members are
    /// disabled; for anyone else each [`MemberView::disabled`] is `None`
    /// (rules 8 and 18). The owner reads it in the trash too, where no
    /// member reaches the artifact. Access is checked again against the
    /// list itself, which the store reads in one snapshot, so a member
    /// revoked, or an artifact moved to the trash, after the first check
    /// is not found rather than shown the list.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::NotFound`] or [`ArtifactError::Unavailable`].
    pub fn access(&self, actor: &ActorId, id: &ArtifactId) -> Result<AccessView, ArtifactError> {
        self.owned(actor, id, Need::Read)?;
        let list = self
            .store
            .access_list(id)?
            .ok_or(ArtifactError::NotFound(Missing::Artifact))?;
        let is_owner = list.owner == *actor;
        let is_member = !list.in_trash && list.members.iter().any(|member| member.user == *actor);
        if !is_owner && !is_member {
            return Err(ArtifactError::NotFound(Missing::Artifact));
        }
        Ok(AccessView {
            owner: list.owner,
            owner_label: list.owner_label,
            members: list
                .members
                .into_iter()
                .map(|member| MemberView {
                    user: member.user,
                    label: member.label,
                    role: member.role,
                    disabled: is_owner.then_some(member.disabled),
                })
                .collect(),
            version: list.version,
        })
    }

    /// Whether `actor`, who may read an artifact, may also make the change
    /// `need` names on it, without making one: what a runtime surface offers
    /// by role (decision 0060 rule 3).
    ///
    /// # Errors
    ///
    /// [`ArtifactError::NotFound`] or [`ArtifactError::Unavailable`].
    pub fn allows(
        &self,
        actor: &ActorId,
        id: &ArtifactId,
        need: Need,
    ) -> Result<bool, ArtifactError> {
        match self.reachable(actor, id, need) {
            Ok(_) => Ok(true),
            Err(ArtifactError::Forbidden) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// The access generation `actor` holds on an artifact (decision 0060
    /// rule 16): [`AccessGeneration::NEVER`] for an actor who was never a
    /// member, as the owner. A request reads it before its access check, so
    /// that what it registers afterwards can be confirmed against it. It is
    /// the actor's own and says nothing about other users.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::Unavailable`].
    pub fn access_generation(
        &self,
        actor: &ActorId,
        id: &ArtifactId,
    ) -> Result<AccessGeneration, ArtifactError> {
        Ok(self.store.access_generation(id, actor)?)
    }

    /// One page of the artifacts the actor reaches that match `query`, after
    /// the position `page_token` names, if any: their own and, outside the
    /// trash, those they are a member of, each with the actor's access and
    /// the owner's label (decision 0060 rule 7). The trash lists only the
    /// actor's own.
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
        let mut listed = self.store.list_artifacts(ArtifactListing {
            caller: actor,
            search: query.search.as_ref().map(listing::SearchText::as_str),
            pinned: query.pinned,
            deleted: query.deleted,
            access: query.access,
            order: query.order,
            after: after.as_ref(),
            limit: limit + 1,
        })?;
        let next_page_token = if listed.len() > limit {
            listed.truncate(limit);
            listed
                .last()
                .map(|last| listing::page_token(query, &last.record))
        } else {
            None
        };
        // The access each row grants is the policy's, as for every read; a
        // row the store lists without a role the caller holds is refused.
        let artifacts = listed
            .into_iter()
            .map(|listed| {
                let access =
                    policy::access_of(actor, &listed.record, listed.role).ok_or_else(|| {
                        ArtifactError::Unavailable(
                            "the store listed an unreachable artifact".into(),
                        )
                    })?;
                Ok(PageArtifact {
                    record: listed.record,
                    access,
                    owner_label: listed.owner_label,
                })
            })
            .collect::<Result<_, ArtifactError>>()?;
        Ok(ArtifactPage {
            artifacts,
            next_page_token,
        })
    }

    /// Up to `limit` committed changes to the actor's artifacts after
    /// `after`, with the feed's identity and head. Only an artifact's owner
    /// learns of its changes. Each actor has a feed of its own (decision
    /// 0036): positions count that actor's commits only, and the head is that
    /// actor's newest position, so no number reveals another actor's activity.
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

    /// An artifact the actor may read: its own or one it is a member of,
    /// outside the trash (decision 0060 rule 3).
    pub(crate) fn authorized(
        &self,
        actor: &ActorId,
        id: &ArtifactId,
    ) -> Result<ArtifactRecord, ArtifactError> {
        self.reachable(actor, id, Need::Read)
    }

    /// An artifact outside the trash whose operation `need`s more than a
    /// read: enqueueing a job needs [`Need::Change`].
    pub(crate) fn reachable(
        &self,
        actor: &ActorId,
        id: &ArtifactId,
        need: Need,
    ) -> Result<ArtifactRecord, ArtifactError> {
        self.reachable_with_access(actor, id, need)
            .map(|(record, _)| record)
    }

    /// As [`Self::reachable`], with how `actor` stands towards the artifact.
    fn reachable_with_access(
        &self,
        actor: &ActorId,
        id: &ArtifactId,
        need: Need,
    ) -> Result<(ArtifactRecord, Access), ArtifactError> {
        let (record, access) = self.owned_with_access(actor, id, need)?;
        if record.deleted_at.is_some() {
            return Err(ArtifactError::NotFound(Missing::Artifact));
        }
        Ok((record, access))
    }

    /// An artifact the actor may change, also in the trash: a write there
    /// is answered by [`checked`] and the store (decision 0013). See
    /// [`Self::admit`] for who reaches it.
    fn owned(
        &self,
        actor: &ActorId,
        id: &ArtifactId,
        need: Need,
    ) -> Result<ArtifactRecord, ArtifactError> {
        self.owned_with_access(actor, id, need)
            .map(|(record, _)| record)
    }

    /// As [`Self::owned`], with how `actor` stands towards the artifact.
    fn owned_with_access(
        &self,
        actor: &ActorId,
        id: &ArtifactId,
        need: Need,
    ) -> Result<(ArtifactRecord, Access), ArtifactError> {
        let record = self
            .store
            .artifact(id)?
            .ok_or(ArtifactError::NotFound(Missing::Artifact))?;
        let access = self.admit(actor, &record, need)?;
        Ok((record, access))
    }

    /// Whether `actor` may do what an operation `need`s on `record`. The owner
    /// reaches it, in the trash too; a member reaches it outside the trash
    /// only, with a role that allows what the operation needs, and otherwise
    /// is refused with [`ArtifactError::Forbidden`]; anyone else gets the
    /// answer an unknown name gets (decision 0060 rules 3 and 4).
    fn admit(
        &self,
        actor: &ActorId,
        record: &ArtifactRecord,
        need: Need,
    ) -> Result<Access, ArtifactError> {
        // No member reaches the artifact in the trash (the store holds no
        // role for anyone there), so its role is not looked up.
        let role = if record.owner == *actor || record.deleted_at.is_some() {
            None
        } else {
            self.store.member_role(&record.id, actor)?
        };
        let access = policy::access_of(actor, record, role)
            .ok_or(ArtifactError::NotFound(Missing::Artifact))?;
        if !access.permits(need) {
            return Err(ArtifactError::Forbidden);
        }
        Ok(access)
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

/// The first revision of a create from `release`: `files` as a package when
/// given, and `None` for the release's starter. Missing required paths are
/// refused before a missing `index.html` is (decision 0059 rule 5).
fn typed_package(
    release: &Release,
    files: Option<&FileSet>,
) -> Result<Option<FilePackage>, ArtifactError> {
    let Some(files) = files else {
        return Ok(None);
    };
    let missing = release.missing_from(files);
    if !missing.is_empty() {
        return Err(ArtifactError::TypeRequirements { missing });
    }
    files
        .clone()
        .into_package()
        .map(Some)
        .map_err(ArtifactError::InvalidPackage)
}

/// Refuses a package that lacks a path `release` requires (decision 0059
/// rules 5 and 6); an untyped artifact has no such paths.
fn meets(release: Option<&Release>, package: &FilePackage) -> Result<(), ArtifactError> {
    let missing = release
        .map(|release| release.missing(package))
        .unwrap_or_default();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(ArtifactError::TypeRequirements { missing })
    }
}

/// Refuses changes that delete a path `release` requires. Every revision of
/// the artifact holds those paths, so changes that delete none keep them all,
/// whatever their base and also when the head moves before they commit.
fn keeps<'a>(
    release: Option<&Release>,
    changes: impl IntoIterator<Item = &'a FileChange>,
) -> Result<(), ArtifactError> {
    let Some(release) = release else {
        return Ok(());
    };
    let deleted: BTreeSet<&PackagePath> = changes
        .into_iter()
        .filter_map(|change| match change {
            FileChange::Delete { path } => Some(path),
            FileChange::Put { .. } => None,
        })
        .collect();
    let missing: Vec<PackagePath> = release
        .required
        .iter()
        .filter(|path| deleted.contains(path))
        .cloned()
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(ArtifactError::TypeRequirements { missing })
    }
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
    use artifree_domain::access::{AccessGeneration, Role};
    use artifree_ports::access_store::{
        AccessList, AccessReceipt, AccessRequest, AccessStoreError,
    };
    use artifree_ports::artifact_store::ListedArtifact;

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
    /// With `role` and `list`, it is instead a store whose member passes the
    /// first access check and whose access list is read after a change of
    /// access: the list's own snapshot.
    struct PurgedAfterTheCheck {
        record: ArtifactRecord,
        role: Option<Role>,
        list: Option<AccessList>,
    }

    impl AccessStore for PurgedAfterTheCheck {
        fn access_list(&self, _: &ArtifactId) -> Result<Option<AccessList>, StoreError> {
            self.list
                .clone()
                .map(Some)
                .ok_or_else(|| StoreError::Unavailable("unused".into()))
        }
        fn member_role(&self, _: &ArtifactId, _: &ActorId) -> Result<Option<Role>, StoreError> {
            Ok(self.role)
        }
        fn access_generation(
            &self,
            _: &ArtifactId,
            _: &ActorId,
        ) -> Result<AccessGeneration, StoreError> {
            Err(StoreError::Unavailable("unused".into()))
        }
        fn change_access(&self, _: AccessRequest<'_>) -> Result<AccessReceipt, AccessStoreError> {
            Err(AccessStoreError::Unavailable("unused".into()))
        }
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
        fn replay_receipt(
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
        ) -> Result<Vec<ListedArtifact>, StoreError> {
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
                artifact_type: None,
                title: "Demo".parse()?,
                pinned: false,
                current_revision: SourceRevision(1),
                metadata_revision: MetadataRevision(1),
                deleted_at: None,
                created_at: artifree_domain::identity::UnixMillis(1),
                updated_at: artifree_domain::identity::UnixMillis(1),
            },
            role: None,
            list: None,
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

    /// Decision 0060 rules 4 and 8: a member whom the list read after the
    /// first check no longer names, or who reads it after a move to the
    /// trash, is not found; only the owner is told who is disabled.
    #[test]
    fn the_access_list_is_checked_again_against_its_own_snapshot()
    -> Result<(), Box<dyn std::error::Error>> {
        let owner: ActorId = "owner".parse()?;
        let viewer = ActorId::user([2; 16]);
        let id: ArtifactId = "art_0123456789abcdef0123456789abcdef".parse()?;
        let record = ArtifactRecord {
            id: id.clone(),
            owner: owner.clone(),
            kind: ArtifactKind::App,
            artifact_type: None,
            title: "Demo".parse()?,
            pinned: false,
            current_revision: SourceRevision(1),
            metadata_revision: MetadataRevision(1),
            deleted_at: None,
            created_at: artifree_domain::identity::UnixMillis(1),
            updated_at: artifree_domain::identity::UnixMillis(1),
        };
        let member = artifree_ports::access_store::Member {
            user: viewer.clone(),
            label: "Vi".parse()?,
            role: Role::Viewer,
            disabled: true,
        };
        let owner_label: UserLabel = "Owner".parse()?;
        let list = |members: Vec<artifree_ports::access_store::Member>, in_trash| AccessList {
            owner: owner.clone(),
            owner_label: owner_label.clone(),
            members,
            version: AccessVersion(1),
            in_trash,
        };
        let service = |list| {
            ArtifactService::new(PurgedAfterTheCheck {
                record: record.clone(),
                role: Some(Role::Viewer),
                list: Some(list),
            })
        };
        let hidden = Some(ArtifactError::NotFound(Missing::Artifact));
        assert_eq!(
            service(list(Vec::new(), false)).access(&viewer, &id).err(),
            hidden,
            "revoked"
        );
        assert_eq!(
            service(list(vec![member.clone()], true))
                .access(&viewer, &id)
                .err(),
            hidden,
            "in the trash"
        );
        let shown = service(list(vec![member.clone()], false));
        let disabled =
            |view: AccessView| view.members.iter().map(|m| m.disabled).collect::<Vec<_>>();
        assert_eq!(disabled(shown.access(&viewer, &id)?), [None]);
        assert_eq!(disabled(shown.access(&owner, &id)?), [Some(true)]);
        let trashed = service(list(vec![member], true));
        assert_eq!(disabled(trashed.access(&owner, &id)?), [Some(true)]);
        Ok(())
    }

    /// Decision 0060 rules 4 and 6: the core refuses a member's change of
    /// access for its role before the store is asked, so every adapter
    /// answers `403` and not what the store would.
    #[test]
    fn a_member_changes_no_access_and_the_store_is_not_asked()
    -> Result<(), Box<dyn std::error::Error>> {
        let owner: ActorId = "owner".parse()?;
        let editor = ActorId::user([3; 16]);
        let id: ArtifactId = "art_0123456789abcdef0123456789abcdef".parse()?;
        let service = ArtifactService::new(PurgedAfterTheCheck {
            record: ArtifactRecord {
                id: id.clone(),
                owner,
                kind: ArtifactKind::App,
                artifact_type: None,
                title: "Demo".parse()?,
                pinned: false,
                current_revision: SourceRevision(1),
                metadata_revision: MetadataRevision(1),
                deleted_at: None,
                created_at: UnixMillis(1),
                updated_at: UnixMillis(1),
            },
            role: Some(Role::Editor),
            list: None,
        });
        // The fixture's store answers every change as unavailable.
        assert_eq!(
            service
                .change_access(
                    &editor,
                    &"access-by-editor".parse()?,
                    &id,
                    &ActorId::user([4; 16]),
                    AccessChange::Revoke,
                    AccessVersion(1),
                    UnixMillis(1),
                )
                .err(),
            Some(ArtifactError::Forbidden)
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
            artifact_type: None,
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
