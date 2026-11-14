//! Job tests of the guardian and its renewals (decision 0056 rule 14, row R02ah, amendment of
//! 2026-11-13, the guardian).
//!
//! Every test here is ignored, because it needs the prepared runner of run.sh part 11: the runner
//! sets the payload scenario, runs one test by its exact name, and then checks what the attempt
//! left behind. Locally the tests are only compiled and linted. Each one ends by requiring that no
//! attempt group remains. `job_supervisor_holds_at_its_phase` is the supervisor that run.sh kills at
//! a phase. It reads the `ARTIFREE_HOLD_*` variables that run.sh sets and sets none itself: part 11
//! holds it from `guarded` to `collecting`, and part 12 holds it at `registered` and `created` in
//! the directory that `ARTIFREE_HOLD_DIR` names, which run.sh keeps for a reconciliation unit.

use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use super::super::guardian::GuardianReason;
use super::resource_tests::evidence_of;
use super::tests::{ExecutionDir, assert_no_attempt_groups, refusal_of};
use super::{
    AttemptFailure, AttemptPlan, FIXTURE_PAGE, Heartbeat, LeaseLoss, NotStartedReason, Pause,
    PauseMode, Phase, launch,
};

/// The stand-in store of a plan: the answer to the heartbeat with the given number.
type Store = fn(u32) -> Heartbeat;
/// The lease of the renewal tests. Its renewal window is a third of it, so one second.
const JOB_LEASE: Duration = Duration::from_secs(3);
/// How long the stalled supervisor stops at `running`. It is longer than the window, so the
/// guardian ends the attempt during the stall.
const STALL: Duration = Duration::from_secs(4);
/// The marker that the pause at `running` creates right after the release. Its modification time
/// is the release time, up to the granularity of the file system's clock.
const RELEASE_MARKER: &str = "running";
/// The number of heartbeats that the stand-in store confirms before it loses the lease.
const CONFIRMED_HEARTBEATS: u32 = 2;
/// How long a launch may take before its test fails, so that a missing guardian fails the job
/// instead of hanging it.
const HANG_WITHIN: Duration = Duration::from_secs(150);
/// The least time from the release to the end of an attempt that the guardian's deadline ends: the
/// wall deadline of 120 s (decision 0042 rule 5).
const DEADLINE_FLOOR: Duration = Duration::from_secs(120);
/// The most time from the launch to the end of an attempt that the guardian's deadline ends. The
/// guardian's deadline is 132 s from its start with the fixture deadlines.
const DEADLINE_CEILING: Duration = Duration::from_secs(135);
/// The variable that names the phase at which run.sh holds the supervisor, by its word.
const HOLD_PHASE_VARIABLE: &str = "ARTIFREE_HOLD_PHASE";
/// The variable that is `1` or `0`: whether the held attempt starts its guardian.
const HOLD_GUARDIAN_VARIABLE: &str = "ARTIFREE_HOLD_GUARDIAN";
/// The variable that names the directory that receives the phase's marker file.
const HOLD_MARKERS_VARIABLE: &str = "ARTIFREE_HOLD_MARKERS";
/// The variable that names the execution directory that run.sh made for the held supervisor, so
/// that a second unit can reconcile it (0056, amendment of 2026-11-14). Unset, the test makes its
/// own.
const HOLD_DIR_VARIABLE: &str = "ARTIFREE_HOLD_DIR";
/// Every phase at which a test may hold the supervisor, in the order that an attempt reaches them.
const PHASES: [Phase; 7] = [
    Phase::Registered,
    Phase::Created,
    Phase::Guarded,
    Phase::Started,
    Phase::Ready,
    Phase::Running,
    Phase::Collecting,
];
/// The bytes that the `copy` scenario appends to the page, as the io tests expect them.
const COPIED: &[u8] = b"<!-- copied -->";
/// The asset that the `copy` scenario writes beside the page, as the io tests expect it.
const ASSET: &[u8] = b"collected";

/// The plan of the renewal tests: the job's lease, the given stand-in store, and a pause at
/// `running` that stalls for no time. The zero stall writes the `running` marker right after the
/// release, so that the marker's time is the release time.
fn renewal_plan(markers: &Path, store: Store) -> AttemptPlan {
    AttemptPlan {
        lease: JOB_LEASE,
        store,
        pause: Some(Pause {
            phase: Phase::Running,
            mode: PauseMode::Stall(Duration::ZERO),
            markers: markers.to_path_buf(),
        }),
        ..AttemptPlan::fixture()
    }
}

