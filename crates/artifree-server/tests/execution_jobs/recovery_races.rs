//! The races of a purge and of a trash with its recovery (decision 0057 rules 8 and 11, decision
//! 0042 rule 8), against the real server binary and a real database.
//!
//! The purge case races a descendant that ignores SIGTERM. The purge returns without waiting, the
//! artifact becomes unreadable, and the cleanup tombstone survives a disable, a restore and the
//! forced death of the worker. One fresh attempt runs only after the tombstone settles. The trash
//! case trashes and recovers an artifact at once, at the claim, at a heartbeat and at the
//! publication. The old attempt never renews or publishes, and the recovered artifact runs a
//! fresh keyed job.
//!
//! Both cases are ignored and run only in the Linux enforcement job. `scripts/enforcement/run.sh`
//! starts each one with its payload scenario, and the trash cases with their `ARTIFREE_TRASH_AT`.

use std::error::Error;
use std::fs;
use std::path::Path;
use std::str::FromStr;
use std::time::{Duration, Instant};

use artifree_core::artifacts::ArtifactError;
use artifree_core::jobs::{Clock, SystemClock};
use artifree_domain::MetadataRevision;
use artifree_domain::SourceRevision;
use artifree_domain::identity::{ArtifactId, RequestKey, Title, UnixMillis};
use artifree_domain::job::{AttemptState, JobId, JobState, ProfileName, Revocation};
use artifree_domain::package::{FilePackage, PackagePath};

use super::recovery_support::{
    attempt_rows, clear_pause, pause_launch, restored_copy, set_disabled, set_trashed, wait_marker,
    wait_no_payload,
};
use super::{
    DEADLINE, LEASE_3, Scratch, Store, TestResult, Worker, cancel_and_settle, owner, parsed, within,
};

/// The environment variable that names the trash point of one trash case, as `run.sh` sets it.
const TRASH_AT: &str = "ARTIFREE_TRASH_AT";
/// The file in the case's root whose creation lets the claim case's worker read the job's input.
const HOLD_FILE: &str = "hold";
/// How long a purge may take to return. A purge does not wait for the payload's cleanup, so a
/// longer return means that it waited.
const PURGE_RETURN: Duration = Duration::from_secs(5);
/// How long the publication case holds the collected output, past the trash and the recovery.
const COLLECT_STALL: Duration = Duration::from_secs(4);

/// The point of an attempt's life at which its artifact is trashed and recovered at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrashPoint {
    /// After the claim, while the worker holds before it reads the job's input.
    Claim,
    /// While the payload runs, so that the next renewal must find the revocation.
    Heartbeat,
    /// While the output is collected, before its publication.
    Publication,
}

impl TrashPoint {
    /// The name of the point: the value that `run.sh` gives `ARTIFREE_TRASH_AT`, and the part of
    /// the case's directory and request keys that it names.
    fn name(self) -> &'static str {
        match self {
            Self::Claim => "claim",
            Self::Heartbeat => "heartbeat",
            Self::Publication => "publication",
        }
    }
}

impl FromStr for TrashPoint {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        [Self::Claim, Self::Heartbeat, Self::Publication]
            .into_iter()
            .find(|point| point.name() == text)
            .ok_or_else(|| format!("{TRASH_AT} names no trash point: {text}"))
    }
}

// --- fixtures ----------------------------------------------------------------

/// The metadata revision that the store holds for `artifact` now.
fn metadata_revision(
    store: &Store,
    artifact: &ArtifactId,
) -> Result<MetadataRevision, Box<dyn Error>> {
    let current: i64 = store.sql()?.query_row(
        "SELECT metadata_revision FROM artifacts WHERE id = ?1",
        [artifact.as_str()],
        |row| row.get(0),
    )?;
    Ok(MetadataRevision(u64::try_from(current)?))
}

/// Purges `artifact` under the request key `key`, at the base revision that the store holds now.
fn purge(store: &Store, artifact: &ArtifactId, key: &str) -> TestResult {
    let base = metadata_revision(store, artifact)?;
    store
        .service()?
        .purge(&owner()?, &parsed::<RequestKey>(key)?, artifact, base)?;
    Ok(())
}

