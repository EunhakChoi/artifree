//! Backups taken while the store is written, and restores of them.

mod common;

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use artifree_domain::change::ChangeSequence;
use artifree_domain::identity::{ActorId, ArtifactId, RequestKey, TokenId, UnixMillis};
use artifree_domain::package::PackagePath;
use artifree_domain::{ArtifactKind, MetadataRevision, SourceRevision};
use artifree_ports::artifact_store::{
    ArtifactListing, ArtifactStore, ChangePage, CommitReceipt, Delivery, ListOrder, MetadataUpdate,
    NewArtifact, RequestFingerprint, RequestIdentity, RevisionCommit, StoreError,
};
use artifree_ports::token_store::{NewTokenRecord, TokenStore};
use artifree_providers::backup::{self, BackupError};
use artifree_providers::storage::{SCHEMA_VERSION, SqliteStore};
use artifree_providers::tokens::{SqliteTokenStore, hash_token_secret};

use common::{TempDir, TestResult, package, parsed, sha256};

const LAST_REVISION: u64 = 40;
const BACKUP_AFTER: u64 = 10;

fn owner() -> ActorId {
    parsed("owner")
}

fn key(name: &str) -> RequestKey {
    parsed(&format!("test-key-{name}"))
}

fn body(revision: u64) -> String {
    format!("<h1>revision {revision}</h1>")
}

fn identity<'a>(actor: &'a ActorId, key: &'a RequestKey, fingerprint: &str) -> RequestIdentity<'a> {
    RequestIdentity {
        actor,
        key,
        fingerprint: RequestFingerprint(sha256(fingerprint)),
    }
}

fn create(store: &SqliteStore, request: &str, title: &str) -> Result<CommitReceipt, StoreError> {
    let owner = owner();
    let key = key(request);
    store.create_artifact(NewArtifact {
        request: identity(&owner, &key, &body(1)),
        kind: ArtifactKind::App,
        title: &parsed(title),
        package: &package(&[("index.html", &body(1)), ("style.css", "h1{}")]),
        source: None,
    })
}

fn commit(
    store: &SqliteStore,
    receipt: &CommitReceipt,
    revision: u64,
    fingerprint: &str,
) -> Result<CommitReceipt, StoreError> {
    let owner = owner();
    let key = key(&format!("w{revision}"));
    store.commit_revision(RevisionCommit {
        request: identity(&owner, &key, fingerprint),
        artifact: &receipt.artifact.id,
        base: SourceRevision(revision - 1),
        package: &package(&[("index.html", &body(revision)), ("style.css", "h1{}")]),
    })
}

fn update(
    store: &SqliteStore,
    receipt: &CommitReceipt,
    request: &str,
    base: u64,
    title: Option<&str>,
    pinned: Option<bool>,
    deleted: Option<bool>,
) -> Result<(), StoreError> {
    let owner = owner();
    let key = key(request);
    let title = title.map(parsed);
    store
        .update_metadata(MetadataUpdate {
            request: identity(&owner, &key, request),
            artifact: &receipt.artifact.id,
            base: MetadataRevision(base),
            title: title.as_ref(),
            pinned,
            deleted,
        })
        .map(|_| ())
}

fn token(seed: u8, name: &str) -> NewTokenRecord {
    NewTokenRecord {
        id: TokenId::from_random([seed; 16]),
        name: name.to_owned(),
        secret_hash: hash_token_secret(&format!("secret-{name}")),
        prefix: "afr_ab…".to_owned(),
        created_at: "2026-01-01T00:00:00Z".to_owned(),
    }
}

struct Instance {
    _dir: TempDir,
    database: std::path::PathBuf,
}

fn instance() -> Result<(Instance, SqliteStore, SqliteTokenStore), Box<dyn std::error::Error>> {
    let dir = TempDir::new("backup-source");
    let database = dir.join("artifree.sqlite3");
    let store = SqliteStore::open(&database)?;
    let tokens = SqliteTokenStore::open(&database)?;
    Ok((
        Instance {
            _dir: dir,
            database,
        },
        store,
        tokens,
    ))
}

