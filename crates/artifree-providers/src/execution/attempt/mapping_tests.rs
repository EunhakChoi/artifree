//! Behavioral tests of the syscall mapping and the descriptor ceiling (decision 0056 rule 14,
//! row R02ad, with its amendment of 2026-11-13 on the syscall mapping; decision 0042 rules 4
//! and 5).
//!
//! The tests check the kernel records of the window that each launch opens: the generated probes
//! and their refusals, the answer to `clone3`, the refusal of a thread and of a descendant, the
//! refused lowering of the soft limit, the descriptor ceiling, and the boundary of every task
//! outside the launcher.
//!
//! Every test is a `job_*` test that is ignored in `cargo test`. `scripts/enforcement/run.sh`
//! selects one payload scenario with `prepare.sh scenario <name>` and runs one test by its exact
//! name. The exit statuses are not the evidence: the kernel records are.

use super::super::bootstrap::PAYLOAD;
use super::super::launcher::TOOL_ROOT;
use super::super::probes::{AUDIT_ARCH_X86_64, Expect, PROBES};
use super::super::readiness::{LimitFacts, StatusFacts, parse_limits, parse_status};
use super::AttemptPlan;
use super::kernel_log::{KernelLog, RECORD_WAIT, SeccompRecord, await_records};
use super::resource_tests::{
    evidence_of, field, field_number, payload_line, stdout_text, timed_launch,
};
use super::tests::{assert_no_attempt_groups, attempt_groups};

use std::error::Error;
use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

/// The directory that the probes scenario runs the probe programs from, as the kernel names them.
const PROBE_DIR: &str = "/tools/probes";
/// The signal of a seccomp refusal, `SIGSYS`, as the kernel records it.
const SIGSYS: u32 = 31;
/// The exit status that the launcher reports for a child killed by `SIGSYS` (128 plus 31).
const KILLED_BY_SIGSYS: i32 = 159;
/// The exit status of a probe whose `clone3` the filter answers with `ENOSYS` (38).
const ENOSYS_EXIT: i32 = 38;
/// The exit status of a probe whose `clone3` the kernel rejects with `EINVAL` (22).
const EINVAL_EXIT: i32 = 22;
/// The system-call number of `unshare`.
const UNSHARE: u64 = 272;
/// The system-call number of `prlimit64`, which both `setrlimit` and `prlimit` use.
const PRLIMIT64: u64 = 302;
/// The `Name` of the launcher's tasks, which the boundary check skips.
const LAUNCHER_NAME: &str = "bwrap";
/// The interval between two reads of the attempt groups while the watcher waits.
const POLL: Duration = Duration::from_millis(20);

/// One seccomp record that a job test expects in its window.
struct ExpectedRecord {
    /// What the record is, named in the failure message.
    label: String,
    /// The executable path that the record names.
    exe: String,
    /// The audit architecture that the record names.
    arch: u32,
    /// The system-call number that the record names.
    syscall: u64,
}

/// The record of the payload's refusal of `syscall` under the filter.
fn payload_refusal(syscall: u64) -> ExpectedRecord {
    ExpectedRecord {
        label: format!("the payload's refusal of syscall {syscall}"),
        exe: PAYLOAD.to_owned(),
        arch: AUDIT_ARCH_X86_64,
        syscall,
    }
}

/// The path that a probe runs from, which the kernel names in its record.
fn probe_path(name: &str) -> String {
    format!("{PROBE_DIR}/{name}")
}

/// The records that the probes must leave: one for each probe whose expectation is a refusal,
/// in table order.
fn refused_probe_records() -> Vec<ExpectedRecord> {
    PROBES
        .iter()
        .filter_map(|probe| match probe.expect {
            Expect::Refused { arch, number } => Some(ExpectedRecord {
                label: format!("the probe {} of class {}", probe.name, probe.class),
                exe: probe_path(probe.name),
                arch,
                syscall: u64::from(number),
            }),
            Expect::Allowed | Expect::Enosys => None,
        })
        .collect()
}

