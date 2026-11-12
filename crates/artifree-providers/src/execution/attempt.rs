//! One trusted launch of the fixture stub in its own attempt group (decision 0056 rule 5 and
//! decision 0042 rules 5 and 6).
//!
//! Compiled only into the test build (0056 rule 16). Nothing here is visible outside
//! `execution`, and the payload of this slice only exits (0056 rule 14, R02ab).

use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{self, Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::admission::{self, AdmissionRoots, wait_within};
use super::bootstrap::{EXEC_FAILED, LAUNCH, READY, RELEASE};
use super::lock::{AdmissionLock, LockRefusal};
use super::probe_group::{self, ProbeGroup};
use super::readiness::{self, GroupObservation};
use super::report::HostReport;
use super::{AdmissionRefusal, FIXTURE, InspectionProfile, launcher, policy, read_bounded};

/// The combined stdout and stderr budget of one attempt, in bytes (decision 0042 rule 5).
const OUTPUT_BUDGET: u64 = 1_048_576;
/// The wall deadline of one attempt, counted from the LAUNCH record (decision 0042 rule 5).
const WALL_DEADLINE: Duration = Duration::from_secs(120);
/// The readiness deadline, counted from the LAUNCH record.
const READINESS_DEADLINE: Duration = Duration::from_secs(10);
/// The pause between two polls of the stub and of the readers.
const POLL: Duration = Duration::from_millis(10);
/// How long the provider waits on the control socket once the stub has exited.
const CONTROL_GRACE: Duration = Duration::from_secs(1);
/// The largest read that one pipe reader makes at a time.
const CHUNK_BYTES: usize = 8192;
/// The largest `cgroup.procs` listing that the launch reads when it places the stub.
const LISTING_BYTES: u64 = 4096;
/// The stub's arguments: the fixture's `stub` mode with the fixture identity.
const FIXTURE_STUB: &[&str] = &["stub", FIXTURE.identity];

/// Set once a cleanup was not observed. The group then stays in place and launches are refused.
static QUARANTINED: AtomicBool = AtomicBool::new(false);
/// Numbers the attempt groups of this process, so that no two attempts share a name.
static ATTEMPT_COUNTER: AtomicU32 = AtomicU32::new(0);

/// The inputs of one launch: the stub's arguments and its two deadlines.
#[derive(Clone, Copy, Debug)]
pub(super) struct AttemptPlan {
    /// The arguments after the fixture path.
    pub(super) stub_arguments: &'static [&'static str],
    /// The wall deadline, counted from the LAUNCH record on a monotonic clock.
    pub(super) wall_deadline: Duration,
    /// The readiness deadline, counted from the LAUNCH record on a monotonic clock.
    pub(super) readiness_deadline: Duration,
    /// The bytes sent as the release. Only a test sends anything but RELEASE, to show that the
    /// bootstrap starts no payload on another byte.
    pub(super) release_record: &'static [u8],
}

impl AttemptPlan {
    /// The one plan that the fixture uses: the fixture stub and the decision 0042 deadlines.
    #[must_use]
    pub(super) fn fixture() -> Self {
        Self {
            stub_arguments: FIXTURE_STUB,
            wall_deadline: WALL_DEADLINE,
            readiness_deadline: READINESS_DEADLINE,
            release_record: &RELEASE,
        }
    }
}

/// Why the stub did not reach a run, with the step that failed (decision 0056 rule 5).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NotStartedReason {
    /// The stub could not be spawned or its pipes read, or the LAUNCH record was not sent.
    StubNotStarted,
    /// The stub's pid did not appear in the attempt group's `cgroup.procs`.
    StubNotPlaced,
    /// The READY record did not arrive whole before the readiness deadline.
    NoReadiness,
    /// The provider's independent observation refused the group before release.
    ReadinessNotConfirmed,
    /// The RELEASE record could not be sent.
    ReleaseNotSent,
    /// The bootstrap reported that the payload's `exec` returned.
    ExecFailed,
    /// The control socket gave neither end of stream nor `EXEC_FAILED`.
    ExecUnconfirmed,
}

