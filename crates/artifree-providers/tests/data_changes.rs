//! Data changes in the change feed of the `SQLite` store (decision 0040): which
//! writes record a `data_changed` row, in whose feed, with what positions, in
//! which order inside the write's transaction, and which constraints guard the
//! table.

mod common;

use artifree_domain::change::ChangeSequence;
use artifree_domain::data::{DataChange, DataError, DataLimit, DataPath, DataValue};
use artifree_domain::identity::{ActorId, ArtifactId, UnixMillis};
use artifree_domain::{AppDataVersion, MetadataRevision};
use artifree_ports::artifact_store::{
    ArtifactStore, ChangeKind, ChangeRecord, CommitReceipt, CopySource, Delivery, NewArtifact,
    PurgeRequest, RequestFingerprint, RequestIdentity, StoreError,
};
use artifree_ports::data_store::{DataReceipt, DataStore, DataStoreError, DataWrite};
use artifree_providers::storage::SqliteStore;
use common::{
    TempDir, TestResult, actor, create_demo, database_in, digest, key, package, parsed, revise,
    update_metadata,
};
use rusqlite::Connection;

fn set(at_path: &str, text: &str) -> Result<DataChange, DataError> {
    Ok(DataChange::Set {
        path: parsed::<DataPath>(at_path),
        value: DataValue::parse(text)?,
    })
}

struct Fixture {
    dir: TempDir,
    store: SqliteStore,
    owner: ActorId,
    created: CommitReceipt,
}

impl Fixture {
    /// An artifact of `owner` at source revision 2 and metadata revision 1.
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let dir = TempDir::new();
        let store = SqliteStore::open(&database_in(dir.path()))?;
        let owner = actor("owner");
        let created = create_demo(&store, &owner, &key("create"), "Shared", "<h1>1</h1>")?;
        revise(&store, &created, &key("save"), 1, "<h1>2</h1>")?;
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

