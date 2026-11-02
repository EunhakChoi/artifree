//! Removing an artifact in the trash for good, and deleting content nothing
//! refers to (decision 0029).

mod common;

use std::sync::mpsc;
use std::thread;

use artifree_domain::change::ChangeSequence;
use artifree_domain::identity::{ActorId, ArtifactId, RequestKey};
use artifree_domain::{ArtifactKind, MetadataRevision, SourceRevision};
use artifree_ports::artifact_store::{
    ArtifactListing, ArtifactStore, ChangeKind, CleanupReceipt, CleanupRequest, CommitReceipt,
    ContentSetDigest, Delivery, ListOrder, NewArtifact, PurgeReceipt, PurgeRequest,
    RequestFingerprint, RequestIdentity, StoreError, UnreferencedContent,
};
use artifree_providers::backup::{backup, restore};
use artifree_providers::storage::SqliteStore;
use common::{
    TempDir, TestResult, actor, create_demo, database_in, digest, key, package, parsed, revise,
    update_metadata,
};
use rusqlite::Connection;
use sha2::Digest as _;

const FIRST: &str = "<h1>first</h1>";
const SECOND: &str = "<h1>second</h1>";

struct Fixture {
    dir: TempDir,
    store: SqliteStore,
    owner: ActorId,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let dir = TempDir::new();
        let store = SqliteStore::open(&database_in(dir.path()))?;
        Ok(Self {
            dir,
            store,
            owner: actor("owner"),
        })
    }

    fn database(&self) -> std::path::PathBuf {
        database_in(self.dir.path())
    }

    fn sql(&self) -> Result<Connection, rusqlite::Error> {
        Connection::open(self.database())
    }

    fn count(&self, query: &str) -> Result<i64, rusqlite::Error> {
        self.sql()?.query_row(query, [], |row| row.get(0))
    }

    /// An artifact with two revisions (`FIRST`, then `SECOND`) made by the
    /// keys `create` and `save`.
    fn artifact(&self) -> Result<CommitReceipt, Box<dyn std::error::Error>> {
        let created = create_demo(&self.store, &self.owner, &key("create"), "Secret", FIRST)?;
        revise(&self.store, &created, &key("save"), 1, SECOND)?;
        Ok(created)
    }

    /// Moves the artifact to the trash with the key `delete`; its metadata
    /// revision is then 2.
    fn trash(&self, created: &CommitReceipt) -> Result<(), Box<dyn std::error::Error>> {
        update_metadata(
            &self.store,
            created,
            &key("delete"),
            1,
            None,
            None,
            Some(true),
        )?;
        Ok(())
    }

    fn purge(
        &self,
        id: &ArtifactId,
        request_key: &RequestKey,
        base: u64,
    ) -> Result<PurgeReceipt, StoreError> {
        self.store.purge_artifact(PurgeRequest {
            request: RequestIdentity {
                actor: &self.owner,
                key: request_key,
                fingerprint: RequestFingerprint(digest(&format!("purge {id} {base}"))),
            },
            artifact: id,
            base: MetadataRevision(base),
        })
    }

    /// A copy of the artifact's second revision, made as the core makes one.
    fn copy(
        &self,
        source: &CommitReceipt,
        request_key: &RequestKey,
    ) -> Result<CommitReceipt, StoreError> {
        self.store.create_artifact(NewArtifact {
            request: RequestIdentity {
                actor: &self.owner,
                key: request_key,
                fingerprint: RequestFingerprint(digest("copy")),
            },
            kind: ArtifactKind::App,
            title: &parsed("Copy"),
            package: &package(&[("index.html", SECOND), ("style.css", "h1{}")]),
            source: Some(&source.artifact.id),
        })
    }

    fn bytes(&self, id: &ArtifactId, revision: u64) -> Result<Option<Vec<u8>>, StoreError> {
        self.store
            .revision_file(id, SourceRevision(revision), &parsed("index.html"))
    }

    /// What a deletion of shared content would break, checked from outside the
    /// store and without relying on `SQLite` refusing the delete (a foreign key
    /// failure is a defense, not the assertion): files whose content is gone,
    /// and rows the foreign key check reports.
    fn assert_every_file_has_its_content(&self) -> TestResult {
        let connection = self.sql()?;
        let dangling: i64 = connection.query_row(
            "SELECT count(*) FROM revision_files f
             WHERE NOT EXISTS (SELECT 1 FROM blobs b WHERE b.digest = f.digest)",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(dangling, 0, "revision files lost their content");
        let mut check = connection.prepare("PRAGMA foreign_key_check")?;
        let violations = check.query_map([], |row| row.get::<_, String>(0))?.count();
        assert_eq!(violations, 0, "foreign key check reports violations");
        // Each stored size and digest still describes the bytes that are read.
        let mut files = connection.prepare(
            "SELECT f.size, f.digest, b.content FROM revision_files f
             JOIN blobs b ON b.digest = f.digest",
        )?;
        let rows = files.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })?;
        for row in rows {
            let (size, stored, content) = row?;
            assert_eq!(usize::try_from(size)?, content.len());
            assert_eq!(stored, sha2::Sha256::digest(&content).to_vec());
        }
        Ok(())
    }

    /// A cleanup of the set at `base` under the key `name`.
    fn cleanup(&self, name: &str, base: ContentSetDigest) -> Result<CleanupReceipt, StoreError> {
        self.store.remove_unreferenced_content(CleanupRequest {
            request: RequestIdentity {
                actor: &self.owner,
                key: &key(name),
                fingerprint: RequestFingerprint(digest(&format!("cleanup {base}"))),
            },
            base,
        })
    }

    /// The dry run, then a cleanup of exactly what it reported.
    fn clean_up(&self, name: &str) -> Result<CleanupReceipt, StoreError> {
        self.cleanup(name, self.unreferenced()?.digest)
    }

    /// Plants content that no revision file refers to, as an interrupted
    /// operation might leave it.
    fn plant_stray(&self, body: &str) -> Result<(), rusqlite::Error> {
        self.sql()?.execute(
            "INSERT INTO blobs (digest, content) VALUES (?1, CAST(?2 AS BLOB))",
            rusqlite::params![digest(body).as_slice(), body],
        )?;
        Ok(())
    }

    fn assert_nothing_unreferenced(&self) -> TestResult {
        let found = self.unreferenced()?;
        assert_eq!((found.blobs, found.bytes), (0, 0), "unreferenced content");
        Ok(())
    }

    fn unreferenced(&self) -> Result<UnreferencedContent, StoreError> {
        self.store.unreferenced_content()
    }
}

