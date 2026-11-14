//! Disable and enable cases of the recovery matrix of R02bc (decision 0057 rule 20). A disable and
//! an enable that both happen before a heartbeat, a publication or a restart end the attempt on
//! the generation it pinned, and the mismatch is not forgotten across a restart or a restore
//! (0042 rule 6; 0057 rules 5, 7 and 9).

use std::env;
use std::error::Error;

use artifree_domain::identity::StateGeneration;
use artifree_domain::job::{AttemptState, JobId, JobState, Revocation};
use artifree_providers::tokens::SqliteTokenStore;

use super::recovery_support::{
    attempt_rows, clear_pause, pause_launch, restored_copy, wait_marker,
};
use super::{
    DEADLINE, LEASE_3, OWNER, Scratch, Store, TestResult, Worker, cancel_and_settle, owner, within,
};

/// The environment variable that names the across case of one job test: `restart` or `restore`.
const ACROSS: &str = "ARTIFREE_ACROSS";
/// The time that a disable records. Only whether the owner is disabled matters to the store.
const DISABLED_AT: &str = "2026-11-14T00:00:00Z";
/// The pause that stalls an attempt at collecting for four seconds, which is long enough for a
/// disable and an enable, and short of the thirty-second default lease.
const COLLECTING_STALL: &str = "collecting stall 4000";
/// The pause that holds an attempt once its payload is released.
const RUNNING_HOLD: &str = "running hold";

/// The two ways a case carries its disable across a worker's end: a new worker on the same
/// directories, or a worker on a restored copy of the data directory beside the same execution
/// directory.
#[derive(Debug)]
enum Across {
    /// A new worker on the case's own data and execution directories.
    Restart,
    /// A worker on the restored copy of the data directory.
    Restore,
}

/// The across case that `ARTIFREE_ACROSS` names.
fn across_case(name: &str) -> Result<Across, Box<dyn Error>> {
    Ok(match name {
        "restart" => Across::Restart,
        "restore" => Across::Restore,
        other => return Err(format!("{ACROSS} names no across case: {other}").into()),
    })
}

/// Disables the owner when `disabled` is true and enables it when it is false. The token store
/// makes each change, and each change raises the owner's state generation (0042 rule 6).
fn set_disabled(store: &Store, disabled: bool) -> TestResult {
    use artifree_ports::user_store::UserStore;

    let at: Option<&str> = disabled.then_some(DISABLED_AT);
    SqliteTokenStore::open(&store.path)?.set_disabled(&owner()?, at)?;
    Ok(())
}

/// The owner's state generation as the store holds it now.
fn current_generation(store: &Store) -> Result<i64, Box<dyn Error>> {
    let generation: i64 = store.sql()?.query_row(
        "SELECT state_generation FROM users WHERE id = ?1",
        [OWNER],
        |row| row.get(0),
    )?;
    Ok(generation)
}

/// Checks that the one attempt of `job` pinned the owner's state generation as it stands now. A
/// claim made after a disable and an enable pins the generation after both.
fn assert_pinned_now(store: &Store, job: &JobId, what: &str) -> TestResult {
    let current = u64::try_from(current_generation(store)?)?;
    let attempt = store.attempt_of(job)?;
    let pinned = attempt.actor_generation.map(StateGeneration::get);
    assert_eq!(
        pinned,
        Some(current),
        "{what} must pin the owner's current state generation"
    );
    Ok(())
}

/// A disable and an enable, both before the attempt's next heartbeat, stop the attempt with the
/// generation reason: the worker reports `stopped-generation`, the attempt's group is gone, the
/// job publishes no counts, and the job ends cancelled. A job enqueued after the enable claims
/// under the new generation and is cancelled through the usual path (0042 rule 6; 0057 rules 5,
/// 7 and 9).
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_disable_and_enable_before_a_heartbeat_stops_the_attempt() -> TestResult {
    let scratch = Scratch::new("disable-heartbeat")?;
    let store = Store::new(&scratch.data())?;
    store.create_artifact()?;
    let job = store.enqueue_fixture("heartbeat")?;
    let mut worker = Worker::spawn(&scratch, &LEASE_3)?;
    let group = worker.released_attempt(&store)?;
    set_disabled(&store, true)?;
    set_disabled(&store, false)?;
    worker.wait_stderr(
        &format!("attempt of job {}: stopped-generation", job.as_str()),
        within(DEADLINE),
    )?;
    store.wait_for_state(&job, JobState::Cancelled)?;
    let attempt = store.attempt_of(&job)?;
    assert_eq!(
        attempt.state,
        AttemptState::Settled,
        "a cancelled job's attempt must be settled"
    );
    assert_eq!(
        attempt.revoked,
        Some(Revocation::Generation),
        "the disable and the enable must be stored as the attempt's revocation"
    );
    assert!(
        !group.exists(),
        "the attempt group {} must be gone once the job is cancelled",
        group.display()
    );
    let row = store.job_row(&job)?;
    assert_eq!(
        (row.files, row.bytes),
        (None, None),
        "an attempt stopped by the generation publishes no counts"
    );
    let next = store.enqueue_fixture("after-enable")?;
    let next_group = worker.released_attempt(&store)?;
    assert_pinned_now(&store, &next, "a job claimed after the enable")?;
    cancel_and_settle(&store, &mut worker, &next, &next_group)
}

