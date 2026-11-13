//! Behavioral tests of the crash part of one attempt (decision 0056 rule 14, row R02ag, with the
//! amendment of 2026-11-13, crashes, and decision 0042 rules 5 and 6): a crashing payload and its
//! children leave no core, a routing that would pipe or socket the core is refused before any group
//! exists, and a routing change during an attempt refuses it and quarantines the process.
//!
//! The line parser is the only test that runs in the normal `cargo test`. Every `job_*` test is
//! ignored here, because each one needs the Linux enforcement runner with `LimitCORE=1M`.
//! `scripts/enforcement/run.sh` (part 10) prepares each scenario or routing with `prepare.sh` and
//! runs one test by its exact name. The collector counters of the pipe and socket routings are
//! checked by run.sh, not here.

use super::super::AdmissionRefusal;
use super::resource_tests::{evidence_of, field, stdout_text};
use super::tests::{ExecutionDir, assert_no_attempt_groups, refusal_of};
use super::{AttemptEvidence, AttemptFailure, AttemptPlan, OutputRefusal, launch};

use std::env;
use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

/// The core limit, in bytes, that the unit's `LimitCORE=1M` sets for both the soft and the hard
/// value, as `/proc/self/limits` prints it.
const UNIT_CORE_LIMIT: &str = "1048576";
/// The directory where systemd-coredump keeps the cores that it stores.
const STORED_CORES: &str = "/var/lib/systemd/coredump";
/// The prefix of the scratch listing line of the output root, before its size.
const OUTPUT_ROOT_LINE: &str = "out\tdirectory\t";
/// The environment variable that names the markers directory. run.sh sets it on the unit, and the
/// test only reads it.
const MARKERS_VARIABLE: &str = "ARTIFREE_ROUTING_MARKERS";
/// The marker that the routing-change test creates once its first launch was refused.
const FIRST_MARKER: &str = "first";
/// The marker that run.sh creates once the routing is restored.
const RESTORED_MARKER: &str = "restored";
/// How long the routing-change test waits for the restored marker.
const RESTORE_WITHIN: Duration = Duration::from_secs(60);
/// The pause between two looks at the markers directory.
const MARKER_POLL: Duration = Duration::from_millis(100);

/// Requires that this test process runs under the unit's core limit, so that a job test exercises
/// the limit that the bootstrap must lower. Both the soft and the hard value must be
/// `UNIT_CORE_LIMIT`.
fn require_unit_core_limit() -> Result<(), Box<dyn Error>> {
    let limits = fs::read_to_string("/proc/self/limits")?;
    let (soft, hard) = core_limit_words(&limits)?;
    assert!(
        soft == UNIT_CORE_LIMIT && hard == UNIT_CORE_LIMIT,
        "the test process must run under LimitCORE=1M, got soft={soft} hard={hard}"
    );
    Ok(())
}

/// The soft and the hard words of the `Max core file size` line of a `/proc/<pid>/limits` text.
fn core_limit_words(limits: &str) -> Result<(&str, &str), Box<dyn Error>> {
    let rest = limits
        .lines()
        .find_map(|line| line.strip_prefix("Max core file size"))
        .ok_or("the limits text has no core file size line")?;
    let mut words = rest.split_whitespace();
    match (words.next(), words.next()) {
        (Some(soft), Some(hard)) => Ok((soft, hard)),
        _ => Err(format!("the core file size line has no soft and hard word: {rest:?}").into()),
    }
}

/// The number of entries in the directory where systemd-coredump keeps stored cores. A missing
/// directory holds none.
fn stored_cores() -> Result<usize, Box<dyn Error>> {
    let entries = match fs::read_dir(STORED_CORES) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    let mut count = 0;
    for entry in entries {
        entry?;
        count += 1;
    }
    Ok(count)
}

/// What one child line reports: the signal that ended the child, or `None` for `signal=none`, and
/// whether the kernel reported a core.
type ChildOutcome = (Option<u32>, bool);

/// The outcome of one child, read from the first payload line that starts with `prefix`.
///
/// # Errors
///
/// Returns an error when no line starts with `prefix`, when a field is missing, when the signal is
/// not a number, or when the core flag is neither `0` nor `1`.
fn child_outcome(text: &str, prefix: &str) -> Result<ChildOutcome, Box<dyn Error>> {
    let line = text
        .lines()
        .find(|line| line.starts_with(prefix))
        .ok_or_else(|| format!("no payload line starts with {prefix:?}"))?;
    let signal = match field(line, "signal")? {
        "none" => None,
        number => Some(number.parse::<u32>()?),
    };
    let core = match field(line, "core")? {
        "0" => false,
        "1" => true,
        other => return Err(format!("the core flag must be 0 or 1, got {other:?}").into()),
    };
    Ok((signal, core))
}