/// Fingerprint of the request `create_demo` makes for `body`, to retry it.
fn retry_create(
    fixture: &Fixture,
    request_key: &RequestKey,
    body: &str,
) -> Result<CommitReceipt, StoreError> {
    create_demo(&fixture.store, &fixture.owner, request_key, "Secret", body)
}

#[test]
fn an_artifact_outside_the_trash_cannot_be_purged() -> TestResult {
    let fixture = Fixture::new()?;
    let created = fixture.artifact()?;
    let id = &created.artifact.id;

    let outcome = fixture.purge(id, &key("purge"), 1);

    assert_eq!(outcome, Err(StoreError::NotInTrash));
    assert!(fixture.store.artifact(id)?.is_some());
    assert_eq!(fixture.bytes(id, 2)?.as_deref(), Some(SECOND.as_bytes()));
    assert_eq!(fixture.count("SELECT count(*) FROM blobs")?, 3);
    // The refused request left nothing behind, not even its key.
    assert_eq!(
        fixture.count("SELECT count(*) FROM request_keys WHERE request_key = 'test-key-purge'")?,
        0
    );
    // The same key is free for a purge that is allowed.
    fixture.trash(&created)?;
    assert_eq!(
        fixture.purge(id, &key("purge"), 2)?.delivery,
        Delivery::Committed
    );
    Ok(())
}

#[test]
fn a_purge_prepared_against_an_older_trash_state_conflicts() -> TestResult {
    let fixture = Fixture::new()?;
    let created = fixture.artifact()?;
    let id = &created.artifact.id;
    fixture.trash(&created)?;
    // Recovered (metadata 3) and deleted again (metadata 4).
    update_metadata(
        &fixture.store,
        &created,
        &key("recover"),
        2,
        None,
        None,
        Some(false),
    )?;
    update_metadata(
        &fixture.store,
        &created,
        &key("delete-again"),
        3,
        None,
        None,
        Some(true),
    )?;

    let outcome = fixture.purge(id, &key("old"), 2);

    assert_eq!(
        outcome,
        Err(StoreError::MetadataConflict {
            current: MetadataRevision(4)
        })
    );
    assert!(
        fixture.store.artifact(id)?.is_some(),
        "the artifact was not removed"
    );
    assert_eq!(fixture.bytes(id, 2)?.as_deref(), Some(SECOND.as_bytes()));
    assert_eq!(
        fixture.purge(id, &key("new"), 4)?.delivery,
        Delivery::Committed
    );
    Ok(())
}

