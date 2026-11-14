//! Kill and restart cases of the recovery matrix of R02bc (decision 0057 rule 20). A supervisor is
//! killed at a phase of its attempt, or between the removal of the attempt's record and its
//! settlement; a restart on the same directories must settle the killed attempt before it claims
//! the job again (decision 0057 rules 10 and 19, rule 22).
//!
//! Every case is ignored and runs only in the enforcement job, as the other cases of this target.

use std::error::Error;

use artifree_domain::job::{AttemptState, JobId, JobState};
use artifree_ports::job_store::{AttemptRow, JobStore};

use super::recovery_support::{
    QUARANTINE_FILE, attempt_rows, clear_pause, pause_launch, record_path, wait_marker,
    wait_no_payload,
};
use super::{
    LEASE_3, Scratch, Store, TestResult, Worker, group_name, payload_processes, runs_the_payload,
};

/// The environment variable that names the phase of the first case. The enforcement job sets it
/// to one of `KILL_PHASES`, one case per phase.
const KILL_PHASE: &str = "ARTIFREE_KILL_PHASE";
/// The phases that the first case may name, by their words in the pause file and the markers.
/// `settled` is the drop of the held capacity after the attempt was settled (decision 0057 rule
/// 10). The phase `collecting` has its own case, which is the second one of this file.
const KILL_PHASES: [&str; 7] = [
    "registered",
    "created",
    "guarded",
    "started",
    "ready",
    "running",
    "settled",
];
/// The start of the operator's text on stderr, which a worker prints when it refuses to claim
/// because the execution directory is quarantined (decision 0057 rule 19).
const OPERATOR_TEXT: &str = "operator action is required";

/// The phase that the first case pauses at, named by `ARTIFREE_KILL_PHASE`. A name that is not
/// one of `KILL_PHASES` is an error, so that a case never runs without its phase.
fn kill_phase() -> Result<&'static str, Box<dyn Error>> {
    let named = std::env::var(KILL_PHASE)?;
    KILL_PHASES
        .into_iter()
        .find(|phase| *phase == named)
        .ok_or_else(|| {
            format!("{KILL_PHASE} must name one of {KILL_PHASES:?}, not {named:?}").into()
        })
}

/// The one unsettled attempt of the store. Each case pauses while its job has one attempt, so an
/// answer of any other count is an error.
fn only_unsettled(store: &Store) -> Result<AttemptRow, Box<dyn Error>> {
    let rows = store.port.unsettled_attempts()?;
    let count = rows.len();
    let [row]: [AttemptRow; 1] = rows
        .try_into()
        .map_err(|_| format!("expected one unsettled attempt, found {count}"))?;
    Ok(row)
}

/// Checks, before the kill, the fact that `phase` leaves once its marker is written (decision
/// 0057 rule 20). At `settled` the attempt is settled, its record is gone and the job still runs,
/// since the job is completed only after the capacity is dropped. At every other phase the
/// attempt is `Live`, and its record and group are as the phase names them.
fn expect_phase_fact(
    store: &Store,
    scratch: &Scratch,
    worker: &Worker,
    job: &JobId,
    phase: &str,
) -> TestResult {
    if phase == "settled" {
        let rows = attempt_rows(store, job)?;
        let [row] = rows.as_slice() else {
            return Err(format!("expected one attempt at {phase}, found {}", rows.len()).into());
        };
        assert_eq!(
            row.state,
            AttemptState::Settled,
            "the attempt must be settled at {phase}"
        );
        assert_eq!(
            row.revoked, None,
            "a settled attempt is not revoked at {phase}"
        );
        assert_eq!(
            store.job_row(job)?.state,
            JobState::Running.as_str(),
            "the job must still run at {phase}"
        );
        assert!(
            !record_path(&scratch.execution(), &row.attempt).exists(),
            "the record must be gone at {phase}"
        );
        return Ok(());
    }
    let row = only_unsettled(store)?;
    assert_eq!(
        row.state,
        AttemptState::Live,
        "the attempt must be Live at {phase}"
    );
    let record = record_path(&scratch.execution(), &row.attempt);
    let group = worker.group.join(group_name(&row.attempt));
    match phase {
        "registered" => {
            assert!(
                record.exists(),
                "the record must exist at {phase}: {}",
                record.display()
            );
            assert!(
                !group.exists(),
                "no group may exist yet at {phase}: {}",
                group.display()
            );
        }
        "created" => {
            assert!(
                group.is_dir(),
                "the group must exist at {phase}: {}",
                group.display()
            );
            assert!(
                !runs_the_payload(&group),
                "no payload may run yet at {phase}"
            );
        }
        "guarded" | "started" | "ready" | "running" => {
            assert!(
                group.is_dir(),
                "the group must exist at {phase}: {}",
                group.display()
            );
        }
        "collecting" => {
            assert!(
                !record.exists(),
                "the record must be removed at {phase}: {}",
                record.display()
            );
            assert!(
                !group.exists(),
                "the group must be gone at {phase}: {}",
                group.display()
            );
        }
        other => return Err(format!("no fact is named for the phase {other}").into()),
    }
    Ok(())
}

/// Kills the worker with SIGKILL and waits until no payload runs. A killed supervisor writes
/// nothing, so the attempt rows are the rows that `before` holds: a row that the kill had left
/// `Live` is still `Live`, and a settled one is still settled (decision 0057 rule 10).
fn kill_supervisor(
    store: &Store,
    job: &JobId,
    worker: &mut Worker,
    before: &[AttemptRow],
) -> TestResult {
    worker.kill()?;
    wait_no_payload()?;
    let after = attempt_rows(store, job)?;
    assert_eq!(
        after, before,
        "a killed supervisor must leave every attempt row as it was"
    );
    Ok(())
}

