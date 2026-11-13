//! Behavioral tests of the resource part of one attempt (decision 0056 rule 14, row R02ac, with
//! decision 0042 rules 5 and 6): the one final verdict, the survivor listing, and the `job_*`
//! tests that run the resource payload scenarios in the Linux enforcement job.
//!
//! The verdict and listing tests decide from plain values and run in the normal `cargo test`.
//! Every `job_*` test is ignored here. `scripts/enforcement/run.sh` selects one payload scenario
//! with `prepare.sh scenario <name>` and runs that test by its exact name.

use super::super::usage::GroupUsage;
use super::tests::{ExecutionDir, assert_no_attempt_groups, refusal_of};
use super::{
    AttemptEvidence, AttemptFailure, AttemptPlan, AttemptRefusal, NotStartedReason, Outcome,
    SurvivorListing, await_empty_listing, final_verdict, launch, survivor_listing,
};

use std::error::Error;
use std::fs;
use std::io;
use std::time::{Duration, Instant};

/// The most time that one launch may take, so that the supervisor stays responsive.
const RESPONSIVE_WITHIN: Duration = Duration::from_secs(30);
/// The task limit of the attempt group (`pids.max`).
const TASK_LIMIT: u64 = 64;
/// The most memory that the attempt group may peak at, in bytes (`memory.max`).
const MEMORY_LIMIT_BYTES: u64 = 1_073_741_824;
/// The scratch budget in bytes, as the `scratch-bytes` scenario prints it.
const SCRATCH_BYTES: &str = "134217728";
/// The paths that the `scratch-bytes` scenario tries from outside the scratch, in its order.
const OUTSIDE_PATHS: [&str; 5] = ["/x", "/dev/x", "/dev/shm/x", "/tools/x", "/lib/x"];

/// Counters of a group that peaked at 4 MiB, was never refused a task and was never throttled.
fn quiet_usage() -> GroupUsage {
    GroupUsage {
        memory_peak_bytes: Some(4_194_304),
        oom_kills: 0,
        oom_group_kills: 0,
        task_refusals: 0,
        tasks_peak: Some(3),
        cpu_usage_usec: 1_500,
        cpu_periods: 0,
        cpu_throttled_periods: 0,
    }
}

/// An attempt in which no part faulted: the group was observed and emptied, the listing held no
/// pid, the phase ended cleanly and both captures were read.
fn clean_outcome() -> Outcome {
    Outcome {
        cleanup_observed: true,
        output_exceeded: false,
        phase: Ok(()),
        usage: Some(quiet_usage()),
        survivors: SurvivorListing::Empty,
        captures_read: true,
    }
}

/// The run faults, ranked: cleanup, output, deadline, then counters that cannot be read. Each row
/// also carries the lower-ranked faults that can coexist with its own, so a rule that is ranked
/// too low changes that row's verdict.
#[test]
fn verdict_precedence_of_run_faults() {
    let exec_failed = AttemptFailure::NotStarted(NotStartedReason::ExecFailed);
    let cases: [(&str, Outcome, Result<(), AttemptFailure>); 6] = [
        ("a clean run", clean_outcome(), Ok(())),
        (
            "cleanup not observed",
            Outcome {
                cleanup_observed: false,
                output_exceeded: true,
                phase: Err(AttemptFailure::DeadlineExceeded),
                usage: None,
                survivors: SurvivorListing::Occupied,
                captures_read: false,
            },
            Err(AttemptFailure::CleanupNotObserved),
        ),
        (
            "output budget exceeded",
            Outcome {
                cleanup_observed: true,
                output_exceeded: true,
                phase: Err(AttemptFailure::DeadlineExceeded),
                usage: None,
                survivors: SurvivorListing::Occupied,
                captures_read: false,
            },
            Err(AttemptFailure::OutputLimitExceeded),
        ),
        (
            "wall deadline exceeded",
            Outcome {
                cleanup_observed: true,
                output_exceeded: false,
                phase: Err(AttemptFailure::DeadlineExceeded),
                usage: None,
                survivors: SurvivorListing::Occupied,
                captures_read: false,
            },
            Err(AttemptFailure::DeadlineExceeded),
        ),
        (
            "counters that cannot be read after a run",
            Outcome {
                cleanup_observed: true,
                output_exceeded: false,
                phase: Ok(()),
                usage: None,
                survivors: SurvivorListing::Occupied,
                captures_read: false,
            },
            Err(AttemptFailure::Unobserved),
        ),
        (
            "a start that failed keeps its reason without counters",
            Outcome {
                cleanup_observed: true,
                output_exceeded: false,
                phase: Err(exec_failed),
                usage: None,
                survivors: SurvivorListing::Occupied,
                captures_read: false,
            },
            Err(exec_failed),
        ),
    ];
    for (name, outcome, expected) in cases {
        assert_eq!(
            final_verdict(&outcome),
            expected,
            "{name}: the verdict must follow the precedence, for {outcome:?}"
        );
    }
}