/// Why a launch produced no evidence of a run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AttemptFailure {
    /// Another process holds the admission lock.
    Busy,
    /// The admission lock could not be used.
    LockUnusable,
    /// An earlier cleanup was not observed in this process.
    Quarantined,
    /// The host admission refused, with its refusal.
    Refused(AdmissionRefusal),
    /// The fixture filter could not be built for the policy digest.
    PolicyUnavailable,
    /// The stub did not reach a run.
    NotStarted(NotStartedReason),
    /// The stub's state could not be read, or its output could not be read back.
    Unobserved,
    /// The stub was still running at the wall deadline.
    DeadlineExceeded,
    /// The stub's output went past the combined budget.
    OutputLimitExceeded,
    /// The group, the stub or a reader was not observed gone, so the attempt is quarantined.
    CleanupNotObserved,
}

/// Evidence of one launch that ran to an observed exit.
#[derive(Debug)]
pub(super) struct AttemptEvidence {
    /// The attempt group's directory, which the launch removed.
    pub(super) group: PathBuf,
    /// The limits read back from the attempt group, as control file and value.
    pub(super) limits: Vec<(&'static str, String)>,
    /// How the stub exited.
    pub(super) exit: ExitStatus,
    /// The captured standard output, which is untrusted.
    pub(super) stdout: Vec<u8>,
    /// The captured standard error, which is untrusted.
    pub(super) stderr: Vec<u8>,
    /// The time from the LAUNCH record to the observed exit.
    pub(super) wall_time: Duration,
    /// The attempt group's peak memory in bytes, when the kernel offers it.
    pub(super) peak_memory_bytes: Option<u64>,
    /// What the provider saw in the group before it released the bootstrap.
    pub(super) readiness: GroupObservation,
    /// SHA-256 of the seccomp program, in lowercase hexadecimal.
    pub(super) filter_sha256: String,
    /// The host admission's evidence.
    pub(super) report: HostReport,
}

impl fmt::Display for AttemptEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "attempt_group={}", self.group.display())?;
        for (file, value) in &self.limits {
            writeln!(formatter, "attempt_limit.{file}={value}")?;
        }
        match (self.exit.code(), self.exit.signal()) {
            (Some(code), _) => writeln!(formatter, "exit=code {code}")?,
            (None, Some(signal)) => writeln!(formatter, "exit=signal {signal}")?,
            (None, None) => writeln!(formatter, "exit=unknown")?,
        }
        writeln!(formatter, "wall_time_ms={}", self.wall_time.as_millis())?;
        writeln!(formatter, "stdout_bytes={}", self.stdout.len())?;
        writeln!(formatter, "stderr_bytes={}", self.stderr.len())?;
        match self.peak_memory_bytes {
            Some(bytes) => writeln!(formatter, "attempt_peak_memory_bytes={bytes}")?,
            None => writeln!(formatter, "attempt_peak_memory_bytes=unavailable")?,
        }
        let observed = &self.readiness;
        let status = observed.status;
        let limits = observed.limits;
        writeln!(formatter, "readiness.members={}", observed.members)?;
        writeln!(
            formatter,
            "readiness.bootstrap_pid={}",
            observed.bootstrap_pid
        )?;
        writeln!(formatter, "readiness.no_new_privs={}", status.no_new_privs)?;
        writeln!(formatter, "readiness.seccomp={}", status.seccomp)?;
        writeln!(
            formatter,
            "readiness.seccomp_filters={}",
            status.seccomp_filters
        )?;
        writeln!(formatter, "readiness.cap_inh={:016x}", status.cap_inh)?;
        writeln!(formatter, "readiness.cap_prm={:016x}", status.cap_prm)?;
        writeln!(formatter, "readiness.cap_eff={:016x}", status.cap_eff)?;
        writeln!(formatter, "readiness.cap_bnd={:016x}", status.cap_bnd)?;
        writeln!(formatter, "readiness.cap_amb={:016x}", status.cap_amb)?;
        writeln!(
            formatter,
            "readiness.open_files={} {}",
            limit_word(limits.open_files.soft),
            limit_word(limits.open_files.hard)
        )?;
        writeln!(
            formatter,
            "readiness.core_bytes={} {}",
            limit_word(limits.core_bytes.soft),
            limit_word(limits.core_bytes.hard)
        )?;
        writeln!(formatter, "filter_sha256={}", self.filter_sha256)?;
        write!(formatter, "{}", self.report)
    }
}

/// A limit value as the evidence prints it: its number, or `unlimited`.
fn limit_word(value: Option<u64>) -> String {
    value.map_or_else(|| "unlimited".to_owned(), |number| number.to_string())
}

