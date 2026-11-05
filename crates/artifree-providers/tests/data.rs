//! Behavioral contract of application data in the `SQLite` store (decision
//! 0031): versions, conditional writes, key replay and the shared key space,
//! the trash, purge and its erasure, independent copies, and concurrent writers.

mod common;

use std::sync::{Arc, Barrier};
use std::thread;

use artifree_domain::data::{DataChange, DataError, DataLimit, DataPath, DataValue};
use artifree_domain::identity::{ActorId, ArtifactId, UnixMillis};
use artifree_domain::job::JobKind;
use artifree_domain::{AppDataVersion, MetadataRevision, SourceRevision};
use artifree_ports::artifact_store::{
    ArtifactStore, CleanupRequest, CommitReceipt, CopySource, Delivery, NewArtifact, PurgeRequest,
    RequestFingerprint, RequestIdentity, StoreError,
};
use artifree_ports::data_store::{DataReceipt, DataStore, DataStoreError, DataWrite};
use artifree_ports::job_store::{JobStore, NewJob};
use artifree_providers::storage::SqliteStore;
use common::{
    TempDir, TestResult, actor, create_demo, database_in, digest, key, package, parsed, revise,
    update_metadata,
};
use rusqlite::Connection;

const FIRST: &str = "<h1>first</h1>";
const SECOND: &str = "<h1>second</h1>";

fn at(ms: i64) -> UnixMillis {
    UnixMillis(ms)
}

fn value(text: &str) -> DataValue {
    DataValue::parse(text).unwrap_or_else(|error| panic!("{text}: {error}"))
}

fn path(text: &str) -> DataPath {
    parsed(text)
}

fn set(at_path: &str, text: &str) -> DataChange {
    DataChange::Set {
        path: path(at_path),
        value: value(text),
    }
}

fn merge(at_path: &str, text: &str) -> DataChange {
    DataChange::Merge {
        path: path(at_path),
        patch: value(text),
    }
}

fn remove(at_path: &str) -> DataChange {
    DataChange::Remove {
        path: path(at_path),
    }
}

struct Fixture {
    dir: TempDir,
    store: SqliteStore,
    owner: ActorId,
    created: CommitReceipt,
}

impl Fixture {
    /// An artifact with two source revisions.
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let dir = TempDir::new();
        let store = SqliteStore::open(&database_in(dir.path()))?;
        let owner = actor("owner");
        let created = create_demo(&store, &owner, &key("create"), "Secret", FIRST)?;
        revise(&store, &created, &key("save"), 1, SECOND)?;
        Ok(Self {
            dir,
            store,
            owner,
            created,
        })
    }

    fn id(&self) -> &ArtifactId {
        &self.created.artifact.id
    }

    fn sql(&self) -> Result<Connection, rusqlite::Error> {
        Connection::open(database_in(self.dir.path()))
    }

    /// A write as the core would make it: the fingerprint names the change.
    fn write_as(
        &self,
        who: &ActorId,
        name: &str,
        base: u64,
        change: &DataChange,
    ) -> Result<DataReceipt, DataStoreError> {
        Self::write_to(&self.store, self.id(), who, name, base, change)
    }

    fn write_to(
        store: &SqliteStore,
        artifact: &ArtifactId,
        who: &ActorId,
        name: &str,
        base: u64,
        change: &DataChange,
    ) -> Result<DataReceipt, DataStoreError> {
        let request_key = key(name);
        store.write_data(DataWrite {
            fence: None,
            request: RequestIdentity {
                actor: who,
                key: &request_key,
                fingerprint: RequestFingerprint(digest(&format!("{artifact} {base} {change:?}"))),
            },
            artifact,
            base: AppDataVersion(base),
            change,
            now: at(1_000),
        })
    }

    fn write(
        &self,
        name: &str,
        base: u64,
        change: &DataChange,
    ) -> Result<DataReceipt, DataStoreError> {
        self.write_as(&self.owner, name, base, change)
    }

    fn read_text(&self) -> Result<(u64, String), DataStoreError> {
        let snapshot = self.store.read_data(self.id(), &self.owner)?;
        Ok((snapshot.version.0, snapshot.space.to_canonical()))
    }

    fn delete_artifact(&self, name: &str, base: u64) -> Result<(), StoreError> {
        update_metadata(
            &self.store,
            &self.created,
            &key(name),
            base,
            None,
            None,
            Some(true),
        )
        .map(|_| ())
    }

    fn recover_artifact(&self, name: &str, base: u64) -> Result<(), StoreError> {
        update_metadata(
            &self.store,
            &self.created,
            &key(name),
            base,
            None,
            None,
            Some(false),
        )
        .map(|_| ())
    }

    fn purge(&self, name: &str, base: u64) -> Result<(), StoreError> {
        self.store
            .purge_artifact(PurgeRequest {
                request: RequestIdentity {
                    actor: &self.owner,
                    key: &key(name),
                    fingerprint: RequestFingerprint(digest("purge")),
                },
                artifact: self.id(),
                base: MetadataRevision(base),
            })
            .map(|_| ())
    }
}