#[test]
fn a_purge_deletes_files_and_titles_and_keeps_only_headers() -> TestResult {
    let fixture = Fixture::new()?;
    let created = fixture.artifact()?;
    let id = &created.artifact.id;
    fixture.trash(&created)?;
    assert_eq!(fixture.count("SELECT count(*) FROM blobs")?, 3);

    let receipt = fixture.purge(id, &key("purge"), 2)?;

    assert_eq!(receipt.artifact, *id);
    assert_eq!(receipt.metadata_revision, MetadataRevision(3));
    assert_eq!(receipt.delivery, Delivery::Committed);
    // Gone to every read.
    assert_eq!(fixture.store.artifact(id)?, None);
    assert!(fixture.store.revisions(id)?.is_empty());
    assert_eq!(fixture.store.revision(id, SourceRevision(1))?, None);
    assert_eq!(fixture.store.revision_package(id, SourceRevision(2))?, None);
    assert_eq!(fixture.bytes(id, 1)?, None);
    let listing = |deleted| {
        fixture.store.list_artifacts(ArtifactListing {
            owner: &fixture.owner,
            search: None,
            pinned: None,
            deleted,
            order: ListOrder::Updated,
            after: None,
            limit: 10,
        })
    };
    assert!(listing(true)?.is_empty(), "the trash no longer lists it");
    assert!(listing(false)?.is_empty());
    // What the database still holds of it: no content, listing, or title.
    assert_eq!(fixture.count("SELECT count(*) FROM blobs")?, 0);
    assert_eq!(fixture.count("SELECT count(*) FROM revision_files")?, 0);
    assert_eq!(
        fixture.count("SELECT count(*) FROM metadata_revisions WHERE title <> ''")?,
        0
    );
    assert_eq!(
        fixture.count("SELECT count(*) FROM artifacts WHERE title <> ''")?,
        0
    );
    // Headers, keys, and the artifact row stay, which keeps every foreign key.
    assert_eq!(fixture.count("SELECT count(*) FROM artifacts")?, 1);
    assert_eq!(fixture.count("SELECT count(*) FROM revisions")?, 2);
    assert_eq!(fixture.count("SELECT count(*) FROM metadata_revisions")?, 3);
    assert_eq!(fixture.count("SELECT count(*) FROM request_keys")?, 4);
    let violations: i64 =
        fixture
            .sql()?
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })?;
    assert_eq!(violations, 0);
    Ok(())
}

#[test]
fn a_purge_replays_and_a_key_of_another_request_is_refused() -> TestResult {
    let fixture = Fixture::new()?;
    let created = fixture.artifact()?;
    let id = &created.artifact.id;
    fixture.trash(&created)?;
    let first = fixture.purge(id, &key("purge"), 2)?;
    let changes_before = fixture.count("SELECT count(*) FROM changes")?;

    let again = fixture.purge(id, &key("purge"), 2)?;

    assert_eq!(again.delivery, Delivery::Replayed);
    assert_eq!(
        (again.artifact, again.metadata_revision, again.purged_at),
        (first.artifact, first.metadata_revision, first.purged_at)
    );
    assert_eq!(
        fixture.count("SELECT count(*) FROM changes")?,
        changes_before
    );
    // The key of the purge, used for another artifact in the trash, is reused.
    let other = create_demo(
        &fixture.store,
        &fixture.owner,
        &key("other"),
        "Other",
        "<p>o</p>",
    )?;
    update_metadata(
        &fixture.store,
        &other,
        &key("delete-other"),
        1,
        None,
        None,
        Some(true),
    )?;
    assert_eq!(
        fixture.purge(&other.artifact.id, &key("purge"), 2),
        Err(StoreError::RequestKeyReused)
    );
    assert!(
        fixture.store.artifact(&other.artifact.id)?.is_some(),
        "other kept"
    );
    // The same key with another base names the purged artifact: not found,
    // as every request that names it (decision 0029, item 4).
    assert_eq!(
        fixture.purge(id, &key("purge"), 3),
        Err(StoreError::NotFound)
    );
    // A purge of the purged artifact under a fresh key is not found either.
    assert_eq!(
        fixture.purge(id, &key("fresh"), 3),
        Err(StoreError::NotFound)
    );
    Ok(())
}

#[test]
fn earlier_keys_are_not_found_after_a_purge_and_stay_reserved() -> TestResult {
    let fixture = Fixture::new()?;
    let created = fixture.artifact()?;
    let id = &created.artifact.id;
    fixture.trash(&created)?;
    // Before the purge the trash still replays the create and the save.
    assert_eq!(
        retry_create(&fixture, &key("create"), FIRST)?.delivery,
        Delivery::Replayed
    );
    fixture.purge(id, &key("purge"), 2)?;
    let keys_before = fixture.count("SELECT count(*) FROM request_keys")?;

    // The retry of the create no longer answers with the removed artifact,
    // and does not create another.
    assert_eq!(
        retry_create(&fixture, &key("create"), FIRST),
        Err(StoreError::NotFound)
    );
    assert_eq!(
        revise(&fixture.store, &created, &key("save"), 1, SECOND).err(),
        Some(StoreError::NotFound)
    );
    assert_eq!(
        update_metadata(
            &fixture.store,
            &created,
            &key("delete"),
            1,
            None,
            None,
            Some(true)
        )
        .err(),
        Some(StoreError::NotFound)
    );
    assert_eq!(
        update_metadata(
            &fixture.store,
            &created,
            &key("delete"),
            2,
            None,
            None,
            Some(false)
        )
        .err(),
        Some(StoreError::NotFound)
    );
    // A key never expires: another body under the create's key is still a
    // reuse, and the key is not free for a new artifact.
    assert_eq!(
        retry_create(&fixture, &key("create"), "<h1>other</h1>"),
        Err(StoreError::RequestKeyReused)
    );
    assert_eq!(fixture.count("SELECT count(*) FROM artifacts")?, 1);
    assert_eq!(
        fixture.count("SELECT count(*) FROM request_keys")?,
        keys_before
    );
    // The identifier is never reissued: a new artifact gets a new one.
    let fresh = create_demo(
        &fixture.store,
        &fixture.owner,
        &key("fresh"),
        "Fresh",
        FIRST,
    )?;
    assert_ne!(fresh.artifact.id, *id);
    Ok(())
}

