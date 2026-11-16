//! The declared shared space of an artifact (decision 0060 rule 10, slice
//! S02f1): reached only while the head revision's runtime manifest declares
//! it, written by the role the declaration names, read by every reader, and
//! changed in every reader's feed (rule 13). These tests call the store
//! directly, as a write that passed the core's check reaches it.

mod common;

use artifree_domain::AppDataVersion;
use artifree_domain::access::{AccessVersion, Role, Space};
use artifree_domain::change::ChangeSequence;
use artifree_domain::data::{DataChange, DataValue};
use artifree_domain::identity::{ActorId, ArtifactId, UnixMillis, UserLabel};
use artifree_domain::{MetadataRevision, SourceRevision};
use artifree_ports::access_store::{AccessChange, AccessRequest, AccessStore};
use artifree_ports::artifact_store::{
    ArtifactStore, ChangeKind, CommitReceipt, MetadataUpdate, PurgeRequest, RequestFingerprint,
    RequestIdentity, RevisionCommit,
};
use artifree_ports::data_store::{DataReceipt, DataStore, DataStoreError, DataWrite};
use artifree_ports::user_store::{NewUser, UserStore};
use artifree_providers::storage::SqliteStore;
use artifree_providers::tokens::SqliteTokenStore;
use common::{TempDir, TestResult, actor, create_demo, database_in, digest, key, package, parsed};

/// The kind and data version of one feed row.
type Row = (ChangeKind, Option<AppDataVersion>);

struct Fixture {
    dir: TempDir,
    store: SqliteStore,
    owner: ActorId,
    created: CommitReceipt,
    editor: ActorId,
    viewer: ActorId,
    stranger: ActorId,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let dir = TempDir::new();
        let database = database_in(dir.path());
        let store = SqliteStore::open(&database)?;
        let owner = actor("local");
        let created = create_demo(&store, &owner, &key("create"), "Shared", "v1")?;
        let editor = ActorId::user([1; 16]);
        let viewer = ActorId::user([2; 16]);
        let identity = SqliteTokenStore::open(&database)?;
        for (base, user, role, label) in [
            (0, &editor, Role::Editor, "Ed"),
            (1, &viewer, Role::Viewer, "Vi"),
        ] {
            identity.register(&NewUser {
                id: user.clone(),
                label: parsed::<UserLabel>(label),
                created_at: "2026-11-17T00:00:00Z".to_owned(),
            })?;
            store.change_access(AccessRequest {
                request: RequestIdentity {
                    actor: &owner,
                    key: &key(label),
                    fingerprint: RequestFingerprint(digest(label)),
                },
                artifact: &created.artifact.id,
                user,
                change: AccessChange::Grant(role),
                base: AccessVersion(base),
                now: UnixMillis(1),
            })?;
        }
        Ok(Self {
            dir,
            store,
            owner,
            created,
            editor,
            viewer,
            stranger: ActorId::user([3; 16]),
        })
    }

    fn id(&self) -> &ArtifactId {
        &self.created.artifact.id
    }

    /// Commits revision `base + 1` with `manifest` as its runtime manifest,
    /// or with none.
    fn head(&self, base: u64, manifest: Option<&str>) -> TestResult {
        let mut files = vec![("index.html", "<p>shared</p>")];
        if let Some(manifest) = manifest {
            files.push(("artifree-runtime.json", manifest));
        }
        let name = format!("head-{base}");
        self.store.commit_revision(RevisionCommit {
            request: RequestIdentity {
                actor: &self.owner,
                key: &key(&name),
                fingerprint: RequestFingerprint(digest(&name)),
            },
            artifact: self.id(),
            base: SourceRevision(base),
            package: &package(&files),
        })?;
        Ok(())
    }

    fn write(
        &self,
        by: &ActorId,
        name: &str,
        space: Space,
        base: u64,
        value: &str,
    ) -> Result<DataReceipt, DataStoreError> {
        let change = DataChange::Set {
            path: parsed("note"),
            value: DataValue::parse(value).map_err(DataStoreError::Rejected)?,
        };
        self.store.write_data(DataWrite {
            fence: None,
            request: RequestIdentity {
                actor: by,
                key: &key(name),
                fingerprint: RequestFingerprint(digest(name)),
            },
            artifact: self.id(),
            space,
            base: AppDataVersion(base),
            change: &change,
            now: UnixMillis(1),
        })
    }

    /// The version and canonical text of a space.
    fn read(&self, by: &ActorId, space: Space) -> Result<(u64, String), DataStoreError> {
        let snapshot = self.store.read_data(self.id(), by, space)?;
        Ok((snapshot.version.0, snapshot.space.to_canonical()))
    }

    /// The kinds and data versions of the rows of `user`'s feed.
    fn feed(&self, user: &ActorId) -> Result<Vec<Row>, Box<dyn std::error::Error>> {
        Ok(self
            .store
            .changes(user, ChangeSequence(0), 100)?
            .changes
            .into_iter()
            .map(|change| (change.kind, change.data_version))
            .collect())
    }
}

