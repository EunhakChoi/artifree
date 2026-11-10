//! The source fence of a runtime frame's application data write (decision
//! 0053, rule 10): the write transaction checks the view's source revision
//! against the artifact's head before it looks at the request key, so a write
//! from a view that is no longer current, and a retry of one, is refused with
//! the current revision and never replayed. A plain write (decision 0031) has
//! no source fence.

mod common;

use artifree_domain::change::ChangeSequence;
use artifree_domain::data::{DataChange, DataValue};
use artifree_domain::identity::{ActorId, ArtifactId, RequestKey, UnixMillis};
use artifree_domain::{AppDataVersion, SourceRevision};
use artifree_ports::artifact_store::{
    ArtifactStore, ChangeKind, CommitReceipt, Delivery, RequestFingerprint, RequestIdentity,
    StoreError,
};
use artifree_ports::data_store::{DataReceipt, DataStore, DataStoreError, DataWrite};
use artifree_providers::storage::SqliteStore;
use common::{TempDir, TestResult, actor, create_demo, database_in, digest, key, parsed, revise};
use rusqlite::Connection;

const FIRST: &str = "<h1>first</h1>";
const SECOND: &str = "<h1>second</h1>";

fn value(text: &str) -> DataValue {
    DataValue::parse(text).unwrap_or_else(|error| panic!("{text}: {error}"))
}

fn set(at_path: &str, text: &str) -> DataChange {
    DataChange::Set {
        path: parsed(at_path),
        value: value(text),
    }
}

fn merge(at_path: &str, text: &str) -> DataChange {
    DataChange::Merge {
        path: parsed(at_path),
        patch: value(text),
    }
}

fn remove(at_path: &str) -> DataChange {
    DataChange::Remove {
        path: parsed(at_path),
    }
}

/// The receipt's fields, or the error, so that a failed assertion shows what
/// the write returned.
fn outcome(
    result: Result<DataReceipt, DataStoreError>,
) -> Result<(AppDataVersion, bool, Delivery), DataStoreError> {
    result.map(|receipt| (receipt.version, receipt.changed, receipt.delivery))
}

/// The earlier request that holds a key before a fenced write uses it. Both
/// kinds draw on the actor's one key space (decision 0031).
#[derive(Debug, Clone, Copy)]
enum Earlier {
    /// A plain write of the same actor, with another change.
    DataWrite,
    /// A revision of the artifact.
    Revision,
}

struct Fixture {
    dir: TempDir,
    store: SqliteStore,
    owner: ActorId,
    created: CommitReceipt,
}

impl Fixture {
    /// An artifact of `owner` at source revision 2 with an empty space, as the
    /// data tests have it.
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let dir = TempDir::new();
        let store = SqliteStore::open(&database_in(dir.path()))?;
        let owner = actor("owner");
        let created = create_demo(&store, &owner, &key("create"), "Fenced", FIRST)?;
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

    /// The artifact's current source revision.
    fn head(&self) -> Result<SourceRevision, Box<dyn std::error::Error>> {
        let record = self.store.artifact(self.id())?.ok_or("artifact")?;
        Ok(record.current_revision)
    }

    /// Commits a revision made from `base`, which moves the head to `base + 1`.
    /// The body is new, so the commit is a revision and not a no-op.
    fn move_head(&self, name: &str, base: u64) -> Result<(), StoreError> {
        let body = format!("<h1>{name}</h1>");
        revise(&self.store, &self.created, &key(name), base, &body).map(|_| ())
    }

    /// A plain write, which has no source fence.
    fn plain(
        &self,
        name: &str,
        base: u64,
        change: &DataChange,
    ) -> Result<DataReceipt, DataStoreError> {
        let request_key = key(name);
        self.store
            .write_data(self.write_request(&request_key, base, change))
    }