// --- versions and writes -----------------------------------------------------

#[test]
fn a_space_nobody_wrote_is_empty_at_version_zero() -> TestResult {
    let fixture = Fixture::new()?;
    assert_eq!(fixture.read_text()?, (0, "{}".to_owned()));
    let count: i64 = fixture
        .sql()?
        .query_row("SELECT count(*) FROM app_data", [], |row| row.get(0))?;
    assert_eq!(count, 0, "reading writes nothing");
    Ok(())
}

#[test]
fn each_change_raises_the_version_by_one_and_stores_the_canonical_space() -> TestResult {
    let fixture = Fixture::new()?;
    let first = fixture.write(
        "one",
        0,
        &set("todos/first", "{\"done\":false,\"title\":\"a\"}"),
    )?;
    assert_eq!(
        (first.version, first.changed, first.delivery),
        (AppDataVersion(1), true, Delivery::Committed)
    );
    let second = fixture.write(
        "two",
        1,
        &merge("todos/first", "{\"done\":true,\"title\":null}"),
    )?;
    assert_eq!(second.version, AppDataVersion(2));
    assert_eq!(
        fixture.read_text()?,
        (2, "{\"todos\":{\"first\":{\"done\":true}}}".to_owned())
    );
    let third = fixture.write("three", 2, &remove("todos"))?;
    assert_eq!(third.version, AppDataVersion(3));
    assert_eq!(fixture.read_text()?, (3, "{}".to_owned()));
    // Emptying the space does not reset the version: it only goes up.
    let fourth = fixture.write("four", 3, &set("a", "1"))?;
    assert_eq!(fourth.version, AppDataVersion(4));
    Ok(())
}

/// The data version is its own counter: source revisions, metadata, and a
/// restore (a commit of an earlier revision's files) neither move it nor are
/// moved by it.
#[test]
fn the_data_version_is_independent_of_source_and_metadata_revisions() -> TestResult {
    let fixture = Fixture::new()?;
    let before = fixture.store.artifact(fixture.id())?.ok_or("artifact")?;
    fixture.write("one", 0, &set("n", "1"))?;
    fixture.write("two", 1, &set("n", "2"))?;
    let after = fixture.store.artifact(fixture.id())?.ok_or("artifact")?;
    assert_eq!(after.current_revision, before.current_revision);
    assert_eq!(after.metadata_revision, before.metadata_revision);
    assert_eq!(after.updated_at, before.updated_at);

    // A new source revision, then a restore of revision 1's files as revision 4.
    revise(
        &fixture.store,
        &fixture.created,
        &key("save-2"),
        2,
        "<h1>third</h1>",
    )?;
    revise(&fixture.store, &fixture.created, &key("restore"), 3, FIRST)?;
    update_metadata(
        &fixture.store,
        &fixture.created,
        &key("rename"),
        1,
        Some("Renamed"),
        None,
        None,
    )?;
    assert_eq!(fixture.read_text()?, (2, "{\"n\":2}".to_owned()));
    let revised = fixture.store.artifact(fixture.id())?.ok_or("artifact")?;
    assert_eq!(revised.current_revision, SourceRevision(4));
    let next = fixture.write("three", 2, &set("n", "3"))?;
    assert_eq!(next.version, AppDataVersion(3));
    Ok(())
}

#[test]
fn a_stale_or_future_base_is_a_conflict_that_names_the_current_version() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.write("one", 0, &set("n", "1"))?;
    fixture.write("two", 1, &set("n", "2"))?;
    for base in [0, 1, 3, 99] {
        assert_eq!(
            fixture.write("late", base, &set("n", "9")).err(),
            Some(DataStoreError::VersionConflict {
                current: AppDataVersion(2)
            }),
            "base {base}"
        );
    }
    assert_eq!(fixture.read_text()?, (2, "{\"n\":2}".to_owned()));
    // A refusal records no key: the same key works with the right base.
    let retried = fixture.write("late", 2, &set("n", "3"))?;
    assert_eq!(retried.delivery, Delivery::Committed);
    // A no-op is checked against the base too.
    assert_eq!(
        fixture.write("no-op", 1, &remove("nothing")).err(),
        Some(DataStoreError::VersionConflict {
            current: AppDataVersion(3)
        })
    );
    Ok(())
}

