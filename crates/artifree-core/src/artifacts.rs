//! HTML file-package artifacts: create, conditional revision commits and
//! restores, duplicates, conditional metadata updates, lists, and
//! revision-pinned reads.
//!
//! Every operation checks the actor's authority against current stored
//! metadata. An artifact in the trash is missing to every request but its
//! recovery and the replay of a request made before the delete (decision
//! 0013). Commit success means durable acceptance. Source history is kept
//! separate from future application data and access policy. A duplicate
//! copies files only, since artifacts have no application data or uploaded
//! assets yet. Publishing is not implemented.

use std::fmt;

use artifree_domain::change::ChangeSequence;
use artifree_domain::identity::{ActorId, ArtifactId, RequestKey, Title};
use artifree_domain::package::{FileChange, FilePackage, PackageError, PackagePath};
use artifree_domain::{ArtifactKind, MetadataRevision, SourceRevision};
use artifree_ports::artifact_store::{
    ArtifactListing, ArtifactRecord, ArtifactStore, ChangePage, CommitReceipt, MetadataReceipt,
    MetadataUpdate, NewArtifact, RequestFingerprint, RequestIdentity, RevisionCommit,
    RevisionRecord, StoreError,
};
use sha2::{Digest, Sha256};

use crate::listing::{self, ArtifactPage, ListQuery};
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
    /// The metadata change was prepared against an older metadata revision;
    /// nothing was written.
    MetadataConflict {
        /// The current metadata revision.
        current: MetadataRevision,
    },
    /// The page token is malformed or belongs to another order or filters.
    InvalidPageToken,
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
            Self::MetadataConflict { current } => write!(
                formatter,
                "artifact metadata changed; current metadata revision is {}",
                current.0
            ),
            Self::InvalidPageToken => formatter
                .write_str("page token is malformed or belongs to another order or filters"),
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
            StoreError::MetadataConflict { current } => Self::MetadataConflict { current },
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

    /// Creates an artifact owned by `actor` whose revision 1 has the files
    /// of `revision` of `id`, byte for byte, with its own history. The title
    /// is `title`, or the source's current title followed by " (Copy)"; the
    /// copy is not pinned. Only files are copied. The source is not touched.
    ///
    /// Retrying with the same key returns the original receipt even after
    /// the source was renamed or revised.
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
    ) -> Result<CommitReceipt, ArtifactError> {
        let record = self.owned(actor, id)?;
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
        let package = checked(&record, self.revision_of(&record, revision))?;
        let title = title.cloned().unwrap_or_else(|| record.title.copy_of());
        let receipt = self.store.create_artifact(NewArtifact {
            request: RequestIdentity {
                actor,
                key,
                fingerprint: hasher.finish(),
            },
            kind: record.kind,
            title: &title,
            package: &package,
            // The store refuses the copy unless its source is still outside
            // the trash when the copy commits.
            source: Some(id),
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
    fn authorized(
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