/// The survivor and start faults, ranked: survivors, then the not-started reasons of the control
/// channel, then unread captures. Each row also carries the lower-ranked faults that can coexist
/// with its own.
#[test]
fn verdict_precedence_of_survivors_and_start() {
    let exec_failed = AttemptFailure::NotStarted(NotStartedReason::ExecFailed);
    let unconfirmed = AttemptFailure::NotStarted(NotStartedReason::ExecUnconfirmed);
    let cases: [(&str, Outcome, Result<(), AttemptFailure>); 6] = [
        (
            "a clean run without a survivor listing",
            Outcome {
                survivors: SurvivorListing::NotChecked,
                ..clean_outcome()
            },
            Err(AttemptFailure::Unobserved),
        ),
        (
            "survivors above exec failed",
            Outcome {
                phase: Err(exec_failed),
                survivors: SurvivorListing::Occupied,
                captures_read: false,
                ..clean_outcome()
            },
            Err(AttemptFailure::Survivors),
        ),
        (
            "unreadable listing above exec failed",
            Outcome {
                phase: Err(exec_failed),
                survivors: SurvivorListing::Unreadable,
                captures_read: false,
                ..clean_outcome()
            },
            Err(AttemptFailure::Unobserved),
        ),
        (
            "exec failed above unread captures",
            Outcome {
                phase: Err(exec_failed),
                captures_read: false,
                ..clean_outcome()
            },
            Err(exec_failed),
        ),
        (
            "exec unconfirmed above unread captures",
            Outcome {
                phase: Err(unconfirmed),
                captures_read: false,
                ..clean_outcome()
            },
            Err(unconfirmed),
        ),
        (
            "unread captures",
            Outcome {
                captures_read: false,
                ..clean_outcome()
            },
            Err(AttemptFailure::Unobserved),
        ),
    ];
    for (name, outcome, expected) in cases {
        assert_eq!(
            final_verdict(&outcome),
            expected,
            "{name}: the verdict must follow the precedence, for {outcome:?}"
        );
    }
}

/// A group that was killed by its OOM is `OutOfMemory` even with survivors listed and a phase
/// that reported a failure. The stub exits by signal 9 and its control channel reads end of
/// stream, which is the `Ok` phase, so both phases are checked.
#[test]
fn oom_outranks_survivors() {
    let unconfirmed = AttemptFailure::NotStarted(NotStartedReason::ExecUnconfirmed);
    let mut killed = quiet_usage();
    killed.oom_kills = 1;
    killed.oom_group_kills = 1;
    for phase in [Err(unconfirmed), Ok(())] {
        let outcome = Outcome {
            cleanup_observed: true,
            output_exceeded: false,
            phase,
            usage: Some(killed),
            survivors: SurvivorListing::Occupied,
            captures_read: false,
        };
        assert_eq!(
            final_verdict(&outcome),
            Err(AttemptFailure::OutOfMemory),
            "an OOM kill must outrank the survivors, for {outcome:?}"
        );
    }
}

/// The survivor listing: an empty listing holds no pid, one pid or several make it occupied, and
/// a read error, a line that is not a pid or text that is not UTF-8 is unreadable.
#[test]
fn survivor_listing_cases() {
    let cases: [(&str, io::Result<Vec<u8>>, SurvivorListing); 6] = [
        ("an empty listing", Ok(Vec::new()), SurvivorListing::Empty),
        ("one pid", Ok(b"4242\n".to_vec()), SurvivorListing::Occupied),
        (
            "two pids",
            Ok(b"12\n13\n".to_vec()),
            SurvivorListing::Occupied,
        ),
        (
            "a read error",
            Err(io::Error::other("cgroup.procs cannot be read")),
            SurvivorListing::Unreadable,
        ),
        (
            "a line that is not a pid",
            Ok(b"x\n".to_vec()),
            SurvivorListing::Unreadable,
        ),
        (
            "text that is not UTF-8",
            Ok(vec![0xff]),
            SurvivorListing::Unreadable,
        ),
    ];
    for (name, listing, expected) in cases {
        assert_eq!(
            survivor_listing(listing),
            expected,
            "{name}: the listing must classify as {expected:?}"
        );
    }
}