type Other = (CommitReceipt, NewTokenRecord, NewTokenRecord, ChangePage);

/// Writes the other domains that exist before the backup: a trashed, renamed,
/// pinned artifact, and an active and a revoked token.
fn write_other_domains(
    store: &SqliteStore,
    tokens: &SqliteTokenStore,
) -> Result<Other, Box<dyn std::error::Error>> {
    let trashed = create(store, "create-trashed", "Doomed")?;
    update(
        store,
        &trashed,
        "rename",
        1,
        Some("Renamed"),
        Some(true),
        None,
    )?;
    update(store, &trashed, "trash", 2, None, None, Some(true))?;
    let active = token(1, "active");
    let revoked = token(2, "revoked");
    tokens.create(&active)?;
    tokens.create(&revoked)?;
    tokens.revoke(&revoked.id, "2026-02-02T00:00:00Z")?;
    let feed = store.changes(&owner(), ChangeSequence(0), 1000)?;
    Ok((trashed, active, revoked, feed))
}

/// Backs up `database` once the writer, which commits revisions of `created`
/// up to [`LAST_REVISION`], has passed [`BACKUP_AFTER`]. The writer keeps going.
fn backup_during_writes(
    database: &Path,
    store: &SqliteStore,
    created: &CommitReceipt,
    output: &Path,
) -> TestResult {
    let reached = AtomicU64::new(1);
    thread::scope(|scope| -> TestResult {
        let writer = scope.spawn(|| -> Result<(), StoreError> {
            let mut current = created.clone();
            for revision in 2..=LAST_REVISION {
                current = commit(store, &current, revision, &body(revision))?;
                reached.store(revision, Ordering::Release);
            }
            Ok(())
        });
        while reached.load(Ordering::Acquire) < BACKUP_AFTER {
            thread::yield_now();
        }
        let summary = backup::create_backup(database, output)?;
        assert_eq!(summary.schema_version, SCHEMA_VERSION);
        writer.join().map_err(|_| "writer panicked")??;
        Ok(())
    })
}

/// The snapshot is one commit boundary: revision numbers are contiguous, the
/// snapshot is neither empty nor complete, and every file reads as its
/// revision wrote it. Returns the snapshot's head revision.
fn assert_history(
    restored: &SqliteStore,
    id: &ArtifactId,
) -> Result<u64, Box<dyn std::error::Error>> {
    let head = restored.artifact(id)?.ok_or("artifact")?.current_revision.0;
    assert!(
        (BACKUP_AFTER..=LAST_REVISION).contains(&head),
        "snapshot head {head} should lie where the writer was when it began"
    );
    let revisions = restored.revisions(id)?;
    assert_eq!(
        revisions.iter().map(|r| r.revision.0).collect::<Vec<_>>(),
        (1..=head).rev().collect::<Vec<_>>(),
        "every revision up to the head, none beyond"
    );
    for revision in &revisions {
        let number = revision.revision.0;
        let html = restored
            .revision_file(id, revision.revision, &PackagePath::entry())?
            .ok_or("file")?;
        assert_eq!(html, body(number).into_bytes(), "revision {number} bytes");
        let entry = revision
            .files
            .iter()
            .find(|file| file.path == PackagePath::entry())
            .ok_or("entry in manifest")?;
        assert_eq!(
            entry.sha256,
            sha256(&body(number)),
            "revision {number} digest"
        );
    }
    Ok(head)
}

/// Request keys replay: the same body returns the original receipt, another
/// body is a conflict, and the key just past the snapshot was never consumed.
fn assert_replays(restored: &SqliteStore, created: &CommitReceipt, head: u64) -> TestResult {
    let kept = commit(restored, created, 2, &body(2))?;
    assert_eq!(kept.delivery, Delivery::Replayed);
    assert_eq!(kept.revision.revision, SourceRevision(2));
    assert_eq!(
        commit(restored, created, 2, "a different body").err(),
        Some(StoreError::RequestKeyReused)
    );
    let next = head + 1;
    let at_head = CommitReceipt {
        artifact: restored.artifact(&created.artifact.id)?.ok_or("artifact")?,
        ..created.clone()
    };
    let fresh = commit(restored, &at_head, next, &body(next))?;
    assert_eq!(
        (fresh.delivery, fresh.revision.revision),
        (Delivery::Committed, SourceRevision(next)),
        "a key the snapshot did not hold is still unused"
    );
    Ok(())
}

