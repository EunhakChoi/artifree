//! Local persistent storage: a single `SQLite` database file.
//!
//! Metadata, immutable revisions, request-key results, and file bytes live in
//! one database, so every commit is one `SQLite` transaction. Identical file
//! content is stored once and shared by revisions. The database uses WAL mode
//! with `synchronous=FULL`; a returned commit survives a process crash or power
//! loss on storage that honors flushes.
//!
//! Each operation opens its own connection. `SQLite` serializes writers with
//! its file lock, and `BEGIN IMMEDIATE` makes the conditional revision check
//! and the write one atomic step across threads and processes.
//!
//! Each commit appends one row to the change feed in its transaction; the
//! row's `AUTOINCREMENT` key is the feed sequence. The feed's random identity
//! is created with the database. The feed also keeps the latest commit time,
//! and each commit takes a later one, so times follow commit order even when
//! the clock is set back.
//!
//! Titles, pins, and the trash state are kept per metadata revision, apart
//! from source revisions, and every request key records the source and
//! metadata revision its request produced. A replay therefore reports the
//! artifact as of that request, also after later renames or a delete.
//!
//! An artifact in the trash keeps every row; only its `deleted_at` is set.
//! Each write decides its key and the trash first, in one order (see
//! `settle`).
//!
//! A purge (decision 0029) leaves a tombstone: the artifact row with its
//! `purged_at`, the revision and metadata headers, request keys, and change
//! feed entries stay, so every foreign key holds, the identifier stays taken,
//! and the feed keeps its sequences. File listings, titles, and content no
//! other revision refers to are deleted. Reads treat a purged artifact as
//! missing.
//!
//! Request keys and feed rows are retained indefinitely. Server profiles with
//! `PostgreSQL` and object storage remain planned.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::schema;
use artifree_domain::change::{ChangeSequence, FeedId};
use artifree_domain::identity::{ActorId, ArtifactId, Title, UnixMillis};
use artifree_domain::package::{FilePackage, PackagePath};
use artifree_domain::{ArtifactKind, MetadataRevision, SourceRevision};
use artifree_ports::artifact_store::{
    ArtifactListing, ArtifactRecord, ArtifactStore, ChangeKind, ChangePage, ChangeRecord,
    CommitReceipt, Delivery, FileEntry, ListOrder, MetadataReceipt, MetadataUpdate, NewArtifact,
    PurgeReceipt, PurgeRequest, RequestIdentity, RevisionCommit, RevisionRecord, StoreError,
    UnreferencedContent,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use sha2::{Digest, Sha256};

pub use crate::schema::SCHEMA_VERSION;

/// File name of the database inside a data directory. The server, backup, and
/// restore all use it.
pub const DATABASE_FILE: &str = "artifree.sqlite3";

const BUSY_TIMEOUT: Duration = Duration::from_secs(10);

/// Artifact storage in one local `SQLite` database file.
#[derive(Debug, Clone)]
pub struct SqliteStore {
    path: PathBuf,
}

impl SqliteStore {
    /// Opens or creates the database at `path` and upgrades its schema to
    /// the current version (the schema module, which also owns the
    /// access token table). A version 1 database gains the change feed; its
    /// earlier commits have no feed entries. A version 1 or 2 database gains
    /// metadata revisions, a version 1 to 3 database the latest commit time,
    /// a version 1 to 4 database the trash, with no artifact in it, a
    /// version 1 to 5 database the token table unless it has one, and a
    /// version 1 to 6 database the purge tombstone column.
    ///
    /// The upgrade runs in one transaction before the file is switched to WAL.
    /// An open that fails leaves the file byte for byte as it was: a newer
    /// schema version, a non-empty file without a version, and a failed
    /// upgrade step all leave it unchanged. An empty or missing file is a new
    /// database.
    ///
    /// # Errors
    ///
    /// [`StoreError::Unavailable`] when the file cannot be opened, uses a newer
    /// schema version, is not an Artifree database, or the schema cannot be
    /// created or upgraded.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let store = Self {
            path: path.to_owned(),
        };
        let mut connection = store.connect()?;
        // Upgrading first: a refused or failed open must not touch the file,
        // and the switch to WAL rewrites its header.
        schema::upgrade(&mut connection)?;
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(unavailable)?;
        Ok(store)
    }

    fn connect(&self) -> Result<Connection, StoreError> {
        let connection = Connection::open(&self.path).map_err(unavailable)?;
        configure(&connection).map_err(unavailable)?;
        Ok(connection)
    }

    fn write<T>(
        &self,
        operation: impl FnOnce(&Transaction<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.write_with(false, operation)
    }

    /// As [`Self::write`], for a write that deletes content for good: freed
    /// pages are overwritten with zeros (`secure_delete`), so the deleted
    /// bytes do not stay in the database file's free pages. Copies in the
    /// write-ahead log until it is checkpointed and truncated, and in earlier
    /// backups, remain (decision 0029).
    fn write_erasing<T>(
        &self,
        operation: impl FnOnce(&Transaction<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.write_with(true, operation)
    }

    /// A revision's header and files, read from one snapshot: a purge that
    /// commits meanwhile is seen whole or not at all, never as a header with
    /// no files. `between` runs after the header is read; only a test passes
    /// anything, to commit a purge at that point.
    fn revision_between(
        &self,
        id: &ArtifactId,
        revision: SourceRevision,
        between: impl FnOnce(),
    ) -> Result<Option<RevisionRecord>, StoreError> {
        let mut connection = self.connect()?;
        let snapshot = connection.transaction().map_err(unavailable)?;
        read_revision(&snapshot, id, revision, between)
    }

    /// Every revision of an artifact, newest first, from one snapshot like
    /// [`Self::revision_between`]; `between` runs after the numbers are read.
    fn revisions_between(
        &self,
        id: &ArtifactId,
        between: impl FnOnce(),
    ) -> Result<Vec<RevisionRecord>, StoreError> {
        let mut connection = self.connect()?;
        let snapshot = connection.transaction().map_err(unavailable)?;
        let numbers = {
            let mut statement = snapshot
                .prepare(
                    "SELECT r.revision FROM revisions r JOIN artifacts a ON a.id = r.artifact_id
                     WHERE r.artifact_id = ?1 AND a.purged_at IS NULL ORDER BY r.revision DESC",
                )
                .map_err(unavailable)?;
            let rows = statement
                .query_map([id.as_str()], |row| row.get::<_, i64>(0))
                .map_err(unavailable)?;
            let mut numbers = Vec::new();
            for number in rows {
                numbers.push(stored_revision(number.map_err(unavailable)?)?);
            }
            numbers
        };
        between();
        let mut records = Vec::new();
        for revision in numbers {
            records.push(revision_record(&snapshot, id, revision)?.ok_or_else(|| {
                StoreError::Unavailable("revision disappeared while listing".into())
            })?);
        }
        Ok(records)
    }

    fn write_with<T>(
        &self,
        erase: bool,
        operation: impl FnOnce(&Transaction<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let mut connection = self.connect()?;
        if erase {
            // Before the transaction: the setting cannot change inside one.
            connection
                .pragma_update(None, "secure_delete", true)
                .map_err(unavailable)?;
        }
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        // Dropping an uncommitted transaction rolls it back.
        let value = operation(&transaction)?;
        transaction.commit().map_err(unavailable)?;
        Ok(value)
    }
}

/// The settings every connection that writes the database has: the store's,
/// and backup and restore's upgrade and feed renewal.
pub(crate) fn configure(connection: &Connection) -> rusqlite::Result<()> {
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.pragma_update(None, "foreign_keys", true)?;
    connection.pragma_update(None, "synchronous", "FULL")
}

impl ArtifactStore for SqliteStore {
    fn create_artifact(&self, request: NewArtifact<'_>) -> Result<CommitReceipt, StoreError> {
        self.write(|transaction| {
            // A copy needs its source in place; any other create, nothing.
            let source = match request.source {
                Some(source) => in_place(transaction, source)?.map(|_| ()),
                None => Some(()),
            };
            match settle(key_use(transaction, request.request)?, source)? {
                Settled::Replay(id, position) => return replayed(transaction, &id, position),
                Settled::Proceed(()) => {}
            }
            let id = ArtifactId::from_random(random_bytes()?);
            let now = commit_time(transaction)?;
            let revision = SourceRevision(1);
            let metadata = MetadataRevision(1);
            transaction
                .execute(
                    "INSERT INTO artifacts (id, owner, kind, title, pinned, current_revision,
                         metadata_revision, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7, ?7)",
                    params![
                        id.as_str(),
                        request.request.actor.as_str(),
                        request.kind.as_str(),
                        request.title.as_str(),
                        sql_revision(revision)?,
                        sql_metadata(metadata)?,
                        now.0
                    ],
                )
                .map_err(unavailable)?;
            insert_metadata(
                transaction,
                &id,
                metadata,
                MetadataValues {
                    title: request.title,
                    pinned: false,
                    deleted_at: None,
                },
                request.request.actor,
                now,
            )?;
            insert_revision(
                transaction,
                &id,
                revision,
                request.request,
                request.package,
                now,
            )?;
            let position = (revision, metadata);
            record_key(transaction, request.request, &id, position, now)?;
            record_change(transaction, &id, ChangeKind::Created, position, now)?;
            committed(transaction, &id, position)
        })
    }

    fn commit_revision(&self, request: RevisionCommit<'_>) -> Result<CommitReceipt, StoreError> {
        self.write(|transaction| {
            let target = in_place(transaction, request.artifact)?;
            let current = match settle(key_use(transaction, request.request)?, target)? {
                Settled::Replay(id, position) => return replayed(transaction, &id, position),
                Settled::Proceed(current) => current,
            };
            if current.current_revision != request.base {
                return Err(StoreError::RevisionConflict {
                    current: current.current_revision,
                });
            }
            if same_files(transaction, request.artifact, request.base, request.package)? {
                return Err(StoreError::Unchanged);
            }
            let next = SourceRevision(current.current_revision.0 + 1);
            let now = commit_time(transaction)?;
            transaction
                .execute(
                    "UPDATE artifacts SET current_revision = ?2, updated_at = ?3 WHERE id = ?1",
                    params![request.artifact.as_str(), sql_revision(next)?, now.0],
                )
                .map_err(unavailable)?;
            insert_revision(
                transaction,
                request.artifact,
                next,
                request.request,
                request.package,
                now,
            )?;
            let position = (next, current.metadata_revision);
            record_key(
                transaction,
                request.request,
                request.artifact,
                position,
                now,
            )?;
            record_change(
                transaction,
                request.artifact,
                ChangeKind::Revised,
                position,
                now,
            )?;
            committed(transaction, request.artifact, position)
        })
    }

    fn update_metadata(&self, request: MetadataUpdate<'_>) -> Result<MetadataReceipt, StoreError> {
        self.write(|transaction| {
            // A recovery is the one update that reaches the trash.
            let target = artifact_record(transaction, request.artifact)?
                .filter(|record| record.deleted_at.is_none() || request.recovers());
            let current = match settle(key_use(transaction, request.request)?, target)? {
                Settled::Replay(id, position) => {
                    return Ok(MetadataReceipt {
                        artifact: snapshot(transaction, &id, position)?,
                        delivery: Delivery::Replayed,
                    });
                }
                Settled::Proceed(current) => current,
            };
            if current.metadata_revision != request.base {
                return Err(StoreError::MetadataConflict {
                    current: current.metadata_revision,
                });
            }
            let now = commit_time(transaction)?;
            let values = MetadataValues {
                title: request.title.unwrap_or(&current.title),
                pinned: request.pinned.unwrap_or(current.pinned),
                // Only a recovery reaches the trash, so a delete starts now.
                deleted_at: match request.deleted {
                    Some(true) => Some(now),
                    Some(false) => None,
                    None => current.deleted_at,
                },
            };
            let kind = match (current.deleted_at, values.deleted_at) {
                (None, Some(_)) => ChangeKind::Deleted,
                (Some(_), None) => ChangeKind::Recovered,
                _ => ChangeKind::MetadataUpdated,
            };
            let metadata = if kind == ChangeKind::MetadataUpdated
                && *values.title == current.title
                && values.pinned == current.pinned
            {
                current.metadata_revision
            } else {
                let next = MetadataRevision(current.metadata_revision.0 + 1);
                transaction
                    .execute(
                        "UPDATE artifacts
                         SET title = ?2, pinned = ?3, metadata_revision = ?4, deleted_at = ?5
                         WHERE id = ?1",
                        params![
                            request.artifact.as_str(),
                            values.title.as_str(),
                            values.pinned,
                            sql_metadata(next)?,
                            values.deleted_at.map(|time| time.0)
                        ],
                    )
                    .map_err(unavailable)?;
                insert_metadata(
                    transaction,
                    request.artifact,
                    next,
                    values,
                    request.request.actor,
                    now,
                )?;
                record_change(
                    transaction,
                    request.artifact,
                    kind,
                    (current.current_revision, next),
                    now,
                )?;
                next
            };
            let position = (current.current_revision, metadata);
            record_key(
                transaction,
                request.request,
                request.artifact,
                position,
                now,
            )?;
            Ok(MetadataReceipt {
                artifact: snapshot(transaction, request.artifact, position)?,
                delivery: Delivery::Committed,
            })
        })
    }

    fn replay_create(
        &self,
        request: RequestIdentity<'_>,
    ) -> Result<Option<CommitReceipt>, StoreError> {
        let mut connection = self.connect()?;
        // One read snapshot, so the key and the artifact it names agree.
        let snapshot = connection.transaction().map_err(unavailable)?;
        match key_use(&snapshot, request)? {
            KeyUse::Same(id, position) => replayed(&snapshot, &id, position).map(Some),
            KeyUse::New | KeyUse::Reused | KeyUse::Gone(..) => Ok(None),
        }
    }

    fn purge_artifact(&self, request: PurgeRequest<'_>) -> Result<PurgeReceipt, StoreError> {
        self.write_erasing(|transaction| {
            // The order of `settle`: a replay, then not found, then a reused
            // key. The fingerprint names the operation, so a key that finds
            // its artifact purged was the purge's own.
            let target = artifact_record(transaction, request.artifact)?;
            match (key_use(transaction, request.request)?, &target) {
                (KeyUse::Gone(id, position), _) => {
                    return Ok(PurgeReceipt {
                        metadata_revision: position.1,
                        purged_at: purged_at(transaction, &id)?,
                        artifact: id,
                        delivery: Delivery::Replayed,
                    });
                }
                (KeyUse::Same(..), _) => {
                    return Err(corrupt("the key of a purge names an artifact in place"));
                }
                (_, None) => return Err(StoreError::NotFound),
                (KeyUse::Reused, Some(_)) => return Err(StoreError::RequestKeyReused),
                (KeyUse::New, Some(_)) => {}
            }
            let current = target.ok_or(StoreError::NotFound)?;
            if current.deleted_at.is_none() {
                return Err(StoreError::NotInTrash);
            }
            if current.metadata_revision != request.base {
                return Err(StoreError::MetadataConflict {
                    current: current.metadata_revision,
                });
            }
            let now = commit_time(transaction)?;
            let next = MetadataRevision(current.metadata_revision.0 + 1);
            remove_files(transaction, request.artifact)?;
            // Titles are scrubbed from every metadata revision: they describe
            // what was removed. The tombstone keeps only the headers.
            transaction
                .execute(
                    "UPDATE metadata_revisions SET title = '' WHERE artifact_id = ?1",
                    [request.artifact.as_str()],
                )
                .map_err(unavailable)?;
            transaction
                .execute(
                    "INSERT INTO metadata_revisions
                         (artifact_id, metadata_revision, title, pinned, deleted_at, created_by,
                          created_at)
                     VALUES (?1, ?2, '', ?3, ?4, ?5, ?6)",
                    params![
                        request.artifact.as_str(),
                        sql_metadata(next)?,
                        current.pinned,
                        current.deleted_at.map(|time| time.0),
                        request.request.actor.as_str(),
                        now.0
                    ],
                )
                .map_err(unavailable)?;
            transaction
                .execute(
                    "UPDATE artifacts
                     SET title = '', metadata_revision = ?2, purged_at = ?3 WHERE id = ?1",
                    params![request.artifact.as_str(), sql_metadata(next)?, now.0],
                )
                .map_err(unavailable)?;
            let position = (current.current_revision, next);
            record_key(
                transaction,
                request.request,
                request.artifact,
                position,
                now,
            )?;
            record_change(
                transaction,
                request.artifact,
                ChangeKind::Purged,
                position,
                now,
            )?;
            Ok(PurgeReceipt {
                artifact: request.artifact.clone(),
                metadata_revision: next,
                purged_at: now,
                delivery: Delivery::Committed,
            })
        })
    }

    fn unreferenced_content(&self) -> Result<UnreferencedContent, StoreError> {
        let mut connection = self.connect()?;
        let snapshot = connection.transaction().map_err(unavailable)?;
        count_unreferenced(&snapshot)
    }

    fn remove_unreferenced_content(&self) -> Result<UnreferencedContent, StoreError> {
        // The count and the delete are one write transaction: a commit that
        // refers to content is wholly before it, which keeps the content, or
        // wholly after, which stores it again.
        self.write_erasing(|transaction| {
            let found = count_unreferenced(transaction)?;
            transaction
                .execute(
                    "DELETE FROM blobs WHERE NOT EXISTS
                         (SELECT 1 FROM revision_files f WHERE f.digest = blobs.digest)",
                    [],
                )
                .map_err(unavailable)?;
            Ok(found)
        })
    }

    fn artifact(&self, id: &ArtifactId) -> Result<Option<ArtifactRecord>, StoreError> {
        artifact_record(&self.connect()?, id)
    }

    fn list_artifacts(
        &self,
        listing: ArtifactListing<'_>,
    ) -> Result<Vec<ArtifactRecord>, StoreError> {
        let connection = self.connect()?;
        let mut statement = connection
            .prepare(match listing.order {
                ListOrder::Updated => LIST_BY_UPDATE,
                ListOrder::Created => LIST_BY_CREATION,
            })
            .map_err(unavailable)?;
        let rows = statement
            .query_map(
                params![
                    listing.owner.as_str(),
                    listing.search,
                    listing.pinned,
                    listing.after.map(|position| position.at.0),
                    listing.after.map(|position| position.id.as_str()),
                    i64::try_from(listing.limit).unwrap_or(i64::MAX),
                    listing.deleted
                ],
                RawArtifact::from_row,
            )
            .map_err(unavailable)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row.map_err(unavailable)?.into_record()?);
        }
        Ok(records)
    }

    fn revisions(&self, id: &ArtifactId) -> Result<Vec<RevisionRecord>, StoreError> {
        self.revisions_between(id, || {})
    }

    fn revision(
        &self,
        id: &ArtifactId,
        revision: SourceRevision,
    ) -> Result<Option<RevisionRecord>, StoreError> {
        self.revision_between(id, revision, || {})
    }

    fn revision_package(
        &self,
        id: &ArtifactId,
        revision: SourceRevision,
    ) -> Result<Option<FilePackage>, StoreError> {
        let connection = self.connect()?;
        let mut statement = connection
            .prepare(
                "SELECT f.path, b.content FROM revision_files f
                 JOIN blobs b ON b.digest = f.digest
                 WHERE f.artifact_id = ?1 AND f.revision = ?2 ORDER BY f.path",
            )
            .map_err(unavailable)?;
        let rows = statement
            .query_map(params![id.as_str(), sql_revision(revision)?], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
            })
            .map_err(unavailable)?;
        let mut files = Vec::new();
        for row in rows {
            let (path, content) = row.map_err(unavailable)?;
            files.push((stored_path(&path)?, content));
        }
        if files.is_empty() {
            return Ok(None);
        }
        FilePackage::new(files)
            .map(Some)
            .map_err(|error| StoreError::Unavailable(format!("stored package is invalid: {error}")))
    }

    fn revision_file(
        &self,
        id: &ArtifactId,
        revision: SourceRevision,
        path: &PackagePath,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        self.connect()?
            .query_row(
                "SELECT b.content FROM revision_files f JOIN blobs b ON b.digest = f.digest
                 WHERE f.artifact_id = ?1 AND f.revision = ?2 AND f.path = ?3",
                params![id.as_str(), sql_revision(revision)?, path.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(unavailable)
    }

    fn changes(
        &self,
        owner: &ActorId,
        after: ChangeSequence,
        limit: usize,
    ) -> Result<ChangePage, StoreError> {
        let mut connection = self.connect()?;
        // One read transaction: the page and the head come from one snapshot.
        let snapshot = connection.transaction().map_err(unavailable)?;
        let feed: Vec<u8> = snapshot
            .query_row("SELECT id FROM feed", [], |row| row.get(0))
            .map_err(unavailable)?;
        let feed = FeedId::from_random(feed.try_into().map_err(|_| corrupt("feed id length"))?);
        let head: Option<i64> = snapshot
            .query_row(
                "SELECT seq FROM sqlite_sequence WHERE name = 'changes'",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(unavailable)?;
        let mut statement = snapshot
            .prepare(
                "SELECT c.sequence, c.artifact_id, c.kind, c.revision, c.metadata_revision,
                     c.committed_at
                 FROM changes c
                 JOIN artifacts a ON a.id = c.artifact_id
                 WHERE c.sequence > ?1 AND a.owner = ?2
                 ORDER BY c.sequence LIMIT ?3",
            )
            .map_err(unavailable)?;
        let rows = statement
            .query_map(
                params![
                    i64::try_from(after.0).map_err(corrupt)?,
                    owner.as_str(),
                    i64::try_from(limit).unwrap_or(i64::MAX)
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .map_err(unavailable)?;
        let mut changes = Vec::new();
        for row in rows {
            let (sequence, artifact, kind, revision, metadata, committed_at) =
                row.map_err(unavailable)?;
            changes.push(ChangeRecord {
                sequence: stored_sequence(sequence)?,
                artifact: artifact.parse().map_err(corrupt)?,
                kind: match kind.as_str() {
                    "created" => ChangeKind::Created,
                    "revised" => ChangeKind::Revised,
                    "metadata_updated" => ChangeKind::MetadataUpdated,
                    "deleted" => ChangeKind::Deleted,
                    "recovered" => ChangeKind::Recovered,
                    "purged" => ChangeKind::Purged,
                    other => return Err(corrupt(format!("change kind {other}"))),
                },
                revision: stored_revision(revision)?,
                metadata_revision: stored_metadata(metadata)?,
                committed_at: UnixMillis(committed_at),
            });
        }
        Ok(ChangePage {
            feed,
            changes,
            head: stored_sequence(head.unwrap_or(0))?,
        })
    }
}

/// A source revision and the metadata revision that went with it.
type Position = (SourceRevision, MetadataRevision);

/// What the request key says about a request.
enum KeyUse {
    /// No earlier request used the key.
    New,
    /// The same request used it, and left its artifact at this position.
    Same(ArtifactId, Position),
    /// The same request used it, and its artifact was purged since. Only a
    /// retry of the purge itself can still be answered.
    Gone(ArtifactId, Position),
    /// A request with different content used it.
    Reused,
}

fn key_use(
    transaction: &Transaction<'_>,
    request: RequestIdentity<'_>,
) -> Result<KeyUse, StoreError> {
    let stored: Option<(Vec<u8>, String, i64, i64, bool)> = transaction
        .query_row(
            "SELECT k.fingerprint, k.artifact_id, k.revision, k.metadata_revision,
                 a.purged_at IS NOT NULL
             FROM request_keys k JOIN artifacts a ON a.id = k.artifact_id
             WHERE k.actor = ?1 AND k.request_key = ?2",
            params![request.actor.as_str(), request.key.as_str()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()
        .map_err(unavailable)?;
    let Some((fingerprint, artifact_id, revision, metadata, purged)) = stored else {
        return Ok(KeyUse::New);
    };
    // The fingerprint covers the operation, so a key used by one kind of
    // request never replays as another.
    if fingerprint.as_slice() != request.fingerprint.0.as_slice() {
        return Ok(KeyUse::Reused);
    }
    let id = artifact_id.parse().map_err(corrupt)?;
    let position = (stored_revision(revision)?, stored_metadata(metadata)?);
    Ok(if purged {
        KeyUse::Gone(id, position)
    } else {
        KeyUse::Same(id, position)
    })
}

/// How a write goes on after [`settle`].
enum Settled<T> {
    /// It replays the same request, which left this artifact at this
    /// position.
    Replay(ArtifactId, Position),
    /// It is a new request, and its target is this.
    Proceed(T),
}

/// The first answer to every write, in one order (decision 0013): the same
/// request replays; a write whose target is missing or in the trash is not
/// found, also when its key was used for another request; and only then is a
/// reused key refused. `target` is what the write needs in place: the
/// artifact, or a copy's source. A request whose artifact was purged is not
/// found like one in the trash, but it cannot replay (decision 0029).
fn settle<T>(key: KeyUse, target: Option<T>) -> Result<Settled<T>, StoreError> {
    match (key, target) {
        (KeyUse::Same(id, position), _) => Ok(Settled::Replay(id, position)),
        (KeyUse::Gone(..), _) | (_, None) => Err(StoreError::NotFound),
        (KeyUse::Reused, Some(_)) => Err(StoreError::RequestKeyReused),
        (KeyUse::New, Some(target)) => Ok(Settled::Proceed(target)),
    }
}

/// The receipt of an earlier create or revision commit, replayed.
fn replayed(
    transaction: &Transaction<'_>,
    id: &ArtifactId,
    position: Position,
) -> Result<CommitReceipt, StoreError> {
    let mut receipt = committed(transaction, id, position)?;
    receipt.delivery = Delivery::Replayed;
    Ok(receipt)
}

/// The artifact, when it exists outside the trash.
fn in_place(
    transaction: &Transaction<'_>,
    id: &ArtifactId,
) -> Result<Option<ArtifactRecord>, StoreError> {
    Ok(artifact_record(transaction, id)?.filter(|record| record.deleted_at.is_none()))
}

/// When a purged artifact was removed.
fn purged_at(connection: &Connection, id: &ArtifactId) -> Result<UnixMillis, StoreError> {
    let at: Option<i64> = connection
        .query_row(
            "SELECT purged_at FROM artifacts WHERE id = ?1",
            [id.as_str()],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    at.map(UnixMillis)
        .ok_or_else(|| corrupt("a purged artifact has no purge time"))
}

/// Deletes the listings of all files of an artifact's revisions, and the
/// content that no other revision file refers to afterwards. Content shared
/// with a copy, or with another revision, stays.
fn remove_files(transaction: &Transaction<'_>, id: &ArtifactId) -> Result<(), StoreError> {
    let digests: Vec<Vec<u8>> = transaction
        .prepare("SELECT DISTINCT digest FROM revision_files WHERE artifact_id = ?1")
        .map_err(unavailable)?
        .query_map([id.as_str()], |row| row.get(0))
        .map_err(unavailable)?
        .collect::<Result<_, _>>()
        .map_err(unavailable)?;
    transaction
        .execute(
            "DELETE FROM revision_files WHERE artifact_id = ?1",
            [id.as_str()],
        )
        .map_err(unavailable)?;
    let mut remove = transaction
        .prepare_cached(
            "DELETE FROM blobs WHERE digest = ?1
               AND NOT EXISTS (SELECT 1 FROM revision_files f WHERE f.digest = ?1)",
        )
        .map_err(unavailable)?;
    for digest in digests {
        remove.execute([digest]).map_err(unavailable)?;
    }
    Ok(())
}

/// How much content no revision file refers to.
fn count_unreferenced(connection: &Connection) -> Result<UnreferencedContent, StoreError> {
    let (blobs, bytes): (i64, i64) = connection
        .query_row(
            "SELECT count(*), coalesce(sum(length(content)), 0) FROM blobs b
             WHERE NOT EXISTS (SELECT 1 FROM revision_files f WHERE f.digest = b.digest)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(unavailable)?;
    Ok(UnreferencedContent {
        blobs: u64::try_from(blobs).map_err(corrupt)?,
        bytes: u64::try_from(bytes).map_err(corrupt)?,
    })
}

fn insert_revision(
    transaction: &Transaction<'_>,
    id: &ArtifactId,
    revision: SourceRevision,
    request: RequestIdentity<'_>,
    package: &FilePackage,
    now: UnixMillis,
) -> Result<(), StoreError> {
    let number = sql_revision(revision)?;
    transaction
        .execute(
            "INSERT INTO revisions (artifact_id, revision, created_by, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![id.as_str(), number, request.actor.as_str(), now.0],
        )
        .map_err(unavailable)?;
    let mut blob = transaction
        .prepare_cached("INSERT OR IGNORE INTO blobs (digest, content) VALUES (?1, ?2)")
        .map_err(unavailable)?;
    let mut file = transaction
        .prepare_cached(
            "INSERT INTO revision_files (artifact_id, revision, path, size, digest) VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .map_err(unavailable)?;
    for (path, content) in package.files() {
        let digest: [u8; 32] = Sha256::digest(content).into();
        blob.execute(params![digest.as_slice(), content])
            .map_err(unavailable)?;
        let size = i64::try_from(content.len()).map_err(corrupt)?;
        file.execute(params![
            id.as_str(),
            number,
            path.as_str(),
            size,
            digest.as_slice()
        ])
        .map_err(unavailable)?;
    }
    Ok(())
}

/// The values one metadata revision records.
#[derive(Clone, Copy)]
struct MetadataValues<'a> {
    title: &'a Title,
    pinned: bool,
    deleted_at: Option<UnixMillis>,
}

/// Records `values` as `metadata` of the artifact.
fn insert_metadata(
    transaction: &Transaction<'_>,
    id: &ArtifactId,
    metadata: MetadataRevision,
    values: MetadataValues<'_>,
    actor: &ActorId,
    now: UnixMillis,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "INSERT INTO metadata_revisions
                 (artifact_id, metadata_revision, title, pinned, deleted_at, created_by, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id.as_str(),
                sql_metadata(metadata)?,
                values.title.as_str(),
                values.pinned,
                values.deleted_at.map(|time| time.0),
                actor.as_str(),
                now.0
            ],
        )
        .map_err(unavailable)?;
    Ok(())
}

/// Records that the request's key produced `position` of the artifact.
fn record_key(
    transaction: &Transaction<'_>,
    request: RequestIdentity<'_>,
    id: &ArtifactId,
    (revision, metadata): Position,
    now: UnixMillis,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "INSERT INTO request_keys
                 (actor, request_key, fingerprint, artifact_id, revision, metadata_revision, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                request.actor.as_str(),
                request.key.as_str(),
                request.fingerprint.0.as_slice(),
                id.as_str(),
                sql_revision(revision)?,
                sql_metadata(metadata)?,
                now.0
            ],
        )
        .map_err(unavailable)?;
    Ok(())
}

/// Appends a commit that left the artifact at `position` to the change feed.
fn record_change(
    transaction: &Transaction<'_>,
    id: &ArtifactId,
    kind: ChangeKind,
    (revision, metadata): Position,
    now: UnixMillis,
) -> Result<(), StoreError> {
    let kind = match kind {
        ChangeKind::Created => "created",
        ChangeKind::Revised => "revised",
        ChangeKind::MetadataUpdated => "metadata_updated",
        ChangeKind::Deleted => "deleted",
        ChangeKind::Recovered => "recovered",
        ChangeKind::Purged => "purged",
        _ => {
            return Err(StoreError::Unavailable(
                "this store cannot record that change".into(),
            ));
        }
    };
    transaction
        .execute(
            "INSERT INTO changes (artifact_id, kind, revision, metadata_revision, committed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id.as_str(),
                kind,
                sql_revision(revision)?,
                sql_metadata(metadata)?,
                now.0
            ],
        )
        .map_err(unavailable)?;
    Ok(())
}

/// The receipt of the commit that created the source revision of
/// `position`, also when later commits followed it.
fn committed(
    connection: &Connection,
    id: &ArtifactId,
    position: Position,
) -> Result<CommitReceipt, StoreError> {
    let revision = revision_record(connection, id, position.0)?
        .ok_or_else(|| StoreError::Unavailable("committed revision is missing".into()))?;
    Ok(CommitReceipt {
        artifact: snapshot(connection, id, position)?,
        revision,
        delivery: Delivery::Committed,
    })
}

/// The artifact as it was at `position`: owner, kind, and creation time
/// never change; title, pin, and trash state are those of the metadata
/// revision, and the update time is that of the source revision.
fn snapshot(
    connection: &Connection,
    id: &ArtifactId,
    (revision, metadata): Position,
) -> Result<ArtifactRecord, StoreError> {
    connection
        .query_row(
            "SELECT a.id, a.owner, a.kind, m.title, m.pinned, r.revision, m.metadata_revision,
                 a.created_at, r.created_at, m.deleted_at
             FROM artifacts a
             JOIN revisions r ON r.artifact_id = a.id AND r.revision = ?2
             JOIN metadata_revisions m ON m.artifact_id = a.id AND m.metadata_revision = ?3
             WHERE a.id = ?1",
            params![
                id.as_str(),
                sql_revision(revision)?,
                sql_metadata(metadata)?
            ],
            RawArtifact::from_row,
        )
        .optional()
        .map_err(unavailable)?
        .ok_or_else(|| StoreError::Unavailable("committed state is missing".into()))?
        .into_record()
}

fn artifact_record(
    connection: &Connection,
    id: &ArtifactId,
) -> Result<Option<ArtifactRecord>, StoreError> {
    connection
        .query_row(
            "SELECT id, owner, kind, title, pinned, current_revision, metadata_revision,
                 created_at, updated_at, deleted_at
             FROM artifacts WHERE id = ?1 AND purged_at IS NULL",
            [id.as_str()],
            RawArtifact::from_row,
        )
        .optional()
        .map_err(unavailable)?
        .map(RawArtifact::into_record)
        .transpose()
}

/// An owner's artifacts by update time. `?2` is the search text, `?3` the
/// pin, `?4` and `?5` the position to continue after, `?6` the limit, and
/// `?7` whether to list the trash. `lower` folds ASCII letters only.
const LIST_BY_UPDATE: &str = "
SELECT id, owner, kind, title, pinned, current_revision, metadata_revision, created_at, updated_at,
    deleted_at
FROM artifacts
WHERE owner = ?1
  AND purged_at IS NULL
  AND (deleted_at IS NOT NULL) = ?7
  AND (?2 IS NULL OR instr(lower(title), lower(?2)) > 0)
  AND (?3 IS NULL OR pinned = ?3)
  AND (?4 IS NULL OR updated_at < ?4 OR (updated_at = ?4 AND id > ?5))
ORDER BY updated_at DESC, id
LIMIT ?6";

/// As [`LIST_BY_UPDATE`], by creation time.
const LIST_BY_CREATION: &str = "
SELECT id, owner, kind, title, pinned, current_revision, metadata_revision, created_at, updated_at,
    deleted_at
FROM artifacts
WHERE owner = ?1
  AND purged_at IS NULL
  AND (deleted_at IS NOT NULL) = ?7
  AND (?2 IS NULL OR instr(lower(title), lower(?2)) > 0)
  AND (?3 IS NULL OR pinned = ?3)
  AND (?4 IS NULL OR created_at < ?4 OR (created_at = ?4 AND id > ?5))
ORDER BY created_at DESC, id
LIMIT ?6";

/// Whether `package` has exactly the paths and bytes of a stored revision,
/// compared by digest.
fn same_files(
    connection: &Connection,
    id: &ArtifactId,
    revision: SourceRevision,
    package: &FilePackage,
) -> Result<bool, StoreError> {
    let Some(stored) = revision_record(connection, id, revision)? else {
        return Ok(false);
    };
    let digests: BTreeMap<&PackagePath, [u8; 32]> = stored
        .files
        .iter()
        .map(|entry| (&entry.path, entry.sha256))
        .collect();
    Ok(digests.len() == package.len()
        && package
            .files()
            .all(|(path, content)| digests.get(path) == Some(&Sha256::digest(content).into())))
}

fn revision_record(
    connection: &Connection,
    id: &ArtifactId,
    revision: SourceRevision,
) -> Result<Option<RevisionRecord>, StoreError> {
    read_revision(connection, id, revision, || {})
}

/// The header, then the files, of a revision. The two reads are consistent
/// only within one transaction; `between` runs after the header.
fn read_revision(
    connection: &Connection,
    id: &ArtifactId,
    revision: SourceRevision,
    between: impl FnOnce(),
) -> Result<Option<RevisionRecord>, StoreError> {
    let number = sql_revision(revision)?;
    let header: Option<(String, i64)> = connection
        .query_row(
            "SELECT r.created_by, r.created_at FROM revisions r
             JOIN artifacts a ON a.id = r.artifact_id
             WHERE r.artifact_id = ?1 AND r.revision = ?2 AND a.purged_at IS NULL",
            params![id.as_str(), number],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(unavailable)?;
    let Some((created_by, created_at)) = header else {
        return Ok(None);
    };
    between();
    let mut statement = connection
        .prepare_cached(
            "SELECT path, size, digest FROM revision_files
             WHERE artifact_id = ?1 AND revision = ?2 ORDER BY path",
        )
        .map_err(unavailable)?;
    let rows = statement
        .query_map(params![id.as_str(), number], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })
        .map_err(unavailable)?;
    let mut files = Vec::new();
    for row in rows {
        let (path, size, digest) = row.map_err(unavailable)?;
        files.push(FileEntry {
            path: stored_path(&path)?,
            size: u64::try_from(size).map_err(corrupt)?,
            sha256: digest
                .try_into()
                .map_err(|_| corrupt("a file digest is not 32 bytes"))?,
        });
    }
    Ok(Some(RevisionRecord {
        revision,
        created_by: created_by.parse().map_err(corrupt)?,
        created_at: UnixMillis(created_at),
        files,
    }))
}

struct RawArtifact {
    id: String,
    owner: String,
    kind: String,
    title: String,
    pinned: bool,
    current_revision: i64,
    metadata_revision: i64,
    created_at: i64,
    updated_at: i64,
    deleted_at: Option<i64>,
}

impl RawArtifact {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            owner: row.get(1)?,
            kind: row.get(2)?,
            title: row.get(3)?,
            pinned: row.get(4)?,
            current_revision: row.get(5)?,
            metadata_revision: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
            deleted_at: row.get(9)?,
        })
    }

    fn into_record(self) -> Result<ArtifactRecord, StoreError> {
        let kind: ArtifactKind = self.kind.parse().map_err(corrupt)?;
        let title: Title = self.title.parse().map_err(corrupt)?;
        Ok(ArtifactRecord {
            id: self.id.parse().map_err(corrupt)?,
            owner: self.owner.parse().map_err(corrupt)?,
            kind,
            title,
            pinned: self.pinned,
            current_revision: stored_revision(self.current_revision)?,
            metadata_revision: stored_metadata(self.metadata_revision)?,
            deleted_at: self.deleted_at.map(UnixMillis),
            created_at: UnixMillis(self.created_at),
            updated_at: UnixMillis(self.updated_at),
        })
    }
}

fn stored_path(value: &str) -> Result<PackagePath, StoreError> {
    value.parse().map_err(corrupt)
}

fn stored_revision(value: i64) -> Result<SourceRevision, StoreError> {
    u64::try_from(value).map(SourceRevision).map_err(corrupt)
}

fn stored_metadata(value: i64) -> Result<MetadataRevision, StoreError> {
    u64::try_from(value).map(MetadataRevision).map_err(corrupt)
}

fn sql_metadata(metadata: MetadataRevision) -> Result<i64, StoreError> {
    i64::try_from(metadata.0).map_err(corrupt)
}

fn stored_sequence(value: i64) -> Result<ChangeSequence, StoreError> {
    u64::try_from(value).map(ChangeSequence).map_err(corrupt)
}

fn sql_revision(revision: SourceRevision) -> Result<i64, StoreError> {
    i64::try_from(revision.0).map_err(corrupt)
}

/// The time of a commit: the clock, or one millisecond after the latest
/// commit when the clock reads that time or earlier, as after it is set back.
/// Commit times then follow commit order, which update and creation order
/// rely on. Records the time as the latest.
fn commit_time(transaction: &Transaction<'_>) -> Result<UnixMillis, StoreError> {
    let clock = now()?;
    let latest: i64 = transaction
        .query_row("SELECT committed_at FROM feed", [], |row| row.get(0))
        .map_err(unavailable)?;
    let time = if latest < clock.0 {
        clock.0
    } else {
        latest
            .checked_add(1)
            .ok_or_else(|| corrupt("the latest commit time is out of range"))?
    };
    transaction
        .execute("UPDATE feed SET committed_at = ?1", [time])
        .map_err(unavailable)?;
    Ok(UnixMillis(time))
}

fn now() -> Result<UnixMillis, StoreError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| {
            StoreError::Unavailable(format!("system clock is before 1970: {error}"))
        })?;
    i64::try_from(elapsed.as_millis())
        .map(UnixMillis)
        .map_err(corrupt)
}