/// A group that is still being torn down when the stub's exit is observed is read again until it
/// is empty: two occupied listings and then an empty one make no survivors.
#[test]
fn a_group_that_empties_within_the_grace_has_no_survivors() {
    let mut listings =
        vec![Ok(b"7\n8\n".to_vec()), Ok(b"8\n".to_vec()), Ok(Vec::new())].into_iter();
    let mut reads = 0_u32;
    let verdict = await_empty_listing(
        || {
            reads += 1;
            listings
                .next()
                .unwrap_or_else(|| Err(io::Error::other("read past the listings")))
        },
        Duration::from_secs(5),
    );
    assert_eq!(
        verdict,
        SurvivorListing::Empty,
        "the group emptied within the grace"
    );
    assert_eq!(
        reads, 3,
        "the listing must be read until it is empty and no further"
    );
}

/// A listing that stays occupied after the grace is a survivor, and the wait ends.
#[test]
fn a_group_that_stays_occupied_has_survivors() {
    let mut reads = 0_u32;
    let started = Instant::now();
    let verdict = await_empty_listing(
        || {
            reads += 1;
            Ok(b"4242\n".to_vec())
        },
        Duration::from_millis(200),
    );
    assert_eq!(
        verdict,
        SurvivorListing::Occupied,
        "the process outlived the grace"
    );
    assert!(
        reads >= 2,
        "the listing must be read again before the grace ends, read {reads} times"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the wait must end soon after the grace"
    );
}

/// A listing that was occupied and then cannot be read keeps its survivors.
#[test]
fn an_unreadable_listing_after_an_occupied_one_keeps_the_survivors() {
    let mut listings = vec![
        Ok(b"9\n".to_vec()),
        Err(io::Error::other("cgroup.procs cannot be read")),
    ]
    .into_iter();
    let verdict = await_empty_listing(
        || {
            listings
                .next()
                .unwrap_or_else(|| Err(io::Error::other("read past the listings")))
        },
        Duration::from_secs(5),
    );
    assert_eq!(
        verdict,
        SurvivorListing::Occupied,
        "the processes were seen before the failed read"
    );
}

/// A listing that cannot be read ends the wait at once: an unreadable group is not waited on.
#[test]
fn an_unreadable_listing_ends_the_wait() {
    let mut reads = 0_u32;
    let verdict = await_empty_listing(
        || {
            reads += 1;
            Err(io::Error::other("cgroup.procs cannot be read"))
        },
        Duration::from_secs(5),
    );
    assert_eq!(
        verdict,
        SurvivorListing::Unreadable,
        "the listing could not be read"
    );
    assert_eq!(reads, 1, "an unreadable listing must not be read again");
}

// The job tests below need the prepared Linux enforcement runner. Each one runs by its exact name,
// after `prepare.sh scenario` selected its payload, so none of them runs in the local suite.

/// Runs one launch of `plan` in a new execution directory and returns its outcome with the time
/// that it took. The launch must return within `RESPONSIVE_WITHIN`.
pub(super) fn timed_launch(
    label: &str,
    plan: &AttemptPlan,
) -> Result<(Result<AttemptEvidence, AttemptRefusal>, Duration), Box<dyn Error>> {
    let dir = ExecutionDir::new(label)?;
    let started = Instant::now();
    let outcome = launch(plan, dir.path());
    let elapsed = started.elapsed();
    assert!(
        elapsed < RESPONSIVE_WITHIN,
        "the {label} launch took {elapsed:?}, so the supervisor did not stay responsive"
    );
    assert_no_attempt_processes(label)?;
    Ok((outcome, elapsed))
}

/// The names that the kernel gives the processes of an attempt in `/proc/<pid>/comm`: the
/// launcher, and the fixture and payload programs, whose names the kernel cuts to 15 bytes.
const ATTEMPT_PROCESS_NAMES: [&str; 2] = ["bwrap", "artifree-execut"];
/// The prefix of every program path inside the sandbox. A process whose first argument starts
/// with it was started inside an attempt: the payload, a child role or a generated probe, whose
/// `comm` is its own file name and matches no entry of `ATTEMPT_PROCESS_NAMES`.
const SANDBOX_PROGRAM_PREFIX: &str = "/tools/";

