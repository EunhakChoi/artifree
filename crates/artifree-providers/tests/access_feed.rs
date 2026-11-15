//! Who receives each change of a shared artifact (decision 0060 rules 13 and
//! 14, slice S02c): a change of the artifact reaches its owner and every
//! current member, each in their own numbering; an access change is a row of
//! its own; a revoked user receives nothing after its revocation; the trash
//! keeps every row but the move itself from members.

mod common;

use artifree_domain::access::{AccessVersion, Role};
use artifree_domain::change::ChangeSequence;
use artifree_domain::data::{DataChange, DataPath, DataValue};
use artifree_domain::identity::{ActorId, UnixMillis, UserLabel};
use artifree_domain::job::{JobKind, JobState, WorkerId};
use artifree_domain::{AppDataVersion, MetadataRevision, SourceRevision};
use artifree_ports::access_store::{AccessChange, AccessRequest, AccessStore};
use artifree_ports::artifact_store::{
    ArtifactStore, ChangeKind, CommitReceipt, MetadataUpdate, PurgeRequest, RequestFingerprint,
    RequestIdentity, RevisionCommit,
};
use artifree_ports::data_store::{DataStore, DataWrite};
use artifree_ports::job_store::{JobClaim, JobStore, NewJob};
use artifree_ports::user_store::{NewUser, UserStore};
use artifree_providers::storage::SqliteStore;
use artifree_providers::tokens::SqliteTokenStore;
use common::{TempDir, TestResult, actor, create_demo, database_in, digest, key, package, parsed};

type Feed = Vec<(u64, ChangeKind)>;

struct Fixture {
    dir: TempDir,
    store: SqliteStore,
    owner: ActorId,
    created: CommitReceipt,
    ana: ActorId,
    ben: ActorId,
    cy: ActorId,
    /// The access version.
    version: std::cell::Cell<u64>,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let dir = TempDir::new();
        let path = database_in(dir.path());
        let store = SqliteStore::open(&path)?;
        let identity = SqliteTokenStore::open(&path)?;
        let owner = actor("local");
        let created = create_demo(&store, &owner, &key("create"), "Shared", "v1")?;
        let mut users = Vec::new();
        for (label, seed) in [("Ana", 1), ("Ben", 2), ("Cy", 3)] {
            let id = ActorId::user([seed; 16]);
            identity.register(&NewUser {
                id: id.clone(),
                label: parsed::<UserLabel>(label),
                created_at: "2026-11-16T00:00:00Z".to_owned(),
            })?;
            users.push(id);
        }
        let [ana, ben, cy] =
            <[ActorId; 3]>::try_from(users).map_err(|_| "three users were registered")?;
        Ok(Self {
            dir,
            store,
            owner,
            created,
            ana,
            ben,
            cy,
            version: std::cell::Cell::new(0),
        })
    }

    fn access(&self, name: &str, user: &ActorId, change: AccessChange) -> TestResult {
        let receipt = self.store.change_access(AccessRequest {
            request: RequestIdentity {
                actor: &self.owner,
                key: &key(name),
                fingerprint: RequestFingerprint(digest(&format!("{user}{change:?}"))),
            },
            artifact: &self.created.artifact.id,
            user,
            change,
            base: AccessVersion(self.version.get()),
            now: UnixMillis(1),
        })?;
        self.version.set(receipt.version.0);
        Ok(())
    }

    fn grant(&self, name: &str, user: &ActorId, role: Role) -> TestResult {
        self.access(name, user, AccessChange::Grant(role))
    }

    fn commit(&self, by: &ActorId, name: &str, base: u64) -> TestResult {
        let request_key = key(name);
        self.store.commit_revision(RevisionCommit {
            request: RequestIdentity {
                actor: by,
                key: &request_key,
                fingerprint: RequestFingerprint(digest(name)),
            },
            artifact: &self.created.artifact.id,
            base: SourceRevision(base),
            package: &package(&[("index.html", name), ("style.css", "h1{}")]),
        })?;
        Ok(())
    }

    fn enqueue(&self, by: &ActorId, name: &str) -> TestResult {
        let job_key = key(name);
        self.store.enqueue_job(NewJob {
            request: RequestIdentity {
                actor: by,
                key: &job_key,
                fingerprint: RequestFingerprint(digest(name)),
            },
            artifact: &self.created.artifact.id,
            revision: SourceRevision(1),
            kind: JobKind::VerifyRevision,
            now: UnixMillis(1),
        })?;
        Ok(())
    }

    fn trash(&self, name: &str, base: u64, deleted: bool) -> TestResult {
        let request_key = key(name);
        self.store.update_metadata(MetadataUpdate {
            request: RequestIdentity {
                actor: &self.owner,
                key: &request_key,
                fingerprint: RequestFingerprint(digest(name)),
            },
            artifact: &self.created.artifact.id,
            base: MetadataRevision(base),
            title: None,
            pinned: None,
            deleted: Some(deleted),
        })?;
        Ok(())
    }

    /// The kinds of the rows of `user`'s feed, with their numbers.
    fn feed(&self, user: &ActorId) -> Result<Feed, Box<dyn std::error::Error>> {
        Ok(self
            .store
            .changes(user, ChangeSequence(0), 100)?
            .changes
            .into_iter()
            .map(|change| (change.sequence.0, change.kind))
            .collect())
    }

    /// The kinds of the rows of `user`'s feed.
    fn kinds(&self, user: &ActorId) -> Result<Vec<ChangeKind>, Box<dyn std::error::Error>> {
        Ok(self.feed(user)?.into_iter().map(|(_, kind)| kind).collect())
    }
}