/// Creates a second artifact of the case's user, with one page, and returns its id. A store with
/// two artifacts has no single artifact to read, so the cases name each artifact they use.
fn second_artifact(store: &Store) -> Result<ArtifactId, Box<dyn Error>> {
    let package = FilePackage::new(vec![(
        parsed::<PackagePath>("index.html")?,
        b"<h1>second</h1>".to_vec(),
    )])?;
    let created = store.service()?.create(
        &owner()?,
        &parsed::<RequestKey>("create-second")?,
        &parsed::<Title>("Second")?,
        &package,
    )?;
    Ok(created.artifact.id)
}

/// Enqueues one fixture job of `artifact` at its first revision, under the request key
/// `fixture-<name>`. It is `Store::enqueue_fixture` with the artifact named.
fn enqueue_on(store: &Store, artifact: &ArtifactId, name: &str) -> Result<JobId, Box<dyn Error>> {
    let receipt = store.service()?.enqueue_profile_job(
        &owner()?,
        &parsed::<RequestKey>(&format!("fixture-{name}"))?,
        artifact,
        SourceRevision(1),
        &parsed::<ProfileName>("fixture")?,
        SystemClock.now(),
    )?;
    Ok(receipt.job.id)
}

/// Checks that the one attempt of `job` is an unsettled tombstone: `cleanup_pending`, revoked as
/// `trash`, and without a settlement time. The purge keeps the trash reason that the trash stored
/// first, since a purge is possible only in the trash (decision 0057 rule 11).
fn tombstone_holds(store: &Store, job: &JobId, when: &str) -> TestResult {
    let rows = attempt_rows(store, job)?;
    let [row] = rows.as_slice() else {
        return Err(format!(
            "{when}: expected one attempt of {}, found {}",
            job.as_str(),
            rows.len()
        )
        .into());
    };
    assert_eq!(
        row.state,
        AttemptState::CleanupPending,
        "{when}: the tombstone must stay cleanup pending"
    );
    assert_eq!(
        row.revoked,
        Some(Revocation::Trash),
        "{when}: the tombstone keeps the trash reason"
    );
    assert!(
        row.settled_at.is_none(),
        "{when}: an unsettled tombstone has no settlement time"
    );
    Ok(())
}

/// Checks the settled tombstone of `job` after the restart. Its one attempt is settled with the
/// trash reason, its group is gone, and the job ends no earlier than the settlement. Returns the
/// settlement time.
fn assert_settled_tombstone(
    store: &Store,
    job: &JobId,
    group: &Path,
) -> Result<UnixMillis, Box<dyn Error>> {
    let rows = attempt_rows(store, job)?;
    let [row] = rows.as_slice() else {
        return Err(format!(
            "expected one attempt of {}, found {}",
            job.as_str(),
            rows.len()
        )
        .into());
    };
    assert_eq!(
        row.state,
        AttemptState::Settled,
        "the tombstone must settle once its group is empty"
    );
    assert_eq!(
        row.revoked,
        Some(Revocation::Trash),
        "the settled tombstone keeps its trash reason"
    );
    assert!(
        !group.exists(),
        "the purged attempt's group {} must be gone once it settles",
        group.display()
    );
    let settled = row
        .settled_at
        .ok_or("the settled tombstone has no settlement time")?;
    let finished = store
        .job_row(job)?
        .finished_at
        .ok_or("the cancelled job has no end time")?;
    assert!(
        finished >= settled.0,
        "the job ended at {finished}, before its attempt settled at {}",
        settled.0
    );
    Ok(settled)
}

// --- the trash cases ---------------------------------------------------------

/// Starts the worker of the old attempt for `point`: with a hold file after the claim, with the
/// three-second lease for the heartbeat, and with a stall at the collection for the publication.
/// The publication's pause is written before the spawn, since the worker reads it at its launch.
fn start_old_attempt(scratch: &Scratch, point: TrashPoint) -> Result<Worker, Box<dyn Error>> {
    match point {
        TrashPoint::Claim => {
            let hold = scratch.path(HOLD_FILE);
            let text = hold.to_str().ok_or("the hold file's path is not UTF-8")?;
            Worker::spawn(scratch, &["--hold-file", text])
        }
        TrashPoint::Heartbeat => Worker::spawn(scratch, &LEASE_3),
        TrashPoint::Publication => {
            let line = format!("collecting stall {}", COLLECT_STALL.as_millis());
            pause_launch(&scratch.execution(), &line)?;
            Worker::spawn(scratch, &[])
        }
    }
}