fn assert_other_domains(restored: &SqliteStore, database: &Path, other: &Other) -> TestResult {
    let (trashed, active, revoked, feed_before) = other;
    // Metadata and the trash.
    let doomed = restored.artifact(&trashed.artifact.id)?.ok_or("trashed")?;
    assert_eq!(doomed.title.as_str(), "Renamed");
    assert!(doomed.pinned);
    assert_eq!(doomed.metadata_revision, MetadataRevision(3));
    assert!(doomed.deleted_at.is_some_and(|at| at > UnixMillis(0)));
    let in_trash = restored.list_artifacts(ArtifactListing {
        owner: &owner(),
        search: None,
        pinned: None,
        deleted: true,
        order: ListOrder::Updated,
        after: None,
        limit: 10,
    })?;
    assert_eq!(
        in_trash.iter().map(|r| &r.id).collect::<Vec<_>>(),
        [&trashed.artifact.id]
    );

    // The feed keeps its identity and the changes made before the backup.
    let page = restored.changes(&owner(), ChangeSequence(0), 1000)?;
    assert_eq!(page.feed, feed_before.feed, "feed identity");
    assert_eq!(
        page.changes[..feed_before.changes.len()],
        feed_before.changes[..],
        "changes made before the backup"
    );

    // Token records, including revocation.
    let tokens = SqliteTokenStore::open(database)?;
    let found = tokens
        .find_by_hash(&active.secret_hash)?
        .ok_or("active token")?;
    assert!(found.is_active());
    assert_eq!(found.name, "active");
    let gone = tokens
        .find_by_hash(&revoked.secret_hash)?
        .ok_or("revoked token")?;
    assert_eq!(gone.revoked_at.as_deref(), Some("2026-02-02T00:00:00Z"));
    Ok(())
}

#[test]
fn a_backup_taken_during_writes_restores_one_consistent_history() -> TestResult {
    let (source, store, tokens) = instance()?;
    let created = create(&store, "create", "Live")?;
    let other = write_other_domains(&store, &tokens)?;

    let target = TempDir::new("backup-target");
    let output = target.join("snapshot.sqlite3");
    backup_during_writes(&source.database, &store, &created, &output)?;

    let restored_dir = TempDir::new("backup-restored");
    let restored_db = restored_dir.join("artifree.sqlite3");
    let summary = backup::restore_backup(&output, &restored_db)?;
    assert_eq!(summary.schema_version, SCHEMA_VERSION);
    let restored = SqliteStore::open(&restored_db)?;

    let head = assert_history(&restored, &created.artifact.id)?;
    assert_replays(&restored, &created, head)?;
    assert_other_domains(&restored, &restored_db, &other)
}

#[cfg(unix)]
#[test]
fn a_backup_is_readable_by_its_owner_only() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let (source, store, _tokens) = instance()?;
    create(&store, "create", "Demo")?;
    let target = TempDir::new("backup-mode");
    let output = target.join("snapshot.sqlite3");
    backup::create_backup(&source.database, &output)?;
    let mode = std::fs::metadata(&output)?.permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "backup mode");
    let restored = target.join("restored.sqlite3");
    backup::restore_backup(&output, &restored)?;
    let mode = std::fs::metadata(&restored)?.permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "restored database mode");
    Ok(())
}

fn leftovers(dir: &TempDir) -> Result<Vec<String>, std::io::Error> {
    std::fs::read_dir(dir.path())?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect()
}