/// The time from the release to now, read from the modification time of the `running` marker.
fn since_release(markers: &Path) -> Result<Duration, Box<dyn Error>> {
    let modified = fs::metadata(markers.join(RELEASE_MARKER))?.modified()?;
    let elapsed = SystemTime::now().duration_since(modified)?;
    Ok(elapsed)
}

/// The store that confirms the first heartbeats and then loses the lease with `part`.
fn after_two(heartbeat: u32, part: LeaseLoss) -> Heartbeat {
    if heartbeat < CONFIRMED_HEARTBEATS {
        Heartbeat::Confirmed
    } else {
        Heartbeat::Lost(part)
    }
}

/// Loses the lease by a revoked fence, after the confirmed heartbeats.
fn fence_after_two(heartbeat: u32) -> Heartbeat {
    after_two(heartbeat, LeaseLoss::Fence)
}

/// Loses the lease by a cancel, after the confirmed heartbeats.
fn cancel_after_two(heartbeat: u32) -> Heartbeat {
    after_two(heartbeat, LeaseLoss::Cancelled)
}

/// Loses the lease by a changed actor generation, after the confirmed heartbeats.
fn generation_after_two(heartbeat: u32) -> Heartbeat {
    after_two(heartbeat, LeaseLoss::Generation)
}

/// Loses the lease by lost authority, after the confirmed heartbeats.
fn authority_after_two(heartbeat: u32) -> Heartbeat {
    after_two(heartbeat, LeaseLoss::Authority)
}

/// The store that does not answer.
fn unavailable(_heartbeat: u32) -> Heartbeat {
    Heartbeat::Unavailable
}

/// The store that confirms every heartbeat.
fn confirms(_heartbeat: u32) -> Heartbeat {
    Heartbeat::Confirmed
}

/// Runs one launch of `plan` in `dir` on its own thread, and returns its exit status or its
/// failure. An attempt that does not end within `within` fails the test, so that a missing
/// guardian fails the job instead of hanging it.
fn launch_within(
    plan: AttemptPlan,
    dir: PathBuf,
    within: Duration,
) -> Result<Result<ExitStatus, AttemptFailure>, Box<dyn Error>> {
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("guardian-launch".to_owned())
        .spawn(move || {
            let outcome = launch(&plan, &dir)
                .map(|evidence| evidence.exit)
                .map_err(|refusal| refusal.failure);
            // The receiver is dropped only after the timeout, when nobody reads the outcome.
            sender.send(outcome).ok();
        })?;
    match receiver.recv_timeout(within) {
        Ok(outcome) => Ok(outcome),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            Err(format!("the attempt did not end within {within:?}").into())
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err("the attempt thread ended without an outcome".into())
        }
    }
}

/// Runs a plan whose supervisor never ends the attempt by itself, and requires that the guardian
/// ends it at its deadline: `Guardian(Deadline)`, at least `DEADLINE_FLOOR` after the release and
/// within `DEADLINE_CEILING` of the launch. The attempt must leave no group behind.
fn assert_ends_at_the_guardian_deadline(
    label: &str,
    plan: AttemptPlan,
    dir: &ExecutionDir,
) -> Result<(), Box<dyn Error>> {
    let started = Instant::now();
    let outcome = launch_within(plan, dir.path().to_path_buf(), HANG_WITHIN)?;
    let elapsed = started.elapsed();
    let since = since_release(dir.path())?;
    assert_eq!(
        outcome,
        Err(AttemptFailure::Guardian(GuardianReason::Deadline)),
        "the {label} attempt must end at the guardian's deadline"
    );
    assert!(
        since >= DEADLINE_FLOOR,
        "the {label} attempt must run at least {DEADLINE_FLOOR:?} after the release, took {since:?}"
    );
    assert!(
        (DEADLINE_FLOOR..DEADLINE_CEILING).contains(&elapsed),
        "the {label} attempt must end within {DEADLINE_CEILING:?} of the launch, took {elapsed:?}"
    );
    assert_no_attempt_groups()
}

/// The files that the `copy` scenario leaves in the output root, in path order.
fn copied_files() -> Vec<(String, Vec<u8>)> {
    vec![
        ("assets/result.txt".to_owned(), ASSET.to_vec()),
        ("index.html".to_owned(), [FIXTURE_PAGE, COPIED].concat()),
    ]
}

/// The phase that the held supervisor waits at, named by its word.
fn hold_phase(word: &str) -> Result<Phase, Box<dyn Error>> {
    PHASES
        .into_iter()
        .find(|phase| phase.word() == word)
        .ok_or_else(|| format!("{HOLD_PHASE_VARIABLE} names no phase: {word:?}").into())
}