/// Requires that the window holds exactly the `expected` records: each one exactly once, with the
/// signal `SIGSYS`, and no other seccomp record.
fn require_exactly(records: &[SeccompRecord], expected: &[ExpectedRecord]) {
    for wanted in expected {
        let found = records
            .iter()
            .filter(|record| {
                record.exe == wanted.exe
                    && record.sig == SIGSYS
                    && record.arch == wanted.arch
                    && record.syscall == wanted.syscall
            })
            .count();
        assert_eq!(
            found, 1,
            "{} must be recorded exactly once, found {found} in {records:?}",
            wanted.label
        );
    }
    assert_eq!(
        records.len(),
        expected.len(),
        "the window must hold the expected seccomp records and no other, got {records:?}"
    );
}

/// Whether some output line starts with `prefix`.
fn begins_a_line(text: &str, prefix: &str) -> bool {
    text.lines().any(|line| line.starts_with(prefix))
}

/// The first output line that starts with `prefix`.
pub(super) fn line_with<'a>(text: &'a str, prefix: &str) -> Result<&'a str, Box<dyn Error>> {
    text.lines()
        .find(|line| line.starts_with(prefix))
        .ok_or_else(|| format!("the output has no line that starts with `{prefix}`").into())
}

/// Checks the probe lines of the probes scenario: one line for each probe, and the summary line
/// that closes them.
fn check_probe_report(text: &str) -> Result<(), Box<dyn Error>> {
    let mut reported: Vec<&str> = Vec::new();
    for line in text.lines() {
        if line.split_whitespace().next() == Some("probe") {
            reported.push(field(line, "name")?);
        }
    }
    reported.sort_unstable();
    let mut wanted: Vec<&str> = PROBES.iter().map(|probe| probe.name).collect();
    wanted.sort_unstable();
    assert_eq!(
        reported, wanted,
        "the probes report must name each probe exactly once"
    );
    assert_eq!(
        payload_line(text, "probes")?,
        format!("probes done count={}", PROBES.len())
    );
    Ok(())
}

/// The report line of the probe `name` in the probes scenario.
fn probe_line<'a>(text: &'a str, name: &str) -> Result<&'a str, Box<dyn Error>> {
    text.lines()
        .find(|line| {
            line.split_whitespace().next() == Some("probe")
                && field(line, "name").is_ok_and(|found| found == name)
        })
        .ok_or_else(|| format!("the probes report has no line for {name}").into())
}

/// The exit code that the probes report printed for the probe `name`.
fn probe_code<'a>(text: &'a str, name: &str) -> Result<&'a str, Box<dyn Error>> {
    field(probe_line(text, name)?, "code")
}

/// Whether a probe report line shows a normal exit: no signal, and a code that is an exit status
/// (0 to 255). A probe killed before its call, or by any signal other than `SIGSYS`, leaves no
/// record, so the records alone do not show that the call ran.
fn exited_normally(line: &str) -> bool {
    field(line, "signal").is_ok_and(|signal| signal == "none")
        && field(line, "code").is_ok_and(|code| code.parse::<u8>().is_ok())
}