/// Requires that no process of an attempt is left on the host once the launch returned
/// (decision 0042 rule 6). It reads `/proc` while the job's unit still runs, so it does not
/// rest on systemd killing what is left when the unit stops. A process that ends while it is
/// listed is skipped.
fn assert_no_attempt_processes(label: &str) -> Result<(), Box<dyn Error>> {
    let mut left = Vec::new();
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(pid) = name.to_str() else {
            continue;
        };
        if pid.is_empty() || !pid.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let Ok(comm) = fs::read_to_string(entry.path().join("comm")) else {
            continue;
        };
        let comm = comm.trim_end();
        let started_inside = fs::read(entry.path().join("cmdline")).is_ok_and(|cmdline| {
            cmdline
                .split(|byte| *byte == 0)
                .next()
                .is_some_and(|program| program.starts_with(SANDBOX_PROGRAM_PREFIX.as_bytes()))
        });
        if ATTEMPT_PROCESS_NAMES.contains(&comm) || started_inside {
            left.push(format!("{pid} {comm}"));
        }
    }
    assert!(
        left.is_empty(),
        "the {label} launch left attempt processes behind: {left:?}"
    );
    Ok(())
}

/// The evidence of a launch that must run. A refusal is an error that names its failure.
pub(super) fn evidence_of(
    outcome: Result<AttemptEvidence, AttemptRefusal>,
) -> Result<AttemptEvidence, Box<dyn Error>> {
    outcome.map_err(|refusal| format!("the attempt was refused: {:?}", refusal.failure).into())
}

/// The refusal of a launch that must not run, with the group's counters.
fn refusal_with_usage(
    outcome: Result<AttemptEvidence, AttemptRefusal>,
) -> Result<AttemptRefusal, Box<dyn Error>> {
    match outcome {
        Ok(evidence) => Err(format!(
            "the attempt must be refused, but it exited with {:?}",
            evidence.exit
        )
        .into()),
        Err(refusal) => Ok(refusal),
    }
}

/// The captured standard output of an attempt, which must be UTF-8 for the payload lines to be
/// read.
pub(super) fn stdout_text(evidence: &AttemptEvidence) -> Result<&str, Box<dyn Error>> {
    Ok(std::str::from_utf8(&evidence.stdout)?)
}

/// The first payload line whose first word is `word`.
pub(super) fn payload_line<'a>(text: &'a str, word: &str) -> Result<&'a str, Box<dyn Error>> {
    text.lines()
        .find(|line| line.split_whitespace().next() == Some(word))
        .ok_or_else(|| format!("the payload printed no `{word}` line").into())
}

/// The value of the `key=value` field `key` in one payload line.
pub(super) fn field<'a>(line: &'a str, key: &str) -> Result<&'a str, Box<dyn Error>> {
    line.split_whitespace()
        .filter_map(|token| token.split_once('='))
        .find_map(|(name, value)| (name == key).then_some(value))
        .ok_or_else(|| format!("the payload line {line:?} has no `{key}` field").into())
}

/// The value of a numeric `key=value` field in one payload line.
pub(super) fn field_number(line: &str, key: &str) -> Result<u64, Box<dyn Error>> {
    let number: u64 = field(line, key)?.parse()?;
    Ok(number)
}

/// Checks the shape that the thread bomb and the process bomb share: the payload stopped at the
/// task limit with `WouldBlock`, and the group counted the refused creations.
fn check_task_bomb(word: &str, evidence: &AttemptEvidence) -> Result<(), Box<dyn Error>> {
    assert_eq!(
        evidence.exit.code(),
        Some(0),
        "the {word} payload must exit 0 after its refusal"
    );
    let text = stdout_text(evidence)?;
    let line = payload_line(text, word)?;
    let made = field_number(line, "made")?;
    assert!(
        (1..TASK_LIMIT).contains(&made),
        "the {word} payload must make at least one task and fewer than {TASK_LIMIT}, got {made}"
    );
    assert_eq!(
        field(line, "refused")?,
        "WouldBlock",
        "the {word} payload must stop with WouldBlock: {line}"
    );
    assert!(
        evidence.usage.task_refusals >= 1,
        "the group must count a refused task, got {}",
        evidence.usage.task_refusals
    );
    let Some(peak) = evidence.usage.tasks_peak else {
        return Err(format!("the {word} group's peak task count is unavailable").into());
    };
    assert!(
        peak <= TASK_LIMIT,
        "the {word} group peaked at {peak} tasks, above the limit of {TASK_LIMIT}"
    );
    Ok(())
}