#[test]
fn the_feed_keeps_its_sequences_and_announces_the_purge() -> TestResult {
    let fixture = Fixture::new()?;
    let created = fixture.artifact()?;
    let id = &created.artifact.id;
    fixture.trash(&created)?;
    let before = fixture
        .store
        .changes(&fixture.owner, ChangeSequence(0), 100)?;
    assert_eq!(before.head, ChangeSequence(3));

    fixture.purge(id, &key("purge"), 2)?;

    let after = fixture
        .store
        .changes(&fixture.owner, ChangeSequence(0), 100)?;
    assert_eq!(after.feed, before.feed);
    assert_eq!(after.head, ChangeSequence(4), "one more, never fewer");
    assert_eq!(
        after.changes[..3],
        before.changes[..],
        "earlier rows are kept"
    );
    let purge = &after.changes[3];
    assert_eq!(
        (
            purge.sequence,
            purge.kind,
            purge.revision,
            purge.metadata_revision
        ),
        (
            ChangeSequence(4),
            ChangeKind::Purged,
            SourceRevision(2),
            MetadataRevision(3)
        )
    );
    // A reader that had seen the delete learns of the purge and nothing else.
    let behind = fixture
        .store
        .changes(&fixture.owner, ChangeSequence(3), 100)?;
    assert_eq!(behind.changes.len(), 1);
    assert_eq!(behind.changes[0].kind, ChangeKind::Purged);
    // Another owner learns nothing of it.
    let stranger = fixture
        .store
        .changes(&actor("someone-else"), ChangeSequence(0), 100)?;
    assert!(stranger.changes.is_empty());
    assert_eq!(stranger.head, ChangeSequence(4));
    // A replay appends nothing; the next commit continues the sequence.
    fixture.purge(id, &key("purge"), 2)?;
    let next = create_demo(
        &fixture.store,
        &fixture.owner,
        &key("next"),
        "Next",
        "<p>n</p>",
    )?;
    let page = fixture
        .store
        .changes(&fixture.owner, ChangeSequence(4), 100)?;
    assert_eq!(page.changes.len(), 1);
    assert_eq!(page.changes[0].artifact, next.artifact.id);
    assert_eq!(page.changes[0].sequence, ChangeSequence(5));
    Ok(())
}

#[test]
fn a_copy_keeps_its_history_and_bytes_when_the_source_is_purged() -> TestResult {
    let fixture = Fixture::new()?;
    let source = fixture.artifact()?;
    let copy = fixture.copy(&source, &key("copy"))?;
    let copy_id = &copy.artifact.id;
    revise(
        &fixture.store,
        &copy,
        &key("copy-save"),
        1,
        "<h1>copy 2</h1>",
    )?;
    fixture.trash(&source)?;
    // Blobs: FIRST, SECOND, style.css, copy 2; the copy shares SECOND and the style.
    assert_eq!(fixture.count("SELECT count(*) FROM blobs")?, 4);

    fixture.purge(&source.artifact.id, &key("purge"), 2)?;

    // Only content nothing else refers to was deleted: FIRST.
    assert_eq!(fixture.count("SELECT count(*) FROM blobs")?, 3);
    fixture.assert_every_file_has_its_content()?;
    for shared in [SECOND, "h1{}"] {
        let present = fixture.sql()?.query_row(
            "SELECT count(*) FROM blobs WHERE digest = ?1",
            [digest(shared).as_slice()],
            |row| row.get::<_, i64>(0),
        )?;
        assert_eq!(present, 1, "shared content {shared:?} was deleted");
    }
    assert_eq!(
        fixture.bytes(copy_id, 1)?.as_deref(),
        Some(SECOND.as_bytes())
    );
    assert_eq!(
        fixture.bytes(copy_id, 2)?.as_deref(),
        Some(b"<h1>copy 2</h1>".as_slice())
    );
    assert_eq!(fixture.store.revisions(copy_id)?.len(), 2);
    assert_eq!(
        fixture
            .store
            .artifact(copy_id)?
            .map(|r| r.title.as_str().to_owned()),
        Some("Copy".to_owned())
    );
    fixture.assert_nothing_unreferenced()?;
    // The copy's request still replays, although its source is gone.
    let replay = fixture
        .store
        .replay_create(RequestIdentity {
            actor: &fixture.owner,
            key: &key("copy"),
            fingerprint: RequestFingerprint(digest("copy")),
        })?
        .ok_or("the copy's receipt")?;
    assert_eq!(replay.delivery, Delivery::Replayed);
    assert_eq!(replay.artifact.id, *copy_id);
    assert_eq!(replay.revision.files.len(), 2);
    // A new copy of the purged source is not found.
    assert_eq!(
        fixture.copy(&source, &key("again")),
        Err(StoreError::NotFound)
    );
    Ok(())
}