#[test]
fn a_backup_never_replaces_an_existing_file() -> TestResult {
    let (source, store, _tokens) = instance()?;
    create(&store, "create", "Demo")?;
    let target = TempDir::new("backup-exists");
    let output = target.join("snapshot.sqlite3");
    std::fs::write(&output, b"precious")?;
    let error = backup::create_backup(&source.database, &output).err();
    assert_eq!(error, Some(BackupError::Exists(output.clone())));
    assert_eq!(std::fs::read(&output)?, b"precious");
    assert_eq!(
        leftovers(&target)?,
        ["snapshot.sqlite3"],
        "no scratch files"
    );
    Ok(())
}

#[test]
fn a_backup_of_a_missing_database_creates_nothing() -> TestResult {
    let dir = TempDir::new("backup-missing");
    let database = dir.join("artifree.sqlite3");
    let output = dir.join("snapshot.sqlite3");
    assert_eq!(
        backup::create_backup(&database, &output).err(),
        Some(BackupError::Missing(database))
    );
    assert!(leftovers(&dir)?.is_empty(), "{:?}", leftovers(&dir)?);
    Ok(())
}

#[test]
fn a_database_of_a_newer_schema_is_not_backed_up() -> TestResult {
    let (source, store, _tokens) = instance()?;
    create(&store, "create", "Demo")?;
    rusqlite::Connection::open(&source.database)?.pragma_update(None, "user_version", 99)?;
    let target = TempDir::new("backup-newer");
    let output = target.join("snapshot.sqlite3");
    assert_eq!(
        backup::create_backup(&source.database, &output).err(),
        Some(BackupError::NewerSchema {
            found: 99,
            supported: SCHEMA_VERSION
        })
    );
    assert!(leftovers(&target)?.is_empty());
    Ok(())
}

#[test]
fn a_restore_never_replaces_an_existing_database() -> TestResult {
    let (source, store, _tokens) = instance()?;
    create(&store, "create", "Demo")?;
    let target = TempDir::new("restore-exists");
    let output = target.join("snapshot.sqlite3");
    backup::create_backup(&source.database, &output)?;
    let existing = target.join("artifree.sqlite3");
    std::fs::write(&existing, b"live data")?;
    assert_eq!(
        backup::restore_backup(&output, &existing).err(),
        Some(BackupError::Exists(existing.clone()))
    );
    assert_eq!(std::fs::read(&existing)?, b"live data");
    assert_eq!(
        leftovers(&target)?.len(),
        2,
        "only the backup and the live file remain: {:?}",
        leftovers(&target)?
    );
    Ok(())
}

#[test]
fn a_backup_of_a_newer_schema_is_not_restored() -> TestResult {
    let (source, store, _tokens) = instance()?;
    create(&store, "create", "Demo")?;
    let target = TempDir::new("restore-newer");
    let output = target.join("snapshot.sqlite3");
    backup::create_backup(&source.database, &output)?;
    rusqlite::Connection::open(&output)?.pragma_update(None, "user_version", 99)?;
    let before = std::fs::read(&output)?;
    let database = target.join("restored.sqlite3");
    assert_eq!(
        backup::restore_backup(&output, &database).err(),
        Some(BackupError::NewerSchema {
            found: 99,
            supported: SCHEMA_VERSION
        })
    );
    assert!(!database.exists());
    assert_eq!(std::fs::read(&output)?, before, "the backup is untouched");
    assert_eq!(leftovers(&target)?, ["snapshot.sqlite3"]);
    Ok(())
}

#[test]
fn a_file_that_is_not_a_database_is_not_restored() -> TestResult {
    let target = TempDir::new("restore-damaged");
    let output = target.join("snapshot.sqlite3");
    std::fs::write(&output, b"this is not a database, only text of some length")?;
    let database = target.join("restored.sqlite3");
    let error = backup::restore_backup(&output, &database).err();
    assert!(
        matches!(error, Some(BackupError::Damaged(_))),
        "expected Damaged, got {error:?}"
    );
    assert!(!database.exists());
    assert_eq!(leftovers(&target)?, ["snapshot.sqlite3"]);
    Ok(())
}