/// Checks the report of one descriptor role: `prefix` is `descriptors` for the payload and
/// `descriptors-child` for the child that it starts.
fn check_descriptor_report(text: &str, prefix: &str) -> Result<(), Box<dyn Error>> {
    let limits = line_with(text, &format!("{prefix} limits "))?;
    assert_eq!(
        field(limits, "soft")?,
        "256",
        "the {prefix} soft descriptor limit"
    );
    assert_eq!(
        field(limits, "hard")?,
        "256",
        "the {prefix} hard descriptor limit"
    );
    assert_eq!(
        field(limits, "core-soft")?,
        "0",
        "the {prefix} soft core limit"
    );
    assert_eq!(
        field(limits, "core-hard")?,
        "0",
        "the {prefix} hard core limit"
    );
    let open: Vec<u32> = field(limits, "open")?
        .split(',')
        .map(str::parse::<u32>)
        .collect::<Result<Vec<u32>, _>>()?;
    for fd in [0, 1, 2] {
        assert!(
            open.contains(&fd),
            "the {prefix} report must hold descriptor {fd}, got {open:?}"
        );
    }
    assert!(
        open.iter().all(|fd| *fd <= 3),
        "the {prefix} report may hold only descriptors 0 to 3, got {open:?}"
    );
    let opened = line_with(text, &format!("{prefix} opened="))?;
    let duplicated = line_with(text, &format!("{prefix} duplicated="))?;
    for line in [opened, duplicated] {
        assert_eq!(
            field(line, "highest")?,
            "255",
            "the descriptor count must stop at 255 in {line:?}"
        );
        assert_eq!(
            field(line, "error")?,
            "24",
            "the descriptor count must stop with EMFILE (24) in {line:?}"
        );
    }
    let kept = field_number(opened, "opened")?;
    assert!(
        kept >= 1,
        "the {prefix} report must open at least one file, got {kept}"
    );
    Ok(())
}

/// Runs one payload scenario that the filter must kill with `SIGSYS` on `syscall`. The output must
/// begin the scenario and must not reach the line that the refusal prevents, and the kernel must
/// hold exactly the payload's record for that call.
fn check_refused_by_kill(
    label: &str,
    begun: &str,
    prevented: &str,
    syscall: u64,
) -> Result<(), Box<dyn Error>> {
    let mut log = KernelLog::open_at_end()?;
    let (outcome, _) = timed_launch(label, &AttemptPlan::fixture())?;
    let evidence = evidence_of(outcome)?;
    assert_eq!(
        evidence.exit.code(),
        Some(KILLED_BY_SIGSYS),
        "the {label} payload must be killed with SIGSYS (exit 159)"
    );
    let text = stdout_text(&evidence)?;
    assert!(
        begins_a_line(text, begun),
        "the {label} payload must print `{begun}`, got {text:?}"
    );
    assert!(
        !begins_a_line(text, prevented),
        "the {label} payload must not reach `{prevented}`, got {text:?}"
    );
    let records = await_records(&mut log, 1, RECORD_WAIT)?;
    require_exactly(&records, &[payload_refusal(syscall)]);
    assert_no_attempt_groups()
}

/// Stops the boundary watcher when it is dropped, so that the watcher ends even when the launch
/// panics.
pub(super) struct StopOnDrop<'a>(pub(super) &'a AtomicBool);

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// What the watcher read from one process of the attempt group.
struct ProcessReading {
    /// The process id, for the failure messages.
    pid: u32,
    /// The process's limits, from `/proc/<pid>/limits`.
    limits: LimitFacts,
    /// The status of each task other than the launcher, with its task id.
    tasks: Vec<(u32, StatusFacts)>,
}

/// The error number `ESRCH`, which a `/proc` read returns for a task that ended after it was listed.
const ESRCH: i32 = 3;

/// Whether a `/proc` read failed because the process or task ended: the file is gone
/// (`ENOENT`) or the task was reaped between the listing and the read (`ESRCH`).
pub(super) fn process_ended(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(ESRCH)
}

/// The `Name` field of a `/proc/<pid>/status` text.
fn status_name(text: &str) -> Option<&str> {
    text.lines()
        .find_map(|line| line.strip_prefix("Name:"))
        .map(str::trim)
}