#[test]
fn replay_create_answers_only_for_a_create_whose_artifact_exists() -> TestResult {
    let fixture = Fixture::new()?;
    let created = fixture.artifact()?;
    let replay = |request_key: &RequestKey, fingerprint| {
        fixture.store.replay_create(RequestIdentity {
            actor: &fixture.owner,
            key: request_key,
            fingerprint: RequestFingerprint(fingerprint),
        })
    };

    assert_eq!(
        replay(&key("create"), digest(FIRST))?.map(|r| r.delivery),
        Some(Delivery::Replayed)
    );
    assert!(
        replay(&key("unknown"), digest(FIRST))?.is_none(),
        "no such key"
    );
    assert!(
        replay(&key("create"), digest("other"))?.is_none(),
        "another fingerprint"
    );
    // Another actor's key space is separate.
    let stranger = fixture.store.replay_create(RequestIdentity {
        actor: &actor("someone-else"),
        key: &key("create"),
        fingerprint: RequestFingerprint(digest(FIRST)),
    })?;
    assert!(stranger.is_none());
    fixture.trash(&created)?;
    fixture.purge(&created.artifact.id, &key("purge"), 2)?;
    assert!(
        replay(&key("create"), digest(FIRST))?.is_none(),
        "the artifact was purged"
    );
    Ok(())
}

#[test]
fn only_unreferenced_content_is_removed_and_the_dry_run_removes_nothing() -> TestResult {
    let fixture = Fixture::new()?;
    let live = fixture.artifact()?;
    let trashed = create_demo(
        &fixture.store,
        &fixture.owner,
        &key("t"),
        "T",
        "<h1>trashed</h1>",
    )?;
    update_metadata(
        &fixture.store,
        &trashed,
        &key("t-delete"),
        1,
        None,
        None,
        Some(true),
    )?;
    let copy = fixture.copy(&live, &key("copy"))?;
    fixture.assert_nothing_unreferenced()?;
    // Content no file refers to: a leftover of some other operation.
    fixture.sql()?.execute(
        "INSERT INTO blobs (digest, content) VALUES (zeroblob(32), CAST('stray!' AS BLOB))",
        [],
    )?;
    let blobs_before = fixture.count("SELECT count(*) FROM blobs")?;

    let dry_run = fixture.unreferenced()?;

    assert_eq!((dry_run.blobs, dry_run.bytes), (1, 6));
    assert_eq!(
        fixture.count("SELECT count(*) FROM blobs")?,
        blobs_before,
        "dry run deleted"
    );

    let removed = fixture.cleanup("clean-1", dry_run.digest)?;

    assert_eq!(
        removed,
        CleanupReceipt {
            blobs: 1,
            bytes: 6,
            delivery: Delivery::Committed
        }
    );
    assert_eq!(
        fixture.count("SELECT count(*) FROM blobs")?,
        blobs_before - 1
    );
    // Nothing a file refers to is gone, in the trash or not, or shared.
    fixture.assert_every_file_has_its_content()?;
    assert_eq!(
        fixture.bytes(&live.artifact.id, 1)?.as_deref(),
        Some(FIRST.as_bytes())
    );
    assert_eq!(
        fixture.bytes(&live.artifact.id, 2)?.as_deref(),
        Some(SECOND.as_bytes())
    );
    assert_eq!(
        fixture.bytes(&copy.artifact.id, 1)?.as_deref(),
        Some(SECOND.as_bytes())
    );
    assert_eq!(
        fixture
            .store
            .revision_package(&trashed.artifact.id, SourceRevision(1))?
            .map(|p| p.len()),
        Some(2)
    );
    // A new run finds nothing.
    fixture.assert_nothing_unreferenced()?;
    let again = fixture.clean_up("clean-2")?;
    assert_eq!((again.blobs, again.bytes), (0, 0));
    Ok(())
}

/// The apply decides again: content the dry run listed that a commit has
/// referred to since is kept, and the cleanup that reviewed it is refused
/// because the set is no longer the one reviewed.
#[test]
fn content_a_commit_refers_to_after_the_dry_run_is_kept() -> TestResult {
    let fixture = Fixture::new()?;
    let body = "<h1>late</h1>";
    // Stray content whose digest is the one a commit is about to store.
    fixture.sql()?.execute(
        "INSERT INTO blobs (digest, content) VALUES (?1, CAST(?2 AS BLOB))",
        rusqlite::params![digest(body).as_slice(), body],
    )?;
    let dry_run = fixture.unreferenced()?;
    assert_eq!(dry_run.blobs, 1);

    // A commit of the same bytes refers to the stored content.
    let created = create_demo(&fixture.store, &fixture.owner, &key("late"), "Late", body)?;
    let refused = fixture.cleanup("clean-late", dry_run.digest);

    assert_eq!(refused, Err(StoreError::ContentChanged));
    assert_eq!(
        fixture.bytes(&created.artifact.id, 1)?.as_deref(),
        Some(body.as_bytes())
    );
    fixture.assert_every_file_has_its_content()?;
    Ok(())
}