use ChangeKind::{
    AccessChanged, AccessGranted, AccessRevoked, Created, DataChanged, Deleted, JobChanged, Purged,
    Recovered, Revised,
};

#[test]
fn a_commit_reaches_the_owner_and_every_member_each_in_its_own_numbering() -> TestResult {
    let f = Fixture::new()?;
    f.grant("grant-ana", &f.ana, Role::Editor)?;
    f.grant("grant-ben", &f.ben, Role::Viewer)?;
    f.commit(&f.ana, "ana-save", 1)?;
    assert_eq!(
        f.kinds(&f.owner)?,
        [Created, AccessGranted, AccessGranted, Revised]
    );
    // Each member's numbering starts at its own first row, with no gap.
    assert_eq!(
        f.feed(&f.ana)?,
        [(1, AccessGranted), (2, AccessGranted), (3, Revised)]
    );
    assert_eq!(f.feed(&f.ben)?, [(1, AccessGranted), (2, Revised)]);
    assert!(f.feed(&f.cy)?.is_empty(), "a stranger receives nothing");
    Ok(())
}

#[test]
fn a_role_change_and_a_revocation_are_rows_and_end_the_revoked_users_feed() -> TestResult {
    let f = Fixture::new()?;
    f.grant("grant-ana", &f.ana, Role::Editor)?;
    f.grant("grant-ben", &f.ben, Role::Editor)?;
    // The role a member already has is no change, and no row.
    f.grant("grant-ben-again", &f.ben, Role::Editor)?;
    f.grant("downgrade-ben", &f.ben, Role::Viewer)?;
    f.access("revoke-ben", &f.ben, AccessChange::Revoke)?;
    f.commit(&f.ana, "ana-save", 1)?;
    assert_eq!(
        f.kinds(&f.ben)?,
        [AccessGranted, AccessChanged, AccessRevoked],
        "the revocation is the revoked user's last row of the artifact"
    );
    assert_eq!(
        f.kinds(&f.ana)?,
        [
            AccessGranted,
            AccessGranted,
            AccessChanged,
            AccessRevoked,
            Revised
        ]
    );
    assert_eq!(
        f.kinds(&f.owner)?,
        [
            Created,
            AccessGranted,
            AccessGranted,
            AccessChanged,
            AccessRevoked,
            Revised
        ]
    );
    Ok(())
}

#[test]
fn the_move_to_the_trash_is_a_members_last_row_until_the_recovery() -> TestResult {
    let f = Fixture::new()?;
    f.grant("grant-ana", &f.ana, Role::Viewer)?;
    f.trash("owner-trash", 1, true)?;
    f.trash("owner-recover", 2, false)?;
    f.trash("owner-trash-2", 3, true)?;
    let purge_key = key("owner-purge");
    f.store.purge_artifact(PurgeRequest {
        request: RequestIdentity {
            actor: &f.owner,
            key: &purge_key,
            fingerprint: RequestFingerprint(digest("purge")),
        },
        artifact: &f.created.artifact.id,
        base: MetadataRevision(4),
    })?;
    assert_eq!(
        f.kinds(&f.ana)?,
        [AccessGranted, Deleted, Recovered, Deleted],
        "the purge in the trash reaches only the owner"
    );
    assert_eq!(
        f.kinds(&f.owner)?,
        [Created, AccessGranted, Deleted, Recovered, Deleted, Purged]
    );
    Ok(())
}

#[test]
fn a_members_own_data_and_jobs_reach_only_that_member() -> TestResult {
    let f = Fixture::new()?;
    f.grant("grant-ana", &f.ana, Role::Editor)?;
    f.grant("grant-ben", &f.ben, Role::Viewer)?;
    let data_key = key("ana-data");
    f.store.write_data(DataWrite {
        fence: None,
        request: RequestIdentity {
            actor: &f.ana,
            key: &data_key,
            fingerprint: RequestFingerprint(digest("data")),
        },
        artifact: &f.created.artifact.id,
        base: AppDataVersion(0),
        change: &DataChange::Set {
            path: parsed::<DataPath>("note"),
            value: DataValue::parse("1")?,
        },
        now: UnixMillis(1),
    })?;
    f.enqueue(&f.ana, "ana-job1")?;
    assert_eq!(
        f.kinds(&f.ana)?,
        [AccessGranted, AccessGranted, DataChanged, JobChanged]
    );
    assert_eq!(f.kinds(&f.ben)?, [AccessGranted]);
    assert_eq!(f.kinds(&f.owner)?, [Created, AccessGranted, AccessGranted]);
    Ok(())
}