/// Waits until the old attempt has reached `point`: the claim's line for the claim, a released
/// payload for the heartbeat, and the collection marker for the publication.
fn reach_point(
    scratch: &Scratch,
    store: &Store,
    worker: &mut Worker,
    job: &JobId,
    point: TrashPoint,
) -> TestResult {
    match point {
        TrashPoint::Claim => worker.wait_event("the claimed line of the job", |event| {
            event["event"] == "claimed" && event["job"] == job.as_str()
        }),
        TrashPoint::Heartbeat => {
            worker.released_attempt(store)?;
            Ok(())
        }
        TrashPoint::Publication => wait_marker(&scratch.execution(), "collecting"),
    }
}

/// Trashes `artifact` and recovers it at once, as two requests with their own keys. The old
/// attempt then sees a trash that is already undone, which must still revoke it (decision 0057
/// rule 8).
fn trash_and_recover(store: &Store, artifact: &ArtifactId, point: TrashPoint) -> TestResult {
    set_trashed(store, artifact, true, &format!("trash-{}", point.name()))?;
    set_trashed(store, artifact, false, &format!("recover-{}", point.name()))
}

/// Lets the old attempt go on after the recovery, and waits for what the point makes the attempt
/// do. The claim's worker reads its input once the hold file exists. The heartbeat's worker stops
/// at its next renewal. The publication's worker goes on once its pause is cleared.
fn resume_old_attempt(
    scratch: &Scratch,
    worker: &mut Worker,
    job: &JobId,
    point: TrashPoint,
) -> TestResult {
    match point {
        TrashPoint::Claim => {
            fs::write(scratch.path(HOLD_FILE), b"")?;
            Ok(())
        }
        TrashPoint::Heartbeat => worker.wait_stderr(
            &format!("attempt of job {}: stopped-trash", job.as_str()),
            within(DEADLINE),
        ),
        TrashPoint::Publication => clear_pause(&scratch.execution()),
    }
}

/// Checks what the old attempt left: the job publishes no counts, its one attempt is settled and
/// revoked as `trash`, and stdout has no `succeeded` line for the job.
fn assert_old_attempt_never_publishes(
    store: &Store,
    worker: &mut Worker,
    job: &JobId,
) -> TestResult {
    worker.wait_event("the cancelled line of the job", |event| {
        event["event"] == "job"
            && event["job"] == job.as_str()
            && event["state"] == JobState::Cancelled.as_str()
    })?;
    let row = store.job_row(job)?;
    assert_eq!(
        (row.files, row.bytes),
        (None, None),
        "a trashed attempt publishes no counts"
    );
    let attempt = store.attempt_of(job)?;
    assert_eq!(
        attempt.state,
        AttemptState::Settled,
        "the old attempt must settle"
    );
    assert_eq!(
        attempt.revoked,
        Some(Revocation::Trash),
        "the old attempt must be revoked as trash"
    );
    worker.pump();
    let published = worker.events()?.iter().any(|event| {
        event["job"] == job.as_str() && event["state"] == JobState::Succeeded.as_str()
    });
    assert!(
        !published,
        "a trashed attempt of {} must not print a succeeded line",
        job.as_str()
    );
    Ok(())
}

/// Runs a fresh keyed job on the recovered artifact once the old attempt has settled. The claim
/// and the publication cases see it succeed. The heartbeat case's payload never ends, so that
/// case cancels the job instead.
fn run_fresh_job(store: &Store, worker: &mut Worker, point: TrashPoint) -> TestResult {
    let fresh = store.enqueue_fixture(&format!("fresh-{}", point.name()))?;
    match point {
        TrashPoint::Heartbeat => {
            let group = worker.released_attempt(store)?;
            cancel_and_settle(store, worker, &fresh, &group)
        }
        TrashPoint::Claim | TrashPoint::Publication => {
            store.wait_for_state(&fresh, JobState::Succeeded)
        }
    }
}