    /// A write to the space of `who`, as the core makes it: the fingerprint
    /// names the artifact, the base, and the change.
    fn write_as(
        &self,
        who: &ActorId,
        name: &str,
        base: u64,
        change: &DataChange,
    ) -> Result<DataReceipt, DataStoreError> {
        let request_key = key(name);
        self.store.write_data(DataWrite {
            request: RequestIdentity {
                actor: who,
                key: &request_key,
                fingerprint: RequestFingerprint(digest(&format!(
                    "{} {base} {change:?}",
                    self.id()
                ))),
            },
            artifact: self.id(),
            base: AppDataVersion(base),
            change,
            now: UnixMillis(1_000),
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

    /// Every change of `who`'s feed, in order.
    fn feed(&self, who: &ActorId) -> Result<Vec<ChangeRecord>, StoreError> {
        Ok(self.store.changes(who, ChangeSequence(0), 1_000)?.changes)
    }

    fn head(&self, who: &ActorId) -> Result<ChangeSequence, StoreError> {
        Ok(self.store.changes(who, ChangeSequence(0), 1)?.head)
    }

    /// The data changes of `who`'s feed as (data version, revision, metadata
    /// revision).
    fn data_changes(&self, who: &ActorId) -> Result<Vec<(u64, u64, u64)>, StoreError> {
        Ok(self
            .feed(who)?
            .iter()
            .filter(|change| change.kind == ChangeKind::DataChanged)
            .map(|change| {
                (
                    change.data_version.map_or(0, |version| version.0),
                    change.revision.0,
                    change.metadata_revision.0,
                )
            })
            .collect())
    }
}

/// A set, an update, and a delete each append one row with the version they
/// left the space at and the artifact's positions at that moment, which are
/// never the data version.
#[test]
fn each_changing_write_appends_one_row_with_the_new_version_and_the_current_positions() -> TestResult
{
    let fixture = Fixture::new()?;
    let owner = fixture.owner.clone();
    let before = fixture.head(&owner)?;
    assert_eq!(before, ChangeSequence(2), "a create and a revision");

    fixture.write("one", 0, &set("a", "1")?)?;
    update_metadata(
        &fixture.store,
        &fixture.created,
        &key("rename"),
        1,
        Some("Renamed"),
        None,
        None,
    )?;
    let merge = DataChange::Merge {
        path: parsed("b"),
        patch: DataValue::parse("{\"c\":2}")?,
    };
    fixture.write("two", 1, &merge)?;
    revise(
        &fixture.store,
        &fixture.created,
        &key("save-3"),
        2,
        "<h1>3</h1>",
    )?;
    let remove = DataChange::Remove { path: parsed("a") };
    fixture.write("three", 2, &remove)?;

    assert_eq!(
        fixture.data_changes(&owner)?,
        [(1, 2, 1), (2, 2, 2), (3, 3, 2)],
        "(data version, revision, metadata revision) of the set, the update, the delete"
    );
    // One row per write and nothing else besides the artifact's own commits:
    // create, save, [set], rename, [update], save, [delete].
    let kinds: Vec<_> = fixture.feed(&owner)?.iter().map(|c| c.kind).collect();
    assert_eq!(
        kinds,
        [
            ChangeKind::Created,
            ChangeKind::Revised,
            ChangeKind::DataChanged,
            ChangeKind::MetadataUpdated,
            ChangeKind::DataChanged,
            ChangeKind::Revised,
            ChangeKind::DataChanged,
        ]
    );
    // Only the data rows carry a data version.
    for change in fixture.feed(&owner)? {
        assert_eq!(
            change.data_version.is_some(),
            change.kind == ChangeKind::DataChanged,
            "{change:?}"
        );
    }
    Ok(())
}

/// A replay, a write that changes nothing, a stale base, a path conflict, an
/// oversized space, and a write to an artifact in the trash record nothing and
/// do not move the head.
#[test]
fn a_replay_a_no_op_and_every_refusal_record_nothing() -> TestResult {
    let fixture = Fixture::new()?;
    let owner = fixture.owner.clone();
    let first = fixture.write("one", 0, &set("a", "\"text\"")?)?;
    assert_eq!(first.delivery, Delivery::Committed);
    let head = fixture.head(&owner)?;
    assert_eq!(fixture.data_changes(&owner)?.len(), 1);

    // The same request again: a replay of the original result.
    let replay = fixture.write("one", 0, &set("a", "\"text\"")?)?;
    assert_eq!(replay.delivery, Delivery::Replayed);
    // A delete of a path nothing is at changes nothing; its key is recorded.
    let no_op = fixture.write(
        "nothing",
        1,
        &DataChange::Remove {
            path: parsed("zzz"),
        },
    )?;
    assert_eq!(
        (no_op.changed, no_op.delivery),
        (false, Delivery::Committed)
    );
    // Refusals.
    assert_eq!(
        fixture.write("stale", 0, &set("b", "1")?).err(),
        Some(DataStoreError::VersionConflict {
            current: AppDataVersion(1)
        })
    );
    assert_eq!(
        fixture.write("through", 1, &set("a/b", "1")?).err(),
        Some(DataStoreError::Rejected(DataError::PathConflict))
    );
    let big = format!("\"{}\"", "x".repeat(250_000));
    for (index, name) in ["c", "d", "e", "f"].iter().enumerate() {
        let base = 1 + u64::try_from(index)?;
        fixture.write(&format!("fill-{name}"), base, &set(name, &big)?)?;
    }
    let head_after_fill = fixture.head(&owner)?;
    assert_eq!(
        fixture.write("overflow", 5, &set("g", &big)?).err(),
        Some(DataStoreError::Rejected(DataError::TooLarge(
            DataLimit::SpaceBytes
        )))
    );
    update_metadata(
        &fixture.store,
        &fixture.created,
        &key("trash"),
        1,
        None,
        None,
        Some(true),
    )?;
    let head_in_trash = fixture.head(&owner)?;
    assert_eq!(
        fixture.write("in-trash", 5, &set("h", "1")?).err(),
        Some(DataStoreError::NotFound)
    );

    assert_eq!(
        head,
        ChangeSequence(2 + 1),
        "before the fill: only the first write"
    );
    assert_eq!(
        head_after_fill,
        ChangeSequence(3 + 4),
        "the replay, the no-op, and the refusals added nothing; the four fills did"
    );
    assert_eq!(
        head_in_trash.0,
        head_after_fill.0 + 1,
        "only the trashing; the refused overflow added nothing"
    );
    assert_eq!(
        fixture.head(&owner)?,
        head_in_trash,
        "the write to the trashed artifact added nothing"
    );
    assert_eq!(fixture.data_changes(&owner)?.len(), 5);
    Ok(())
}

/// A failure of the `changes` insert fails the whole write: the space, the
/// version, the key, and the feed stay as they were, and the key is free for a
/// retry. The same holds for every write operation.
#[test]
fn a_failing_change_insert_fails_the_whole_write_and_leaves_space_and_key() -> TestResult {
    let fixture = Fixture::new()?;
    let owner = fixture.owner.clone();
    fixture.sql()?.execute_batch(
        "CREATE TRIGGER fail BEFORE INSERT ON changes WHEN new.kind = 'data_changed'
         BEGIN SELECT RAISE(ABORT, 'injected'); END;",
    )?;

    let refused = fixture.write("one", 0, &set("a", "1")?).err();
    assert!(
        matches!(&refused, Some(DataStoreError::Unavailable(reason)) if reason.contains("injected")),
        "{refused:?}"
    );

    let snapshot = fixture.store.read_data(fixture.id(), &owner)?;
    assert_eq!(
        (snapshot.version, snapshot.space.is_empty()),
        (AppDataVersion(0), true),
        "the space was not written"
    );
    let connection = fixture.sql()?;
    for (table, expected) in [("data_requests", 0_i64), ("app_data", 0)] {
        let rows: i64 =
            connection.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
        assert_eq!(
            rows, expected,
            "{table}: the key and the row are rolled back"
        );
    }
    assert_eq!(
        fixture.head(&owner)?,
        ChangeSequence(2),
        "the feed head did not move"
    );

    // The key was not spent: the same request succeeds once the cause is gone.
    connection.execute_batch("DROP TRIGGER fail;")?;
    let retried = fixture.write("one", 0, &set("a", "1")?)?;
    assert_eq!(
        (retried.delivery, retried.version),
        (Delivery::Committed, AppDataVersion(1))
    );
    assert_eq!(fixture.data_changes(&owner)?, [(1, 2, 1)]);
    Ok(())
}

/// The row is appended after the data row and after the request key, in the
/// same transaction: a trigger that fires when either of them is written
/// while a row for the same version already exists aborts the write.
#[test]
fn the_row_is_recorded_after_the_data_row_and_the_key() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.sql()?.execute_batch(
        "CREATE TRIGGER data_first BEFORE INSERT ON app_data
             WHEN EXISTS (SELECT 1 FROM changes WHERE kind = 'data_changed'
                          AND artifact_id = new.artifact_id AND data_version = new.data_version)
         BEGIN SELECT RAISE(ABORT, 'recorded before the data row'); END;
         CREATE TRIGGER data_update_first BEFORE UPDATE ON app_data
             WHEN EXISTS (SELECT 1 FROM changes WHERE kind = 'data_changed'
                          AND artifact_id = new.artifact_id AND data_version = new.data_version)
         BEGIN SELECT RAISE(ABORT, 'recorded before the data row'); END;
         CREATE TRIGGER key_first BEFORE INSERT ON data_requests
             WHEN EXISTS (SELECT 1 FROM changes WHERE kind = 'data_changed'
                          AND artifact_id = new.artifact_id AND data_version = new.data_version
                          AND new.changed = 1)
         BEGIN SELECT RAISE(ABORT, 'recorded before the key'); END;",
    )?;

    // The first write inserts the data row, later ones update it.
    fixture.write("one", 0, &set("a", "1")?)?;
    fixture.write("two", 1, &set("a", "2")?)?;

    assert_eq!(fixture.data_changes(&fixture.owner)?.len(), 2);
    Ok(())
}

/// In the store, which does not check ownership, a write as another actor on
/// the owner's artifact goes to that actor's feed and leaves the owner's head.
#[test]
fn a_write_by_another_actor_goes_to_that_actors_feed_and_not_the_owners() -> TestResult {
    let fixture = Fixture::new()?;
    let owner = fixture.owner.clone();
    let other = actor("other");
    let owner_head = fixture.head(&owner)?;
    assert_eq!(fixture.head(&other)?, ChangeSequence(0));

    let receipt = fixture.write_as(&other, "theirs", 0, &set("a", "1")?)?;
    assert_eq!(receipt.version, AppDataVersion(1));

    assert_eq!(fixture.data_changes(&other)?, [(1, 2, 1)]);
    assert_eq!(fixture.head(&other)?, ChangeSequence(1));
    assert_eq!(
        fixture.head(&owner)?,
        owner_head,
        "the owner's head did not move"
    );
    assert!(
        fixture.data_changes(&owner)?.is_empty(),
        "the owner's feed has no row for the other actor's space"
    );
    // The owner's own space is still unwritten, and a write of the owner
    // follows the owner's numbering.
    fixture.write("mine", 0, &set("b", "1")?)?;
    assert_eq!(fixture.head(&owner)?, ChangeSequence(owner_head.0 + 1));
    assert_eq!(fixture.head(&other)?, ChangeSequence(1));
    Ok(())
}

fn new_copy(fixture: &Fixture, name: &str, copy_data: bool) -> Result<CommitReceipt, StoreError> {
    fixture.store.create_artifact(NewArtifact {
        request: RequestIdentity {
            actor: &fixture.owner,
            key: &key(name),
            fingerprint: RequestFingerprint(digest(name)),
        },
        kind: fixture.created.artifact.kind,
        title: &parsed("Copy"),
        package: &package(&[("index.html", "<h1>2</h1>")]),
        source: Some(CopySource {
            artifact: fixture.id(),
            copy_data,
        }),
    })
}

/// A duplicate that copies the data records only `created`, and a purge only
/// `purged` (and the `deleted` before it), however much data the artifact has.
#[test]
fn a_copy_of_data_records_only_created_and_a_purge_only_purged() -> TestResult {
    let fixture = Fixture::new()?;
    let owner = fixture.owner.clone();
    fixture.write("one", 0, &set("a", "1")?)?;
    let after_write = fixture.head(&owner)?;

    let copy = new_copy(&fixture, "copy", true)?;
    let copied = fixture.store.read_data(&copy.artifact.id, &owner)?;
    assert_eq!(copied.version, AppDataVersion(1), "the data was copied");
    let feed = fixture.feed(&owner)?;
    let added: Vec<_> = feed
        .iter()
        .filter(|change| change.sequence > after_write)
        .map(|change| (change.kind, change.artifact.clone(), change.data_version))
        .collect();
    assert_eq!(
        added,
        [(ChangeKind::Created, copy.artifact.id.clone(), None)],
        "the copy's feed entry is its creation alone"
    );

    let before_purge = fixture.head(&owner)?;
    update_metadata(
        &fixture.store,
        &fixture.created,
        &key("trash"),
        1,
        None,
        None,
        Some(true),
    )?;
    fixture.store.purge_artifact(PurgeRequest {
        request: RequestIdentity {
            actor: &owner,
            key: &key("purge"),
            fingerprint: RequestFingerprint(digest("purge")),
        },
        artifact: fixture.id(),
        base: MetadataRevision(2),
    })?;
    let kinds: Vec<_> = fixture
        .feed(&owner)?
        .iter()
        .filter(|change| change.sequence > before_purge)
        .map(|change| (change.kind, change.data_version))
        .collect();
    assert_eq!(
        kinds,
        [(ChangeKind::Deleted, None), (ChangeKind::Purged, None)],
        "erasing the data records no data change"
    );
    assert_eq!(fixture.data_changes(&owner)?.len(), 1, "the one write");
    Ok(())
}

/// Each constraint of the rebuilt table refuses its inconsistent row, and a
/// consistent row passes, so each refusal is that constraint's and not a
/// foreign key's.
#[test]
fn each_check_of_the_change_table_refuses_its_inconsistent_row() -> TestResult {
    let fixture = Fixture::new()?;
    let connection = fixture.sql()?;
    let insert = |number: u32, kind: &str, data_version: &str| {
        connection.execute(
            &format!(
                "INSERT INTO changes
                     (actor, actor_seq, artifact_id, kind, revision, metadata_revision,
                      data_version, committed_at)
                 VALUES ('check', {number}, ?1, '{kind}', 1, 1, {data_version}, 5)"
            ),
            [fixture.id().as_str()],
        )
    };

    insert(1, "data_changed", "1")?;
    insert(2, "data_changed", "9007199254740991")?;
    insert(3, "revised", "NULL")?;
    for (number, kind, data_version, why) in [
        (4, "data_changed", "NULL", "a data change without a version"),
        (5, "revised", "1", "another kind with a version"),
        (6, "data_changed", "0", "a version below 1"),
        (7, "data_changed", "-1", "a negative version"),
        (8, "something_else", "NULL", "a kind outside the list"),
        (9, "created", "0", "another kind with version 0"),
    ] {
        let error = insert(number, kind, data_version)
            .err()
            .ok_or_else(|| format!("accepted {why}"))?;
        assert!(
            error.to_string().contains("CHECK constraint failed"),
            "{why}: {error}"
        );
    }
    let accepted: i64 = connection.query_row(
        "SELECT count(*) FROM changes WHERE actor = 'check'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(accepted, 3);
    Ok(())
}