#[test]
fn a_change_that_alters_nothing_keeps_the_version_and_records_its_key() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.write("one", 0, &set("n", "1"))?;
    for (name, change) in [
        ("same-set", set("n", "1.0")),
        ("same-merge", merge("", "{\"n\":1,\"gone\":null}")),
        ("missing", remove("missing/deeper")),
    ] {
        let receipt = fixture.write(name, 1, &change)?;
        assert_eq!(
            (receipt.version, receipt.changed),
            (AppDataVersion(1), false),
            "{name}"
        );
    }
    // The key is spent: the same request replays, another one is refused.
    let replay = fixture.write("same-set", 1, &set("n", "1.0"))?;
    assert_eq!(replay.delivery, Delivery::Replayed);
    assert_eq!(
        fixture.write("same-set", 1, &set("n", "2")).err(),
        Some(DataStoreError::RequestKeyReused)
    );
    assert_eq!(fixture.read_text()?, (1, "{\"n\":1}".to_owned()));
    // Removing a member writes a version; removing it again does not.
    assert_eq!(
        fixture.write("rm", 1, &remove("n"))?.version,
        AppDataVersion(2)
    );
    assert!(!fixture.write("rm-again", 2, &remove("n"))?.changed);
    Ok(())
}

#[test]
fn an_empty_space_that_stays_empty_writes_no_row() -> TestResult {
    let fixture = Fixture::new()?;
    let receipt = fixture.write("clear", 0, &remove(""))?;
    assert_eq!(
        (receipt.version, receipt.changed),
        (AppDataVersion(0), false)
    );
    let rows: i64 = fixture
        .sql()?
        .query_row("SELECT count(*) FROM app_data", [], |row| row.get(0))?;
    assert_eq!(rows, 0);
    Ok(())
}

#[test]
fn a_refused_change_writes_nothing_and_leaves_the_key_free() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.write("one", 0, &set("a", "\"text\""))?;
    assert_eq!(
        fixture.write("through", 1, &set("a/b", "1")).err(),
        Some(DataStoreError::Rejected(DataError::PathConflict))
    );
    assert_eq!(
        fixture.write("root", 1, &set("", "[1]")).err(),
        Some(DataStoreError::Rejected(DataError::RootNotObject))
    );
    let big = format!("\"{}\"", "x".repeat(250_000));
    for (index, name) in ["b", "c", "d", "e"].iter().enumerate() {
        let base = 1 + u64::try_from(index)?;
        fixture.write(&format!("fill-{name}"), base, &set(name, &big))?;
    }
    assert_eq!(
        fixture.write("overflow", 5, &set("f", &big)).err(),
        Some(DataStoreError::Rejected(DataError::TooLarge(
            DataLimit::SpaceBytes
        ))),
    );
    assert_eq!(fixture.read_text()?.0, 5);
    // None of the refusals spent its key.
    let count: i64 = fixture
        .sql()?
        .query_row("SELECT count(*) FROM data_requests", [], |row| row.get(0))?;
    assert_eq!(count, 5);
    assert_eq!(
        fixture.write("through", 5, &set("z", "1"))?.delivery,
        Delivery::Committed
    );
    Ok(())
}

// --- retries -----------------------------------------------------------------

/// The answer to a request is the version it produced, also after the space
/// moved on, and without writing again.
#[test]
fn a_replay_after_a_lost_response_returns_the_original_version() -> TestResult {
    let fixture = Fixture::new()?;
    let original = fixture.write("lost", 0, &set("n", "1"))?;
    fixture.write("next", 1, &set("n", "2"))?;
    fixture.write("later", 2, &set("n", "3"))?;
    let replay = fixture.write("lost", 0, &set("n", "1"))?;
    assert_eq!(
        replay,
        DataReceipt {
            delivery: Delivery::Replayed,
            ..original
        }
    );
    assert_eq!(fixture.read_text()?, (3, "{\"n\":3}".to_owned()));
    // A replay of a no-op reports the version it saw, not the later one.
    let noop = fixture.write("noop", 3, &set("n", "3"))?;
    fixture.write("move", 3, &set("n", "4"))?;
    let replay = fixture.write("noop", 3, &set("n", "3"))?;
    assert_eq!((replay.version, replay.changed), (noop.version, false));
    assert_eq!(replay.delivery, Delivery::Replayed);
    Ok(())
}

