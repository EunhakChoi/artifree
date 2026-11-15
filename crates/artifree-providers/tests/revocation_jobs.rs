//! What a revocation and a downgrade take from a member's jobs and replays
//! (decision 0060 rules 11, 15 and 17, slice S02d1): the queued jobs of a user
//! who may no longer enqueue on the artifact end cancelled in the same
//! transaction, a running one ends normally; the owner cancels a member's
//! job; and a member's stored commit replays only while its role allows one.

mod common;

use std::collections::BTreeMap;

use artifree_domain::SourceRevision;
use artifree_domain::access::{AccessVersion, Role};
use artifree_domain::change::ChangeSequence;
use artifree_domain::identity::{ActorId, ArtifactId, UnixMillis, UserLabel};
use artifree_domain::job::{JobFailure, JobId, JobKind, JobState, WorkerId};
use artifree_ports::access_store::{AccessChange, AccessRequest, AccessStore};
use artifree_ports::artifact_store::{
    ArtifactStore, ChangeKind, CommitReceipt, RequestFingerprint, RequestIdentity, RevisionCommit,
    StoreError,
};
use artifree_ports::job_store::{
    Claim, JobCancel, JobClaim, JobStore, JobStoreError, NewJob, Retry,
};
use artifree_ports::user_store::{NewUser, UserStore};
use artifree_providers::storage::SqliteStore;
use artifree_providers::tokens::SqliteTokenStore;
use common::{TempDir, TestResult, actor, create_demo, database_in, digest, key, package, parsed};

struct Fixture {
    _dir: TempDir,
    store: SqliteStore,
    owner: ActorId,
    shared: CommitReceipt,
    other: CommitReceipt,
    ana: ActorId,
    ben: ActorId,
}

impl Fixture {
    /// Two artifacts of one owner; Ana and Ben edit both.
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let dir = TempDir::new();
        let path = database_in(dir.path());
        let store = SqliteStore::open(&path)?;
        let identity = SqliteTokenStore::open(&path)?;
        let owner = actor("local");
        let shared = create_demo(&store, &owner, &key("create-shared"), "Shared", "v1")?;
        let other = create_demo(&store, &owner, &key("create-other"), "Other", "v1")?;
        let ana = ActorId::user([1; 16]);
        let ben = ActorId::user([2; 16]);
        for (id, label) in [(&ana, "Ana"), (&ben, "Ben")] {
            identity.register(&NewUser {
                id: id.clone(),
                label: parsed::<UserLabel>(label),
                created_at: "2026-11-16T00:00:00Z".to_owned(),
            })?;
        }
        let fixture = Self {
            _dir: dir,
            store,
            owner,
            shared,
            other,
            ana,
            ben,
        };
        for artifact in [&fixture.shared, &fixture.other] {
            for user in [&fixture.ana, &fixture.ben] {
                fixture.access(
                    &artifact.artifact.id,
                    user,
                    AccessChange::Grant(Role::Editor),
                )?;
            }
        }
        Ok(fixture)
    }

    fn access(&self, artifact: &ArtifactId, user: &ActorId, change: AccessChange) -> TestResult {
        let base = self
            .store
            .access_list(artifact)?
            .ok_or("the artifact has no access list")?
            .version;
        let name = format!("access-{artifact}-{user}-{}", base.0);
        self.store.change_access(AccessRequest {
            request: RequestIdentity {
                actor: &self.owner,
                key: &key(&name),
                fingerprint: RequestFingerprint(digest(&name)),
            },
            artifact,
            user,
            change,
            base: AccessVersion(base.0),
            now: UnixMillis(5),
        })?;
        Ok(())
    }

    fn enqueue(
        &self,
        by: &ActorId,
        artifact: &ArtifactId,
        name: &str,
    ) -> Result<JobId, Box<dyn std::error::Error>> {
        Ok(self
            .store
            .enqueue_job(NewJob {
                request: RequestIdentity {
                    actor: by,
                    key: &key(name),
                    fingerprint: RequestFingerprint(digest(name)),
                },
                artifact,
                revision: SourceRevision(1),
                kind: JobKind::VerifyRevision,
                now: UnixMillis(1),
            })?
            .job
            .id)
    }

    fn claim(&self, now: i64) -> Result<Claim, JobStoreError> {
        self.store.claim_job(JobClaim {
            worker: &parsed::<WorkerId>("worker-one"),
            now: UnixMillis(now),
            lease_ms: 1_000,
            known_profiles: &[],
            execution: None,
        })
    }

    fn state(&self, job: &JobId) -> Result<JobState, Box<dyn std::error::Error>> {
        Ok(self.store.job(job)?.ok_or("the job is gone")?.state)
    }

    fn cancel(&self, by: &ActorId, job: &JobId, name: &str) -> Result<JobState, JobStoreError> {
        self.store
            .cancel_job(JobCancel {
                request: RequestIdentity {
                    actor: by,
                    key: &key(name),
                    fingerprint: RequestFingerprint(digest(name)),
                },
                job,
                now: UnixMillis(3),
            })
            .map(|receipt| receipt.job.state)
    }

    fn kinds(&self, user: &ActorId) -> Result<Vec<ChangeKind>, Box<dyn std::error::Error>> {
        Ok(self
            .store
            .changes(user, ChangeSequence(0), 100)?
            .changes
            .into_iter()
            .map(|change| change.kind)
            .collect())
    }
}

