//! Behavioral contract of the local `SQLite` artifact store.
use std::fmt::Display;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::Duration;

use artifree_domain::identity::{ActorId, RequestKey, Title};
use artifree_domain::package::{FilePackage, PackagePath};
use artifree_domain::{ArtifactKind, ChangeCursor, SourceRevision};
use artifree_ports::artifact_store::{
    ArtifactStore, ChangeKind, ChangePage, CommitReceipt, Delivery, NewArtifact,
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
        store.artifacts_owned_by(&owner)?.len(),
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
    assert_eq!(store.artifacts_owned_by(&owner)?.len(), 1);

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

/// One owner's feed after a cursor: `(cursor, revision, kind)` per change,
/// and the head.
type Feed = (Vec<(u64, u64, ChangeKind)>, u64);

fn feed(store: &SqliteStore, owner: &ActorId, after: u64) -> Result<Feed, StoreError> {
    let ChangePage { changes, head } = store.changes(owner, ChangeCursor(after), 100)?;
    let triples = changes
        .iter()
        .map(|change| (change.cursor.0, change.revision.0, change.kind))
        .collect();
    Ok((triples, head.0))
}

#[test]
fn every_commit_records_one_change_and_replays_record_none() -> TestResult {
    let db = TempDb::new();
    let store = SqliteStore::open(&db.path())?;
    let owner = actor("owner");
    assert_eq!(
        feed(&store, &owner, 0)?,
        (vec![], 0),
        "an empty feed has head 0"
    );
    let created = create(&store, &owner, &key("create"), 1)?;
    commit(&store, &created, &key("save"), 1, "<h1>v2</h1>")?;
    // Retries of both, and a stale save, write nothing.
    create(&store, &owner, &key("create"), 1)?;
    commit(&store, &created, &key("save"), 1, "<h1>v2</h1>")?;
    let stale = commit(&store, &created, &key("stale"), 1, "<h1>stale</h1>");
    assert!(matches!(stale, Err(StoreError::RevisionConflict { .. })));
    assert_eq!(
        feed(&store, &owner, 0)?,
        (
            vec![(1, 1, ChangeKind::Created), (2, 2, ChangeKind::Revised)],
            2
        )
    );
    let page = store.changes(&owner, ChangeCursor(0), 1)?;
    assert_eq!(page.changes.len(), 1, "the limit bounds a page");
    assert_eq!(page.changes[0].artifact, created.artifact.id);
    assert_eq!(page.changes[0].committed_at, created.artifact.updated_at);
    assert_eq!(
        feed(&store, &owner, 1)?.0,
        vec![(2, 2, ChangeKind::Revised)]
    );
    assert_eq!(feed(&store, &owner, 2)?, (vec![], 2));
    Ok(())
}

#[test]
fn the_feed_shows_only_the_owners_changes_but_the_global_head() -> TestResult {
    let db = TempDb::new();
    let store = SqliteStore::open(&db.path())?;
    let (one, two) = (actor("one"), actor("two"));
    create(&store, &one, &key("one"), 1)?;
    create(&store, &two, &key("two"), 1)?;
    assert_eq!(
        feed(&store, &one, 0)?,
        (vec![(1, 1, ChangeKind::Created)], 2)
    );
    assert_eq!(
        feed(&store, &two, 0)?,
        (vec![(2, 1, ChangeKind::Created)], 2)
    );
    Ok(())
}

#[test]
fn a_rolled_back_commit_records_no_change() -> TestResult {
    let db = TempDb::new();
    let store = SqliteStore::open(&db.path())?;
    let owner = actor("owner");
    let created = create(&store, &owner, &key("a"), 1)?;
    // Fail the change record itself, the last write of the commit.
    let connection = rusqlite::Connection::open(db.path())?;
    connection.execute_batch(
        "CREATE TRIGGER fail_change BEFORE INSERT ON changes BEGIN SELECT RAISE(ABORT, 'injected'); END;",
    )?;
    let failed = commit(&store, &created, &key("b"), 1, "<h1>lost</h1>");
    assert!(
        matches!(failed, Err(StoreError::Unavailable(ref reason)) if reason.contains("injected")),
        "{failed:?}"
    );
    assert_eq!(store.revisions(&created.artifact.id)?.len(), 1);
    assert_eq!(
        feed(&store, &owner, 0)?,
        (vec![(1, 1, ChangeKind::Created)], 1)
    );

    connection.execute_batch("DROP TRIGGER fail_change;")?;
    let retried = commit(&store, &created, &key("b"), 1, "<h1>lost</h1>")?;
    assert_eq!(
        retried.delivery,
        Delivery::Committed,
        "the key was not used"
    );
    assert_eq!(
        feed(&store, &owner, 1)?.0,
        vec![(2, 2, ChangeKind::Revised)]
    );
    Ok(())
}

#[test]
fn a_version_one_database_gains_the_feed_and_keeps_its_data() -> TestResult {
    let db = TempDb::new();
    let store = SqliteStore::open(&db.path())?;
    let owner = actor("owner");
    let created = create(&store, &owner, &key("a"), 1)?;
    // Recreate the version 1 layout: no changes table.
    let connection = rusqlite::Connection::open(db.path())?;
    connection.execute_batch("DROP TABLE changes; PRAGMA user_version = 1;")?;
    drop(connection);

    let store = SqliteStore::open(&db.path())?;
    let version: i64 =
        rusqlite::Connection::open(db.path())?
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
    assert_eq!(version, 2);
    assert_eq!(entry(&store, &created, 1)?, Some(b"<h1>v1</h1>".to_vec()));
    assert_eq!(
        create(&store, &owner, &key("a"), 1)?.delivery,
        Delivery::Replayed,
        "request keys survive the upgrade"
    );
    // Earlier commits have no rows; new ones do.
    assert_eq!(feed(&store, &owner, 0)?, (vec![], 0));
    commit(&store, &created, &key("b"), 1, "<h1>v2</h1>")?;
    assert_eq!(
        feed(&store, &owner, 0)?,
        (vec![(1, 2, ChangeKind::Revised)], 1)
    );
    Ok(())
}