#[test]
fn the_same_key_for_another_request_is_refused_and_writes_nothing() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.write("key", 0, &set("n", "1"))?;
    for change in [set("n", "2"), set("m", "1"), merge("n", "2"), remove("n")] {
        assert_eq!(
            fixture.write("key", 0, &change).err(),
            Some(DataStoreError::RequestKeyReused),
            "{change:?}"
        );
    }
    // The same text for another base is another request, so another actor's
    // key space is the only place it may be reused.
    assert_eq!(
        fixture.write("key", 1, &set("n", "1")).err(),
        Some(DataStoreError::RequestKeyReused)
    );
    let other = actor("other");
    assert_eq!(
        fixture.write_as(&other, "key", 0, &set("n", "1"))?.delivery,
        Delivery::Committed,
        "keys are per actor"
    );
    assert_eq!(fixture.read_text()?, (1, "{\"n\":1}".to_owned()));
    Ok(())
}

/// One key space per actor over artifact requests, cleanups, jobs, and data
/// writes, checked in both directions in the transaction of the write.
#[test]
fn the_key_space_is_shared_with_every_other_kind_of_request_in_both_directions() -> TestResult {
    let fixture = Fixture::new()?;
    let cleanup = |name: &str| {
        let base = fixture.store.unreferenced_content()?.digest;
        fixture.store.remove_unreferenced_content(CleanupRequest {
            request: RequestIdentity {
                actor: &fixture.owner,
                key: &key(name),
                fingerprint: RequestFingerprint(digest(&format!("cleanup {base}"))),
            },
            base,
        })
    };
    let enqueue = |name: &str| {
        fixture.store.enqueue_job(NewJob {
            request: RequestIdentity {
                actor: &fixture.owner,
                key: &key(name),
                fingerprint: RequestFingerprint(digest("enqueue")),
            },
            artifact: fixture.id(),
            revision: SourceRevision(1),
            kind: JobKind::VerifyRevision,
            now: at(1),
        })
    };

    // Used first by another kind of request: a data write is refused.
    // `create` and `save` are the artifact requests of the fixture.
    cleanup("swept")?;
    enqueue("queued")?;
    for used in ["create", "save", "swept", "queued"] {
        assert_eq!(
            fixture.write(used, 0, &set("n", "1")).err(),
            Some(DataStoreError::RequestKeyReused),
            "a data write under the key of {used}"
        );
    }

    // Used first by a data write: every other kind is refused.
    fixture.write("written", 0, &set("n", "1"))?;
    assert_eq!(
        create_demo(
            &fixture.store,
            &fixture.owner,
            &key("written"),
            "Other",
            "<p>x</p>"
        )
        .err(),
        Some(StoreError::RequestKeyReused),
        "an artifact create"
    );
    assert_eq!(
        revise(
            &fixture.store,
            &fixture.created,
            &key("written"),
            2,
            "<p>y</p>"
        )
        .err(),
        Some(StoreError::RequestKeyReused),
        "a save"
    );
    assert_eq!(
        update_metadata(
            &fixture.store,
            &fixture.created,
            &key("written"),
            1,
            Some("Renamed"),
            None,
            None
        )
        .err(),
        Some(StoreError::RequestKeyReused),
        "a metadata update"
    );
    assert_eq!(
        cleanup("written").err(),
        Some(StoreError::RequestKeyReused),
        "a cleanup"
    );
    assert!(
        matches!(
            enqueue("written"),
            Err(artifree_ports::job_store::JobStoreError::RequestKeyReused)
        ),
        "a job request"
    );
    // The same text is free for a different actor.
    create_demo(
        &fixture.store,
        &actor("other"),
        &key("written"),
        "Other",
        "<p>x</p>",
    )?;
    Ok(())
}

// --- access ------------------------------------------------------------------