/// A disable and an enable made while the attempt collects its output refuse its publication: the
/// job ends cancelled with no counts, its attempt is settled and revoked `generation`, and its
/// group is gone. The queued job waits behind it, and it is claimed after the enable under the
/// new generation and succeeds (0042 rule 6; 0057 rules 5, 9 and 10).
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_disable_and_enable_before_publication_cancels_the_job() -> TestResult {
    let scratch = Scratch::new("disable-publication")?;
    let store = Store::new(&scratch.data())?;
    store.create_artifact()?;
    let job = store.enqueue_fixture("publication")?;
    let queued = store.enqueue_fixture("queued")?;
    pause_launch(&scratch.execution(), COLLECTING_STALL)?;
    let worker = Worker::spawn(&scratch, &[])?;
    let (_, group) = worker.started_attempt(&store)?;
    wait_marker(&scratch.execution(), "collecting")?;
    set_disabled(&store, true)?;
    set_disabled(&store, false)?;
    // The pause is cleared before the queued job launches, so only the first attempt stalls.
    clear_pause(&scratch.execution())?;
    store.wait_for_state(&job, JobState::Cancelled)?;
    let row = store.job_row(&job)?;
    assert_eq!(
        (row.files, row.bytes),
        (None, None),
        "a publication refused by the generation writes no counts"
    );
    let attempt = store.attempt_of(&job)?;
    assert_eq!(
        attempt.state,
        AttemptState::Settled,
        "a cancelled job's attempt must be settled"
    );
    assert_eq!(
        attempt.revoked,
        Some(Revocation::Generation),
        "the refused publication must be stored as the attempt's revocation"
    );
    assert!(
        !group.exists(),
        "the attempt group {} must be gone once the job is cancelled",
        group.display()
    );
    store.wait_for_state(&queued, JobState::Succeeded)?;
    assert_pinned_now(&store, &queued, "the queued job claimed after the enable")
}

/// A disable and an enable made while the attempt runs are not forgotten: after a restart of the
/// worker on the same directories, or after a restore of the database beside the same execution
/// directory, the job ends cancelled with its one attempt settled and publishes no counts. A
/// restore also stores the lease revocation that the restore marks (decision 0057 rule 13).
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_disable_and_enable_is_not_forgotten_across_a_restart_or_restore() -> TestResult {
    let name = env::var(ACROSS)?;
    let case = across_case(&name)?;
    let scratch = Scratch::new(&format!("disable-{name}"))?;
    let store = Store::new(&scratch.data())?;
    store.create_artifact()?;
    let job = store.enqueue_fixture("across")?;
    pause_launch(&scratch.execution(), RUNNING_HOLD)?;
    let mut worker = Worker::spawn(&scratch, &LEASE_3)?;
    wait_marker(&scratch.execution(), "running")?;
    set_disabled(&store, true)?;
    set_disabled(&store, false)?;
    let data = match case {
        Across::Restart => scratch.data(),
        Across::Restore => restored_copy(&scratch, "restored")?,
    };
    worker.kill()?;
    clear_pause(&scratch.execution())?;
    let _successor = Worker::spawn_in(&data, &scratch.execution(), &LEASE_3)?;
    let after = Store::new(&data)?;
    after.wait_for_state(&job, JobState::Cancelled)?;
    let row = after.job_row(&job)?;
    assert_eq!(
        row.attempts, 1,
        "the job keeps the one claim it spent before the disable"
    );
    assert_eq!(
        (row.files, row.bytes),
        (None, None),
        "a job that the generation ends publishes no counts"
    );
    let rows = attempt_rows(&after, &job)?;
    let [attempt] = rows.as_slice() else {
        return Err(format!("expected one attempt row, found {}", rows.len()).into());
    };
    assert_eq!(
        attempt.state,
        AttemptState::Settled,
        "the attempt of the job must be settled after the successor's cleanup"
    );
    if matches!(case, Across::Restore) {
        assert_eq!(
            attempt.revoked,
            Some(Revocation::Lease),
            "the restore must mark the attempt's cleanup pending as a lease revocation"
        );
    }
    Ok(())
}
