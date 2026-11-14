//! Restore and quarantine cases of the recovery matrix of R02bc (decision 0057 rule 20).

use std::error::Error;
use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

use artifree_domain::job::{AttemptState, JobId, JobState, Revocation};

use super::recovery_support::{
    keeps_attempts, pause_launch, record_path, restored_copy, wait_marker,
};
use super::{DEADLINE, LEASE_3, Scratch, Store, TestResult, Worker, poll, within};

/// The persisted quarantine file of an execution directory. Its lines are the tag, then
/// `reason=<word>`.
const QUARANTINE_FILE: &str = "quarantine";
/// The stderr line of a worker that refuses its work because the execution is quarantined.
const REFUSAL: &str = "execution unavailable: operator action is required";
/// The child group that makes the attempt group's removal fail once the group is seen empty.
const STUCK_GROUP: &str = "stuck";

/// The value of the `reason=` line of the quarantine file in `execution`, or `None` when the file
/// is absent. A file without that line has no reason, so it is `None` as well.
fn quarantine_reason(execution: &Path) -> Result<Option<String>, Box<dyn Error>> {
    let text = match fs::read_to_string(execution.join(QUARANTINE_FILE)) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(text
        .lines()
        .find_map(|line| line.strip_prefix("reason="))
        .map(str::to_owned))
}

/// A restored database beside a fresh execution directory settles nothing and quarantines
/// (decision 0057 rules 13 and 19). The copy is taken while the first worker holds its attempt
/// after the registry record is written, and that worker is killed there. A second worker on the
/// copy, with an empty execution directory, finds the copy's attempt row, which the restore marked
/// `cleanup_pending`. No record in its directory can settle the row, so the second worker
/// quarantines the provider for `registry` and refuses. The copy keeps the row unsettled, and the
/// job keeps its one claim.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_a_restored_database_beside_a_fresh_directory_settles_nothing_and_quarantines() -> TestResult
{
    let scratch = Scratch::new("restore")?;
    let store = Store::new(&scratch.data())?;
    store.create_artifact()?;
    let job = store.enqueue_fixture("restore")?;
    pause_launch(&scratch.execution(), "registered hold")?;
    let mut first = Worker::spawn(&scratch, &LEASE_3)?;
    wait_marker(&scratch.execution(), "registered")?;
    let held = store.attempt_of(&job)?;
    assert_eq!(
        held.state,
        AttemptState::Live,
        "a worker held after its registry record has a live attempt row"
    );
    let restored = restored_copy(&scratch, "restored")?;
    first.kill()?;
    let fresh = scratch.other_execution("fresh")?;
    let mut second = Worker::spawn_in(&restored, &fresh, &LEASE_3)?;
    second.wait_stderr(REFUSAL, within(DEADLINE))?;
    assert_eq!(
        quarantine_reason(&fresh)?.as_deref(),
        Some("registry"),
        "a refusal on the restored database quarantines the provider for `registry`"
    );
    let restored_store = Store::new(&restored)?;
    let row = restored_store.attempt_of(&job)?;
    assert_eq!(
        row.state,
        AttemptState::CleanupPending,
        "the restored attempt stays cleanup pending, because no record settles it"
    );
    assert_eq!(
        row.revoked,
        Some(Revocation::Lease),
        "the restore stores the lease as the attempt's revocation"
    );
    assert!(
        row.settled_at.is_none(),
        "a record-less attempt of the restored database must not be settled"
    );
    let restored_job = restored_store.job_row(&job)?;
    assert_eq!(
        (restored_job.state.as_str(), restored_job.attempts),
        (JobState::Running.as_str(), 1),
        "the job keeps its one claim and stays running"
    );
    keeps_attempts(&restored_store, &job, 1, Duration::from_secs(6))
}

/// A cleanup that is not observed keeps the quarantine (decisions 0042 rule 6 and 0057 rule 20).
/// The attempt's group gets an empty child group once the payload runs, so the group's removal
/// fails after the group was seen empty. The cleanup is then not observed: the record stays, the
/// quarantine file says `cleanup`, and neither the job nor its attempt ends. A restarted worker
/// refuses its capacity, and a second job spends no claim.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_a_stuck_cleanup_keeps_the_quarantine() -> TestResult {
    let scratch = Scratch::new("stuck")?;
    let store = Store::new(&scratch.data())?;
    store.create_artifact()?;
    let job = store.enqueue_fixture("stuck")?;
    let mut worker = Worker::spawn(&scratch, &LEASE_3)?;
    let group = worker.released_attempt(&store)?;
    let outcome = stuck_cleanup(&scratch, &store, &mut worker, &job, &group);
    // The child group and the attempt group are removed best effort, so a failed case leaves no
    // cgroup behind. A removal that fails is not a failure of the case.
    let _ = fs::remove_dir(group.join(STUCK_GROUP));
    let _ = fs::remove_dir(&group);
    outcome
}

/// The body of `job_a_stuck_cleanup_keeps_the_quarantine`, from the released payload on.
fn stuck_cleanup(
    scratch: &Scratch,
    store: &Store,
    worker: &mut Worker,
    job: &JobId,
    group: &Path,
) -> TestResult {
    let attempt = store.attempt_of(job)?.attempt;
    fs::create_dir(group.join(STUCK_GROUP))?;
    store.sql()?.execute(
        "UPDATE jobs SET cancel_requested = 1 WHERE id = ?1",
        [job.as_str()],
    )?;
    poll(within(DEADLINE), "the cleanup quarantine", || {
        Ok((quarantine_reason(&scratch.execution())?.as_deref() == Some("cleanup")).then_some(()))
    })?;
    assert!(
        record_path(&scratch.execution(), &attempt).exists(),
        "a cleanup that is not observed keeps the attempt's record"
    );
    worker.wait_stderr(REFUSAL, within(DEADLINE))?;
    keeps_attempts(store, job, 1, Duration::from_secs(6))?;
    let held = store.job_row(job)?;
    assert_eq!(
        held.state,
        JobState::Running.as_str(),
        "a job whose attempt is not settled does not end"
    );
    let row = store.attempt_of(job)?;
    assert_eq!(
        row.state,
        AttemptState::CleanupPending,
        "a cleanup that is not observed leaves the attempt cleanup_pending, unsettled (0057 rules 10 and 19)"
    );
    worker.kill()?;
    let mut restarted = Worker::spawn(scratch, &LEASE_3)?;
    restarted.wait_stderr(REFUSAL, within(DEADLINE))?;
    let next = store.enqueue_fixture("after-stuck")?;
    keeps_attempts(store, &next, 0, Duration::from_secs(4))?;
    let waiting = store.job_row(&next)?;
    assert_eq!(
        (waiting.state.as_str(), waiting.attempts),
        (JobState::Queued.as_str(), 0),
        "a restarted worker refuses capacity, so a second job stays queued with no claim"
    );
    restarted.kill()?;
    // A worker of the same database on another execution directory sees the row of another
    // directory that is cleanup_pending, and is quarantined as well (0057 rule 19).
    let mut other = Worker::spawn_in(
        &scratch.data(),
        &scratch.other_execution("other-directory")?,
        &LEASE_3,
    )?;
    other.wait_stderr(REFUSAL, within(DEADLINE))?;
    keeps_attempts(store, &next, 0, Duration::from_secs(4))?;
    Ok(())
}