/// Whether the held attempt starts its guardian, from the flag `1` or `0`.
fn hold_guardian(flag: &str) -> Result<bool, Box<dyn Error>> {
    match flag {
        "1" => Ok(true),
        "0" => Ok(false),
        other => Err(format!("{HOLD_GUARDIAN_VARIABLE} must be 0 or 1, got {other:?}").into()),
    }
}

/// A lease lost by any of its four parts ends the attempt as `LeaseLost` with that part, within
/// `JOB_LEASE` of the release. The `LeaseLost` verdict also shows that the guardian reported no
/// reason and was reaped: a reason would rank first, and an unreaped guardian would make the
/// cleanup unobserved.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_lease_loss_ends_the_attempt() -> Result<(), Box<dyn Error>> {
    let cases: [(Store, LeaseLoss, &str); 4] = [
        (fence_after_two, LeaseLoss::Fence, "fence"),
        (cancel_after_two, LeaseLoss::Cancelled, "cancel"),
        (generation_after_two, LeaseLoss::Generation, "generation"),
        (authority_after_two, LeaseLoss::Authority, "authority"),
    ];
    for (store, part, label) in cases {
        let dir = ExecutionDir::new(&format!("guardian-lease-{label}"))?;
        let plan = renewal_plan(dir.path(), store);
        let failure = refusal_of(launch(&plan, dir.path()))?;
        let since = since_release(dir.path())?;
        assert_eq!(
            failure,
            AttemptFailure::LeaseLost(part),
            "a lease lost by its {label} part must end the attempt as LeaseLost({part:?})"
        );
        assert!(
            since < JOB_LEASE,
            "the {label} attempt must end within {JOB_LEASE:?} of the release, took {since:?}"
        );
        assert_no_attempt_groups()?;
    }
    Ok(())
}

/// The store that lost the lease to a cancel before the first heartbeat.
fn cancelled_from_the_start(_heartbeat: u32) -> Heartbeat {
    Heartbeat::Lost(LeaseLoss::Cancelled)
}

/// With the checkpoint before the release that the worker asks for, a store that answers a stop
/// or does not answer at readiness ends the attempt before the payload is released: no `running`
/// marker is written, and the group is gone (decision 0042 rule 3; decision 0057 rule 7).
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_a_stop_at_readiness_releases_no_payload() -> Result<(), Box<dyn Error>> {
    let cases: [(Store, AttemptFailure, &str); 2] = [
        (
            cancelled_from_the_start,
            AttemptFailure::LeaseLost(LeaseLoss::Cancelled),
            "cancel",
        ),
        (
            unavailable,
            AttemptFailure::NotStarted(NotStartedReason::CheckpointUnavailable),
            "unavailable",
        ),
    ];
    for (store, expected, label) in cases {
        let dir = ExecutionDir::new(&format!("guardian-readiness-{label}"))?;
        let plan = AttemptPlan {
            checkpoint_at_release: true,
            ..renewal_plan(dir.path(), store)
        };
        let failure = refusal_of(launch(&plan, dir.path()))?;
        assert_eq!(
            failure, expected,
            "a {label} answer at readiness must end the attempt as {expected:?}"
        );
        assert!(
            !dir.path().join(RELEASE_MARKER).exists(),
            "the {label} attempt must not release its payload"
        );
        assert_no_attempt_groups()?;
    }
    Ok(())
}

/// An unavailable store sends no renewal, so the guardian's window passes and it ends the attempt
/// as `Guardian(RenewalExpired)` within `JOB_LEASE` of the release.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_an_unavailable_store_lets_the_guardian_end_it() -> Result<(), Box<dyn Error>> {
    let dir = ExecutionDir::new("guardian-unavailable")?;
    let plan = renewal_plan(dir.path(), unavailable);
    let failure = refusal_of(launch(&plan, dir.path()))?;
    let since = since_release(dir.path())?;
    assert_eq!(
        failure,
        AttemptFailure::Guardian(GuardianReason::RenewalExpired),
        "an unavailable store must let the guardian end the attempt as renewal-expired"
    );
    assert!(
        since < JOB_LEASE,
        "the unavailable attempt must end within {JOB_LEASE:?} of the release, took {since:?}"
    );
    assert_no_attempt_groups()
}