/// Reads the tasks other than the launcher of one process, and its limits. It returns `None` when
/// the process ended, or when it has no task other than the launcher.
fn read_process(pid: u32) -> Result<Option<ProcessReading>, Box<dyn Error>> {
    let entries = match fs::read_dir(format!("/proc/{pid}/task")) {
        Ok(entries) => entries,
        Err(error) if process_ended(&error) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut tasks = Vec::new();
    for entry in entries {
        let entry = entry?;
        let tid: u32 = entry.file_name().to_string_lossy().parse()?;
        let text = match fs::read_to_string(entry.path().join("status")) {
            Ok(text) => text,
            Err(error) if process_ended(&error) => continue,
            Err(error) => return Err(error.into()),
        };
        if status_name(&text) == Some(LAUNCHER_NAME) {
            continue;
        }
        let status = parse_status(&text).map_err(|failure| {
            format!("task {tid} of process {pid} has an unreadable status: {failure:?}")
        })?;
        tasks.push((tid, status));
    }
    if tasks.is_empty() {
        return Ok(None);
    }
    let limits_text = match fs::read_to_string(format!("/proc/{pid}/limits")) {
        Ok(text) => text,
        Err(error) if process_ended(&error) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let limits = parse_limits(&limits_text)
        .map_err(|failure| format!("process {pid} has unreadable limits: {failure:?}"))?;
    Ok(Some(ProcessReading { pid, limits, tasks }))
}

/// Reads one attempt group. It returns the processes that the group lists when it lists at least
/// two processes other than the launcher and one of them has two or more tasks; otherwise `None`.
fn read_group(group: &Path) -> Result<Option<Vec<ProcessReading>>, Box<dyn Error>> {
    let listing = match fs::read_to_string(group.join("cgroup.procs")) {
        Ok(listing) => listing,
        Err(error) if process_ended(&error) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut processes = Vec::new();
    for line in listing.lines() {
        let pid: u32 = line.trim().parse()?;
        if let Some(process) = read_process(pid)? {
            processes.push(process);
        }
    }
    let threaded = processes.iter().any(|process| process.tasks.len() >= 2);
    if processes.len() >= 2 && threaded {
        Ok(Some(processes))
    } else {
        Ok(None)
    }
}

/// Polls the attempt groups until one of them shows the payload with two threads and a second
/// process, and returns that reading. It gives up when `stop` is set, and reports the last listing
/// error that it saw.
fn watch_boundary(stop: &AtomicBool) -> Result<Vec<ProcessReading>, Box<dyn Error>> {
    let mut last = String::from("no attempt group was listed");
    while !stop.load(Ordering::SeqCst) {
        match attempt_groups() {
            Ok(groups) => {
                for group in groups {
                    if let Some(processes) = read_group(&group)? {
                        return Ok(processes);
                    }
                }
            }
            Err(error) => last = error.to_string(),
        }
        thread::sleep(POLL);
    }
    Err(format!(
        "the launch ended before the payload had two threads and a child; last listing: {last}"
    )
    .into())
}

/// Checks the boundary of one task: the no-new-privileges flag, the filter, the empty capability
/// sets, and the descriptor and core limits of its process.
fn check_task(pid: u32, tid: u32, status: &StatusFacts, limits: &LimitFacts) {
    let place = format!("task {tid} of process {pid}");
    let status_wanted: [(&str, u64, u64); 8] = [
        ("NoNewPrivs", status.no_new_privs, 1),
        ("Seccomp", status.seccomp, 2),
        ("Seccomp_filters", status.seccomp_filters, 2),
        ("CapInh", status.cap_inh, 0),
        ("CapPrm", status.cap_prm, 0),
        ("CapEff", status.cap_eff, 0),
        ("CapBnd", status.cap_bnd, 0),
        ("CapAmb", status.cap_amb, 0),
    ];
    for (name, actual, wanted) in status_wanted {
        assert_eq!(actual, wanted, "{name} of the {place}");
    }
    let limits_wanted: [(&str, Option<u64>, Option<u64>); 4] = [
        ("Max open files soft", limits.open_files.soft, Some(256)),
        ("Max open files hard", limits.open_files.hard, Some(256)),
        ("Max core file size soft", limits.core_bytes.soft, Some(0)),
        ("Max core file size hard", limits.core_bytes.hard, Some(0)),
    ];
    for (name, actual, wanted) in limits_wanted {
        assert_eq!(actual, wanted, "{name} of the {place}");
    }
}

/// Checks every task that the watcher read, and returns how many there were.
fn check_processes(processes: &[ProcessReading]) -> usize {
    let mut checked: usize = 0;
    for process in processes {
        for (tid, status) in &process.tasks {
            check_task(process.pid, *tid, status, &process.limits);
            checked += 1;
        }
    }
    checked
}

/// Requires that the probe `clone3` structure is rejected outside the filter: the kernel
/// implements `clone3`, so the refusal under the filter is the filter's and not the kernel's.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_clone3_control_outside_the_sandbox() -> Result<(), Box<dyn Error>> {
    let probe = Path::new(TOOL_ROOT).join("probes").join("ns_clone3");
    let status = Command::new(&probe).env_clear().status()?;
    assert_eq!(
        status.code(),
        Some(EINVAL_EXIT),
        "the kernel must reject the clone3 structure with EINVAL (22) outside the filter"
    );
    Ok(())
}

/// Runs every generated probe under the filter. Each refused probe leaves exactly its record, the
/// allowed and `ENOSYS` probes exit on their own and leave none, and the window holds no other
/// seccomp record.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_probes_are_refused_with_their_records() -> Result<(), Box<dyn Error>> {
    let mut log = KernelLog::open_at_end()?;
    let (outcome, _) = timed_launch("probes", &AttemptPlan::fixture())?;
    let evidence = evidence_of(outcome)?;
    assert_eq!(
        evidence.exit.code(),
        Some(0),
        "the probes payload must exit 0"
    );
    let text = stdout_text(&evidence)?;
    check_probe_report(text)?;
    // The two x32 probes are expected like the others, so their records are required. The mapping
    // document marks the x32 class FAILED on this kernel, because it has no control here, so these
    // two records are not counted as evidence that the x32 class is mapped.
    let expected = refused_probe_records();
    let records = await_records(&mut log, expected.len(), RECORD_WAIT)?;
    for probe in PROBES
        .iter()
        .filter(|probe| !matches!(probe.expect, Expect::Refused { .. }))
    {
        // The probe must have run its call and exited on its own. A probe that died before the
        // call, or of another signal, leaves no record either, so the absence of a record is not
        // enough.
        let line = probe_line(text, probe.name)?;
        assert!(
            exited_normally(line),
            "the probe {} of class {} must exit with a code and no signal, got {line:?}",
            probe.name,
            probe.class
        );
        let exe = probe_path(probe.name);
        let found = records.iter().filter(|record| record.exe == exe).count();
        assert_eq!(
            found, 0,
            "the probe {} of class {} must leave no record, found {found}",
            probe.name, probe.class
        );
    }
    require_exactly(&records, &expected);
    for name in [
        "ok_prlimit_query",
        "ok_fork_child_gettid",
        "ns_fork_child_unshare",
    ] {
        assert_eq!(
            probe_code(text, name)?,
            "0",
            "the parent of {name} must exit 0"
        );
    }
    // The `ns_clone3` probe prints code 38 (ENOSYS) here. The code is not asserted: the records
    // decide, and the answer of `clone3` is checked by its own job test.
    assert_no_attempt_groups()
}

/// Runs the `clone3` payload, which replaces itself with the `ns_clone3` probe. The filter answers
/// the call with `ENOSYS`, so the probe exits 38 and the window holds no record.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_clone3_is_refused_with_enosys() -> Result<(), Box<dyn Error>> {
    let mut log = KernelLog::open_at_end()?;
    let (outcome, _) = timed_launch("clone3", &AttemptPlan::fixture())?;
    let evidence = evidence_of(outcome)?;
    assert_eq!(
        evidence.exit.code(),
        Some(ENOSYS_EXIT),
        "the clone3 payload must exit with ENOSYS (38)"
    );
    let records = await_records(&mut log, 0, RECORD_WAIT)?;
    require_exactly(&records, &[]);
    assert_no_attempt_groups()
}