/// A reader thread's result: the bytes it kept, or the read error that ended it.
type Capture = JoinHandle<io::Result<Vec<u8>>>;

/// What one launch holds while its group exists. `settle` ends every part of it.
#[derive(Debug, Default)]
struct Session {
    /// The stub, once it has been spawned.
    child: Option<Child>,
    /// The stub's exit status, once the poll observed it.
    exit: Option<ExitStatus>,
    /// The stdout reader, once it has been started.
    stdout: Option<Capture>,
    /// The stderr reader, once it has been started.
    stderr: Option<Capture>,
    /// The bytes that both readers have taken in so far, kept or discarded.
    counted: Arc<AtomicU64>,
    /// Set once the readers have taken in more than `OUTPUT_BUDGET` bytes.
    exceeded: Arc<AtomicBool>,
}

/// What the group phase measured before the group was ended.
#[derive(Debug)]
struct Passed {
    /// The limits read back from the attempt group.
    limits: Vec<(&'static str, String)>,
    /// What the provider saw in the group before the release.
    readiness: GroupObservation,
    /// How the stub exited.
    exit: ExitStatus,
    /// The time from the LAUNCH record to the observed exit.
    wall_time: Duration,
}

/// What the cleanup of the group yielded.
#[derive(Debug)]
struct Settled {
    /// The group's peak memory, read before it was removed.
    peak_memory_bytes: Option<u64>,
    /// The captured standard output, or the read error that ended it.
    stdout: io::Result<Vec<u8>>,
    /// The captured standard error, or the read error that ended it.
    stderr: io::Result<Vec<u8>>,
}

/// Runs one attempt: the admission lock, the quarantine check, the whole host admission, then
/// the attempt group. The group is always ended and removed, or the attempt is quarantined
/// (decision 0042 rule 6).
///
/// `execution_dir` holds the admission lock only. The group is created beside the admission's
/// `supervisor` child, never in `execution_dir`.
pub(super) fn launch(
    plan: &AttemptPlan,
    execution_dir: &Path,
) -> Result<AttemptEvidence, AttemptFailure> {
    // The stub and the bootstrap use the fixed tool root, so the roots that are verified here
    // must be the same ones: no caller chooses them.
    let roots = &admission::production_roots();
    // Held until this function returns, so that no second launch can start meanwhile.
    let _lock = AdmissionLock::acquire(execution_dir).map_err(lock_failure)?;
    if QUARANTINED.load(Ordering::SeqCst) {
        return Err(AttemptFailure::Quarantined);
    }
    let (report, delegated) = admission::admit_for_attempt(InspectionProfile::fixture(), roots)
        .map_err(AttemptFailure::Refused)?;
    let filter_sha256 = policy::policy_sha256().map_err(|_| AttemptFailure::PolicyUnavailable)?;
    let own_pid_namespace = fs::read_link(roots.proc_self.join("ns").join("pid"))
        .map_err(|_| AttemptFailure::Refused(AdmissionRefusal::NamespacesUnavailable))?;
    let name = format!(
        "attempt-{}-{}",
        process::id(),
        ATTEMPT_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let group = probe_group::create_probe(&delegated, &name).map_err(AttemptFailure::Refused)?;
    let group_path = delegated.join(&name);

    // From here on every path ends the group in `settle`, whatever the group phase returned.
    let mut session = Session::default();
    let phase = run_group(
        plan,
        roots,
        &group,
        &group_path,
        &own_pid_namespace,
        &mut session,
    );
    let settled = settle(group, &mut session)?;
    let passed = phase?;
    let stdout = settled.stdout.map_err(|_| AttemptFailure::Unobserved)?;
    let stderr = settled.stderr.map_err(|_| AttemptFailure::Unobserved)?;
    if session.exceeded.load(Ordering::SeqCst) {
        return Err(AttemptFailure::OutputLimitExceeded);
    }
    Ok(AttemptEvidence {
        group: group_path,
        limits: passed.limits,
        exit: passed.exit,
        stdout,
        stderr,
        wall_time: passed.wall_time,
        peak_memory_bytes: settled.peak_memory_bytes,
        readiness: passed.readiness,
        filter_sha256,
        report,
    })
}

/// Maps the lock's refusal: contention is `Busy`, and nothing else is then done.
fn lock_failure(refusal: LockRefusal) -> AttemptFailure {
    match refusal {
        LockRefusal::Busy => AttemptFailure::Busy,
        LockRefusal::Unusable => AttemptFailure::LockUnusable,
    }
}

/// Records that a cleanup was not observed, so that later launches in this process are refused,
/// and names that failure.
fn quarantine() -> AttemptFailure {
    QUARANTINED.store(true, Ordering::SeqCst);
    AttemptFailure::CleanupNotObserved
}

/// The group phase: limits, the stub, its placement, the readiness check and release, the wait
/// for exit, and the exec confirmation. The caller ends the group whatever this returns.
fn run_group(
    plan: &AttemptPlan,
    roots: &AdmissionRoots<'_>,
    group: &ProbeGroup,
    group_path: &Path,
    own_pid_namespace: &Path,
    session: &mut Session,
) -> Result<Passed, AttemptFailure> {
    let limits = probe_group::apply_limits(group).map_err(AttemptFailure::Refused)?;
    let mut control = start_stub(plan, roots, session)?;
    place_stub(group, group_path, session)?;
    let (readiness, start) = release(plan, roots, &mut control, group_path, own_pid_namespace)?;
    let (exit, wall_time) = await_exit(plan, session, start)?;
    session.exit = Some(exit);
    confirm_started(&mut control)?;
    Ok(Passed {
        limits,
        readiness,
        exit,
        wall_time,
    })
}

/// Starts the stub with its standard input on a control socket and its two pipes captured.
/// Returns the provider's end of the socket.
fn start_stub(
    plan: &AttemptPlan,
    roots: &AdmissionRoots<'_>,
    session: &mut Session,
) -> Result<UnixStream, AttemptFailure> {
    let (control, child_end) = UnixStream::pair()
        .map_err(|_| AttemptFailure::NotStarted(NotStartedReason::StubNotStarted))?;
    // The command holds the child's end of the socket, so it is dropped right after the spawn.
    let spawned = {
        let mut command = Command::new(roots.tool_root.join(launcher::FIXTURE_FILE));
        command
            .args(plan.stub_arguments)
            .env_clear()
            .stdin(Stdio::from(OwnedFd::from(child_end)))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.spawn()
    };
    let mut child =
        spawned.map_err(|_| AttemptFailure::NotStarted(NotStartedReason::StubNotStarted))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    // Stored before the readers start, so that a failed reader still lets `settle` end the stub.
    session.child = Some(child);
    let stdout_reader = start_reader(stdout, session)?;
    session.stdout = Some(stdout_reader);
    let stderr_reader = start_reader(stderr, session)?;
    session.stderr = Some(stderr_reader);
    Ok(control)
}

/// Starts one reader thread for a pipe. Both readers share the output budget.
fn start_reader<R: Read + Send + 'static>(
    pipe: Option<R>,
    session: &Session,
) -> Result<Capture, AttemptFailure> {
    let pipe = pipe.ok_or(AttemptFailure::NotStarted(NotStartedReason::StubNotStarted))?;
    let counted = Arc::clone(&session.counted);
    let exceeded = Arc::clone(&session.exceeded);
    thread::Builder::new()
        .spawn(move || capture(pipe, &counted, &exceeded))
        .map_err(|_| AttemptFailure::NotStarted(NotStartedReason::StubNotStarted))
}

/// Reads `pipe` to its end. Bytes that fit the shared budget are kept. The rest are read and
/// discarded, and `exceeded` is set (decision 0042 rule 5).
fn capture(mut pipe: impl Read, counted: &AtomicU64, exceeded: &AtomicBool) -> io::Result<Vec<u8>> {
    let mut kept = Vec::new();
    let mut chunk = [0u8; CHUNK_BYTES];
    loop {
        let read = match pipe.read(&mut chunk) {
            Ok(0) => return Ok(kept),
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let received = u64::try_from(read).unwrap_or(u64::MAX);
        let before = counted.fetch_add(received, Ordering::SeqCst);
        let room = OUTPUT_BUDGET.saturating_sub(before);
        if received > room {
            exceeded.store(true, Ordering::SeqCst);
        }
        let keep = usize::try_from(room.min(received)).unwrap_or(0);
        if let Some(bytes) = chunk.get(..keep) {
            kept.extend_from_slice(bytes);
        }
    }
}

/// Writes the stub's pid to the group and confirms from `cgroup.procs` that the group holds it.
fn place_stub(
    group: &ProbeGroup,
    group_path: &Path,
    session: &Session,
) -> Result<(), AttemptFailure> {
    let not_placed = AttemptFailure::NotStarted(NotStartedReason::StubNotPlaced);
    let pid = session.child.as_ref().map(Child::id).ok_or(not_placed)?;
    probe_group::add_process(group, pid).map_err(|_| not_placed)?;
    let listing =
        read_bounded(&group_path.join("cgroup.procs"), LISTING_BYTES).map_err(|_| not_placed)?;
    if lists_pid(&listing, pid) {
        Ok(())
    } else {
        Err(not_placed)
    }
}

/// Whether a `cgroup.procs` listing holds `pid` on one of its lines.
fn lists_pid(listing: &[u8], pid: u32) -> bool {
    std::str::from_utf8(listing).is_ok_and(|text| {
        text.lines().any(|line| {
            let parsed: Option<u32> = line.parse().ok();
            parsed == Some(pid)
        })
    })
}

/// Sends LAUNCH, reads READY before the readiness deadline, observes the group independently and
/// only then sends RELEASE. Returns the observation and the instant of LAUNCH.
fn release(
    plan: &AttemptPlan,
    roots: &AdmissionRoots<'_>,
    control: &mut UnixStream,
    group_path: &Path,
    own_pid_namespace: &Path,
) -> Result<(GroupObservation, Instant), AttemptFailure> {
    let start = Instant::now();
    control
        .write_all(&LAUNCH)
        .map_err(|_| AttemptFailure::NotStarted(NotStartedReason::StubNotStarted))?;
    read_ready(control, start, plan.readiness_deadline)?;
    // Only a group confirmed by the provider's own reads may be released (0042 rule 6).
    let observation = readiness::observe_group(
        group_path,
        Path::new("/proc"),
        roots.cgroup_mount,
        own_pid_namespace,
    )
    .map_err(|_| AttemptFailure::NotStarted(NotStartedReason::ReadinessNotConfirmed))?;
    control
        .write_all(plan.release_record)
        .map_err(|_| AttemptFailure::NotStarted(NotStartedReason::ReleaseNotSent))?;
    Ok((observation, start))
}

/// Reads exactly the READY record before the deadline, which counts from `start`.
fn read_ready(
    control: &mut UnixStream,
    start: Instant,
    deadline: Duration,
) -> Result<(), AttemptFailure> {
    let refused = AttemptFailure::NotStarted(NotStartedReason::NoReadiness);
    let mut received = [0u8; READY.len()];
    let mut filled = 0;
    while filled < received.len() {
        let remaining = deadline
            .checked_sub(start.elapsed())
            .filter(|left| !left.is_zero())
            .ok_or(refused)?;
        control
            .set_read_timeout(Some(remaining))
            .map_err(|_| refused)?;
        let Some(slot) = received.get_mut(filled..) else {
            return Err(refused);
        };
        match control.read(slot) {
            Ok(0) => return Err(refused),
            Ok(count) => filled += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(refused),
        }
    }
    if received == READY {
        Ok(())
    } else {
        Err(refused)
    }
}

/// Polls the stub until it exits, the wall deadline passes or the output budget is exceeded.
/// Returns the exit and the time from `start` to the observed exit.
fn await_exit(
    plan: &AttemptPlan,
    session: &mut Session,
    start: Instant,
) -> Result<(ExitStatus, Duration), AttemptFailure> {
    let child = session.child.as_mut().ok_or(AttemptFailure::Unobserved)?;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // An exit that is first seen at or after the deadline is past it too: the
                // deadline is absolute, whatever the polling interval.
                let elapsed = start.elapsed();
                running_verdict(
                    elapsed,
                    plan.wall_deadline,
                    session.exceeded.load(Ordering::SeqCst),
                )?;
                return Ok((status, elapsed));
            }
            Ok(None) => {}
            Err(_) => return Err(AttemptFailure::Unobserved),
        }
        // The wall deadline after release cannot be proven with a payload that only exits;
        // R02ac's hang proves it.
        running_verdict(
            start.elapsed(),
            plan.wall_deadline,
            session.exceeded.load(Ordering::SeqCst),
        )?;
        thread::sleep(POLL);
    }
}

