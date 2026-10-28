//! Behavioral contract of the local `SQLite` artifact store.
use std::fmt::Display;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::Duration;

use artifree_domain::change::ChangeSequence;
use artifree_domain::identity::{ActorId, ArtifactId, RequestKey, Title, UnixMillis};
use artifree_domain::package::{FilePackage, PackagePath};
use artifree_domain::{ArtifactKind, MetadataRevision, SourceRevision};
use artifree_ports::artifact_store::{
    ArtifactListing, ArtifactRecord, ArtifactStore, ChangeKind, ChangePage, CommitReceipt,
    Delivery, ListOrder, ListPosition, MetadataReceipt, MetadataUpdate, NewArtifact,
    RequestFingerprint, RequestIdentity, RevisionCommit, StoreError,
};
use artifree_providers::storage::SqliteStore;
use sha2::{Digest, Sha256};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct TempDb {
    directory: PathBuf,
}

impl TempDb {
    fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!(
            "artifree-store-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory)
            .unwrap_or_else(|error| panic!("cannot create {}: {error}", directory.display()));
        Self { directory }
    }

    fn path(&self) -> PathBuf {
        self.directory.join("artifree.sqlite3")
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn parsed<T: FromStr>(value: &str) -> T
where
    T::Err: Display,
{
    value
        .parse()
        .unwrap_or_else(|error| panic!("invalid fixture {value:?}: {error}"))
}

fn actor(name: &str) -> ActorId {
    parsed(name)
}

fn key(name: &str) -> RequestKey {
    parsed(&format!("test-key-{name}"))
}

fn package(files: &[(&str, &str)]) -> FilePackage {
    FilePackage::new(
        files
            .iter()
            .map(|(path, content)| (parsed::<PackagePath>(path), content.as_bytes().to_vec()))
            .collect(),
    )
    .unwrap_or_else(|error| panic!("invalid fixture package: {error}"))
}

fn create(
    store: &SqliteStore,
    owner: &ActorId,
    request_key: &RequestKey,
    fingerprint: u8,
) -> Result<CommitReceipt, StoreError> {
    let title: Title = parsed("Demo");
    store.create_artifact(NewArtifact {
        request: RequestIdentity {
            actor: owner,
            key: request_key,
            fingerprint: RequestFingerprint([fingerprint; 32]),
        },
        kind: ArtifactKind::App,
        title: &title,
        package: &package(&[("index.html", "<h1>v1</h1>"), ("style.css", "h1{}")]),
        source: None,
    })
}

fn commit(
    store: &SqliteStore,
    created: &CommitReceipt,
    request_key: &RequestKey,
    base: u64,
    body: &str,
) -> Result<CommitReceipt, StoreError> {
    let owner = &created.artifact.owner;
    store.commit_revision(RevisionCommit {
        request: RequestIdentity {
            actor: owner,
            key: request_key,
            fingerprint: RequestFingerprint(fingerprint_of(body)),
        },
        artifact: &created.artifact.id,
        base: SourceRevision(base),
        package: &package(&[("index.html", body), ("style.css", "h1{}")]),
    })
}

fn fingerprint_of(body: &str) -> [u8; 32] {
    Sha256::digest(body).into()
}

fn entry(
    store: &SqliteStore,
    receipt: &CommitReceipt,
    revision: u64,
) -> Result<Option<Vec<u8>>, StoreError> {
    store.revision_file(
        &receipt.artifact.id,
        SourceRevision(revision),
        &PackagePath::entry(),
    )
}

#[test]
fn committed_revisions_survive_reopening_the_database() -> TestResult {
    let db = TempDb::new();
    let created = create(
        &SqliteStore::open(&db.path())?,
        &actor("owner"),
        &key("a"),
        1,
    )?;
    commit(
        &SqliteStore::open(&db.path())?,
        &created,
        &key("b"),
        1,
        "<h1>v2</h1>",
    )?;

    let reopened = SqliteStore::open(&db.path())?;
    let record = reopened
        .artifact(&created.artifact.id)?
        .ok_or("artifact lost after reopen")?;
    assert_eq!(record.current_revision, SourceRevision(2));
    assert_eq!(
        entry(&reopened, &created, 1)?.as_deref(),
        Some(b"<h1>v1</h1>".as_slice()),
        "revision 1 must stay immutable"
    );
    assert_eq!(
        entry(&reopened, &created, 2)?.as_deref(),
        Some(b"<h1>v2</h1>".as_slice())
    );
    let history: Vec<u64> = reopened
        .revisions(&created.artifact.id)?
        .iter()
        .map(|revision| revision.revision.0)
        .collect();
    assert_eq!(history, [2, 1]);
    Ok(())
}

#[test]
fn retry_with_same_key_and_fingerprint_replays_without_writing() -> TestResult {
    let db = TempDb::new();
    let store = SqliteStore::open(&db.path())?;
    let owner = actor("owner");
    let first = create(&store, &owner, &key("create"), 1)?;
    let retry = create(&store, &owner, &key("create"), 1)?;
    assert_eq!(first.delivery, Delivery::Committed);
    assert_eq!(retry.delivery, Delivery::Replayed);
    assert_eq!(retry.artifact.id, first.artifact.id);
    assert_eq!(
        listed(&store, &owner, None, None, ListOrder::Updated, None, 500)?.len(),
        1,
        "a replay must not create a second artifact"
    );

    let saved = commit(&store, &first, &key("save"), 1, "<h1>v2</h1>")?;
    // Later commits must not leak into the replayed receipts; the timestamp
    // tick makes a leaked update time observable.
    thread::sleep(Duration::from_millis(5));
    commit(&store, &first, &key("save-later"), 2, "<h1>v3</h1>")?;
    let replayed = commit(&store, &first, &key("save"), 1, "<h1>v2</h1>")?;
    assert_eq!(
        replayed,
        CommitReceipt {
            delivery: Delivery::Replayed,
            ..saved
        },
        "a lost response must be recoverable, unchanged, after later commits"
    );
    assert_eq!(
        create(&store, &owner, &key("create"), 1)?,
        CommitReceipt {
            delivery: Delivery::Replayed,
            ..first.clone()
        },
        "a replayed create still reports revision 1"
    );
    assert_eq!(
        store
            .artifact(&first.artifact.id)?
            .map(|record| record.current_revision),
        Some(SourceRevision(3))
    );
    Ok(())
}

#[test]
fn reusing_a_key_for_different_content_is_rejected() -> TestResult {
    let db = TempDb::new();
    let store = SqliteStore::open(&db.path())?;
    let owner = actor("owner");
    let created = create(&store, &owner, &key("shared"), 1)?;
    assert_eq!(
        create(&store, &owner, &key("shared"), 2),
        Err(StoreError::RequestKeyReused)
    );
    assert_eq!(
        commit(&store, &created, &key("shared"), 1, "<h1>other</h1>"),
        Err(StoreError::RequestKeyReused)
    );
    assert_eq!(
        listed(&store, &owner, None, None, ListOrder::Updated, None, 500)?.len(),
        1
    );

    let other = actor("other");
    let separate = create(&store, &other, &key("shared"), 2)?;
    assert_eq!(
        separate.delivery,
        Delivery::Committed,
        "request keys are scoped to their actor"
    );
    Ok(())
}

#[test]
fn stale_base_conflicts_and_does_not_consume_the_key() -> TestResult {
    let db = TempDb::new();
    let store = SqliteStore::open(&db.path())?;
    let created = create(&store, &actor("owner"), &key("a"), 1)?;
    commit(&store, &created, &key("winner"), 1, "<h1>winner</h1>")?;

    assert_eq!(
        commit(&store, &created, &key("loser"), 1, "<h1>loser</h1>"),
        Err(StoreError::RevisionConflict {
            current: SourceRevision(2)
        })
    );
    assert_eq!(
        entry(&store, &created, 2)?.as_deref(),
        Some(b"<h1>winner</h1>".as_slice())
    );
    let rebased = commit(&store, &created, &key("loser"), 2, "<h1>rebased</h1>")?;
    assert_eq!(
        rebased.revision.revision,
        SourceRevision(3),
        "a rejected request must not reserve its key"
    );
    Ok(())
}

#[test]
fn commit_to_missing_artifact_is_not_found() -> TestResult {
    let db = TempDb::new();
    let store = SqliteStore::open(&db.path())?;
    let created = create(&store, &actor("owner"), &key("a"), 1)?;
    let mut missing = created.clone();
    missing.artifact.id = artifree_domain::identity::ArtifactId::from_random([7; 16]);
    assert_eq!(
        commit(&store, &missing, &key("b"), 1, "<p>x</p>"),
        Err(StoreError::NotFound)
    );
    Ok(())
}

#[test]
fn concurrent_writers_from_one_base_produce_exactly_one_revision() -> TestResult {
    let db = TempDb::new();
    let store = Arc::new(SqliteStore::open(&db.path())?);
    let created = Arc::new(create(&store, &actor("owner"), &key("a"), 1)?);
    let writers: Vec<_> = (0..8)
        .map(|index| {
            let store = Arc::clone(&store);
            let created = Arc::clone(&created);
            thread::spawn(move || {
                commit(
                    &store,
                    &created,
                    &key(&format!("writer-{index}")),
                    1,
                    &format!("<p>{index}</p>"),
                )
            })
        })
        .collect();
    let mut committed = 0;
    for writer in writers {
        match writer.join().map_err(|_| "writer panicked")? {
            Ok(receipt) => {
                assert_eq!(receipt.revision.revision, SourceRevision(2));
                committed += 1;
            }
            Err(StoreError::RevisionConflict { current }) => assert_eq!(current, SourceRevision(2)),
            Err(other) => return Err(format!("unexpected writer failure: {other}").into()),
        }
    }
    assert_eq!(committed, 1, "exactly one writer may win base revision 1");
    assert_eq!(store.revisions(&created.artifact.id)?.len(), 2);
    Ok(())
}

#[test]
fn failed_commit_leaves_no_partial_revision() -> TestResult {
    let db = TempDb::new();
    let store = SqliteStore::open(&db.path())?;
    let created = create(&store, &actor("owner"), &key("a"), 1)?;
    // Fail the final statement of the commit, after files and metadata were written.
    let connection = rusqlite::Connection::open(db.path())?;
    connection.execute_batch(
        "CREATE TRIGGER fail_key BEFORE INSERT ON request_keys BEGIN SELECT RAISE(ABORT, 'injected'); END;",
    )?;

    let failed = commit(&store, &created, &key("b"), 1, "<h1>partial</h1>");
    assert!(
        matches!(failed, Err(StoreError::Unavailable(ref reason)) if reason.contains("injected")),
        "{failed:?}"
    );
    assert_eq!(
        store
            .artifact(&created.artifact.id)?
            .map(|record| record.current_revision),
        Some(SourceRevision(1))
    );
    assert_eq!(store.revisions(&created.artifact.id)?.len(), 1);
    assert_eq!(entry(&store, &created, 2)?, None);
    let orphan_blobs: i64 = connection.query_row(
        "SELECT count(*) FROM blobs WHERE digest NOT IN (SELECT digest FROM revision_files)",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(orphan_blobs, 0, "rolled-back content must not remain");

    connection.execute_batch("DROP TRIGGER fail_key;")?;
    assert_eq!(
        commit(&store, &created, &key("b"), 1, "<h1>partial</h1>")?.delivery,
        Delivery::Committed
    );
    Ok(())
}

#[test]
fn a_commit_of_the_base_files_is_refused_with_its_conflict_check() -> TestResult {
    let db = TempDb::new();
    let store = SqliteStore::open(&db.path())?;
    let owner = actor("owner");
    let created = create(&store, &owner, &key("create"), 1)?;
    // The base's own files, against the current base: nothing is written.
    let same = commit(&store, &created, &key("same"), 1, "<h1>v1</h1>");
    assert_eq!(same.err(), Some(StoreError::Unchanged));
    assert_eq!(feed(&store, &owner, 0, 100)?.1, 1, "no feed entry");
    commit(&store, &created, &key("v2"), 1, "<h1>v2</h1>")?;
    // The same files against a base that is no longer current: the conflict
    // is what the caller needs to know.
    let stale = commit(&store, &created, &key("stale"), 1, "<h1>v1</h1>");
    assert_eq!(
        stale.err(),
        Some(StoreError::RevisionConflict {
            current: SourceRevision(2)
        })
    );
    let current = store
        .artifact(&created.artifact.id)?
        .ok_or("artifact kept")?;
    assert_eq!(current.current_revision, SourceRevision(2));
    Ok(())
}

#[test]
fn unchanged_files_share_stored_content() -> TestResult {
    let db = TempDb::new();
    let store = SqliteStore::open(&db.path())?;
    let created = create(&store, &actor("owner"), &key("a"), 1)?;
    commit(&store, &created, &key("b"), 1, "<h1>v2</h1>")?;
    let blobs: i64 = rusqlite::Connection::open(db.path())?.query_row(
        "SELECT count(*) FROM blobs",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(blobs, 3, "two index.html versions and one shared style.css");
    Ok(())
}

#[test]
fn refuses_a_newer_schema_version() -> TestResult {
    let db = TempDb::new();
    SqliteStore::open(&db.path())?;
    rusqlite::Connection::open(db.path())?.pragma_update(None, "user_version", 99)?;
    let error = SqliteStore::open(&db.path())
        .err()
        .ok_or("newer schema was accepted")?;
    assert!(
        matches!(&error, StoreError::Unavailable(reason) if reason.contains("version 99")),
        "{error}"
    );
    Ok(())
}

/// A change as (sequence, kind, revision).
type Entry = (u64, ChangeKind, u64);

/// The owner's changes after `after`, and the head.
fn feed(
    store: &SqliteStore,
    owner: &ActorId,
    after: u64,
    limit: usize,
) -> Result<(Vec<Entry>, u64), StoreError> {
    let ChangePage { changes, head, .. } = store.changes(owner, ChangeSequence(after), limit)?;
    let changes = changes
        .iter()
        .map(|change| (change.sequence.0, change.kind, change.revision.0))
        .collect();
    Ok((changes, head.0))
}

#[test]
fn each_commit_is_recorded_once_in_feed_order() -> TestResult {
    let db = TempDb::new();
    let store = SqliteStore::open(&db.path())?;
    let owner = actor("owner");
    assert_eq!(feed(&store, &owner, 0, 100)?, (vec![], 0));
    let created = create(&store, &owner, &key("a"), 1)?;
    let saved = commit(&store, &created, &key("b"), 1, "<h1>v2</h1>")?;
    // A replay, a conflict, and a reused key write nothing.
    assert_eq!(
        create(&store, &owner, &key("a"), 1)?.delivery,
        Delivery::Replayed
    );
    assert_eq!(
        commit(&store, &created, &key("b"), 1, "<h1>v2</h1>")?.delivery,
        Delivery::Replayed
    );
    assert!(commit(&store, &created, &key("c"), 1, "<h1>stale</h1>").is_err());
    assert!(commit(&store, &created, &key("b"), 2, "<h1>other</h1>").is_err());

    assert_eq!(
        feed(&store, &owner, 0, 100)?,
        (
            vec![(1, ChangeKind::Created, 1), (2, ChangeKind::Revised, 2)],
            2
        )
    );
    let page = store.changes(&owner, ChangeSequence(0), 100)?;
    assert!(
        page.changes
            .iter()
            .all(|change| change.artifact == created.artifact.id)
    );
    assert_eq!(page.changes[0].committed_at, created.revision.created_at);
    assert_eq!(page.changes[1].committed_at, saved.revision.created_at);
    Ok(())
}

#[test]
fn a_feed_entry_commits_with_its_revision_or_not_at_all() -> TestResult {
    let db = TempDb::new();
    let store = SqliteStore::open(&db.path())?;
    let owner = actor("owner");
    let created = create(&store, &owner, &key("a"), 1)?;
    let connection = rusqlite::Connection::open(db.path())?;
    let fail = |table: &str| {
        connection.execute_batch(&format!(
            "CREATE TRIGGER fail BEFORE INSERT ON {table} BEGIN SELECT RAISE(ABORT, 'injected'); END;"
        ))
    };

    // The feed entry fails: the revision is not committed either.
    fail("changes")?;
    assert!(commit(&store, &created, &key("b"), 1, "<h1>v2</h1>").is_err());
    assert_eq!(store.revisions(&created.artifact.id)?.len(), 1);
    connection.execute_batch("DROP TRIGGER fail;")?;

    // Another statement of the commit fails: no entry is recorded.
    fail("request_keys")?;
    assert!(commit(&store, &created, &key("b"), 1, "<h1>v2</h1>").is_err());
    assert_eq!(
        feed(&store, &owner, 0, 100)?,
        (vec![(1, ChangeKind::Created, 1)], 1)
    );
    connection.execute_batch("DROP TRIGGER fail;")?;

    commit(&store, &created, &key("b"), 1, "<h1>v2</h1>")?;
    assert_eq!(
        feed(&store, &owner, 1, 100)?,
        (vec![(2, ChangeKind::Revised, 2)], 2),
        "a rolled-back commit leaves no gap and no entry"
    );
    Ok(())
}

#[test]
fn owners_read_only_their_changes_and_a_short_page_reaches_the_head() -> TestResult {
    let db = TempDb::new();
    let store = SqliteStore::open(&db.path())?;
    let (alice, bob) = (actor("alice"), actor("bob"));
    let first = create(&store, &alice, &key("a"), 1)?;
    create(&store, &bob, &key("b"), 2)?;
    commit(&store, &first, &key("c"), 1, "<h1>v2</h1>")?;

    assert_eq!(
        feed(&store, &alice, 0, 100)?,
        (
            vec![(1, ChangeKind::Created, 1), (3, ChangeKind::Revised, 2)],
            3
        )
    );
    assert_eq!(
        feed(&store, &bob, 0, 100)?,
        (vec![(2, ChangeKind::Created, 1)], 3)
    );
    // A full page may be followed by more; a short one ends at the head.
    assert_eq!(
        feed(&store, &alice, 0, 1)?,
        (vec![(1, ChangeKind::Created, 1)], 3)
    );
    assert_eq!(
        feed(&store, &alice, 1, 1)?,
        (vec![(3, ChangeKind::Revised, 2)], 3)
    );
    assert_eq!(feed(&store, &alice, 3, 100)?, (vec![], 3));
    Ok(())
}

#[test]
fn a_feed_keeps_its_identity_and_another_database_has_another() -> TestResult {
    let (first, second) = (TempDb::new(), TempDb::new());
    let owner = actor("owner");
    let identity = SqliteStore::open(&first.path())?
        .changes(&owner, ChangeSequence(0), 0)?
        .feed;
    let reopened = SqliteStore::open(&first.path())?
        .changes(&owner, ChangeSequence(0), 0)?
        .feed;
    let other = SqliteStore::open(&second.path())?
        .changes(&owner, ChangeSequence(0), 0)?
        .feed;
    assert_eq!(identity, reopened);
    assert_ne!(identity, other);
    Ok(())
}

/// The schema of version 1, as released, for upgrade tests.
const VERSION_1: &str = "
CREATE TABLE artifacts (
    id TEXT PRIMARY KEY,
    owner TEXT NOT NULL,
    kind TEXT NOT NULL,
    title TEXT NOT NULL,
    current_revision INTEGER NOT NULL CHECK (current_revision >= 1),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;
CREATE INDEX artifacts_by_owner ON artifacts (owner, updated_at DESC, id);
CREATE TABLE revisions (
    artifact_id TEXT NOT NULL REFERENCES artifacts (id),
    revision INTEGER NOT NULL CHECK (revision >= 1),
    created_by TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (artifact_id, revision)
) STRICT, WITHOUT ROWID;
CREATE TABLE blobs (
    digest BLOB PRIMARY KEY CHECK (length(digest) = 32),
    content BLOB NOT NULL
) STRICT;
CREATE TABLE revision_files (
    artifact_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    path TEXT NOT NULL,
    size INTEGER NOT NULL CHECK (size >= 0),
    digest BLOB NOT NULL REFERENCES blobs (digest),
    PRIMARY KEY (artifact_id, revision, path),
    FOREIGN KEY (artifact_id, revision) REFERENCES revisions (artifact_id, revision)
) STRICT, WITHOUT ROWID;
CREATE TABLE request_keys (
    actor TEXT NOT NULL,
    request_key TEXT NOT NULL,
    fingerprint BLOB NOT NULL CHECK (length(fingerprint) = 32),
    artifact_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (actor, request_key),
    FOREIGN KEY (artifact_id, revision) REFERENCES revisions (artifact_id, revision)
) STRICT, WITHOUT ROWID;
";

/// The tables version 2 added, as released.
const VERSION_2: &str = "
CREATE TABLE feed (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    id BLOB NOT NULL CHECK (length(id) = 16)
) STRICT;
CREATE TABLE changes (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    artifact_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('created', 'revised')),
    FOREIGN KEY (artifact_id, revision) REFERENCES revisions (artifact_id, revision)
) STRICT;
";

const OLD_ID: &str = "art_0123456789abcdef0123456789abcdef";

/// A database of `version` holding one artifact, "Old", at revision 2,
/// created by the request key "test-key-old" with fingerprint 7s.
fn old_database(db: &TempDb, version: i64) -> Result<(), rusqlite::Error> {
    let connection = rusqlite::Connection::open(db.path())?;
    connection.execute_batch(VERSION_1)?;
    connection.execute_batch(&format!(
        "INSERT INTO artifacts VALUES ('{OLD_ID}', 'owner', 'app', 'Old', 2, 100, 200);
         INSERT INTO revisions VALUES ('{OLD_ID}', 1, 'owner', 100), ('{OLD_ID}', 2, 'owner', 200);
         INSERT INTO blobs VALUES (zeroblob(32), CAST('<h1>old</h1>' AS BLOB));
         INSERT INTO revision_files VALUES
             ('{OLD_ID}', 1, 'index.html', 12, zeroblob(32)),
             ('{OLD_ID}', 2, 'index.html', 12, zeroblob(32));
         INSERT INTO request_keys VALUES
             ('owner', 'test-key-old', zeroblob(32), '{OLD_ID}', 1, 100);"
    ))?;
    if version >= 2 {
        connection.execute_batch(VERSION_2)?;
        connection.execute_batch(&format!(
            "INSERT INTO feed VALUES (1, zeroblob(16));
             INSERT INTO changes (sequence, artifact_id, revision, kind) VALUES
                 (1, '{OLD_ID}', 1, 'created'), (2, '{OLD_ID}', 2, 'revised');
             UPDATE sqlite_sequence SET seq = 5 WHERE name = 'changes';"
        ))?;
    }
    connection.pragma_update(None, "user_version", version)
}

fn old_artifact() -> ArtifactId {
    parsed(OLD_ID)
}

fn user_version(db: &TempDb) -> Result<i64, rusqlite::Error> {
    rusqlite::Connection::open(db.path())?
        .pragma_query_value(None, "user_version", |row| row.get(0))
}

#[test]
fn a_version_1_database_gains_the_feed_and_metadata_and_keeps_its_data() -> TestResult {
    let db = TempDb::new();
    old_database(&db, 1)?;
    let store = SqliteStore::open(&db.path())?;
    let owner = actor("owner");
    assert_eq!(
        feed(&store, &owner, 0, 100)?,
        (vec![], 0),
        "earlier commits have no entries"
    );
    let old = store.artifact(&old_artifact())?.ok_or("artifact kept")?;
    assert_eq!(
        (old.title.as_str(), old.pinned, old.metadata_revision),
        ("Old", false, MetadataRevision(1))
    );
    let receipt = store.update_metadata(MetadataUpdate {
        request: RequestIdentity {
            actor: &owner,
            key: &key("rename"),
            fingerprint: RequestFingerprint([1; 32]),
        },
        artifact: &old_artifact(),
        base: MetadataRevision(1),
        title: Some(&parsed("New")),
        pinned: None,
        deleted: None,
    })?;
    assert_eq!(receipt.artifact.metadata_revision, MetadataRevision(2));
    assert_eq!(
        feed(&store, &owner, 0, 100)?,
        (vec![(1, ChangeKind::MetadataUpdated, 2)], 1)
    );
    assert_eq!(user_version(&db)?, 5);
    Ok(())
}

#[test]
fn a_version_2_database_keeps_its_feed_head_and_its_request_keys() -> TestResult {
    let db = TempDb::new();
    old_database(&db, 2)?;
    let store = SqliteStore::open(&db.path())?;
    let owner = actor("owner");
    let page = store.changes(&owner, ChangeSequence(0), 100)?;
    assert_eq!(
        page.head,
        ChangeSequence(5),
        "the head survives the rebuild"
    );
    let kept: Vec<_> = page
        .changes
        .iter()
        .map(|change| {
            (
                change.sequence.0,
                change.kind,
                change.revision.0,
                change.metadata_revision,
                change.committed_at,
            )
        })
        .collect();
    assert_eq!(
        kept,
        [
            (
                1,
                ChangeKind::Created,
                1,
                MetadataRevision(1),
                UnixMillis(100)
            ),
            (
                2,
                ChangeKind::Revised,
                2,
                MetadataRevision(1),
                UnixMillis(200)
            ),
        ]
    );
    // An old key still replays, as of its request.
    let replayed = store.create_artifact(NewArtifact {
        request: RequestIdentity {
            actor: &owner,
            key: &key("old"),
            fingerprint: RequestFingerprint([0; 32]),
        },
        kind: ArtifactKind::App,
        title: &parsed("Old"),
        package: &package(&[("index.html", "<h1>old</h1>")]),
        source: None,
    })?;
    assert_eq!(replayed.delivery, Delivery::Replayed);
    assert_eq!(replayed.artifact.current_revision, SourceRevision(1));
    assert_eq!(replayed.artifact.updated_at, UnixMillis(100));
    assert_eq!(replayed.artifact.title.as_str(), "Old");
    // New commits continue after the head, not after the last row.
    let old = store.artifact(&old_artifact())?.ok_or("artifact kept")?;
    store.commit_revision(RevisionCommit {
        request: RequestIdentity {
            actor: &owner,
            key: &key("new"),
            fingerprint: RequestFingerprint([2; 32]),
        },
        artifact: &old.id,
        base: SourceRevision(2),
        package: &package(&[("index.html", "<h1>new</h1>")]),
    })?;
    assert_eq!(
        feed(&store, &owner, 2, 100)?,
        (vec![(6, ChangeKind::Revised, 3)], 6)
    );
    assert_eq!(user_version(&db)?, 5);
    Ok(())
}

fn update(
    store: &SqliteStore,
    created: &CommitReceipt,
    request_key: &RequestKey,
    base: u64,
    title: Option<&str>,
    pinned: Option<bool>,
) -> Result<MetadataReceipt, StoreError> {
    let title: Option<Title> = title.map(parsed);
    store.update_metadata(MetadataUpdate {
        request: RequestIdentity {
            actor: &created.artifact.owner,
            key: request_key,
            fingerprint: RequestFingerprint(fingerprint_of(&format!("{title:?} {pinned:?}"))),
        },
        artifact: &created.artifact.id,
        base: MetadataRevision(base),
        title: title.as_ref(),
        pinned,
        deleted: None,
    })
}

fn metadata(record: &ArtifactRecord) -> (&str, bool, u64, u64) {
    (
        record.title.as_str(),
        record.pinned,
        record.metadata_revision.0,
        record.current_revision.0,
    )
}

#[test]
fn metadata_updates_change_title_and_pin_without_a_revision() -> TestResult {
    let db = TempDb::new();
    let store = SqliteStore::open(&db.path())?;
    let owner = actor("owner");
    let created = create(&store, &owner, &key("a"), 1)?;
    assert_eq!(metadata(&created.artifact), ("Demo", false, 1, 1));

    let renamed = update(&store, &created, &key("b"), 1, Some("Renamed"), None)?;
    assert_eq!(renamed.delivery, Delivery::Committed);
    assert_eq!(metadata(&renamed.artifact), ("Renamed", false, 2, 1));
    let pinned = update(&store, &created, &key("c"), 2, None, Some(true))?;
    assert_eq!(metadata(&pinned.artifact), ("Renamed", true, 3, 1));
    let current = store.artifact(&created.artifact.id)?.ok_or("artifact")?;
    assert_eq!(metadata(&current), ("Renamed", true, 3, 1));
    assert_eq!(
        current.updated_at, created.artifact.updated_at,
        "metadata does not change the source update time"
    );
    assert_eq!(store.revisions(&created.artifact.id)?.len(), 1);

    // A stale base conflicts and leaves the key unused.
    assert_eq!(
        update(&store, &created, &key("d"), 2, Some("Lost"), None),
        Err(StoreError::MetadataConflict {
            current: MetadataRevision(3)
        })
    );
    // Values already stored change nothing but record the key.
    let unchanged = update(&store, &created, &key("d"), 3, Some("Renamed"), Some(true))?;
    assert_eq!(unchanged.delivery, Delivery::Committed);
    assert_eq!(metadata(&unchanged.artifact), ("Renamed", true, 3, 1));
    assert_eq!(
        update(&store, &created, &key("d"), 3, Some("Renamed"), Some(true))?.delivery,
        Delivery::Replayed
    );

    let page = store.changes(&owner, ChangeSequence(0), 100)?;
    let entries: Vec<_> = page
        .changes
        .iter()
        .map(|change| (change.kind, change.revision.0, change.metadata_revision.0))
        .collect();
    assert_eq!(
        entries,
        [
            (ChangeKind::Created, 1, 1),
            (ChangeKind::MetadataUpdated, 1, 2),
            (ChangeKind::MetadataUpdated, 1, 3),
        ]
    );
    Ok(())
}

#[test]
fn replays_report_the_artifact_as_of_their_request() -> TestResult {
    let db = TempDb::new();
    let store = SqliteStore::open(&db.path())?;
    let owner = actor("owner");
    let created = create(&store, &owner, &key("a"), 1)?;
    commit(&store, &created, &key("b"), 1, "<h1>v2</h1>")?;
    update(&store, &created, &key("c"), 1, Some("Second"), None)?;
    commit(&store, &created, &key("d"), 2, "<h1>v3</h1>")?;
    update(&store, &created, &key("e"), 2, Some("Third"), Some(true))?;

    let save = commit(&store, &created, &key("b"), 1, "<h1>v2</h1>")?;
    assert_eq!(save.delivery, Delivery::Replayed);
    assert_eq!(metadata(&save.artifact), ("Demo", false, 1, 2));
    let rename = update(&store, &created, &key("c"), 1, Some("Second"), None)?;
    assert_eq!(rename.delivery, Delivery::Replayed);
    assert_eq!(metadata(&rename.artifact), ("Second", false, 2, 2));
    let later = commit(&store, &created, &key("d"), 2, "<h1>v3</h1>")?;
    assert_eq!(metadata(&later.artifact), ("Second", false, 2, 3));

    // A key belongs to one request, whatever its kind.
    assert_eq!(
        update(&store, &created, &key("b"), 3, Some("Other"), None),
        Err(StoreError::RequestKeyReused)
    );
    Ok(())
}

#[test]
fn a_metadata_update_commits_with_its_feed_entry_or_not_at_all() -> TestResult {
    let db = TempDb::new();
    let store = SqliteStore::open(&db.path())?;
    let owner = actor("owner");
    let created = create(&store, &owner, &key("a"), 1)?;
    let connection = rusqlite::Connection::open(db.path())?;
    connection.execute_batch(
        "CREATE TRIGGER fail BEFORE INSERT ON changes BEGIN SELECT RAISE(ABORT, 'injected'); END;",
    )?;
    assert!(update(&store, &created, &key("b"), 1, Some("Renamed"), None).is_err());
    let current = store.artifact(&created.artifact.id)?.ok_or("artifact")?;
    assert_eq!(metadata(&current), ("Demo", false, 1, 1));
    connection.execute_batch("DROP TRIGGER fail;")?;
    let retried = update(&store, &created, &key("b"), 1, Some("Renamed"), None)?;
    assert_eq!(
        retried.delivery,
        Delivery::Committed,
        "the key was not used"
    );
    assert_eq!(metadata(&retried.artifact), ("Renamed", false, 2, 1));
    Ok(())
}

fn listed(
    store: &SqliteStore,
    owner: &ActorId,
    search: Option<&str>,
    pinned: Option<bool>,
    order: ListOrder,
    after: Option<&ListPosition>,
    limit: usize,
) -> Result<Vec<String>, StoreError> {
    Ok(store
        .list_artifacts(ArtifactListing {
            owner,
            search,
            pinned,
            deleted: false,
            order,
            after,
            limit,
        })?
        .into_iter()
        .map(|record| record.title.as_str().to_owned())
        .collect())
}

/// Five artifacts of one owner with fixed times: creation follows the
/// order of the titles, and "beta" and "ALPHA notes" were updated at the
/// same time, last. "Gamma ä" is pinned. Another owner has one artifact.
/// Returns the store and the titles in update order.
fn listing_fixture(
    db: &TempDb,
) -> Result<(SqliteStore, [&'static str; 5]), Box<dyn std::error::Error>> {
    let store = SqliteStore::open(&db.path())?;
    let owner = actor("owner");
    let mut ids = Vec::new();
    for (index, title) in ["Alpha plan", "beta", "ALPHA notes", "Gamma ä", "Ä upper"]
        .into_iter()
        .enumerate()
    {
        let receipt = store.create_artifact(NewArtifact {
            request: RequestIdentity {
                actor: &owner,
                key: &key(&format!("list-{index}")),
                fingerprint: RequestFingerprint([1; 32]),
            },
            kind: ArtifactKind::App,
            title: &parsed(title),
            package: &package(&[("index.html", "<h1>x</h1>")]),
            source: None,
        })?;
        ids.push(receipt.artifact.id);
    }
    create(&store, &actor("other"), &key("theirs"), 1)?;
    let connection = rusqlite::Connection::open(db.path())?;
    for (index, id) in ids.iter().enumerate() {
        let created = i64::try_from(index)? * 10;
        let updated = if index == 1 || index == 2 {
            100
        } else {
            created
        };
        connection.execute(
            "UPDATE artifacts SET created_at = ?2, updated_at = ?3 WHERE id = ?1",
            rusqlite::params![id.as_str(), created, updated],
        )?;
    }
    connection.execute(
        "UPDATE artifacts SET pinned = 1 WHERE id = ?1",
        [ids[3].as_str()],
    )?;
    // Equal times are ordered by identifier.
    let tied = if ids[1].as_str() < ids[2].as_str() {
        ["beta", "ALPHA notes"]
    } else {
        ["ALPHA notes", "beta"]
    };
    Ok((
        store,
        [tied[0], tied[1], "Ä upper", "Gamma ä", "Alpha plan"],
    ))
}

#[test]
fn lists_filter_by_title_and_pin_in_either_order() -> TestResult {
    let db = TempDb::new();
    let (store, by_update) = listing_fixture(&db)?;
    let owner = actor("owner");
    let all = |order| listed(&store, &owner, None, None, order, None, 10);
    assert_eq!(all(ListOrder::Updated)?, by_update);
    assert_eq!(
        all(ListOrder::Created)?,
        ["Ä upper", "Gamma ä", "ALPHA notes", "beta", "Alpha plan"]
    );
    // ASCII letters match regardless of case; other characters exactly.
    assert_eq!(
        listed(
            &store,
            &owner,
            Some("alpha"),
            None,
            ListOrder::Created,
            None,
            10
        )?,
        ["ALPHA notes", "Alpha plan"]
    );
    assert_eq!(
        listed(
            &store,
            &owner,
            Some("ä"),
            None,
            ListOrder::Created,
            None,
            10
        )?,
        ["Gamma ä"]
    );
    assert_eq!(
        listed(
            &store,
            &owner,
            None,
            Some(true),
            ListOrder::Created,
            None,
            10
        )?,
        ["Gamma ä"]
    );
    assert_eq!(
        listed(
            &store,
            &owner,
            None,
            Some(false),
            ListOrder::Created,
            None,
            10
        )?
        .len(),
        4
    );
    Ok(())
}

#[test]
fn list_pages_continue_after_the_last_position_through_a_tie() -> TestResult {
    let db = TempDb::new();
    let (store, by_update) = listing_fixture(&db)?;
    let owner = actor("owner");
    let mut paged = Vec::new();
    let mut after: Option<ListPosition> = None;
    loop {
        let page = store.list_artifacts(ArtifactListing {
            owner: &owner,
            search: None,
            pinned: None,
            deleted: false,
            order: ListOrder::Updated,
            after: after.as_ref(),
            limit: 1,
        })?;
        let Some(last) = page.last() else { break };
        paged.push(last.title.as_str().to_owned());
        after = Some(ListPosition {
            at: last.updated_at,
            id: last.id.clone(),
        });
    }
    assert_eq!(paged, by_update);
    Ok(())
}

#[test]
fn commit_times_follow_commit_order_after_the_clock_was_set_back() -> TestResult {
    for version in [1, 2] {
        let db = TempDb::new();
        old_database(&db, version)?;
        // The old artifact was created an hour ahead of the clock, which was
        // then set back. In version 2 its feed entries are out of order:
        // the last one is older than the first. Version 1 has none.
        let ahead = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis(),
        )? + 3_600_000;
        let connection = rusqlite::Connection::open(db.path())?;
        connection.execute(
            "UPDATE revisions SET created_at = ?1 WHERE revision = 1",
            [ahead],
        )?;
        connection.execute("UPDATE artifacts SET created_at = ?1", [ahead])?;
        let store = SqliteStore::open(&db.path())?;
        let owner = actor("owner");
        let old = store.artifact(&old_artifact())?.ok_or("artifact kept")?;

        let created = create(&store, &owner, &key("created"), 1)?;
        let saved = store.commit_revision(RevisionCommit {
            request: RequestIdentity {
                actor: &owner,
                key: &key("saved"),
                fingerprint: RequestFingerprint([2; 32]),
            },
            artifact: &old.id,
            base: SourceRevision(2),
            package: &package(&[("index.html", "<h1>new</h1>")]),
        })?;
        update(&store, &created, &key("renamed"), 1, Some("Renamed"), None)?;
        assert_eq!(
            [created.revision.created_at, saved.revision.created_at],
            [UnixMillis(ahead + 1), UnixMillis(ahead + 2)],
            "version {version}: each commit follows the latest one"
        );
        let after = if version == 1 { 0 } else { 2 };
        let page = store.changes(&owner, ChangeSequence(after), 100)?;
        let times: Vec<UnixMillis> = page
            .changes
            .iter()
            .map(|change| change.committed_at)
            .collect();
        assert_eq!(
            times,
            [ahead + 1, ahead + 2, ahead + 3].map(UnixMillis),
            "version {version}"
        );
        // The source changed last is listed first; a rename changes no
        // update time.
        assert_eq!(
            listed(&store, &owner, None, None, ListOrder::Updated, None, 10)?,
            ["Old", "Renamed"],
            "version {version}"
        );
        assert_eq!(
            listed(&store, &owner, None, None, ListOrder::Created, None, 10)?,
            ["Renamed", "Old"],
            "version {version}"
        );
        assert_eq!(user_version(&db)?, 5);
    }
    Ok(())
}

/// Moves the artifact to the trash (`true`) or out of it (`false`), with a
/// fingerprint of its own for each.
fn set_deleted(
    store: &SqliteStore,
    created: &CommitReceipt,
    request_key: &RequestKey,
    base: u64,
    deleted: bool,
) -> Result<MetadataReceipt, StoreError> {
    store.update_metadata(MetadataUpdate {
        request: RequestIdentity {
            actor: &created.artifact.owner,
            key: request_key,
            fingerprint: RequestFingerprint([if deleted { 0xd1 } else { 0xd0 }; 32]),
        },
        artifact: &created.artifact.id,
        base: MetadataRevision(base),
        title: None,
        pinned: None,
        deleted: Some(deleted),
    })
}

fn copy_of(
    store: &SqliteStore,
    source: &CommitReceipt,
    request_key: &RequestKey,
) -> Result<CommitReceipt, StoreError> {
    store.create_artifact(NewArtifact {
        request: RequestIdentity {
            actor: &source.artifact.owner,
            key: request_key,
            fingerprint: RequestFingerprint([0xc0; 32]),
        },
        kind: ArtifactKind::App,
        title: &parsed("Copy"),
        package: &package(&[("index.html", "<h1>copy</h1>")]),
        source: Some(&source.artifact.id),
    })
}

fn trash_titles(store: &SqliteStore, owner: &ActorId) -> Result<Vec<String>, StoreError> {
    Ok(store
        .list_artifacts(ArtifactListing {
            owner,
            search: None,
            pinned: None,
            deleted: true,
            order: ListOrder::Updated,
            after: None,
            limit: 10,
        })?
        .into_iter()
        .map(|record| record.title.as_str().to_owned())
        .collect())
}

#[test]
fn a_write_in_the_trash_is_its_replay_or_not_found() -> TestResult {
    let db = TempDb::new();
    let store = SqliteStore::open(&db.path())?;
    let owner = actor("owner");
    let created = create(&store, &owner, &key("a"), 1)?;
    commit(&store, &created, &key("save"), 1, "<h1>v2</h1>")?;
    let deleted = set_deleted(&store, &created, &key("delete"), 1, true)?;
    let deleted_at = deleted.artifact.deleted_at.ok_or("a time in the trash")?;

    // Requests made before the delete replay, as of their request.
    let save = commit(&store, &created, &key("save"), 1, "<h1>v2</h1>")?;
    assert_eq!(save.delivery, Delivery::Replayed);
    assert_eq!(save.artifact.deleted_at, None);
    let delete = set_deleted(&store, &created, &key("delete"), 1, true)?;
    assert_eq!(delete.delivery, Delivery::Replayed);
    assert_eq!(delete.artifact.deleted_at, Some(deleted_at));

    // Everything else is not found, a key used for another request too.
    let refused = [
        (
            "save",
            commit(&store, &created, &key("new"), 2, "<h1>v3</h1>").err(),
        ),
        (
            "save with a reused key",
            commit(&store, &created, &key("save"), 2, "<h1>v3</h1>").err(),
        ),
        (
            "rename",
            update(&store, &created, &key("rename"), 2, Some("New"), None).err(),
        ),
        (
            "delete again",
            set_deleted(&store, &created, &key("again"), 2, true).err(),
        ),
        ("copy", copy_of(&store, &created, &key("copy")).err()),
        (
            "copy with a reused key",
            copy_of(&store, &created, &key("save")).err(),
        ),
    ];
    for (what, error) in refused {
        assert_eq!(error, Some(StoreError::NotFound), "{what}");
    }
    let stored = store.artifact(&created.artifact.id)?.ok_or("kept")?;
    assert_eq!(metadata(&stored), ("Demo", false, 2, 2), "nothing written");
    assert_eq!(stored.deleted_at, Some(deleted_at));
    assert_eq!(store.revisions(&created.artifact.id)?.len(), 2);

    // It is listed only in the trash.
    assert_eq!(
        listed(&store, &owner, None, None, ListOrder::Updated, None, 10)?,
        Vec::<String>::new()
    );
    assert_eq!(trash_titles(&store, &owner)?, ["Demo"]);
    Ok(())
}

#[test]
fn a_recovery_is_the_one_update_that_reaches_the_trash() -> TestResult {
    let db = TempDb::new();
    let store = SqliteStore::open(&db.path())?;
    let owner = actor("owner");
    let created = create(&store, &owner, &key("a"), 1)?;
    let copy = copy_of(&store, &created, &key("copy"))?;
    assert_eq!(copy.delivery, Delivery::Committed, "a source in place");
    set_deleted(&store, &created, &key("delete"), 1, true)?;
    let kept = store.artifact(&copy.artifact.id)?.ok_or("copy kept")?;
    assert_eq!(kept.deleted_at, None, "a copy outlives its source's delete");

    // A recovery is conditional and keyed like any update.
    assert_eq!(
        set_deleted(&store, &created, &key("recover"), 1, false),
        Err(StoreError::MetadataConflict {
            current: MetadataRevision(2)
        })
    );
    assert_eq!(
        set_deleted(&store, &created, &key("delete"), 2, false),
        Err(StoreError::RequestKeyReused),
        "the delete's key does not recover"
    );
    let recovered = set_deleted(&store, &created, &key("recover"), 2, false)?;
    assert_eq!(recovered.artifact.deleted_at, None);
    assert_eq!(metadata(&recovered.artifact), ("Demo", false, 3, 1));
    assert_eq!(
        entry(&store, &created, 1)?,
        Some(b"<h1>v1</h1>".to_vec()),
        "the bytes stay"
    );
    // Outside the trash a recovery changes nothing.
    let again = set_deleted(&store, &created, &key("recover-again"), 3, false)?;
    assert_eq!(again.artifact.metadata_revision, MetadataRevision(3));

    // A delete that also renames is a delete.
    let both = store.update_metadata(MetadataUpdate {
        request: RequestIdentity {
            actor: &owner,
            key: &key("rename-and-delete"),
            fingerprint: RequestFingerprint([0xd2; 32]),
        },
        artifact: &created.artifact.id,
        base: MetadataRevision(3),
        title: Some(&parsed("Gone")),
        pinned: None,
        deleted: Some(true),
    })?;
    assert_eq!(metadata(&both.artifact), ("Gone", false, 4, 1));
    assert_eq!(trash_titles(&store, &owner)?, ["Gone"]);

    let page = store.changes(&owner, ChangeSequence(0), 100)?;
    let kinds: Vec<_> = page
        .changes
        .iter()
        .filter(|change| change.artifact == created.artifact.id)
        .map(|change| (change.kind, change.metadata_revision.0))
        .collect();
    assert_eq!(
        kinds,
        [
            (ChangeKind::Created, 1),
            (ChangeKind::Deleted, 2),
            (ChangeKind::Recovered, 3),
            (ChangeKind::Deleted, 4),
        ]
    );
    Ok(())
}