#[test]
fn spaces_are_separate_per_actor() -> TestResult {
    let fixture = Fixture::new()?;
    let other = actor("other");
    fixture.write("mine", 0, &set("n", "1"))?;
    let theirs = fixture.write_as(&other, "theirs", 0, &set("n", "7"))?;
    assert_eq!(
        theirs.version,
        AppDataVersion(1),
        "their own version history"
    );
    let snapshot = fixture.store.read_data(fixture.id(), &other)?;
    assert_eq!(snapshot.space.to_canonical(), "{\"n\":7}");
    assert_eq!(fixture.read_text()?, (1, "{\"n\":1}".to_owned()));
    Ok(())
}

#[test]
fn an_unknown_artifact_is_not_found_for_reads_and_writes() -> TestResult {
    let fixture = Fixture::new()?;
    let unknown: ArtifactId = parsed(&format!("art_{}", "0".repeat(32)));
    assert_eq!(
        fixture.store.read_data(&unknown, &fixture.owner).err(),
        Some(DataStoreError::NotFound)
    );
    assert_eq!(
        Fixture::write_to(
            &fixture.store,
            &unknown,
            &fixture.owner,
            "n",
            0,
            &set("n", "1")
        )
        .err(),
        Some(DataStoreError::NotFound)
    );
    Ok(())
}

#[test]
fn the_trash_hides_data_and_a_recovery_brings_it_back_unchanged() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.write("one", 0, &set("secret", "\"kept\""))?;
    fixture.delete_artifact("delete", 1)?;

    assert_eq!(
        fixture.store.read_data(fixture.id(), &fixture.owner).err(),
        Some(DataStoreError::NotFound)
    );
    assert_eq!(
        fixture.write("two", 1, &set("n", "1")).err(),
        Some(DataStoreError::NotFound)
    );
    // A retry of a request made before the delete is not found either: data
    // is as unreadable as the artifact (decision 0031).
    assert_eq!(
        fixture.write("one", 0, &set("secret", "\"kept\"")).err(),
        Some(DataStoreError::NotFound)
    );
    // The key of the refused write was not spent.
    fixture.recover_artifact("recover", 2)?;
    assert_eq!(
        fixture.read_text()?,
        (1, "{\"secret\":\"kept\"}".to_owned()),
        "data and version survive the trash"
    );
    assert_eq!(
        fixture.write("two", 1, &set("n", "1"))?.version,
        AppDataVersion(2)
    );
    assert_eq!(
        fixture
            .write("one", 0, &set("secret", "\"kept\""))?
            .delivery,
        Delivery::Replayed,
        "after the recovery the retry replays"
    );
    Ok(())
}

// --- purge -------------------------------------------------------------------