/// The decision for one poll of the stub, running or just seen exited: output past the budget first, then the wall
/// deadline.
fn running_verdict(
    elapsed: Duration,
    wall_deadline: Duration,
    output_exceeded: bool,
) -> Result<(), AttemptFailure> {
    if output_exceeded {
        return Err(AttemptFailure::OutputLimitExceeded);
    }
    if elapsed >= wall_deadline {
        return Err(AttemptFailure::DeadlineExceeded);
    }
    Ok(())
}

/// After the stub exited, `EXEC_FAILED` on the control socket means the exec of the payload
/// returned, so the attempt did not start (0056 rule 5). End of stream means only that no exec
/// failure was reported: a bootstrap that was killed between the release and the exec leaves
/// the same end of stream. Such an attempt ends by a signal or a non-zero status and is a
/// failed attempt for every caller; no descriptor of this chain closes at the exec alone,
/// because the launcher and its init keep their copies of the stub's standard input. Any other
/// answer is unconfirmed.
fn confirm_started(control: &mut UnixStream) -> Result<(), AttemptFailure> {
    let unconfirmed = AttemptFailure::NotStarted(NotStartedReason::ExecUnconfirmed);
    control
        .set_read_timeout(Some(CONTROL_GRACE))
        .map_err(|_| unconfirmed)?;
    let mut answer = [0u8; EXEC_FAILED.len()];
    match control.read(&mut answer) {
        Ok(0) => Ok(()),
        Ok(_) if answer == EXEC_FAILED => {
            Err(AttemptFailure::NotStarted(NotStartedReason::ExecFailed))
        }
        _ => Err(unconfirmed),
    }
}