#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_thread_bomb_stops_at_the_task_limit() -> Result<(), Box<dyn Error>> {
    let (outcome, _) = timed_launch("threads", &AttemptPlan::fixture())?;
    check_task_bomb("threads", &evidence_of(outcome)?)?;
    assert_no_attempt_groups()
}

#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_process_bomb_stops_at_the_task_limit() -> Result<(), Box<dyn Error>> {
    let (outcome, _) = timed_launch("processes", &AttemptPlan::fixture())?;
    check_task_bomb("processes", &evidence_of(outcome)?)?;
    assert_no_attempt_groups()
}

#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_memory_hog_is_killed_with_its_group() -> Result<(), Box<dyn Error>> {
    let (outcome, _) = timed_launch("memory", &AttemptPlan::fixture())?;
    let refusal = refusal_with_usage(outcome)?;
    assert_eq!(
        refusal.failure,
        AttemptFailure::OutOfMemory,
        "the memory hog must end as out of memory"
    );
    let Some(usage) = refusal.usage else {
        return Err("the refused memory hog's counters were not read".into());
    };
    assert!(
        usage.oom_kills >= 1,
        "the group must count an OOM kill, got {}",
        usage.oom_kills
    );
    assert!(
        usage.oom_group_kills >= 1,
        "the group must count a group OOM kill, got {}",
        usage.oom_group_kills
    );
    let Some(peak) = usage.memory_peak_bytes else {
        return Err("the memory hog's peak is unavailable".into());
    };
    assert!(
        peak <= MEMORY_LIMIT_BYTES,
        "the group peaked at {peak} bytes, above its limit of {MEMORY_LIMIT_BYTES}"
    );
    assert_no_attempt_groups()
}

#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_scratch_bytes_are_bounded_and_nothing_else_is_writable() -> Result<(), Box<dyn Error>> {
    let (outcome, _) = timed_launch("scratch-bytes", &AttemptPlan::fixture())?;
    let evidence = evidence_of(outcome)?;
    assert_eq!(
        evidence.exit.code(),
        Some(0),
        "the scratch-bytes payload must exit 0"
    );
    let text = stdout_text(&evidence)?;
    let line = payload_line(text, "scratch")?;
    assert_eq!(
        field(line, "bytes")?,
        SCRATCH_BYTES,
        "the scratch must hold exactly its budget: {line}"
    );
    assert_eq!(
        field(line, "refused")?,
        "StorageFull",
        "the scratch must refuse the write past its budget with ENOSPC: {line}"
    );
    let outside: Vec<(&str, &str)> = text
        .lines()
        .filter_map(|line| line.strip_prefix("outside "))
        .filter_map(|rest| rest.split_once('='))
        .collect();
    let paths: Vec<&str> = outside.iter().map(|(path, _)| *path).collect();
    assert_eq!(
        paths, OUTSIDE_PATHS,
        "the payload must try each path outside the scratch once"
    );
    for (path, result) in &outside {
        assert_ne!(
            *result, "created",
            "the payload created {path} outside the scratch"
        );
    }
    assert_no_attempt_groups()
}

#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_scratch_inodes_are_bounded() -> Result<(), Box<dyn Error>> {
    let (outcome, _) = timed_launch("scratch-inodes", &AttemptPlan::fixture())?;
    let evidence = evidence_of(outcome)?;
    assert_eq!(
        evidence.exit.code(),
        Some(0),
        "the scratch-inodes payload must exit 0"
    );
    let text = stdout_text(&evidence)?;
    let line = payload_line(text, "scratch")?;
    assert_eq!(
        field(line, "files")?,
        // 4,096 inodes: the root, the keeper's output root `out` (0056, amendment of 2026-11-13,
        // input and output) and 4,094 files.
        "4094",
        "the scratch must hold exactly 4094 files beside its root and the output root: {line}"
    );
    assert_eq!(
        field(line, "refused")?,
        "StorageFull",
        "the scratch must refuse the next inode with ENOSPC: {line}"
    );
    let removed = payload_line(text, "after-remove")?;
    assert_eq!(
        field(removed, "mkdir")?,
        "ok",
        "a directory must be created once one file is removed: {removed}"
    );
    assert_no_attempt_groups()
}