#[test]
fn a_purge_deletes_the_space_and_blanks_the_fingerprints_of_its_keys() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.write("one", 0, &set("n", "1"))?;
    fixture.write("two", 1, &set("n", "2"))?;
    // Another artifact's data is untouched.
    let other = create_demo(
        &fixture.store,
        &fixture.owner,
        &key("other"),
        "Other",
        FIRST,
    )?;
    Fixture::write_to(
        &fixture.store,
        &other.artifact.id,
        &fixture.owner,
        "other-write",
        0,
        &set("keep", "true"),
    )?;
    fixture.delete_artifact("delete", 1)?;
    fixture.purge("purge", 2)?;

    let sql = fixture.sql()?;
    let rows: i64 = sql.query_row(
        "SELECT count(*) FROM app_data WHERE artifact_id = ?1",
        [fixture.id().as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(rows, 0, "the space is gone");
    let blanked: i64 = sql.query_row(
        "SELECT count(*) FROM data_requests
         WHERE artifact_id = ?1 AND fingerprint = zeroblob(32)",
        [fixture.id().as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(blanked, 2, "no fingerprint of a written value remains");
    let untouched: i64 = sql.query_row(
        "SELECT count(*) FROM app_data WHERE artifact_id = ?1",
        [other.artifact.id.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(untouched, 1);
    let kept: i64 = sql.query_row(
        "SELECT count(*) FROM data_requests WHERE artifact_id = ?1 AND fingerprint <> zeroblob(32)",
        [other.artifact.id.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(kept, 1);

    assert_eq!(
        fixture.store.read_data(fixture.id(), &fixture.owner).err(),
        Some(DataStoreError::NotFound)
    );
    for name in ["one", "two", "fresh"] {
        assert_eq!(
            fixture.write(name, 0, &set("n", "1")).err(),
            Some(DataStoreError::NotFound),
            "{name}"
        );
    }
    // The keys stay reserved: another kind of request cannot take them.
    assert_eq!(
        create_demo(
            &fixture.store,
            &fixture.owner,
            &key("one"),
            "Again",
            "<p>x</p>"
        )
        .err(),
        Some(StoreError::RequestKeyReused)
    );
    Ok(())
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn file_bytes(fixture: &Fixture) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    fixture
        .sql()?
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
    let mut bytes = std::fs::read(database_in(fixture.dir.path()))?;
    let log = fixture.dir.join("artifree.sqlite3-wal");
    if log.exists() {
        bytes.extend(std::fs::read(log)?);
    }
    Ok(bytes)
}

/// What a write replaces or removes, and what a purge deletes, is overwritten
/// in the file, not only marked free. Earlier backups and a log that was not
/// truncated yet can still hold it (decision 0031).
#[test]
fn replaced_removed_and_purged_data_is_erased_from_the_database_file() -> TestResult {
    let fixture = Fixture::new()?;
    let first = "data-marker-first-4d1e";
    let second = "data-marker-second-9b7a";
    let third = "data-marker-third-c0f2";
    let text = |marker: &str| format!("\"{}\"", marker.repeat(2_000));
    fixture.write("one", 0, &set("a", &text(first)))?;
    assert!(
        contains(&file_bytes(&fixture)?, first.as_bytes()),
        "the premise"
    );

    // Replaced by a write.
    fixture.write("two", 1, &set("a", &text(second)))?;
    let bytes = file_bytes(&fixture)?;
    assert!(contains(&bytes, second.as_bytes()), "the premise");
    assert!(
        !contains(&bytes, first.as_bytes()),
        "a replaced value is still in the database file"
    );

    // Removed by a write.
    fixture.write("three", 2, &remove("a"))?;
    assert!(
        !contains(&file_bytes(&fixture)?, second.as_bytes()),
        "a removed value is still in the database file"
    );

    // Purged with the artifact.
    fixture.write("four", 3, &set("b", &text(third)))?;
    assert!(
        contains(&file_bytes(&fixture)?, third.as_bytes()),
        "the premise"
    );
    fixture.delete_artifact("delete", 1)?;
    fixture.purge("purge", 2)?;
    assert!(
        !contains(&file_bytes(&fixture)?, third.as_bytes()),
        "purged data is still in the database file"
    );
    Ok(())
}

// --- concurrency ------------------------------------------------------------

/// Two connections prepared against the same version: exactly one writes, the
/// other is told the version the winner left. The check and the write are one
/// transaction, so the outcome does not depend on the interleaving.
#[test]
fn two_writers_with_the_same_base_cannot_both_win() -> TestResult {
    for round in 0..10 {
        let fixture = Fixture::new()?;
        let barrier = Arc::new(Barrier::new(2));
        let outcomes: Vec<Result<DataReceipt, DataStoreError>> = thread::scope(|scope| {
            let handles: Vec<_> = ["left", "right"]
                .into_iter()
                .map(|side| {
                    let barrier = Arc::clone(&barrier);
                    let fixture = &fixture;
                    scope.spawn(move || {
                        let store = SqliteStore::open(&database_in(fixture.dir.path()))?;
                        let change = set("winner", &format!("\"{side}\""));
                        barrier.wait();
                        Ok::<_, StoreError>(Fixture::write_to(
                            &store,
                            fixture.id(),
                            &fixture.owner,
                            &format!("race-{side}-{round}"),
                            0,
                            &change,
                        ))
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .unwrap_or_else(|_| Ok(Err(DataStoreError::Unavailable("panic".into()))))
                        .unwrap_or_else(|error| Err(error.into()))
                })
                .collect()
        });
        let wins = outcomes.iter().filter(|outcome| outcome.is_ok()).count();
        assert_eq!(wins, 1, "round {round}: {outcomes:?}");
        let loser = outcomes
            .iter()
            .find_map(|outcome| outcome.as_ref().err())
            .ok_or("a loser")?;
        assert_eq!(
            *loser,
            DataStoreError::VersionConflict {
                current: AppDataVersion(1)
            }
        );
        assert_eq!(fixture.read_text()?.0, 1);
    }
    Ok(())
}

// --- copies -------------------------------------------------------------------

fn copy_of(fixture: &Fixture, name: &str, copy_data: bool) -> Result<CommitReceipt, StoreError> {
    fixture.store.create_artifact(NewArtifact {
        request: RequestIdentity {
            actor: &fixture.owner,
            key: &key(name),
            fingerprint: RequestFingerprint(digest(&format!("copy {name}"))),
        },
        kind: fixture.created.artifact.kind,
        title: &parsed("Copy"),
        package: &package(&[("index.html", SECOND), ("style.css", "h1{}")]),
        source: Some(CopySource {
            artifact: fixture.id(),
            copy_data,
        }),
    })
}

#[test]
fn a_copy_with_data_gets_its_own_space_and_neither_side_reaches_the_other() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.write("one", 0, &set("list", "[1,2]"))?;
    fixture.write("two", 1, &set("n", "5"))?;
    fixture.write("three", 2, &set("n", "6"))?;

    let copy = copy_of(&fixture, "copy-with-data", true)?;
    let copy_id = copy.artifact.id.clone();
    let copy_space = fixture.store.read_data(&copy_id, &fixture.owner)?;
    assert_eq!(copy_space.space.to_canonical(), "{\"list\":[1,2],\"n\":6}");
    assert_eq!(
        copy_space.version,
        AppDataVersion(1),
        "the copy has a history of its own, not the source's version"
    );

    // Later writes to the source do not reach the copy.
    fixture.write("four", 3, &set("n", "7"))?;
    assert_eq!(
        fixture
            .store
            .read_data(&copy_id, &fixture.owner)?
            .space
            .to_canonical(),
        "{\"list\":[1,2],\"n\":6}"
    );
    // Writes to the copy do not reach the source, and have their own keys'
    // versions.
    let in_copy = Fixture::write_to(
        &fixture.store,
        &copy_id,
        &fixture.owner,
        "copy-write",
        1,
        &set("only", "\"copy\""),
    )?;
    assert_eq!(in_copy.version, AppDataVersion(2));
    assert_eq!(
        fixture.read_text()?,
        (4, "{\"list\":[1,2],\"n\":7}".to_owned())
    );
    // A later removal of everything in the source leaves the copy alone.
    fixture.write("five", 4, &remove(""))?;
    assert_eq!(
        fixture
            .store
            .read_data(&copy_id, &fixture.owner)?
            .space
            .to_canonical(),
        "{\"list\":[1,2],\"n\":6,\"only\":\"copy\"}"
    );
    Ok(())
}

#[test]
fn a_copy_without_data_or_from_an_empty_space_has_an_unwritten_space() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.write("one", 0, &set("n", "1"))?;
    let plain = copy_of(&fixture, "plain-copy", false)?;
    let snapshot = fixture
        .store
        .read_data(&plain.artifact.id, &fixture.owner)?;
    assert_eq!((snapshot.version.0, snapshot.space.is_empty()), (0, true));

    fixture.write("clear", 1, &remove(""))?;
    let from_empty = copy_of(&fixture, "empty-copy", true)?;
    let snapshot = fixture
        .store
        .read_data(&from_empty.artifact.id, &fixture.owner)?;
    assert_eq!((snapshot.version.0, snapshot.space.is_empty()), (0, true));
    let rows: i64 = fixture.sql()?.query_row(
        "SELECT count(*) FROM app_data WHERE artifact_id = ?1",
        [from_empty.artifact.id.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(rows, 0);
    Ok(())
}

#[test]
fn a_copy_takes_only_the_data_of_the_actor_who_copies() -> TestResult {
    let fixture = Fixture::new()?;
    let other = actor("other");
    fixture.write_as(&other, "theirs", 0, &set("n", "\"theirs\""))?;
    let copy = copy_of(&fixture, "mine-only", true)?;
    let snapshot = fixture.store.read_data(&copy.artifact.id, &other)?;
    assert!(snapshot.space.is_empty(), "no one else's data comes along");
    Ok(())
}

#[test]
fn a_replayed_copy_does_not_copy_again() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.write("one", 0, &set("n", "1"))?;
    let first = copy_of(&fixture, "copy-twice", true)?;
    fixture.write("two", 1, &set("n", "2"))?;
    let replay = copy_of(&fixture, "copy-twice", true)?;
    assert_eq!(replay.delivery, Delivery::Replayed);
    assert_eq!(replay.artifact.id, first.artifact.id);
    assert_eq!(
        fixture
            .store
            .read_data(&first.artifact.id, &fixture.owner)?
            .space
            .to_canonical(),
        "{\"n\":1}",
        "the copy keeps the data it was made with"
    );
    Ok(())
}

// --- rows --------------------------------------------------------------------

#[test]
fn the_database_refuses_rows_the_store_cannot_produce() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.write("one", 0, &set("n", "1"))?;
    let sql = fixture.sql()?;
    sql.pragma_update(None, "foreign_keys", true)?;
    let id = fixture.id().as_str();
    for (what, statement) in [
        (
            "a version of zero",
            "UPDATE app_data SET data_version = 0 WHERE artifact_id = ?1",
        ),
        (
            "a document that is not an object",
            "UPDATE app_data SET document = '[1]' WHERE artifact_id = ?1",
        ),
        (
            "a document that is not JSON",
            "UPDATE app_data SET document = '{' WHERE artifact_id = ?1",
        ),
        (
            "a space of an artifact that does not exist",
            "UPDATE app_data SET artifact_id = 'art_none' WHERE artifact_id = ?1",
        ),
        (
            "a key with a short fingerprint",
            "UPDATE data_requests SET fingerprint = x'00' WHERE artifact_id = ?1",
        ),
        (
            "a key that changed nothing and everything",
            "UPDATE data_requests SET changed = 2 WHERE artifact_id = ?1",
        ),
    ] {
        assert!(sql.execute(statement, [id]).is_err(), "accepted {what}");
    }
    Ok(())
}

/// A real `SQLite` write carrying an explicit authentication generation.
fn outbox_write(
    store: &SqliteStore,
    actor: &ActorId,
    artifact: &ArtifactId,
    fence: &artifree_ports::data_store::OutboxFence,
) -> Result<DataReceipt, DataStoreError> {
    store.write_data(DataWrite {
        request: RequestIdentity {
            actor,
            key: &key("outbox-fenced"),
            fingerprint: RequestFingerprint(digest("outbox-fenced")),
        },
        artifact,
        base: AppDataVersion(0),
        change: &set("item", "1"),
        now: at(1000),
        fence: Some(fence),
    })
}

#[test]
fn outbox_transaction_rechecks_authentication_generation_before_replay() -> TestResult {
    use artifree_domain::identity::StateGeneration;
    use artifree_ports::data_store::OutboxFence;
    use artifree_ports::user_store::{NewUser, UserStore};
    use artifree_providers::tokens::SqliteTokenStore;

    let dir = TempDir::new();
    let database = database_in(dir.path());
    let store = SqliteStore::open(&database)?;
    let users = SqliteTokenStore::open(&database)?;
    let actor = ActorId::user([9; 16]);
    users.register(&NewUser {
        id: actor.clone(),
        label: parsed("Example"),
        created_at: "2026-11-06T00:00:00Z".to_owned(),
    })?;
    let created = create_demo(&store, &actor, &key("outbox-create"), "Example", FIRST)?;
    let binding = store.outbox_binding(&actor, StateGeneration::FIRST)?;
    let original = OutboxFence {
        expected: binding.clone(),
        authenticated_generation: StateGeneration::FIRST,
    };
    assert_eq!(
        outbox_write(&store, &actor, &created.artifact.id, &original)?.delivery,
        Delivery::Committed
    );
    users.set_disabled(&actor, Some("2026-11-06T01:00:00Z"))?;
    assert_eq!(
        outbox_write(&store, &actor, &created.artifact.id, &original),
        Err(DataStoreError::OutboxBindingChanged)
    );
    assert_eq!(
        store.outbox_binding(&actor, StateGeneration::FIRST),
        Err(DataStoreError::OutboxBindingChanged)
    );
    let enabled = users.set_disabled(&actor, None)?;
    assert_eq!(enabled.state_generation.get(), 3);
    assert_eq!(
        store.outbox_binding(&actor, StateGeneration::FIRST),
        Err(DataStoreError::OutboxBindingChanged)
    );
    let current = store.outbox_binding(&actor, enabled.state_generation)?;
    // Even a fresh expected tuple cannot authorize an authentication snapshot
    // captured before disable/re-enable. Both are checked within the write.
    let stale_auth = OutboxFence {
        expected: current.clone(),
        authenticated_generation: StateGeneration::FIRST,
    };
    assert_eq!(
        outbox_write(&store, &actor, &created.artifact.id, &stale_auth),
        Err(DataStoreError::OutboxBindingChanged)
    );
    assert_eq!(
        outbox_write(&store, &actor, &created.artifact.id, &original),
        Err(DataStoreError::OutboxBindingChanged)
    );
    let admitted = OutboxFence {
        expected: current,
        authenticated_generation: enabled.state_generation,
    };
    assert_eq!(
        outbox_write(&store, &actor, &created.artifact.id, &admitted)?.delivery,
        Delivery::Replayed
    );
    Ok(())
}