/// Ends every part of the attempt after any outcome of the group phase (decision 0042 rule 6):
/// kills the group and waits until it is empty, reaps the stub, joins both readers and reads
/// the peak. The group is removed only when every part was observed. Otherwise the attempt is
/// quarantined and the group stays in place.
fn settle(group: ProbeGroup, session: &mut Session) -> Result<Settled, AttemptFailure> {
    // One deadline for the whole cleanup (0042 rule 6: five seconds), not one per part.
    let deadline = Instant::now().checked_add(probe_group::CLEANUP_DEADLINE);
    let remaining = || {
        deadline.map_or(Duration::ZERO, |deadline| {
            deadline.saturating_duration_since(Instant::now())
        })
    };
    let emptied =
        probe_group::kill_and_await_empty(&group, probe_group::CLEANUP_DEADLINE, Instant::now)
            .is_ok();
    let reaped = reap_child(session.child.as_mut(), session.exit.is_some(), remaining());
    let stdout = join_reader(session.stdout.take(), remaining());
    let stderr = join_reader(session.stderr.take(), remaining());
    let peak_memory_bytes = if emptied {
        probe_group::read_peak(&group)
    } else {
        None
    };
    let (Some(stdout), Some(stderr)) = (stdout, stderr) else {
        return Err(quarantine());
    };
    if !emptied || !reaped {
        return Err(quarantine());
    }
    probe_group::remove_probe(group).map_err(|_| quarantine())?;
    Ok(Settled {
        peak_memory_bytes,
        stdout,
        stderr,
    })
}

/// Kills the stub if it may still run, then waits for it for at most `within`, with at least
/// one look. Returns whether it was reaped. A kill error is ignored only when the exit is
/// already known.
fn reap_child(child: Option<&mut Child>, exited: bool, within: Duration) -> bool {
    let Some(child) = child else {
        return true;
    };
    let killed = child.kill();
    if killed.is_err() && !exited {
        return false;
    }
    wait_within(child, within).is_ok()
}

/// Waits, for at most `within`, for one reader to finish and returns what it captured. It
/// looks at least once. `None` means that the reader did not finish or that it panicked.
fn join_reader(reader: Option<Capture>, within: Duration) -> Option<io::Result<Vec<u8>>> {
    let Some(reader) = reader else {
        return Some(Ok(Vec::new()));
    };
    let started = Instant::now();
    while !reader.is_finished() {
        if started.elapsed() >= within {
            return None;
        }
        thread::sleep(POLL);
    }
    reader.join().ok()
}

#[cfg(test)]
mod tests;