    /// A runtime write, fenced to the source revision `pinned`.
    fn fenced(
        &self,
        name: &str,
        base: u64,
        change: &DataChange,
        pinned: SourceRevision,
    ) -> Result<DataReceipt, DataStoreError> {
        let request_key = key(name);
        self.store
            .write_runtime_data(self.write_request(&request_key, base, change), pinned)
    }

    /// The write as the core makes it. The fingerprint names the artifact, the
    /// base, and the change, so a retry of the same request matches the first.
    fn write_request<'a>(
        &'a self,
        request_key: &'a RequestKey,
        base: u64,
        change: &'a DataChange,
    ) -> DataWrite<'a> {
        DataWrite {
            fence: None,
            request: RequestIdentity {
                actor: &self.owner,
                key: request_key,
                fingerprint: RequestFingerprint(digest(&format!(
                    "{} {base} {change:?}",
                    self.id()
                ))),
            },
            artifact: self.id(),
            base: AppDataVersion(base),
            change,
            now: UnixMillis(1_000),
        }
    }

    /// The owner's space as canonical text, with its version.
    fn space(&self) -> Result<(u64, String), DataStoreError> {
        let snapshot = self.store.read_data(self.id(), &self.owner)?;
        Ok((snapshot.version.0, snapshot.space.to_canonical()))
    }

    /// The owner's feed position, and the version of each data change in it.
    fn feed(&self) -> Result<(ChangeSequence, Vec<u64>), StoreError> {
        let page = self.store.changes(&self.owner, ChangeSequence(0), 1_000)?;
        let versions: Vec<u64> = page
            .changes
            .iter()
            .filter(|change| change.kind == ChangeKind::DataChanged)
            .map(|change| change.data_version.map_or(0, |version| version.0))
            .collect();
        Ok((page.head, versions))
    }

    /// How many request keys of the owner's are recorded under `name`: 0 or 1.
    fn recorded(&self, name: &str) -> Result<i64, rusqlite::Error> {
        let request_key = key(name);
        self.sql()?.query_row(
            "SELECT count(*) FROM data_requests WHERE actor = ?1 AND request_key = ?2",
            [self.owner.as_str(), request_key.as_str()],
            |row| row.get(0),
        )
    }
}

/// A write pinned to the head commits as a plain write does: the same receipt,
/// the same space, and the same data change in the feed.
#[test]
fn a_write_pinned_to_the_head_commits_like_a_plain_write() -> TestResult {
    let fenced = Fixture::new()?;
    let plain = Fixture::new()?;
    assert_eq!(
        fenced.head()?,
        SourceRevision(2),
        "the fixture is at source revision 2"
    );
    let change = set("n", "1");
    let receipt = outcome(fenced.fenced("at-head", 0, &change, SourceRevision(2)));
    assert_eq!(
        receipt,
        Ok((AppDataVersion(1), true, Delivery::Committed)),
        "a write pinned to the head commits"
    );
    assert_eq!(
        outcome(plain.plain("at-head", 0, &change)),
        receipt,
        "a plain write of the same change gives the same receipt"
    );
    assert_eq!(fenced.space()?, plain.space()?, "the same space");
    assert_eq!(
        fenced.feed()?.1,
        plain.feed()?.1,
        "the same data changes in the feed"
    );
    assert_eq!(fenced.feed()?.1, vec![1], "one data change, at version 1");
    Ok(())
}