pub(crate) fn random_bytes() -> Result<[u8; 16], StoreError> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes)
        .map_err(|error| StoreError::Unavailable(format!("no system randomness: {error}")))?;
    Ok(bytes)
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Used as a map_err adapter, which passes the error by value."
)]
pub(crate) fn unavailable(error: rusqlite::Error) -> StoreError {
    StoreError::Unavailable(error.to_string())
}

fn corrupt(error: impl std::fmt::Display) -> StoreError {
    StoreError::Unavailable(format!("stored data is invalid: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use artifree_domain::identity::RequestKey;
    use artifree_ports::artifact_store::RequestFingerprint;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// A store in its own directory, with one artifact of two files in the
    /// trash, ready to be purged.
    struct Trashed {
        directory: PathBuf,
        store: SqliteStore,
        id: ArtifactId,
        owner: ActorId,
    }

    impl Trashed {
        fn new(name: &str) -> Result<Self, Box<dyn std::error::Error>> {
            let directory = std::env::temp_dir()
                .join(format!("artifree-snapshot-{}-{name}", std::process::id()));
            drop(std::fs::remove_dir_all(&directory));
            std::fs::create_dir_all(&directory)?;
            let store = SqliteStore::open(&directory.join(DATABASE_FILE))?;
            let owner: ActorId = "owner".parse()?;
            let key: RequestKey = "create-key".parse()?;
            let title: Title = "Demo".parse()?;
            let package = FilePackage::new(vec![
                ("index.html".parse()?, b"<h1>a</h1>".to_vec()),
                ("style.css".parse()?, b"h1{}".to_vec()),
            ])?;
            let created = store.create_artifact(NewArtifact {
                request: RequestIdentity {
                    actor: &owner,
                    key: &key,
                    fingerprint: RequestFingerprint([1; 32]),
                },
                kind: ArtifactKind::App,
                title: &title,
                package: &package,
                source: None,
            })?;
            let delete_key: RequestKey = "delete-key".parse()?;
            store.update_metadata(MetadataUpdate {
                request: RequestIdentity {
                    actor: &owner,
                    key: &delete_key,
                    fingerprint: RequestFingerprint([2; 32]),
                },
                artifact: &created.artifact.id,
                base: MetadataRevision(1),
                title: None,
                pinned: None,
                deleted: Some(true),
            })?;
            Ok(Self {
                directory,
                store,
                id: created.artifact.id,
                owner,
            })
        }

        /// Purges the artifact; the commit is complete when this returns.
        fn purge(&self) -> Result<(), Box<dyn std::error::Error>> {
            let key: RequestKey = "purge-key".parse()?;
            self.store.purge_artifact(PurgeRequest {
                request: RequestIdentity {
                    actor: &self.owner,
                    key: &key,
                    fingerprint: RequestFingerprint([3; 32]),
                },
                artifact: &self.id,
                base: MetadataRevision(2),
            })?;
            Ok(())
        }
    }

    impl Drop for Trashed {
        fn drop(&mut self) {
            drop(std::fs::remove_dir_all(&self.directory));
        }
    }

    /// The control: read with no transaction, a purge between the header and
    /// the files tears the read. This is what the snapshot prevents, and it
    /// shows that the hook really commits the purge at that point.
    #[test]
    fn without_a_snapshot_a_purge_between_the_reads_tears_the_revision() -> TestResult {
        let fixture = Trashed::new("control")?;
        let connection = fixture.store.connect()?;
        let mut purged = Ok(());
        let torn = read_revision(&connection, &fixture.id, SourceRevision(1), || {
            purged = fixture.purge();
        })?;
        purged?;
        assert_eq!(
            torn.map(|record| record.files.len()),
            Some(0),
            "the control must reproduce the torn read"
        );
        Ok(())
    }

    #[test]
    fn a_purge_between_the_header_and_the_files_does_not_tear_a_revision() -> TestResult {
        let fixture = Trashed::new("revision")?;
        let mut purged = Ok(());
        let record = fixture
            .store
            .revision_between(&fixture.id, SourceRevision(1), || {
                purged = fixture.purge();
            })?
            .ok_or("the snapshot began before the purge")?;
        purged?;
        let paths: Vec<_> = record.files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, ["index.html", "style.css"]);
        // Once the purge is committed a new read sees nothing at all.
        assert_eq!(
            fixture.store.revision(&fixture.id, SourceRevision(1))?,
            None
        );
        Ok(())
    }

    #[test]
    fn a_purge_after_the_numbers_are_listed_does_not_cut_the_listing_short() -> TestResult {
        let fixture = Trashed::new("listing")?;
        let mut purged = Ok(());
        let records = fixture.store.revisions_between(&fixture.id, || {
            purged = fixture.purge();
        })?;
        purged?;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].files.len(), 2);
        assert!(fixture.store.revisions(&fixture.id)?.is_empty());
        Ok(())
    }
}
