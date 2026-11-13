//! Behavioral contract of the claim and the checkpoint of `run_profile` jobs in
//! the `SQLite` store (decision 0057, rules 1, 5, 6 and 7): the profiles a build
//! does not run, the attempt a claim records, the generation it pins, the order
//! of the checkpoint's reasons, and the kind gates of the job API.
//!
//! Time is a number each call is given, so every test moves it by hand.

mod common;

use std::error::Error;

use artifree_domain::SourceRevision;
use artifree_domain::identity::{ActorId, ArtifactId, StateGeneration, UnixMillis, UserLabel};
use artifree_domain::job::{
    AttemptIdentity, AttemptState, CheckpointAnswer, JobFailure, JobId, JobKind, JobState,
    ProfileName, Revocation, StopReason, VerifyReport, WorkerId,
};
use artifree_ports::artifact_store::{Delivery, RequestFingerprint, RequestIdentity};
use artifree_ports::job_store::{
    AttemptRow, Claim, ClaimedJob, ExecutionClaim, JobClaim, JobReceipt, JobRecord, JobStore,
    JobStoreError, NewJob, NewProfileJob,
};
use artifree_ports::user_store::{NewUser, UserStore};
use artifree_providers::storage::SqliteStore;
use artifree_providers::tokens::SqliteTokenStore;
use common::{TempDir, TestResult, create_demo, database_in, digest, key, parsed};
use rusqlite::Connection;

const FIRST: &str = "<h1>first</h1>";
const LEASE_MS: u64 = 1_000;

/// A tool digest of 64 lowercase hexadecimal digits.
const TOOL: &str = concat!(
    "abababab", "abababab", "abababab", "abababab", "abababab", "abababab", "abababab", "abababab"
);
/// The same digest in capitals, which is refused.
const TOOL_CAPITALS: &str = concat!(
    "ABABABAB", "ABABABAB", "ABABABAB", "ABABABAB", "ABABABAB", "ABABABAB", "ABABABAB", "ABABABAB"
);
/// One digit short of 64, which is refused.
const TOOL_SHORT: &str = concat!(
    "abababab", "abababab", "abababab", "abababab", "abababab", "abababab", "abababab", "abababa"
);
/// An execution directory identity of 32 lowercase hexadecimal digits.
const DIRECTORY: &str = concat!("0123456789abcdef", "0123456789abcdef");
/// One digit short of 32, which is refused.
const DIRECTORY_SHORT: &str = concat!("0123456789abcdef", "0123456789abcde");

fn at(ms: i64) -> UnixMillis {
    UnixMillis(ms)
}

fn worker() -> WorkerId {
    parsed("worker-profile")
}

fn profile_name(name: &str) -> ProfileName {
    parsed(name)
}

fn profiles(names: &[&str]) -> Vec<ProfileName> {
    names.iter().map(|name| profile_name(name)).collect()
}

/// The execution identities of one claim. The attempt identity is the table's
/// key, so each claim in a test takes its own `seed`.
fn execution(seed: u8) -> ExecutionClaim<'static> {
    ExecutionClaim {
        attempt: AttemptIdentity::from_random([seed; 16]),
        tool_digest: TOOL,
        machine: "machine-1",
        boot: "boot-1",
        cgroupns: "cgroup-1",
        directory: DIRECTORY,
    }
}

/// A `run_profile` job as the table holds it. `JobStore::job` does not return
/// that kind, so the tests read its row.
#[derive(Debug, PartialEq, Eq)]
struct Stored {
    state: String,
    attempts: i64,
    fence: i64,
    lease_expires_at: Option<i64>,
}

fn query_stored(connection: &Connection, job: &JobId) -> rusqlite::Result<Stored> {
    connection.query_row(
        "SELECT state, attempts, fence, lease_expires_at FROM jobs WHERE id = ?1",
        [job.as_str()],
        |row| {
            Ok(Stored {
                state: row.get(0)?,
                attempts: row.get(1)?,
                fence: row.get(2)?,
                lease_expires_at: row.get(3)?,
            })
        },
    )
}