/// A commit in flight holds the write lock and refers to content the sweep
/// would otherwise remove. The sweep waits for the lock, so whichever
/// finishes first, the content stays. Not a timing test: both orders are
/// valid, and the outcome must be the same.
#[test]
fn a_cleanup_beside_a_commit_in_flight_keeps_what_the_commit_refers_to() -> TestResult {
    let fixture = Fixture::new()?;
    let held = create_demo(
        &fixture.store,
        &fixture.owner,
        &key("held"),
        "Held",
        "<p>held</p>",
    )?;
    let stray = "<h1>stray</h1>";
    fixture.sql()?.execute(
        "INSERT INTO blobs (digest, content) VALUES (?1, CAST(?2 AS BLOB))",
        rusqlite::params![digest(stray).as_slice(), stray],
    )?;
    let mut connection = fixture.sql()?;
    connection.pragma_update(None, "foreign_keys", true)?;
    // The commit of a second revision, begun but not finished.
    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    transaction.execute(
        "INSERT INTO revisions (artifact_id, revision, created_by, created_at)
         VALUES (?1, 2, 'owner', 1)",
        [held.artifact.id.as_str()],
    )?;
    transaction.execute(
        "INSERT INTO revision_files (artifact_id, revision, path, size, digest)
         VALUES (?1, 2, 'index.html', 14, ?2)",
        rusqlite::params![held.artifact.id.as_str(), digest(stray).as_slice()],
    )?;

    // The dry run cannot see the uncommitted file, so it reports the stray.
    let reviewed = fixture.unreferenced()?;
    assert_eq!(reviewed.blobs, 1);
    let (started, begun) = mpsc::channel();
    let store = fixture.store.clone();
    let cleanup = thread::spawn(move || {
        started.send(()).ok();
        store.remove_unreferenced_content(CleanupRequest {
            request: RequestIdentity {
                actor: &actor("owner"),
                key: &key("clean-flight"),
                fingerprint: RequestFingerprint(digest(&format!("cleanup {}", reviewed.digest))),
            },
            base: reviewed.digest,
        })
    });
    begun.recv()?;
    transaction.commit()?;
    let refused = cleanup.join().map_err(|_| "the cleanup thread panicked")?;

    assert_eq!(
        refused,
        Err(StoreError::ContentChanged),
        "the set the cleanup reviewed was changed by the commit"
    );
    let kept: i64 = fixture.count("SELECT count(*) FROM revision_files WHERE revision = 2")?;
    assert_eq!(kept, 1);
    assert_eq!(
        fixture.bytes(&held.artifact.id, 2)?.as_deref(),
        Some(stray.as_bytes())
    );
    Ok(())
}

/// The first run under a key records what it deleted, with the key, in the
/// same transaction. A retry whose response was lost gets that report back,
/// not "nothing found".
#[test]
fn a_retry_of_a_cleanup_replays_the_original_report() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.plant_stray("<h1>stray</h1>")?;
    let reviewed = fixture.unreferenced()?;
    assert_eq!((reviewed.blobs, reviewed.bytes), (1, 14));

    let first = fixture.cleanup("clean-key", reviewed.digest)?;
    let retry = fixture.cleanup("clean-key", reviewed.digest)?;

    assert_eq!(
        first,
        CleanupReceipt {
            blobs: 1,
            bytes: 14,
            delivery: Delivery::Committed
        }
    );
    assert_eq!(
        retry,
        CleanupReceipt {
            blobs: 1,
            bytes: 14,
            delivery: Delivery::Replayed
        },
        "the retry must report what the first run deleted, not that the set is empty"
    );
    assert_eq!(fixture.count("SELECT count(*) FROM blobs")?, 0);
    // A run under a new key against the now empty set reports nothing.
    let empty = fixture.unreferenced()?;
    let nothing = fixture.cleanup("clean-key-2", empty.digest)?;
    assert_eq!((nothing.blobs, nothing.bytes), (0, 0));
    assert_eq!(nothing.delivery, Delivery::Committed);
    // The record holds counts and the key, never content or names.
    let columns: Vec<String> = fixture
        .sql()?
        .prepare("SELECT name FROM pragma_table_info('cleanup_runs') ORDER BY cid")?
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    assert_eq!(
        columns,
        [
            "actor",
            "request_key",
            "fingerprint",
            "blobs",
            "bytes",
            "created_at"
        ]
    );
    Ok(())
}