#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_log_flood_stops_at_the_output_budget() -> Result<(), Box<dyn Error>> {
    let (outcome, _) = timed_launch("flood", &AttemptPlan::fixture())?;
    assert_eq!(
        refusal_of(outcome)?,
        AttemptFailure::OutputLimitExceeded,
        "the flood must be stopped by the output budget"
    );
    assert_no_attempt_groups()
}

#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_hang_is_killed_at_the_wall_deadline() -> Result<(), Box<dyn Error>> {
    let plan = AttemptPlan {
        wall_deadline: Duration::from_secs(2),
        ..AttemptPlan::fixture()
    };
    let (outcome, elapsed) = timed_launch("hang", &plan)?;
    assert_eq!(
        refusal_of(outcome)?,
        AttemptFailure::DeadlineExceeded,
        "the hang must end at the wall deadline"
    );
    assert!(
        (Duration::from_secs(2)..=Duration::from_secs(10)).contains(&elapsed),
        "the hang must end between 2 s and 10 s after its launch, took {elapsed:?}"
    );
    assert_no_attempt_groups()
}

#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_cpu_spinners_share_one_cpu() -> Result<(), Box<dyn Error>> {
    let (outcome, _) = timed_launch("cpu", &AttemptPlan::fixture())?;
    let evidence = evidence_of(outcome)?;
    assert_eq!(evidence.exit.code(), Some(0), "the cpu payload must exit 0");
    let text = stdout_text(&evidence)?;
    let line = payload_line(text, "spinners")?;
    assert_eq!(
        field(line, "done")?,
        "4",
        "all four spinners must finish: {line}"
    );
    assert!(
        evidence.usage.cpu_throttled_periods >= 1,
        "the spinners must be throttled, got {} throttled periods",
        evidence.usage.cpu_throttled_periods
    );
    let wall_us = u64::try_from(evidence.wall_time.as_micros())?;
    let allowed_us = wall_us * 5 / 4 + 250_000;
    assert!(
        evidence.usage.cpu_usage_usec <= allowed_us,
        "four spinners used {} us of CPU in {wall_us} us of wall time, above one CPU and the allowance of {allowed_us} us",
        evidence.usage.cpu_usage_usec
    );
    assert_no_attempt_groups()
}

/// The survivors branch of the verdict is not reached here: the launcher's init takes its PID
/// namespace with it, so the group's list is empty once that teardown has ended, which the
/// provider waits for up to `TEARDOWN_GRACE` after the stub's exit (the launcher can exit first).
/// That branch is covered by the verdict and listing tables above. What this test shows is that
/// nothing outlives the attempt, through `settle`'s kill and the `/proc` scan of `timed_launch`.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_background_processes_do_not_outlive_a_normal_exit() -> Result<(), Box<dyn Error>> {
    let (outcome, _) = timed_launch("survivors", &AttemptPlan::fixture())?;
    let evidence = evidence_of(outcome)?;
    assert_eq!(
        evidence.exit.code(),
        Some(0),
        "the survivors payload must exit 0 with its children alive"
    );
    let text = stdout_text(&evidence)?;
    let detached = payload_line(text, "detached")?;
    assert_eq!(
        field(detached, "setsid")?,
        "ok",
        "the detached child must start a session: {detached}"
    );
    let ignorer = payload_line(text, "ignorer")?;
    assert_eq!(
        field(ignorer, "alive-after-term")?,
        "true",
        "the ignoring child must survive SIGTERM: {ignorer}"
    );
    assert!(
        text.lines().any(|line| line == "background"),
        "the detached child must print a background line before the exit"
    );
    assert_no_attempt_groups()
}

#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_background_processes_do_not_outlive_the_deadline() -> Result<(), Box<dyn Error>> {
    let plan = AttemptPlan {
        wall_deadline: Duration::from_secs(4),
        ..AttemptPlan::fixture()
    };
    let (outcome, _) = timed_launch("survivors-hang", &plan)?;
    assert_eq!(
        refusal_of(outcome)?,
        AttemptFailure::DeadlineExceeded,
        "the hung survivors must end at the wall deadline"
    );
    assert_no_attempt_groups()
}