/// A trash followed at once by a recovery never lets the old attempt renew or publish, at the
/// claim, at a heartbeat and at the publication. The point is named by `ARTIFREE_TRASH_AT`, and
/// each point runs in its own unit. The job ends cancelled when the old attempt settles, and the
/// recovered artifact runs a fresh keyed job (decision 0057 rule 8).
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_trash_and_recovery_never_publish_the_old_attempt() -> TestResult {
    let point: TrashPoint = parsed(&std::env::var(TRASH_AT)?)?;
    let scratch = Scratch::new(&format!("trash-{}", point.name()))?;
    let store = Store::new(&scratch.data())?;
    let artifact = store.create_artifact()?;
    let job = store.enqueue_fixture(&format!("old-{}", point.name()))?;
    let mut worker = start_old_attempt(&scratch, point)?;
    reach_point(&scratch, &store, &mut worker, &job, point)?;
    trash_and_recover(&store, &artifact, point)?;
    resume_old_attempt(&scratch, &mut worker, &job, point)?;
    store.wait_for_state(&job, JobState::Cancelled)?;
    assert_old_attempt_never_publishes(&store, &mut worker, &job)?;
    run_fresh_job(&store, &mut worker, point)
}

// --- the purge case ----------------------------------------------------------

/// A purge that races a descendant which ignores SIGTERM returns without waiting for the payload,
/// makes the artifact unreadable, and keeps a cleanup tombstone. The tombstone survives a disable,
/// a restore and the forced death of the worker, and only the settlement ends it. Then the job
/// ends cancelled, no replacement attempt has run, and one fresh attempt runs on an accessible
/// artifact (decision 0042 rule 8; decision 0057 rule 11).
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_purge_racing_a_sigterm_ignoring_descendant_keeps_its_tombstone() -> TestResult {
    let scratch = Scratch::new("purge-race")?;
    let store = Store::new(&scratch.data())?;
    let artifact = store.create_artifact()?;
    let job = store.enqueue_fixture("purge-race")?;
    let mut worker = Worker::spawn(&scratch, &LEASE_3)?;
    let group = worker.released_attempt(&store)?;
    // The worker stops here, so no checkpoint or cleanup of its own runs while the purge races
    // the payload's descendant.
    worker.signal("STOP")?;
    set_trashed(&store, &artifact, true, "trash-purge")?;
    let started = Instant::now();
    purge(&store, &artifact, "purge-artifact")?;
    let returned = started.elapsed();
    assert!(
        returned < PURGE_RETURN,
        "a purge must not wait for the payload: it returned after {returned:?}"
    );
    assert!(
        matches!(
            store.service()?.artifact(&owner()?, &artifact),
            Err(ArtifactError::NotFound(_))
        ),
        "a purged artifact must not be readable"
    );
    assert_eq!(
        store.job_row(&job)?.state,
        JobState::Running.as_str(),
        "a purged job with an unsettled attempt stays running until the attempt settles"
    );
    tombstone_holds(&store, &job, "after the purge")?;

    set_disabled(&store, true)?;
    tombstone_holds(&store, &job, "after the disable")?;
    let restored = restored_copy(&scratch, "restored")?;
    let copy = Store::new(&restored)?;
    tombstone_holds(&copy, &job, "in the restored database")?;
    assert_eq!(
        copy.count("SELECT count(*) FROM artifacts WHERE purged_at IS NOT NULL")?,
        1,
        "the restored artifact must stay purged"
    );

    worker.kill()?;
    wait_no_payload()?;
    tombstone_holds(&store, &job, "after the forced death of the worker")?;

    let mut restarted = Worker::spawn(&scratch, &LEASE_3)?;
    store.wait_for_state(&job, JobState::Cancelled)?;
    let settled = assert_settled_tombstone(&store, &job, &group)?;

    set_disabled(&store, false)?;
    let second = second_artifact(&store)?;
    let fresh = enqueue_on(&store, &second, "fresh")?;
    let fresh_group = restarted.released_attempt(&store)?;
    let claimed = store.attempt_of(&fresh)?.created_at;
    assert!(
        claimed >= settled,
        "the fresh attempt was claimed at {}, before the tombstone settled at {}",
        claimed.0,
        settled.0
    );
    cancel_and_settle(&store, &mut restarted, &fresh, &fresh_group)?;
    assert_eq!(
        store.attempt_names(&job)?.len(),
        1,
        "the purged job keeps its one attempt, with no replacement"
    );
    Ok(())
}