/// A supervisor that stalls for `STALL` at `running` renews too late, so the guardian ends the
/// attempt as `Guardian(RenewalExpired)`. The attempt returns only after the stall, so the release
/// is at least `STALL` old when it returns.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_a_stalled_supervisor_is_ended_by_the_guardian() -> Result<(), Box<dyn Error>> {
    let dir = ExecutionDir::new("guardian-stall")?;
    let plan = AttemptPlan {
        lease: JOB_LEASE,
        pause: Some(Pause {
            phase: Phase::Running,
            mode: PauseMode::Stall(STALL),
            markers: dir.path().to_path_buf(),
        }),
        ..AttemptPlan::fixture()
    };
    let failure = refusal_of(launch(&plan, dir.path()))?;
    let since = since_release(dir.path())?;
    assert_eq!(
        failure,
        AttemptFailure::Guardian(GuardianReason::RenewalExpired),
        "a supervisor stalled at running must be ended by the guardian as renewal-expired"
    );
    assert!(
        since >= STALL,
        "the stall must have run for {STALL:?} after the release, took {since:?}"
    );
    assert_no_attempt_groups()
}

/// A faulty supervisor renews after every heartbeat whatever the answer, and the provider's own
/// wall deadline is off, so only the guardian's deadline can end the attempt.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_a_faulty_supervisor_is_ended_at_the_deadline() -> Result<(), Box<dyn Error>> {
    let dir = ExecutionDir::new("guardian-faulty")?;
    let plan = AttemptPlan {
        renew_regardless: true,
        provider_deadline: false,
        ..renewal_plan(dir.path(), fence_after_two)
    };
    assert_ends_at_the_guardian_deadline("faulty", plan, &dir)
}

/// Confirmed renewals with the provider's own wall deadline off: only the guardian's deadline can
/// end the attempt.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_confirmed_renewals_end_at_the_deadline() -> Result<(), Box<dyn Error>> {
    let dir = ExecutionDir::new("guardian-confirmed")?;
    let plan = AttemptPlan {
        provider_deadline: false,
        ..renewal_plan(dir.path(), confirms)
    };
    assert_ends_at_the_guardian_deadline("confirmed", plan, &dir)
}

/// The fixture plan still starts the guardian and still yields its output: the copy scenario's
/// page and asset are collected, and the attempt exits with status 0.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_the_fixture_plan_still_yields_its_output() -> Result<(), Box<dyn Error>> {
    assert!(
        AttemptPlan::fixture().guardian,
        "the fixture plan must start the guardian"
    );
    let dir = ExecutionDir::new("guardian-fixture")?;
    let evidence = evidence_of(launch(&AttemptPlan::fixture(), dir.path()))?;
    assert_eq!(
        evidence.exit.code(),
        Some(0),
        "the fixture payload must exit with status 0, got {:?}",
        evidence.exit
    );
    let collected = evidence
        .output
        .map_err(|refusal| format!("the fixture output was not collected: {refusal:?}"))?;
    let files: Vec<(String, Vec<u8>)> = collected
        .package
        .files()
        .map(|(path, bytes)| (path.as_str().to_owned(), bytes.to_vec()))
        .collect();
    assert_eq!(
        files,
        copied_files(),
        "the output must hold the copied page and the asset, in path order"
    );
    assert_no_attempt_groups()
}

/// The supervisor that run.sh kills at a phase. It holds at the phase named by
/// `ARTIFREE_HOLD_PHASE` (`registered`, `created`, `guarded`, `started`, `ready`, `running` or
/// `collecting`), with the guardian that `ARTIFREE_HOLD_GUARDIAN` names, and it writes its marker
/// into `ARTIFREE_HOLD_MARKERS`. With `ARTIFREE_HOLD_DIR` set, it runs in that execution directory
/// and leaves it in place; otherwise it makes its own. A supervisor killed at `registered` or
/// `created` leaves no guardian, whatever the plan's guardian switch, because the guardian starts
/// only after `created`. A hold does not return, so this test returns only when the phase is never
/// reached, and then it fails.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_supervisor_holds_at_its_phase() -> Result<(), Box<dyn Error>> {
    let phase = hold_phase(&env::var(HOLD_PHASE_VARIABLE)?)?;
    let guardian = hold_guardian(&env::var(HOLD_GUARDIAN_VARIABLE)?)?;
    let markers = PathBuf::from(env::var(HOLD_MARKERS_VARIABLE)?);
    let plan = AttemptPlan {
        guardian,
        pause: Some(Pause {
            phase,
            mode: PauseMode::Hold,
            markers,
        }),
        ..AttemptPlan::fixture()
    };
    let dir = match env::var_os(HOLD_DIR_VARIABLE) {
        Some(path) => ExecutionDir::existing(PathBuf::from(path))?,
        None => ExecutionDir::new("supervisor-holds")?,
    };
    let outcome = launch(&plan, dir.path());
    Err(format!(
        "the supervisor was never held at {}: {:?}",
        phase.word(),
        outcome
            .map(|evidence| evidence.exit)
            .map_err(|refusal| refusal.failure)
    )
    .into())
}
