//! The recovery cases of two workers and of the queue (decision 0057 rule 20, row R02bc). A second
//! worker on another execution directory never runs a new attempt of a job while the old attempt
//! is unsettled. Queued and retry-scheduled jobs wait out a trash and resume after the recovery and
//! a worker restart (decision 0057 rule 8; decision 0030 rule 9).
//!
//! Both cases are ignored and run only in the Linux enforcement job, one per process (decision
//! 0056 rule 12). Each waits for an observable fact, such as a marker, a row or a state, or watches
//! a condition over a window. None waits for a fixed delay in place of a fact.

use std::time::Duration;

use artifree_domain::job::{AttemptState, JobState, Revocation};

use super::recovery_support::{
    attempt_rows, clear_pause, keeps_attempts, pause_launch, set_trashed, wait_marker,
};
use super::{LEASE_3, Scratch, Store, TestResult, Worker};

/// Two workers on one database, with different execution directories, never run an old and a new
/// attempt of one job together. The first worker is killed while its attempt runs, so the row
/// stays unsettled and the lease runs out. A second worker on another directory leaves the job
/// alone for three leases and prints no `claimed` line for it. Once the first directory's worker
/// is restarted, it settles the old attempt before it claims the job again.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_two_workers_never_run_an_old_and_a_new_attempt_together() -> TestResult {
    let scratch = Scratch::new("two-workers")?;
    let store = Store::new(&scratch.data())?;
    store.create_artifact()?;
    let job = store.enqueue_fixture("two-workers")?;
    // The first worker holds at `running`, after its release, so its attempt is live when killed.
    pause_launch(&scratch.execution(), "running hold")?;
    let mut first = Worker::spawn(&scratch, &LEASE_3)?;
    wait_marker(&scratch.execution(), "running")?;
    first.kill()?;
    // The old row is live and belongs to the first directory. The second worker's directory is new.
    let second_execution = scratch.other_execution("second")?;
    let mut second = Worker::spawn_in(&scratch.data(), &second_execution, &LEASE_3)?;
    keeps_attempts(&store, &job, 1, Duration::from_secs(9))?;
    second.pump();
    let claims = second
        .events()?
        .into_iter()
        .filter(|event| event["event"] == "claimed" && event["job"] == job.as_str())
        .count();
    assert_eq!(
        claims,
        0,
        "the second worker printed {claims} claimed line(s) for {} while the old attempt is live",
        job.as_str()
    );
    second.kill()?;
    // The first directory's worker settles the old attempt before it claims the job again.
    clear_pause(&scratch.execution())?;
    let mut restarted = Worker::spawn(&scratch, &LEASE_3)?;
    store.wait_for_state(&job, JobState::Succeeded)?;
    let rows = attempt_rows(&store, &job)?;
    let [old, new] = rows.as_slice() else {
        return Err(format!(
            "expected two attempts of {}, found {}",
            job.as_str(),
            rows.len()
        )
        .into());
    };
    assert_eq!(
        old.state,
        AttemptState::Settled,
        "the old attempt must be settled before the job is claimed again"
    );
    let settled = old
        .settled_at
        .ok_or("the old attempt has no settlement time")?;
    assert!(
        settled.0 <= new.created_at.0,
        "the old attempt settled at {}, after the new attempt was created at {}",
        settled.0,
        new.created_at.0
    );
    assert!(
        old.fence < new.fence,
        "the new attempt must hold a higher fence than {}, found {}",
        old.fence,
        new.fence
    );
    assert_eq!(
        store.job_row(&job)?.attempts,
        2,
        "the job takes one claim for each of its two attempts"
    );
    restarted.kill()?;
    Ok(())
}

/// Queued and retry-scheduled jobs wait out a trash and resume after the recovery and a worker
/// restart. A lease loss schedules the retry while the attempt is held at `ready`, so the
/// revocation written during the hold is found at the release checkpoint. The restarted worker
/// claims neither the retry, the queued job nor the verify job while the artifact is in the trash.
/// After the recovery all three succeed: the retry on its second claim, the queued job on its
/// first.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_queued_and_retry_jobs_resume_after_trash_and_restart() -> TestResult {
    let scratch = Scratch::new("trash-resume")?;
    let store = Store::new(&scratch.data())?;
    let artifact = store.create_artifact()?;
    let retry = store.enqueue_fixture("retry")?;
    pause_launch(&scratch.execution(), "ready stall 3000")?;
    let mut first = Worker::spawn(&scratch, &LEASE_3)?;
    wait_marker(&scratch.execution(), "ready")?;
    store.sql()?.execute(
        "UPDATE execution_attempts SET revoked = 'lease' WHERE job_id = ?1",
        [retry.as_str()],
    )?;
    store.wait_for_state(&retry, JobState::RetryScheduled)?;
    // The one-second backoff keeps the retry out of the first worker's next claim until the kill.
    first.kill()?;
    clear_pause(&scratch.execution())?;
    let rows = attempt_rows(&store, &retry)?;
    let [lost] = rows.as_slice() else {
        return Err(format!(
            "expected one attempt of {} before the trash, found {}",
            retry.as_str(),
            rows.len()
        )
        .into());
    };
    assert_eq!(
        lost.state,
        AttemptState::Settled,
        "the lease loss must settle the retry's attempt"
    );
    assert_eq!(
        lost.revoked,
        Some(Revocation::Lease),
        "the retry's attempt must be revoked by its lease"
    );
    let queued = store.enqueue_fixture("queued")?;
    let verify = store.enqueue_verify("verify")?;
    set_trashed(&store, &artifact, true, "trash-resume")?;
    let mut restarted = Worker::spawn(&scratch, &LEASE_3)?;
    for (job, count) in [(&retry, 1), (&queued, 0)] {
        keeps_attempts(&store, job, count, Duration::from_secs(4))?;
    }
    assert_eq!(
        store.job_row(&verify)?.state,
        JobState::Queued.as_str(),
        "the verify job of a trashed artifact must stay queued"
    );
    set_trashed(&store, &artifact, false, "recover-resume")?;
    for job in [&retry, &queued, &verify] {
        store.wait_for_state(job, JobState::Succeeded)?;
    }
    assert_eq!(
        store.job_row(&retry)?.attempts,
        2,
        "the retry takes its second claim after the recovery"
    );
    assert_eq!(
        store.job_row(&queued)?.attempts,
        1,
        "the queued job takes its first claim after the recovery"
    );
    restarted.kill()?;
    Ok(())
}