#[test]
fn a_revocation_cancels_the_users_queued_jobs_on_that_artifact_only() -> TestResult {
    let f = Fixture::new()?;
    let id = &f.shared.artifact.id;
    let running = f.enqueue(&f.ana, id, "ana-running")?;
    let claim = f.store.claim_job(JobClaim {
        worker: &parsed::<WorkerId>("worker-one"),
        now: UnixMillis(2),
        lease_ms: 1_000,
        known_profiles: &[],
        execution: None,
    })?;
    assert_eq!(
        claim.claimed.map(|claimed| claimed.job.id),
        Some(running.clone())
    );
    let queued = f.enqueue(&f.ana, id, "ana-queued")?;
    let elsewhere = f.enqueue(&f.ana, &f.other.artifact.id, "ana-elsewhere")?;
    let bens = f.enqueue(&f.ben, id, "ben-queued")?;
    f.access(id, &f.ana, AccessChange::Revoke)?;
    assert_eq!(f.state(&queued)?, JobState::Cancelled);
    assert_eq!(
        f.state(&running)?,
        JobState::Running,
        "a running job ends normally"
    );
    assert_eq!(
        f.state(&elsewhere)?,
        JobState::Queued,
        "another artifact's job stays"
    );
    assert_eq!(
        f.state(&bens)?,
        JobState::Queued,
        "another member's job stays"
    );
    assert_eq!(
        f.kinds(&f.ana)?.last(),
        Some(&ChangeKind::AccessRevoked),
        "the cancel writes no row to the revoked user's feed"
    );
    Ok(())
}

#[test]
fn a_downgrade_to_viewer_cancels_the_queued_jobs_and_says_so() -> TestResult {
    let f = Fixture::new()?;
    let id = &f.shared.artifact.id;
    let queued = f.enqueue(&f.ana, id, "ana-queued")?;
    f.access(id, &f.ana, AccessChange::Grant(Role::Viewer))?;
    assert_eq!(f.state(&queued)?, JobState::Cancelled);
    assert_eq!(
        f.kinds(&f.ana)?.last(),
        Some(&ChangeKind::JobChanged),
        "a viewer is still a member, so the cancel reaches its feed"
    );
    // Back to editor: nothing is taken, and a new job stays queued.
    f.access(id, &f.ana, AccessChange::Grant(Role::Editor))?;
    let again = f.enqueue(&f.ana, id, "ana-again")?;
    f.access(id, &f.ben, AccessChange::Grant(Role::Viewer))?;
    assert_eq!(
        f.state(&again)?,
        JobState::Queued,
        "another member's downgrade takes nothing"
    );
    Ok(())
}