/// A store with one artifact of `owner` at revision 1, and the helpers the
/// tests share.
struct Profiles {
    dir: TempDir,
    store: SqliteStore,
    owner: ActorId,
    artifact: ArtifactId,
}

impl Profiles {
    /// An artifact of `owner`.
    fn owned_by(owner: &ActorId) -> Result<Self, Box<dyn Error>> {
        let dir = TempDir::new();
        let store = SqliteStore::open(&database_in(dir.path()))?;
        let created = create_demo(&store, owner, &key("create"), "Profiles", FIRST)?;
        Ok(Self {
            dir,
            store,
            owner: owner.clone(),
            artifact: created.artifact.id,
        })
    }

    /// An artifact of the local owner, whose `users` row holds generation 1.
    fn new() -> Result<Self, Box<dyn Error>> {
        Self::owned_by(&ActorId::owner())
    }

    fn sql(&self) -> Result<Connection, rusqlite::Error> {
        Connection::open(database_in(self.dir.path()))
    }

    fn count(&self, query: &str) -> Result<i64, rusqlite::Error> {
        self.sql()?.query_row(query, [], |row| row.get(0))
    }

    fn stored(&self, job: &JobId) -> Result<Stored, rusqlite::Error> {
        query_stored(&self.sql()?, job)
    }

    fn generation_of(&self, user: &ActorId) -> Result<i64, rusqlite::Error> {
        self.sql()?.query_row(
            "SELECT state_generation FROM users WHERE id = ?1",
            [user.as_str()],
            |row| row.get(0),
        )
    }

    fn identity(&self) -> Result<SqliteTokenStore, Box<dyn Error>> {
        Ok(SqliteTokenStore::open(&database_in(self.dir.path()))?)
    }

    fn register(&self, user: &ActorId, label: &str) -> Result<(), Box<dyn Error>> {
        self.identity()?.register(&NewUser {
            id: user.clone(),
            label: parsed::<UserLabel>(label),
            created_at: "2026-11-03T00:00:00Z".to_owned(),
        })?;
        Ok(())
    }

    /// Enqueues a `run_profile` job of this artifact. The request key and the
    /// fingerprint come from `name`, so the same name replays.
    fn enqueue_profile(
        &self,
        who: &ActorId,
        name: &str,
        profile: &ProfileName,
        now: i64,
    ) -> Result<JobReceipt, JobStoreError> {
        let request_key = key(name);
        self.store.enqueue_profile_job(NewProfileJob {
            request: RequestIdentity {
                actor: who,
                key: &request_key,
                fingerprint: RequestFingerprint(digest(name)),
            },
            artifact: &self.artifact,
            revision: SourceRevision(1),
            profile,
            now: at(now),
        })
    }

    /// Enqueues a `verify_revision` job of this artifact, for the owner.
    fn enqueue_verify(&self, name: &str, now: i64) -> Result<JobRecord, JobStoreError> {
        let request_key = key(name);
        self.store
            .enqueue_job(NewJob {
                request: RequestIdentity {
                    actor: &self.owner,
                    key: &request_key,
                    fingerprint: RequestFingerprint(digest(name)),
                },
                artifact: &self.artifact,
                revision: SourceRevision(1),
                kind: JobKind::VerifyRevision,
                now: at(now),
            })
            .map(|receipt| receipt.job)
    }

    fn claim(
        &self,
        now: i64,
        known: &[ProfileName],
        identities: Option<ExecutionClaim<'_>>,
    ) -> Result<Claim, JobStoreError> {
        self.store.claim_job(JobClaim {
            worker: &worker(),
            now: at(now),
            lease_ms: LEASE_MS,
            known_profiles: known,
            execution: identities,
        })
    }

    /// The claim's job, for a claim that must find one.
    fn claimed(
        &self,
        now: i64,
        known: &[ProfileName],
        identities: Option<ExecutionClaim<'_>>,
    ) -> Result<ClaimedJob, Box<dyn Error>> {
        let claim = self.claim(now, known, identities)?;
        Ok(claim.claimed.ok_or("no job was due")?)
    }