#[test]
fn a_key_used_with_another_base_is_a_reused_key_and_deletes_nothing() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.plant_stray("<h1>one</h1>")?;
    let one = fixture.unreferenced()?;
    fixture.cleanup("clean-key", one.digest)?;
    fixture.plant_stray("<h1>two</h1>")?;
    let two = fixture.unreferenced()?;
    assert_ne!(one.digest, two.digest);

    let refused = fixture.cleanup("clean-key", two.digest);

    assert_eq!(refused, Err(StoreError::RequestKeyReused));
    assert_eq!(
        fixture.count("SELECT count(*) FROM blobs")?,
        1,
        "the second set must not be deleted under the used key"
    );
    Ok(())
}

#[test]
fn a_cleanup_of_a_set_that_is_not_the_reviewed_one_deletes_nothing_and_keeps_its_key() -> TestResult
{
    let fixture = Fixture::new()?;
    fixture.plant_stray("<h1>reviewed</h1>")?;
    let reviewed = fixture.unreferenced()?;
    // Something else becomes unreferenced after the review.
    fixture.plant_stray("<h1>unseen</h1>")?;

    let refused = fixture.cleanup("clean-key", reviewed.digest);

    assert_eq!(refused, Err(StoreError::ContentChanged));
    assert_eq!(
        fixture.count("SELECT count(*) FROM blobs")?,
        2,
        "nothing the caller did not review may be deleted"
    );
    assert_eq!(fixture.count("SELECT count(*) FROM cleanup_runs")?, 0);
    // The refusal consumed nothing: the same key works after a new review.
    let received = fixture.clean_up("clean-key")?;
    assert_eq!(
        (received.blobs, received.delivery),
        (2, Delivery::Committed)
    );
    Ok(())
}

/// One key space: a key an artifact request used cannot start a cleanup, and
/// a key a cleanup used cannot create or change an artifact.
#[test]
fn a_cleanup_shares_its_key_space_with_the_requests_that_name_an_artifact() -> TestResult {
    let fixture = Fixture::new()?;
    let created = fixture.artifact()?;
    let found = fixture.unreferenced()?;

    assert_eq!(
        fixture.cleanup("create", found.digest),
        Err(StoreError::RequestKeyReused)
    );
    assert_eq!(
        fixture.cleanup("save", found.digest),
        Err(StoreError::RequestKeyReused)
    );

    fixture.cleanup("clean-key", found.digest)?;
    assert_eq!(
        create_demo(
            &fixture.store,
            &fixture.owner,
            &key("clean-key"),
            "X",
            FIRST
        )
        .err(),
        Some(StoreError::RequestKeyReused)
    );
    assert_eq!(
        revise(&fixture.store, &created, &key("clean-key"), 2, "<p>y</p>").err(),
        Some(StoreError::RequestKeyReused)
    );
    // Another actor's keys are separate.
    let other = fixture.store.remove_unreferenced_content(CleanupRequest {
        request: RequestIdentity {
            actor: &actor("someone-else"),
            key: &key("clean-key"),
            fingerprint: RequestFingerprint(digest(&format!("cleanup {}", found.digest))),
        },
        base: found.digest,
    })?;
    assert_eq!(other.delivery, Delivery::Committed);
    Ok(())
}

#[test]
fn a_cleanup_report_survives_a_backup_and_restore() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.plant_stray("<h1>stray</h1>")?;
    let reviewed = fixture.unreferenced()?;
    fixture.cleanup("clean-key", reviewed.digest)?;
    std::fs::create_dir(fixture.dir.join("out"))?;
    let output = fixture.dir.join("out").join("backup.sqlite3");
    let target = fixture.dir.join("restored");
    backup(fixture.dir.path(), &output)?;
    restore(&output, &target)?;

    let restored = SqliteStore::open(&database_in(&target))?;
    let retry = restored.remove_unreferenced_content(CleanupRequest {
        request: RequestIdentity {
            actor: &fixture.owner,
            key: &key("clean-key"),
            fingerprint: RequestFingerprint(digest(&format!("cleanup {}", reviewed.digest))),
        },
        base: reviewed.digest,
    })?;
    assert_eq!(
        retry,
        CleanupReceipt {
            blobs: 1,
            bytes: 14,
            delivery: Delivery::Replayed
        }
    );
    Ok(())
}

#[test]
fn a_content_set_digest_is_written_and_read_as_lowercase_hex_only() {
    let digest = ContentSetDigest([0xab; 32]);
    let text = digest.to_string();
    assert_eq!(text, "ab".repeat(32));
    assert_eq!(ContentSetDigest::parse(&text), Some(digest));
    for rejected in [
        text.to_uppercase(),
        text[..62].to_owned(),
        format!("{text}00"),
        format!("{} ", &text[..63]),
        format!("g{}", &text[1..]),
        String::new(),
    ] {
        assert_eq!(ContentSetDigest::parse(&rejected), None, "{rejected:?}");
    }
}