#[test]
fn the_owner_cancels_a_members_job_and_another_member_cannot() -> TestResult {
    let f = Fixture::new()?;
    let id = &f.shared.artifact.id;
    let anas = f.enqueue(&f.ana, id, "ana-queued")?;
    assert!(
        matches!(
            f.cancel(&f.ben, &anas, "ben-cancel"),
            Err(JobStoreError::NotFound)
        ),
        "an editor does not cancel another member's job"
    );
    assert_eq!(f.state(&anas)?, JobState::Queued);
    assert_eq!(
        f.cancel(&f.owner, &anas, "owner-cancel")?,
        JobState::Cancelled
    );
    assert_eq!(
        f.kinds(&f.ana)?.last(),
        Some(&ChangeKind::JobChanged),
        "the cancel is a change of the member's job, in the member's feed"
    );
    // The owner's own key replays.
    assert_eq!(
        f.cancel(&f.owner, &anas, "owner-cancel")?,
        JobState::Cancelled
    );
    Ok(())
}

#[test]
fn a_members_commit_replays_only_while_its_role_allows_a_commit() -> TestResult {
    let f = Fixture::new()?;
    let id = &f.shared.artifact.id;
    let save = key("ana-save");
    let identity = RequestIdentity {
        actor: &f.ana,
        key: &save,
        fingerprint: RequestFingerprint(digest("ana-save")),
    };
    f.store.commit_revision(RevisionCommit {
        request: identity,
        artifact: id,
        base: SourceRevision(1),
        package: &package(&[("index.html", "v2"), ("style.css", "h1{}")]),
    })?;
    assert!(
        f.store.replay_receipt(identity)?.is_some(),
        "an editor's replay"
    );
    f.access(id, &f.ana, AccessChange::Grant(Role::Viewer))?;
    assert!(matches!(
        f.store.replay_receipt(identity),
        Err(StoreError::Forbidden)
    ));
    f.access(id, &f.ana, AccessChange::Revoke)?;
    assert!(matches!(
        f.store.replay_receipt(identity),
        Err(StoreError::NotFound)
    ));
    Ok(())
}

/// Whatever puts a job back in line after the access change (a retry, an
/// expired lease) meets the claim, which starts no attempt for a user who
/// may no longer enqueue on the artifact.
#[test]
fn no_new_attempt_starts_for_a_user_who_may_no_longer_enqueue() -> TestResult {
    let f = Fixture::new()?;
    let id = &f.shared.artifact.id;
    let retried = f.enqueue(&f.ana, id, "ana-retried")?;
    let lease = f.claim(2)?.claimed.ok_or("nothing was claimed")?.lease;
    let expired = f.enqueue(&f.ben, id, "ben-expired")?;
    assert!(f.claim(3)?.claimed.is_some(), "Ben's job runs");
    f.access(id, &f.ana, AccessChange::Revoke)?;
    f.access(id, &f.ben, AccessChange::Grant(Role::Viewer))?;
    let scheduled = f.store.fail_job(
        &lease,
        JobFailure::StorageUnavailable,
        Retry::At(UnixMillis(4)),
        UnixMillis(4),
    )?;
    assert_eq!(scheduled.state, JobState::RetryScheduled);
    // Past both leases: Ana's retry and Ben's expired run are due again.
    let claim = f.claim(5_000)?;
    assert!(claim.claimed.is_none(), "neither runs again");
    let ended: BTreeMap<JobId, JobState> = claim
        .settled
        .into_iter()
        .map(|job| (job.id, job.state))
        .collect();
    assert_eq!(
        ended,
        BTreeMap::from([
            (retried, JobState::Cancelled),
            (expired, JobState::Cancelled)
        ])
    );
    // The control: the owner's job is still taken.
    let owners = f.enqueue(&f.owner, id, "owner-queued")?;
    assert_eq!(
        f.claim(5_001)?.claimed.map(|claimed| claimed.job.id),
        Some(owners)
    );
    Ok(())
}