    fn attempt(&self, attempt: &AttemptIdentity) -> Result<AttemptRow, Box<dyn Error>> {
        Ok(self
            .store
            .attempt_row(attempt)?
            .ok_or("the attempt row is missing")?)
    }
}

#[test]
fn an_unknown_profile_is_ended_execution_unavailable_without_an_attempt() -> TestResult {
    let builds: [(&str, Vec<ProfileName>, Option<ExecutionClaim<'static>>); 2] = [
        ("no profile is known", profiles(&[]), None),
        (
            "another profile is known",
            profiles(&["other"]),
            Some(execution(1)),
        ),
    ];
    for (build, known, identities) in builds {
        let fixture = Profiles::new()?;
        let job = fixture
            .enqueue_profile(&fixture.owner, "unknown", &profile_name("fixture"), 10)?
            .job;
        let verify = fixture.enqueue_verify("verify", 20)?;

        let claim = fixture.claim(100, &known, identities)?;
        let [ended] = claim.settled.as_slice() else {
            let count = claim.settled.len();
            return Err(format!("{build}: expected one ended job, got {count}").into());
        };
        assert_eq!(ended.id, job.id, "{build}");
        assert_eq!(ended.kind, JobKind::RunProfile, "{build}");
        assert_eq!(ended.state, JobState::Failed, "{build}");
        assert_eq!(
            ended.failure,
            Some(JobFailure::ExecutionUnavailable),
            "{build}: the failure names the unknown profile"
        );
        assert_eq!(ended.attempts, 0, "{build}: no attempt is counted");
        assert_eq!(ended.started_at, None, "{build}: it never started");
        assert_eq!(ended.finished_at, Some(at(100)), "{build}");

        let claimed = claim.claimed.ok_or("the claim went on to no job")?;
        assert_eq!(
            claimed.job.id, verify.id,
            "{build}: the claim goes on to the next due job"
        );
        assert_eq!(
            fixture.stored(&job.id)?.fence,
            0,
            "{build}: the fence is not raised"
        );
        assert_eq!(
            fixture.count("SELECT count(*) FROM execution_attempts")?,
            0,
            "{build}: no attempt row is written"
        );
    }
    Ok(())
}

#[test]
fn a_known_profile_without_execution_stays_queued_and_does_not_block_the_next_job() -> TestResult {
    let fixture = Profiles::new()?;
    let job = fixture
        .enqueue_profile(&fixture.owner, "waiting", &profile_name("fixture"), 10)?
        .job;
    let verify = fixture.enqueue_verify("verify", 20)?;
    let known = profiles(&["fixture"]);

    let first = fixture.claim(100, &known, None)?;
    assert!(
        first.settled.is_empty(),
        "nothing is ended: {:?}",
        first.settled
    );
    let claimed = first.claimed.ok_or("the verify job was not claimed")?;
    assert_eq!(
        claimed.job.id, verify.id,
        "the claim passes over the job it cannot run"
    );
    let stored = fixture.stored(&job.id)?;
    assert_eq!(stored.state, JobState::Queued.as_str());
    assert_eq!(stored.attempts, 0, "no attempt is counted");

    let second = fixture.claim(100, &known, None)?;
    assert_eq!(
        second,
        Claim::default(),
        "the claim does not loop over the job it left queued"
    );
    Ok(())
}

#[test]
fn a_known_profile_with_execution_is_claimed_with_one_live_attempt_row() -> TestResult {
    let fixture = Profiles::new()?;
    let job = fixture
        .enqueue_profile(&fixture.owner, "run", &profile_name("fixture"), 10)?
        .job;
    let identities = execution(1);

    let claimed = fixture.claimed(100, &profiles(&["fixture"]), Some(identities))?;
    assert_eq!(claimed.job.id, job.id);
    assert_eq!(claimed.job.kind, JobKind::RunProfile);
    assert_eq!(claimed.job.profile, Some(profile_name("fixture")));
    assert_eq!(claimed.job.state, JobState::Running);
    assert_eq!(claimed.job.attempts, 1);
    assert_eq!(claimed.lease.fence, 1, "the claim raises the fence to 1");

    let expected = AttemptRow {
        attempt: identities.attempt,
        job: job.id.clone(),
        fence: 1,
        actor_generation: Some(StateGeneration::FIRST),
        profile: profile_name("fixture"),
        tool_digest: TOOL.to_owned(),
        machine: "machine-1".to_owned(),
        boot: "boot-1".to_owned(),
        cgroupns: "cgroup-1".to_owned(),
        directory: DIRECTORY.to_owned(),
        state: AttemptState::Live,
        revoked: None,
        created_at: at(100),
        settled_at: None,
    };
    assert_eq!(
        fixture.store.attempt_row(&identities.attempt)?,
        Some(expected.clone()),
        "the claim records its attempt with the owner's generation and host identities"
    );
    assert_eq!(
        fixture.store.unsettled_attempts()?,
        vec![expected],
        "the one attempt is the only unsettled one"
    );
    assert_eq!(
        fixture.count("SELECT count(*) FROM execution_attempts")?,
        1,
        "one claim writes one attempt row"
    );
    Ok(())
}

#[test]
fn an_actor_without_a_users_row_pins_none_and_a_row_that_appears_is_another_generation()
-> TestResult {
    let user = ActorId::user([9; 16]);
    let fixture = Profiles::owned_by(&user)?;
    fixture.enqueue_profile(&user, "unregistered", &profile_name("fixture"), 10)?;
    let identities = execution(1);
    let claimed = fixture.claimed(100, &profiles(&["fixture"]), Some(identities))?;
    assert_eq!(
        fixture.attempt(&identities.attempt)?.actor_generation,
        None,
        "an actor with no users row pins no generation"
    );

    let answer =
        fixture
            .store
            .checkpoint_attempt(&claimed.lease, &identities.attempt, at(200), LEASE_MS)?;
    assert_eq!(
        answer,
        CheckpointAnswer::Continue,
        "no users row is still no change"
    );

    fixture.register(&user, "User 9")?;
    let answer =
        fixture
            .store
            .checkpoint_attempt(&claimed.lease, &identities.attempt, at(300), LEASE_MS)?;
    assert_eq!(
        answer,
        CheckpointAnswer::Stop(StopReason::Generation),
        "a users row that appears after the claim is another generation"
    );
    assert_eq!(
        fixture.attempt(&identities.attempt)?.revoked,
        Some(Revocation::Generation),
        "the generation reason is stored as the attempt's revocation"
    );
    Ok(())
}

#[test]
fn a_disabled_users_job_waits_and_a_never_started_job_claims_under_the_new_generation() -> TestResult
{
    let user = ActorId::user([8; 16]);
    let fixture = Profiles::owned_by(&user)?;
    fixture.register(&user, "User 8")?;
    let job = fixture
        .enqueue_profile(&user, "waiting", &profile_name("fixture"), 10)?
        .job;
    fixture
        .identity()?
        .set_disabled(&user, Some("2026-11-04T00:00:00Z"))?;
    let known = profiles(&["fixture"]);

    let waiting = fixture.claim(100, &known, Some(execution(1)))?;
    assert_eq!(
        waiting.claimed, None,
        "a disabled user's job is not claimed"
    );
    let stored = fixture.stored(&job.id)?;
    assert_eq!(stored.state, JobState::Queued.as_str());
    assert_eq!(stored.attempts, 0, "the wait uses no attempt");

    fixture.identity()?.set_disabled(&user, None)?;
    let identities = execution(2);
    let claimed = fixture.claimed(200, &known, Some(identities))?;
    assert_eq!(claimed.job.id, job.id);
    let generation = u64::try_from(fixture.generation_of(&user)?)?;
    assert_eq!(
        generation, 3,
        "registration, disable and enable each raise the generation"
    );
    assert_eq!(
        fixture.attempt(&identities.attempt)?.actor_generation,
        StateGeneration::new(generation),
        "the claim pins the generation read in its own transaction"
    );

    let answer =
        fixture
            .store
            .checkpoint_attempt(&claimed.lease, &identities.attempt, at(300), LEASE_MS)?;
    assert_eq!(answer, CheckpointAnswer::Continue);
    Ok(())
}

#[test]
fn a_checkpoint_continue_renews_the_lease() -> TestResult {
    let fixture = Profiles::new()?;
    let job = fixture
        .enqueue_profile(&fixture.owner, "renew", &profile_name("fixture"), 10)?
        .job;
    let identities = execution(1);
    let claimed = fixture.claimed(100, &profiles(&["fixture"]), Some(identities))?;
    assert_eq!(
        fixture.stored(&job.id)?.lease_expires_at,
        Some(1_100),
        "the claim leases 1000 ms from its time"
    );

    let answer =
        fixture
            .store
            .checkpoint_attempt(&claimed.lease, &identities.attempt, at(600), LEASE_MS)?;
    assert_eq!(answer, CheckpointAnswer::Continue);
    assert_eq!(
        fixture.stored(&job.id)?.lease_expires_at,
        Some(1_600),
        "Continue renews the lease to now plus lease_ms"
    );
    Ok(())
}

#[test]
fn the_stored_reason_is_answered_before_the_current_state() -> TestResult {
    let fixture = Profiles::new()?;
    let job = fixture
        .enqueue_profile(&fixture.owner, "stored", &profile_name("fixture"), 10)?
        .job;
    let identities = execution(1);
    let claimed = fixture.claimed(100, &profiles(&["fixture"]), Some(identities))?;
    let before = fixture.stored(&job.id)?.lease_expires_at;
    assert_eq!(before, Some(1_100));

    let connection = fixture.sql()?;
    connection.execute("UPDATE execution_attempts SET revoked = 'authority'", [])?;
    connection.execute(
        "UPDATE jobs SET cancel_requested = 1 WHERE kind = 'run_profile'",
        [],
    )?;

    let answer =
        fixture
            .store
            .checkpoint_attempt(&claimed.lease, &identities.attempt, at(600), LEASE_MS)?;
    assert_eq!(
        answer,
        CheckpointAnswer::Stop(StopReason::Authority),
        "the stored revocation is answered, not the current cancel request"
    );
    assert_eq!(
        fixture.stored(&job.id)?.lease_expires_at,
        before,
        "a stopped checkpoint renews no lease"
    );
    Ok(())
}

/// A change made to a claimed attempt before its checkpoint.
enum Change {
    /// A statement over this fixture's one job, artifact, user, or attempt.
    Sql(&'static str),
    /// The attempt is settled through the store, with no revocation.
    Settle,
}

#[test]
fn the_checkpoint_answers_current_state_in_rule_7_order_and_stores_it() -> TestResult {
    let rows: [(&str, Change, CheckpointAnswer, Option<Revocation>); 7] = [
        (
            "cancel requested",
            Change::Sql(
                "UPDATE jobs SET cancel_requested = 1, fence = fence + 1
                 WHERE kind = 'run_profile'",
            ),
            CheckpointAnswer::Stop(StopReason::Cancel),
            Some(Revocation::Cancel),
        ),
        (
            "artifact owner moved",
            Change::Sql("UPDATE artifacts SET owner = 'u-07070707070707070707070707070707'"),
            CheckpointAnswer::Stop(StopReason::Authority),
            Some(Revocation::Authority),
        ),
        (
            "actor generation moved",
            Change::Sql(
                "UPDATE users SET state_generation = state_generation + 1 WHERE id = 'local'",
            ),
            CheckpointAnswer::Stop(StopReason::Generation),
            Some(Revocation::Generation),
        ),
        (
            "job fence raised",
            Change::Sql("UPDATE jobs SET fence = fence + 1 WHERE kind = 'run_profile'"),
            CheckpointAnswer::Stop(StopReason::Fence),
            None,
        ),
        (
            "shutdown stored",
            Change::Sql("UPDATE execution_attempts SET revoked = 'shutdown'"),
            CheckpointAnswer::Stop(StopReason::Lease),
            Some(Revocation::Shutdown),
        ),
        (
            "lease stored",
            Change::Sql("UPDATE execution_attempts SET revoked = 'lease'"),
            CheckpointAnswer::Stop(StopReason::Lease),
            Some(Revocation::Lease),
        ),
        (
            "attempt settled and not revoked",
            Change::Settle,
            CheckpointAnswer::Stop(StopReason::Fence),
            None,
        ),
    ];
    for (name, change, answer, stored_revocation) in rows {
        let fixture = Profiles::new()?;
        fixture.enqueue_profile(&fixture.owner, "row", &profile_name("fixture"), 10)?;
        let identities = execution(1);
        let claimed = fixture.claimed(100, &profiles(&["fixture"]), Some(identities))?;
        match change {
            Change::Sql(sql) => {
                fixture.sql()?.execute(sql, [])?;
            }
            Change::Settle => {
                fixture.store.settle_attempt(&identities.attempt, at(150))?;
            }
        }

        let got = fixture.store.checkpoint_attempt(
            &claimed.lease,
            &identities.attempt,
            at(200),
            LEASE_MS,
        )?;
        assert_eq!(
            got, answer,
            "{name}: the first reason of rule 7 that applies is the answer"
        );
        assert_eq!(
            fixture.attempt(&identities.attempt)?.revoked,
            stored_revocation,
            "{name}: the stored revocation"
        );
    }
    Ok(())
}

#[test]
fn the_checkpoint_of_an_attempt_of_another_job_is_not_found() -> TestResult {
    let fixture = Profiles::new()?;
    fixture.enqueue_profile(&fixture.owner, "first", &profile_name("fixture"), 10)?;
    fixture.enqueue_profile(&fixture.owner, "second", &profile_name("fixture"), 20)?;
    let known = profiles(&["fixture"]);
    let first = fixture.claimed(100, &known, Some(execution(1)))?;
    let second = fixture.claimed(100, &known, Some(execution(2)))?;
    assert_ne!(first.job.id, second.job.id, "two jobs were claimed");

    let store = &fixture.store;
    assert_eq!(
        store.checkpoint_attempt(&first.lease, &execution(2).attempt, at(200), LEASE_MS),
        Err(JobStoreError::NotFound),
        "an attempt of another job is not found through this lease"
    );
    assert_eq!(
        store.checkpoint_attempt(&first.lease, &execution(9).attempt, at(200), LEASE_MS),
        Err(JobStoreError::NotFound),
        "an unknown attempt is not found"
    );
    assert_eq!(
        store.checkpoint_attempt(&second.lease, &execution(2).attempt, at(200), LEASE_MS),
        Ok(CheckpointAnswer::Continue),
        "the job's own attempt is answered"
    );
    Ok(())
}

#[test]
fn the_verify_methods_refuse_a_run_profile_lease_and_the_profile_methods_refuse_a_verify_lease()
-> TestResult {
    let fixture = Profiles::new()?;
    let job = fixture
        .enqueue_profile(&fixture.owner, "profile", &profile_name("fixture"), 10)?
        .job;
    fixture.enqueue_verify("verify", 20)?;
    let known = profiles(&["fixture"]);
    let run = fixture.claimed(100, &known, Some(execution(1)))?;
    let verify = fixture.claimed(100, &known, Some(execution(2)))?;
    assert_eq!(verify.job.kind, JobKind::VerifyRevision);

    let store = &fixture.store;
    let report = VerifyReport {
        files_checked: 0,
        bytes_checked: 0,
        mismatch_count: 0,
        findings: Vec::new(),
    };
    assert!(
        matches!(
            store.heartbeat_job(&run.lease, at(200), LEASE_MS),
            Err(JobStoreError::InvalidRequest(_))
        ),
        "heartbeat_job refuses a run_profile lease"
    );
    assert!(
        matches!(
            store.complete_job(&run.lease, &report, at(200)),
            Err(JobStoreError::InvalidRequest(_))
        ),
        "complete_job refuses a run_profile lease"
    );
    assert_eq!(
        fixture.stored(&job.id)?.state,
        JobState::Running.as_str(),
        "the refused calls changed nothing"
    );
    assert_eq!(
        store.checkpoint_attempt(&verify.lease, &execution(1).attempt, at(200), LEASE_MS),
        Err(JobStoreError::NotFound),
        "a verify_revision lease reaches no attempt"
    );
    Ok(())
}

#[test]
fn a_malformed_execution_claim_is_refused_and_changes_nothing() -> TestResult {
    let fixture = Profiles::new()?;
    let job = fixture
        .enqueue_profile(&fixture.owner, "malformed", &profile_name("fixture"), 10)?
        .job;
    let known = profiles(&["fixture"]);
    let refused = [
        (
            "tool digest in capitals",
            ExecutionClaim {
                tool_digest: TOOL_CAPITALS,
                ..execution(1)
            },
        ),
        (
            "tool digest of 63 characters",
            ExecutionClaim {
                tool_digest: TOOL_SHORT,
                ..execution(1)
            },
        ),
        (
            "directory of 31 characters",
            ExecutionClaim {
                directory: DIRECTORY_SHORT,
                ..execution(1)
            },
        ),
        (
            "empty machine",
            ExecutionClaim {
                machine: "",
                ..execution(1)
            },
        ),
    ];
    for (case, identities) in refused {
        let result = fixture.claim(100, &known, Some(identities));
        assert!(
            matches!(result, Err(JobStoreError::InvalidRequest(_))),
            "{case}: the claim is refused, got {result:?}"
        );
    }

    assert_eq!(
        fixture.count("SELECT count(*) FROM execution_attempts")?,
        0,
        "no refused claim wrote an attempt row"
    );
    let stored = fixture.stored(&job.id)?;
    assert_eq!(stored.state, JobState::Queued.as_str());
    assert_eq!(stored.attempts, 0, "no refused claim counted an attempt");
    assert_eq!(stored.fence, 0, "no refused claim raised the fence");

    let claimed = fixture.claimed(100, &known, Some(execution(3)))?;
    assert_eq!(
        claimed.lease.fence, 1,
        "the first valid claim is the first claim"
    );
    Ok(())
}

#[test]
fn enqueue_of_run_profile_pins_the_profile_and_replays() -> TestResult {
    let fixture = Profiles::new()?;
    let name = profile_name("fixture");

    let first = fixture.enqueue_profile(&fixture.owner, "profile-enqueue", &name, 10)?;
    assert_eq!(first.delivery, Delivery::Committed);
    assert_eq!(first.job.kind, JobKind::RunProfile);
    assert_eq!(first.job.profile, Some(name.clone()));

    let replay = fixture.enqueue_profile(&fixture.owner, "profile-enqueue", &name, 20)?;
    assert_eq!(
        replay.delivery,
        Delivery::Replayed,
        "the same key and fingerprint replay"
    );
    assert_eq!(replay.job.id, first.job.id, "the replay is the same job");
    assert_eq!(replay.job.profile, Some(name.clone()));

    let before = fixture.count("SELECT count(*) FROM jobs")?;
    let refused = fixture.store.enqueue_job(NewJob {
        request: RequestIdentity {
            actor: &fixture.owner,
            key: &key("refused-run"),
            fingerprint: RequestFingerprint(digest("refused")),
        },
        artifact: &fixture.artifact,
        revision: SourceRevision(1),
        kind: JobKind::RunProfile,
        now: at(30),
    });
    assert!(
        matches!(refused, Err(JobStoreError::InvalidRequest(_))),
        "enqueue_job refuses a run_profile job, got {refused:?}"
    );
    assert_eq!(
        fixture.count("SELECT count(*) FROM jobs")?,
        before,
        "the refusal writes no job"
    );

    let key_is_free = fixture.enqueue_verify("refused-run", 40)?;
    assert_eq!(
        key_is_free.kind,
        JobKind::VerifyRevision,
        "the refused request recorded no key, so the key is free"
    );
    Ok(())
}