const EDITORS: &str = r#"{"sdk":"artifree.app.v1","shared":"editors"}"#;
const MEMBERS: &str = r#"{"sdk":"artifree.app.v1","shared":"members"}"#;

#[test]
fn the_shared_space_is_reached_only_while_the_head_declares_it() -> TestResult {
    let f = Fixture::new()?;
    let missing = Err(DataStoreError::NotFound);
    // Revision 1 has no manifest.
    assert_eq!(f.read(&f.owner, Space::Shared), missing);
    assert_eq!(
        f.write(&f.owner, "before", Space::Shared, 0, "1")
            .map(|_| ()),
        Err(DataStoreError::NotFound)
    );
    // A manifest without the member, and one with another value, declare none.
    f.head(1, Some(r#"{"sdk":"artifree.app.v1"}"#))?;
    assert_eq!(f.read(&f.owner, Space::Shared), missing);
    f.head(2, Some(r#"{"sdk":"artifree.app.v1","shared":"viewers"}"#))?;
    assert_eq!(f.read(&f.owner, Space::Shared), missing);
    f.head(3, Some(EDITORS))?;
    let written = f.write(&f.owner, "declared", Space::Shared, 0, "\"one\"")?;
    assert_eq!(written.version, AppDataVersion(1));
    let shared = (1, r#"{"note":"one"}"#.to_owned());
    assert_eq!(f.read(&f.viewer, Space::Shared), Ok(shared.clone()));
    // Each user's own space is apart from it.
    assert_eq!(f.read(&f.viewer, Space::Own), Ok((0, "{}".to_owned())));
    // Removing the declaration from the head makes it unreachable and keeps it.
    f.head(4, None)?;
    assert_eq!(f.read(&f.owner, Space::Shared), missing);
    assert_eq!(
        f.write(&f.owner, "undeclared", Space::Shared, 1, "2")
            .map(|_| ()),
        Err(DataStoreError::NotFound)
    );
    f.head(5, Some(MEMBERS))?;
    assert_eq!(f.read(&f.editor, Space::Shared), Ok(shared));
    Ok(())
}

/// The store checks the caller's access in the snapshot of a shared read,
/// and the declared writer before a write's content (decision 0060 rules 4,
/// 5 and 10), not only where the core checked earlier.
#[test]
fn the_store_checks_access_in_the_read_and_before_the_write() -> TestResult {
    let f = Fixture::new()?;
    f.head(1, Some(EDITORS))?;
    assert!(f.read(&f.viewer, Space::Shared).is_ok());
    assert!(
        f.store
            .admit_write(f.id(), &f.editor, Space::Shared)
            .is_ok()
    );
    assert!(matches!(
        f.store.admit_write(f.id(), &f.viewer, Space::Shared),
        Err(DataStoreError::Forbidden)
    ));
    assert!(matches!(
        f.store.admit_write(f.id(), &f.stranger, Space::Shared),
        Err(DataStoreError::NotFound)
    ));
    f.store.change_access(AccessRequest {
        request: RequestIdentity {
            actor: &f.owner,
            key: &key("revoke-viewer"),
            fingerprint: RequestFingerprint(digest("revoke-viewer")),
        },
        artifact: f.id(),
        user: &f.viewer,
        change: AccessChange::Revoke,
        base: AccessVersion(2),
        now: UnixMillis(2),
    })?;
    assert!(matches!(
        f.read(&f.viewer, Space::Shared),
        Err(DataStoreError::NotFound)
    ));
    Ok(())
}

#[test]
fn the_declaration_names_who_writes_and_every_reader_reads() -> TestResult {
    let f = Fixture::new()?;
    f.head(1, Some(EDITORS))?;
    f.write(&f.editor, "by-editor", Space::Shared, 0, "1")?;
    // A viewer may not write under "editors"; its key stays free.
    assert_eq!(
        f.write(&f.viewer, "by-viewer", Space::Shared, 1, "2")
            .map(|_| ()),
        Err(DataStoreError::Forbidden)
    );
    assert_eq!(f.read(&f.viewer, Space::Shared)?.0, 1);
    // Anyone who is not a reader does not find it, a write or a read.
    assert_eq!(
        f.write(&f.stranger, "by-stranger", Space::Shared, 1, "2")
            .map(|_| ()),
        Err(DataStoreError::NotFound)
    );
    // Under "members" the viewer writes, with the key it was refused under.
    f.head(2, Some(MEMBERS))?;
    let written = f.write(&f.viewer, "by-viewer", Space::Shared, 1, "2")?;
    assert_eq!(written.version, AppDataVersion(2));
    // The head is read where the write commits: declaring "editors" again
    // refuses the viewer's next write.
    f.head(3, Some(EDITORS))?;
    assert_eq!(
        f.write(&f.viewer, "by-viewer-again", Space::Shared, 2, "3")
            .map(|_| ()),
        Err(DataStoreError::Forbidden)
    );
    Ok(())
}

#[test]
fn a_shared_change_reaches_every_reader_and_an_own_change_only_its_writer() -> TestResult {
    let f = Fixture::new()?;
    f.head(1, Some(MEMBERS))?;
    let before = |user: &ActorId| f.feed(user).map(|rows| rows.len());
    let counts = [before(&f.owner)?, before(&f.editor)?, before(&f.viewer)?];
    f.write(&f.viewer, "shared", Space::Shared, 0, "1")?;
    f.write(&f.viewer, "own", Space::Own, 0, "1")?;
    // Writing what is there changes nothing and adds no row.
    f.write(&f.editor, "same", Space::Shared, 1, "1")?;
    let shared = (ChangeKind::SharedDataChanged, Some(AppDataVersion(1)));
    let own = (ChangeKind::DataChanged, Some(AppDataVersion(1)));
    for (user, count, rows) in [
        (&f.owner, counts[0], vec![shared]),
        (&f.editor, counts[1], vec![shared]),
        (&f.viewer, counts[2], vec![shared, own]),
    ] {
        let feed = f.feed(user)?;
        assert_eq!(feed.get(count..), Some(rows.as_slice()), "{user}");
    }
    assert_eq!(f.feed(&f.stranger)?, []);
    Ok(())
}

#[test]
fn the_upgrade_to_schema_20_keeps_every_change_and_takes_shared_rows() -> TestResult {
    let f = Fixture::new()?;
    f.head(1, Some(MEMBERS))?;
    f.write(&f.owner, "own", Space::Own, 0, "1")?;
    let before = f.feed(&f.owner)?;
    let path = database_in(f.dir.path());
    // Schema 19: the change kinds before `shared_data_changed`, no shared table,
    // and a change row whose job is gone.
    rusqlite::Connection::open(&path)?.execute_batch(
        "PRAGMA foreign_keys = OFF;
        CREATE TABLE changes_v19 (
            sequence INTEGER PRIMARY KEY AUTOINCREMENT,
            actor TEXT NOT NULL,
            actor_seq INTEGER NOT NULL CHECK (actor_seq >= 1),
            artifact_id TEXT NOT NULL,
            kind TEXT NOT NULL CHECK (kind IN
                ('created', 'revised', 'metadata_updated', 'deleted', 'recovered', 'purged',
                 'data_changed', 'job_changed', 'access_granted', 'access_changed',
                 'access_revoked')),
            revision INTEGER NOT NULL,
            metadata_revision INTEGER NOT NULL,
            data_version INTEGER,
            job_id TEXT,
            committed_at INTEGER NOT NULL,
            UNIQUE (actor, actor_seq),
            FOREIGN KEY (artifact_id, revision) REFERENCES revisions (artifact_id, revision),
            FOREIGN KEY (artifact_id, metadata_revision)
                REFERENCES metadata_revisions (artifact_id, metadata_revision),
            FOREIGN KEY (job_id) REFERENCES jobs (id),
            CHECK ((kind = 'data_changed') = (data_version IS NOT NULL)),
            CHECK (data_version IS NULL OR data_version >= 1),
            CHECK ((kind = 'job_changed') = (job_id IS NOT NULL))
        ) STRICT;
        INSERT INTO changes_v19 SELECT sequence, actor, actor_seq, artifact_id, kind, revision,
            metadata_revision, data_version, job_id, committed_at FROM changes;
        DROP TABLE changes;
        ALTER TABLE changes_v19 RENAME TO changes;
        INSERT INTO changes (actor, actor_seq, artifact_id, kind, revision, metadata_revision,
                job_id, committed_at)
            SELECT actor, actor_seq + 1, artifact_id, 'job_changed', revision, metadata_revision,
                'job_00000000000000000000000000000000', committed_at
            FROM changes ORDER BY sequence DESC LIMIT 1;
        UPDATE actor_feeds SET head = head + 1
            WHERE actor = (SELECT actor FROM changes ORDER BY sequence DESC LIMIT 1);
        DROP TABLE shared_data;
        PRAGMA user_version = 19;",
    )?;
    // The row above names a job that is gone, as an older file's rows may:
    // the upgrade copies it with the foreign keys relaxed.
    let mut before = before;
    before.push((ChangeKind::JobChanged, None));
    let upgraded = Fixture {
        store: SqliteStore::open(&path)?,
        ..f
    };
    assert_eq!(common::user_version(&path)?, 20);
    assert_eq!(upgraded.feed(&upgraded.owner)?, before);
    upgraded.write(&upgraded.editor, "shared", Space::Shared, 0, "1")?;
    assert_eq!(
        upgraded.feed(&upgraded.owner)?.last(),
        Some(&(ChangeKind::SharedDataChanged, Some(AppDataVersion(1))))
    );
    Ok(())
}

#[test]
fn a_purge_erases_the_shared_space() -> TestResult {
    let f = Fixture::new()?;
    f.head(1, Some(EDITORS))?;
    f.write(&f.owner, "shared", Space::Shared, 0, "\"gone\"")?;
    let rows = || -> Result<i64, rusqlite::Error> {
        rusqlite::Connection::open(database_in(f.dir.path()))?.query_row(
            "SELECT count(*) FROM shared_data",
            [],
            |row| row.get(0),
        )
    };
    assert_eq!(rows()?, 1);
    f.store.update_metadata(MetadataUpdate {
        request: RequestIdentity {
            actor: &f.owner,
            key: &key("trash"),
            fingerprint: RequestFingerprint(digest("trash")),
        },
        artifact: f.id(),
        base: MetadataRevision(1),
        title: None,
        pinned: None,
        deleted: Some(true),
    })?;
    f.store.purge_artifact(PurgeRequest {
        request: RequestIdentity {
            actor: &f.owner,
            key: &key("purge"),
            fingerprint: RequestFingerprint(digest("purge")),
        },
        artifact: f.id(),
        base: MetadataRevision(2),
    })?;
    assert_eq!(rows()?, 0);
    Ok(())
}