/// Whether the payload's standard output holds `line` as one of its lines.
fn has_line(text: &str, line: &str) -> bool {
    text.lines().any(|candidate| candidate == line)
}

/// The scratch listing of an attempt that asked for it. Any other value is an error that names it.
fn scratch_lines(evidence: &AttemptEvidence) -> Result<&[String], Box<dyn Error>> {
    match &evidence.scratch {
        Some(Ok(lines)) => Ok(lines.as_slice()),
        other => Err(format!("the scratch was not listed: {other:?}").into()),
    }
}

/// Requires that the scratch holds exactly the empty output root, with a size, and nothing else.
fn assert_only_output_root(lines: &[String]) {
    let only_root = match lines {
        [only] => only
            .strip_prefix(OUTPUT_ROOT_LINE)
            .is_some_and(|size| size.parse::<u64>().is_ok()),
        _ => false,
    };
    assert!(
        only_root,
        "the scratch must hold only the output root `out` as one directory line, got {lines:?}"
    );
}

/// Runs one crash scenario in the fixture plan with the scratch listed, then checks in this order
/// what it must leave behind: the scratch holds only the output root, no child printed a core flag,
/// systemd-coredump stored nothing new, each child ended by its signal, the stub did not succeed,
/// the payload printed its zero limits, the output was not collected, and no group remains.
///
/// `children` pairs each child's line prefix with the signal that must end it. `limit_lines` are
/// the payload lines that show the core limits after the exec.
fn assert_crash_leaves_no_core(
    scenario: &str,
    children: &[(&str, u32)],
    limit_lines: &[&str],
) -> Result<(), Box<dyn Error>> {
    require_unit_core_limit()?;
    let cores_before = stored_cores()?;
    let plan = AttemptPlan {
        list_scratch: true,
        ..AttemptPlan::fixture()
    };
    let dir = ExecutionDir::new(scenario)?;
    let evidence = evidence_of(launch(&plan, dir.path()))?;
    let text = stdout_text(&evidence)?;
    assert_only_output_root(scratch_lines(&evidence)?);
    let mut signals = Vec::new();
    for (prefix, _) in children {
        let (signal, core) = child_outcome(text, prefix)?;
        assert!(
            !core,
            "the {scenario} child line {prefix:?} must report no core"
        );
        signals.push(signal);
    }
    assert_eq!(
        stored_cores()?,
        cores_before,
        "systemd-coredump stored a core during the {scenario} attempt"
    );
    for ((prefix, expected), signal) in children.iter().zip(signals) {
        assert_eq!(
            signal,
            Some(*expected),
            "the child line {prefix:?} must report signal {expected}"
        );
    }
    assert!(
        !evidence.exit.success(),
        "the {scenario} stub must end by a signal, got {:?}",
        evidence.exit
    );
    for line in limit_lines {
        assert!(
            has_line(text, line),
            "the payload must print {line:?}, so its core limits are zero after the exec"
        );
    }
    assert_eq!(
        evidence.output.as_ref().err().copied(),
        Some(OutputRefusal::NotCollected),
        "the output of the {scenario} attempt must not be collected"
    );
    assert_no_attempt_groups()
}

#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_crashes_leave_no_core() -> Result<(), Box<dyn Error>> {
    assert_crash_leaves_no_core(
        "crash",
        &[("crash child ", 11)],
        &[
            "crash limits soft=0 hard=0",
            "crash-child limits soft=0 hard=0",
        ],
    )
}

#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_setter_crashes_leave_no_core() -> Result<(), Box<dyn Error>> {
    assert_crash_leaves_no_core(
        "crash-setter",
        &[
            ("crash-setter child=limit ", 31),
            ("crash-setter child=dumpable ", 31),
        ],
        &["crash-setter limits soft=0 hard=0"],
    )
}