/// Waits until the restarted worker has succeeded the job, then checks the end that a restart
/// must reach after a killed attempt (decision 0057 rules 10 and 19, rule 22). The killed attempt
/// is settled and not revoked, and it is settled before the second attempt is claimed. The second
/// attempt settles with a larger fence, and the job counts two claims and the copy scenario's
/// counts. No payload survives, and no quarantine file is written.
fn expect_settled_then_reclaimed(
    store: &Store,
    scratch: &Scratch,
    restarted: &mut Worker,
    job: &JobId,
) -> TestResult {
    store.wait_for_state(job, JobState::Succeeded)?;
    restarted.wait_event("the succeeded line of the job", |event| {
        event["event"] == "job"
            && event["job"] == job.as_str()
            && event["state"] == JobState::Succeeded.as_str()
    })?;
    let rows = attempt_rows(store, job)?;
    let [first, second] = rows.as_slice() else {
        return Err(format!("expected two attempts of the job, found {}", rows.len()).into());
    };
    assert_eq!(
        first.state,
        AttemptState::Settled,
        "the killed attempt must be settled before the reclaim"
    );
    assert_eq!(
        first.revoked, None,
        "the killed attempt must not be revoked by the restart"
    );
    assert_eq!(
        second.state,
        AttemptState::Settled,
        "the reclaimed attempt must be settled"
    );
    assert_eq!(
        second.revoked, None,
        "the reclaimed attempt must not be revoked"
    );
    let settled = first
        .settled_at
        .ok_or("the killed attempt has no settlement time")?;
    assert!(
        settled <= second.created_at,
        "the killed attempt settled at {settled:?}, after the reclaim at {:?}",
        second.created_at
    );
    assert!(
        second.fence > first.fence,
        "the reclaim must carry a fresh fence: {} is not above {}",
        second.fence,
        first.fence
    );
    let row = store.job_row(job)?;
    assert_eq!(row.attempts, 2, "the job takes two claims");
    assert_eq!(
        (row.files, row.bytes),
        (Some(2), Some(38)),
        "the copy scenario admits two files and 38 bytes"
    );
    let survivors = payload_processes()?;
    assert!(
        survivors.is_empty(),
        "no payload process may survive the reclaim: {survivors:?}"
    );
    assert!(
        !record_path(&scratch.execution(), &first.attempt).exists(),
        "the killed attempt's record must be gone"
    );
    assert!(
        !scratch.execution().join(QUARANTINE_FILE).exists(),
        "no quarantine file may be written by the restart"
    );
    Ok(())
}

/// A supervisor killed at one phase of its attempt is restarted on the same directories: the
/// restart settles the killed attempt before it claims the job again, no payload survives the
/// kill, and the job then succeeds on a second attempt with a fresh fence (decision 0057 rules 10
/// and 19, rule 22). The enforcement job runs this case once per phase in `ARTIFREE_KILL_PHASE`.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_a_supervisor_killed_at_a_phase_is_cleaned_before_reclaim() -> TestResult {
    let phase = kill_phase()?;
    let scratch = Scratch::new(&format!("kill-{phase}"))?;
    let store = Store::new(&scratch.data())?;
    store.create_artifact()?;
    let job = store.enqueue_fixture(&format!("kill-{phase}"))?;
    pause_launch(&scratch.execution(), &format!("{phase} hold"))?;
    let mut worker = Worker::spawn(&scratch, &LEASE_3)?;
    wait_marker(&scratch.execution(), phase)?;
    expect_phase_fact(&store, &scratch, &worker, &job, phase)?;
    let before = attempt_rows(&store, &job)?;
    kill_supervisor(&store, &job, &mut worker, &before)?;
    clear_pause(&scratch.execution())?;
    let mut restarted = Worker::spawn(&scratch, &LEASE_3)?;
    expect_settled_then_reclaimed(&store, &scratch, &mut restarted, &job)
}

/// A supervisor killed after the record of its attempt is removed, and before the attempt is
/// settled, leaves a live row of this machine with no record. The restart settles that row
/// without a record and the job then succeeds on a second attempt. The restart writes no
/// quarantine and prints no operator text (decision 0057 rule 19).
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_a_kill_between_record_removal_and_settlement_is_settled_without_a_record() -> TestResult {
    let scratch = Scratch::new("kill-collecting")?;
    let store = Store::new(&scratch.data())?;
    store.create_artifact()?;
    let job = store.enqueue_fixture("kill-collecting")?;
    pause_launch(&scratch.execution(), "collecting hold")?;
    let mut worker = Worker::spawn(&scratch, &LEASE_3)?;
    wait_marker(&scratch.execution(), "collecting")?;
    expect_phase_fact(&store, &scratch, &worker, &job, "collecting")?;
    let before = attempt_rows(&store, &job)?;
    kill_supervisor(&store, &job, &mut worker, &before)?;
    clear_pause(&scratch.execution())?;
    let mut restarted = Worker::spawn(&scratch, &LEASE_3)?;
    expect_settled_then_reclaimed(&store, &scratch, &mut restarted, &job)?;
    restarted.pump();
    assert!(
        !restarted.stderr.contains(OPERATOR_TEXT),
        "a record-less row of this machine must not quarantine the directory, stderr: {}",
        restarted.stderr
    );
    Ok(())
}