#[test]
fn a_backup_and_restore_keep_a_purged_artifact_gone_and_its_keys_answering() -> TestResult {
    let fixture = Fixture::new()?;
    let created = fixture.artifact()?;
    let id = &created.artifact.id;
    let survivor = create_demo(
        &fixture.store,
        &fixture.owner,
        &key("survivor"),
        "Keep",
        "<p>keep</p>",
    )?;
    fixture.trash(&created)?;
    let purged = fixture.purge(id, &key("purge"), 2)?;
    std::fs::create_dir(fixture.dir.join("out"))?;
    let output = fixture.dir.join("out").join("backup.sqlite3");
    let target = fixture.dir.join("restored");

    backup(fixture.dir.path(), &output)?;
    restore(&output, &target)?;

    let restored = SqliteStore::open(&database_in(&target))?;
    assert_eq!(restored.artifact(id)?, None);
    assert!(restored.artifact(&survivor.artifact.id)?.is_some());
    // The retry of the purge replays the original result.
    let replay = restored.purge_artifact(PurgeRequest {
        request: RequestIdentity {
            actor: &fixture.owner,
            key: &key("purge"),
            fingerprint: RequestFingerprint(digest(&format!("purge {id} 2"))),
        },
        artifact: id,
        base: MetadataRevision(2),
    })?;
    assert_eq!(replay.delivery, Delivery::Replayed);
    assert_eq!(replay.purged_at, purged.purged_at);
    // The earlier create is still not found, and its key still reserved.
    assert_eq!(
        create_demo(&restored, &fixture.owner, &key("create"), "Secret", FIRST).err(),
        Some(StoreError::NotFound)
    );
    assert_eq!(
        create_demo(
            &restored,
            &fixture.owner,
            &key("create"),
            "Secret",
            "<p>x</p>"
        )
        .err(),
        Some(StoreError::RequestKeyReused)
    );
    let found = restored.unreferenced_content()?;
    assert_eq!((found.blobs, found.bytes), (0, 0));
    // The feed's head is kept and continues.
    let page = restored.changes(&fixture.owner, ChangeSequence(0), 100)?;
    assert_eq!(page.head, ChangeSequence(5));
    Ok(())
}

#[test]
fn a_purge_time_cannot_be_recorded_for_an_artifact_outside_the_trash() -> TestResult {
    let fixture = Fixture::new()?;
    let created = fixture.artifact()?;

    let outcome = fixture.sql()?.execute(
        "UPDATE artifacts SET purged_at = 1 WHERE id = ?1",
        [created.artifact.id.as_str()],
    );

    let error = outcome
        .err()
        .ok_or("the schema accepted a purge of a live artifact")?;
    assert!(error.to_string().contains("CHECK"), "{error}");
    Ok(())
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// What a purge deletes is overwritten, not only marked free: after the log
/// is checkpointed and truncated, neither the database file nor the log holds
/// the bytes. (Earlier backups and a log not yet truncated still can.)
#[test]
fn purged_content_is_erased_from_the_database_file() -> TestResult {
    let marker = "purge-marker-7f3a9c41";
    let body = format!("<p>{}</p>", marker.repeat(3000));
    let fixture = Fixture::new()?;
    let created = create_demo(&fixture.store, &fixture.owner, &key("big"), "Big", &body)?;
    let checkpoint = || -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let connection = fixture.sql()?;
        connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        let mut bytes = std::fs::read(fixture.database())?;
        let log = fixture.dir.join("artifree.sqlite3-wal");
        if log.exists() {
            bytes.extend(std::fs::read(log)?);
        }
        Ok(bytes)
    };
    assert!(
        contains(&checkpoint()?, marker.as_bytes()),
        "the test premise: the content is in the file before the purge"
    );
    update_metadata(
        &fixture.store,
        &created,
        &key("delete"),
        1,
        None,
        None,
        Some(true),
    )?;

    fixture.purge(&created.artifact.id, &key("purge"), 2)?;

    assert!(
        !contains(&checkpoint()?, marker.as_bytes()),
        "purged content is still in the database file"
    );
    Ok(())
}

/// The sweep of unreferenced content erases too.
#[test]
fn removed_unreferenced_content_is_erased_from_the_database_file() -> TestResult {
    let marker = "stray-marker-52d8e0b7";
    let body = marker.repeat(3000);
    let fixture = Fixture::new()?;
    fixture.sql()?.execute(
        "INSERT INTO blobs (digest, content) VALUES (?1, CAST(?2 AS BLOB))",
        rusqlite::params![digest(&body).as_slice(), body],
    )?;
    let checkpoint = || -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        fixture
            .sql()?
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        let mut bytes = std::fs::read(fixture.database())?;
        let log = fixture.dir.join("artifree.sqlite3-wal");
        if log.exists() {
            bytes.extend(std::fs::read(log)?);
        }
        Ok(bytes)
    };
    assert!(contains(&checkpoint()?, marker.as_bytes()), "the premise");

    assert_eq!(fixture.clean_up("clean-erase")?.blobs, 1);

    assert!(
        !contains(&checkpoint()?, marker.as_bytes()),
        "the content stayed"
    );
    Ok(())
}