/// A write pinned to a revision the artifact has moved past is refused with
/// the current revision, and changes nothing. Its base is the current version,
/// so only the source can refuse it.
#[test]
fn a_stale_pin_is_a_source_conflict_that_names_the_head_and_writes_nothing() -> TestResult {
    for (name, change) in [
        ("set", set("n", "2")),
        ("merge", merge("", "{\"m\":true}")),
        ("remove", remove("n")),
    ] {
        let fixture = Fixture::new()?;
        fixture.move_head("move", 2)?;
        assert_eq!(
            fixture.head()?,
            SourceRevision(3),
            "{name}: the head moved past the pin"
        );
        fixture.plain("seed", 0, &set("n", "1"))?;
        let before = (fixture.space()?, fixture.feed()?);
        assert_eq!(
            outcome(fixture.fenced(name, 1, &change, SourceRevision(1))),
            Err(DataStoreError::SourceConflict {
                current: SourceRevision(3)
            }),
            "{name}: a write pinned to revision 1 is refused with the head at 3"
        );
        assert_eq!(
            (fixture.space()?, fixture.feed()?),
            before,
            "{name}: a refused write changes neither the space nor the feed"
        );
        assert_eq!(
            fixture.recorded(name)?,
            0,
            "{name}: a refused write records no key"
        );
    }
    Ok(())
}

/// A retry of a write from a view that is no longer current is refused, not
/// replayed, although the first attempt committed under the same key.
#[test]
fn a_retry_after_the_head_moved_is_refused_not_replayed() -> TestResult {
    let fixture = Fixture::new()?;
    let change = set("n", "1");
    assert_eq!(
        outcome(fixture.fenced("retry", 0, &change, SourceRevision(2))),
        Ok((AppDataVersion(1), true, Delivery::Committed)),
        "the first write, at the head, commits"
    );
    fixture.move_head("move", 2)?;
    let before = (fixture.space()?, fixture.feed()?);
    assert_eq!(
        outcome(fixture.fenced("retry", 0, &change, SourceRevision(2))),
        Err(DataStoreError::SourceConflict {
            current: SourceRevision(3)
        }),
        "a retry after the head moved must be refused, not replayed"
    );
    assert_eq!(
        (fixture.space()?, fixture.feed()?),
        before,
        "the refused retry changes neither the space nor the feed"
    );
    Ok(())
}

/// The source check comes before the key: a key that a plain write of the
/// actor used is a source conflict at a stale head, and not a reused key.
#[test]
fn a_key_a_write_used_is_a_source_conflict_at_a_stale_head() -> TestResult {
    stale_head_refuses_a_key_used_by(Earlier::DataWrite)
}

/// The same for a key that a revision of the artifact used, which the actor's
/// key space shares with the writes.
#[test]
fn a_key_a_revision_used_is_a_source_conflict_at_a_stale_head() -> TestResult {
    stale_head_refuses_a_key_used_by(Earlier::Revision)
}

/// A fenced write with a key that `earlier` used, at a head that moved past
/// the pin. The base is the current version, so only the key or the source can
/// refuse the write.
fn stale_head_refuses_a_key_used_by(earlier: Earlier) -> TestResult {
    let fixture = Fixture::new()?;
    match earlier {
        Earlier::DataWrite => {
            fixture.plain("shared", 0, &set("n", "1"))?;
            fixture.move_head("move", 2)?;
        }
        Earlier::Revision => fixture.move_head("shared", 2)?,
    }
    assert_eq!(
        fixture.head()?,
        SourceRevision(3),
        "{earlier:?}: the head moved past the pin"
    );
    let base = fixture.space()?.0;
    assert_eq!(
        outcome(fixture.fenced("shared", base, &set("n", "2"), SourceRevision(2))),
        Err(DataStoreError::SourceConflict {
            current: SourceRevision(3)
        }),
        "{earlier:?}: a key another request used is a source conflict at a stale head"
    );
    Ok(())
}

/// A plain write has no source fence, so it commits after the head moved.
#[test]
fn a_plain_write_is_not_fenced_and_commits_after_the_head_moved() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.move_head("move", 2)?;
    assert_eq!(
        outcome(fixture.plain("plain", 0, &set("n", "1"))),
        Ok((AppDataVersion(1), true, Delivery::Committed)),
        "a plain write commits whatever the head is"
    );
    assert_eq!(fixture.space()?, (1, "{\"n\":1}".to_owned()));
    Ok(())
}