/// Requires that a launch of the fixture plan is refused as `expected` before any group exists,
/// and that no attempt group remains. The counters of the collectors are checked by run.sh.
fn assert_refused_before_any_group(
    label: &str,
    expected: AdmissionRefusal,
) -> Result<(), Box<dyn Error>> {
    require_unit_core_limit()?;
    let dir = ExecutionDir::new(label)?;
    let failure = refusal_of(launch(&AttemptPlan::fixture(), dir.path()))?;
    assert_eq!(
        failure,
        AttemptFailure::Refused(expected),
        "the {label} launch must be refused before any group exists"
    );
    assert_no_attempt_groups()
}

/// The pipe routing (`|` at the start of `kernel.core_pattern`) must be refused, so that no
/// collector program is started by a crash. run.sh counts the collector's invocations.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_pipe_routing_reaches_no_collector() -> Result<(), Box<dyn Error>> {
    assert_refused_before_any_group("pipe-routing", AdmissionRefusal::PipeCoreRouting)
}

/// The socket routing (`@` at the start of `kernel.core_pattern`) must be refused, so that no
/// listener receives a core. run.sh counts the listener's connections.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_socket_routing_reaches_no_collector() -> Result<(), Box<dyn Error>> {
    assert_refused_before_any_group("socket-routing", AdmissionRefusal::SocketCoreRouting)
}

/// An absolute routing path is not a reviewed literal name, so it must be refused as unknown.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_absolute_routing_is_refused() -> Result<(), Box<dyn Error>> {
    assert_refused_before_any_group("absolute-routing", AdmissionRefusal::UnknownCoreRouting)
}

/// Polls for `path` to exist and requires that it does within `RESTORE_WITHIN`.
fn wait_for_marker(path: &Path) -> Result<(), Box<dyn Error>> {
    let started = Instant::now();
    loop {
        if path.exists() {
            return Ok(());
        }
        if started.elapsed() >= RESTORE_WITHIN {
            return Err(format!(
                "{} did not appear within {RESTORE_WITHIN:?}",
                path.display()
            )
            .into());
        }
        thread::sleep(MARKER_POLL);
    }
}

/// A routing change while an attempt runs ends the group and refuses the attempt as `Routing`,
/// with the admitted bytes differing. The process is then quarantined: a second launch is refused
/// as `Quarantined`, even after run.sh restored the routing.
///
/// run.sh starts the linger scenario, changes the routing once the group exists, then waits for
/// the first marker and restores the routing before it creates the restored marker.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_a_routing_change_stops_the_attempt() -> Result<(), Box<dyn Error>> {
    require_unit_core_limit()?;
    let markers = PathBuf::from(
        env::var_os(MARKERS_VARIABLE).ok_or("run.sh must set the routing markers variable")?,
    );
    let dir = ExecutionDir::new("routing-change")?;
    let plan = AttemptPlan::fixture();
    let first = refusal_of(launch(&plan, dir.path()))?;
    assert_eq!(
        first,
        AttemptFailure::Routing(AdmissionRefusal::CoreRoutingChanged),
        "a routing change during the attempt must refuse it as Routing"
    );
    assert_no_attempt_groups()?;
    fs::write(markers.join(FIRST_MARKER), b"")?;
    wait_for_marker(&markers.join(RESTORED_MARKER))?;
    let second = refusal_of(launch(&plan, dir.path()))?;
    assert_eq!(
        second,
        AttemptFailure::Quarantined,
        "a launch after a routing change must be refused as Quarantined, even after the restore"
    );
    Ok(())
}

/// The parser reads the signal and the core flag of each child line, and refuses a line whose
/// signal or core flag is malformed, or that is absent.
#[test]
fn crash_lines_are_parsed() {
    let sample = "crash limits soft=0 hard=0\n\
        crash child signal=11 core=0\n\
        crash-setter child=limit signal=31 core=1\n\
        crash-setter child=dumpable signal=none core=0\n\
        bad child signal=6 core=2\n\
        torn child signal=x core=0\n";
    let cases: [(&str, Option<ChildOutcome>); 6] = [
        ("crash child ", Some((Some(11), false))),
        ("crash-setter child=limit ", Some((Some(31), true))),
        ("crash-setter child=dumpable ", Some((None, false))),
        ("bad child ", None),
        ("torn child ", None),
        ("missing ", None),
    ];
    for (prefix, expected) in cases {
        let parsed = child_outcome(sample, prefix).ok();
        assert_eq!(
            parsed, expected,
            "the line starting with {prefix:?} must parse to {expected:?}, where None is a refusal"
        );
    }
}