/// Runs the thread refusal: a thread calls `unshare` and the filter kills the process with its
/// one record.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_a_thread_is_refused_with_its_record() -> Result<(), Box<dyn Error>> {
    check_refused_by_kill(
        "thread-refusal",
        "thread-refusal started",
        "thread-refusal returned",
        UNSHARE,
    )
}

/// Runs the descendant refusal: the payload calls `prlimit64` on a child, and the filter kills the
/// process with its one record.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_a_descendants_limit_cannot_be_set() -> Result<(), Box<dyn Error>> {
    check_refused_by_kill(
        "descendant-refusal",
        "descendant pid=",
        "descendant-refusal returned",
        PRLIMIT64,
    )
}

/// Runs the lowering of the soft limit: the payload's `prlimit64` that sets the limit is killed
/// with its one record, before the lowered limit is printed.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_lowering_the_soft_limit_is_refused() -> Result<(), Box<dyn Error>> {
    check_refused_by_kill(
        "lower",
        "lower before soft=256 hard=256",
        "lower set",
        PRLIMIT64,
    )
}

/// Runs the descriptor ceiling: the payload and its child open descriptors until `EMFILE` at 255,
/// and the window holds no seccomp record.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_descriptors_stop_at_the_ceiling() -> Result<(), Box<dyn Error>> {
    let mut log = KernelLog::open_at_end()?;
    let (outcome, _) = timed_launch("descriptors", &AttemptPlan::fixture())?;
    let evidence = evidence_of(outcome)?;
    assert_eq!(
        evidence.exit.code(),
        Some(0),
        "the descriptors payload must exit 0"
    );
    let text = stdout_text(&evidence)?;
    for prefix in ["descriptors", "descriptors-child"] {
        check_descriptor_report(text, prefix)?;
    }
    assert_eq!(
        field(line_with(text, "descriptors child code=")?, "code")?,
        "0",
        "the descriptors child must exit 0"
    );
    let records = await_records(&mut log, 0, RECORD_WAIT)?;
    require_exactly(&records, &[]);
    assert_no_attempt_groups()
}