#[test]
fn a_revoked_members_running_job_writes_nothing_to_its_feed() -> TestResult {
    let f = Fixture::new()?;
    f.grant("grant-ana", &f.ana, Role::Editor)?;
    f.enqueue(&f.ana, "ana-job1")?;
    let claim = f.store.claim_job(JobClaim {
        worker: &parsed::<WorkerId>("worker-one"),
        now: UnixMillis(2),
        lease_ms: 1_000,
        known_profiles: &[],
        execution: None,
    })?;
    let lease = claim.claimed.ok_or("the job was not claimed")?.lease;
    // A revocation leaves a running job running (rule 15); its worker's
    // next transition is still a job change.
    f.access("revoke-ana", &f.ana, AccessChange::Revoke)?;
    f.store.release_job(&lease, UnixMillis(3))?;
    assert_eq!(
        f.kinds(&f.ana)?,
        [AccessGranted, JobChanged, JobChanged, AccessRevoked],
        "the revocation stays the revoked user's last row of the artifact"
    );
    // The job it gave back is queued again, and the next claim ends it
    // instead of running it for a user who is no longer a member.
    let again = f.store.claim_job(JobClaim {
        worker: &parsed::<WorkerId>("worker-one"),
        now: UnixMillis(4),
        lease_ms: 1_000,
        known_profiles: &[],
        execution: None,
    })?;
    assert!(again.claimed.is_none());
    assert_eq!(
        again
            .settled
            .iter()
            .map(|job| job.state)
            .collect::<Vec<_>>(),
        [JobState::Cancelled]
    );
    assert_eq!(f.kinds(&f.ana)?.last(), Some(&AccessRevoked));
    Ok(())
}

/// A version 18 file's change table checks the kinds it knew; the upgrade
/// keeps every row with its numbers, also one whose job is gone, and accepts
/// the access kinds.
#[test]
fn the_upgrade_to_schema_19_keeps_every_change_and_takes_access_rows() -> TestResult {
    let f = Fixture::new()?;
    f.commit(&f.owner, "owner-save", 1)?;
    let before = f.feed(&f.owner)?;
    let path = database_in(f.dir.path());
    rusqlite::Connection::open(&path)?.execute_batch(
        "PRAGMA foreign_keys = OFF;
        CREATE TABLE changes_v18 (
            sequence INTEGER PRIMARY KEY AUTOINCREMENT,
            actor TEXT NOT NULL,
            actor_seq INTEGER NOT NULL CHECK (actor_seq >= 1),
            artifact_id TEXT NOT NULL,
            kind TEXT NOT NULL CHECK (kind IN
                ('created', 'revised', 'metadata_updated', 'deleted', 'recovered', 'purged',
                 'data_changed', 'job_changed')),
            revision INTEGER NOT NULL,
            metadata_revision INTEGER NOT NULL,
            data_version INTEGER,
            job_id TEXT,
            committed_at INTEGER NOT NULL,
            UNIQUE (actor, actor_seq),
            FOREIGN KEY (artifact_id, revision) REFERENCES revisions (artifact_id, revision),
            FOREIGN KEY (artifact_id, metadata_revision)
                REFERENCES metadata_revisions (artifact_id, metadata_revision),
            FOREIGN KEY (job_id) REFERENCES jobs (id)
        ) STRICT;
        INSERT INTO changes_v18 SELECT sequence, actor, actor_seq, artifact_id, kind, revision,
            metadata_revision, data_version, job_id, committed_at FROM changes;
        DROP TABLE changes;
        ALTER TABLE changes_v18 RENAME TO changes;
        INSERT INTO changes (actor, actor_seq, artifact_id, kind, revision, metadata_revision,
                job_id, committed_at)
            SELECT actor, actor_seq + 1, artifact_id, 'job_changed', revision, metadata_revision,
                'job_00000000000000000000000000000000', committed_at
            FROM changes ORDER BY sequence DESC LIMIT 1;
        UPDATE actor_feeds SET head = head + 1
            WHERE actor = (SELECT actor FROM changes ORDER BY sequence DESC LIMIT 1);
        PRAGMA user_version = 18;",
    )?;
    let orphan = before.last().map_or(0, |(sequence, _)| sequence + 1);
    let upgraded = Fixture {
        store: SqliteStore::open(&path)?,
        ..f
    };
    assert_eq!(common::user_version(&path)?, 19);
    let mut kept = before;
    kept.push((orphan, JobChanged));
    assert_eq!(upgraded.feed(&upgraded.owner)?, kept);
    upgraded.grant("grant-ana", &upgraded.ana, Role::Viewer)?;
    assert_eq!(
        upgraded.kinds(&upgraded.owner)?,
        [Created, Revised, JobChanged, AccessGranted]
    );
    Ok(())
}