/// Runs the boundary watch: while the payload has two threads and a child, every task outside the
/// launcher must show the filter, the empty capability sets and the limits of the attempt.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_the_boundary_holds_in_every_task() -> Result<(), Box<dyn Error>> {
    let stop = AtomicBool::new(false);
    let (launched, watched) = thread::scope(|scope| {
        let watcher = scope.spawn(|| watch_boundary(&stop).map_err(|error| error.to_string()));
        let launched = {
            let _stop_watcher = StopOnDrop(&stop);
            timed_launch("watch", &AttemptPlan::fixture())
        };
        (launched, watcher.join())
    });
    let (outcome, _) = launched?;
    let evidence = evidence_of(outcome)?;
    assert_eq!(
        evidence.exit.code(),
        Some(0),
        "the watch payload must exit 0"
    );
    let text = stdout_text(&evidence)?;
    assert!(
        text.lines().any(|line| line == "watch done"),
        "the watch payload must finish, got {text:?}"
    );
    let processes = watched.map_err(|_| "the boundary watcher panicked")??;
    let checked = check_processes(&processes);
    assert!(
        checked >= 3,
        "the watcher read {checked} non-launcher tasks, at least 3 are required"
    );
    assert_no_attempt_groups()
}

/// The normal-exit check reads a report line without a launch: a code and no signal pass, while a
/// signal death or a missing field fails. This runs locally, so the check cannot silently pass.
#[test]
fn a_report_line_shows_a_normal_exit_only_with_a_code_and_no_signal() {
    let cases = [
        ("probe name=ok_gettid code=171 signal=none", true),
        ("probe name=ok_fork_child_gettid code=0 signal=none", true),
        ("probe name=ok_gettid code=none signal=11", false),
        ("probe name=ok_gettid code=none signal=31", false),
        ("probe name=ok_gettid code=0 signal=11", false),
        ("probe name=ok_gettid code=256 signal=none", false),
        ("probe name=ok_gettid code=none signal=none", false),
        ("probe name=ok_gettid signal=none", false),
    ];
    for (line, expected) in cases {
        assert_eq!(
            exited_normally(line),
            expected,
            "the report line {line:?} must count as a normal exit: {expected}"
        );
    }
}
