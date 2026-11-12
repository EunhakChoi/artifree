//! One trusted launch of the fixture stub in its own attempt group (decision 0056 rule 5 and
//! decision 0042 rules 5 and 6).
//!
//! Compiled only into the test build (0056 rule 16). Nothing here is visible outside
//! `execution`.
//!
//! R02ac (0056 rule 14) added the resource part of the group phase. The scratch bounds are read
//! back from the bootstrap's mount table (`readiness`), and the stub runs behind the two seccomp
//! programs of `policy`. The group's counters are read once the group is seen empty (`usage`).
//! After the stub's exit the group's listing decides survivors once the PID namespace's
//! teardown has had `TEARDOWN_GRACE` to finish, and the verdict is typed:
//! `OutOfMemory` and `Survivors` are decided in the order that `final_verdict` documents. The
//! payload scenarios that exercise these limits run only in the enforcement job (`run.sh`).

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
use super::usage::{self, GroupUsage};
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
/// How long the group may take to empty after the stub's exit. The launcher can exit before the
/// kernel has ended the rest of its PID namespace: on the job's runner a process bomb's children
/// were still listed when the stub's exit was observed (main, 2026-11-12). A process that is
/// still listed after this wait is a survivor. The wait never runs past the wall deadline.
const TEARDOWN_GRACE: Duration = Duration::from_secs(2);
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
    /// The group's memory limit killed it (`memory.events` `oom_kill` above zero).
    OutOfMemory,
    /// A process was still in the attempt group when the stub's normal exit was observed
    /// (decision 0042 rule 6).
    Survivors,
    /// The stub's output went past the combined budget.
    OutputLimitExceeded,
    /// The group, the stub or a reader was not observed gone, so the attempt is quarantined.
    CleanupNotObserved,
}

/// Why a launch produced no evidence of a run, with the group's counters when they were read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct AttemptRefusal {
    /// The reason that the launch produced no evidence.
    pub(super) failure: AttemptFailure,
    /// The group's counters. Present whenever the group was created, emptied and read, so that a
    /// refused attempt still shows what it used.
    pub(super) usage: Option<GroupUsage>,
}

/// A refusal of a launch that has no group counters: the failure came before the group was
/// created, or the group was not read.
fn without_usage(failure: AttemptFailure) -> AttemptRefusal {
    AttemptRefusal {
        failure,
        usage: None,
    }
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
    /// The attempt group's counters, read after it was seen empty.
    pub(super) usage: GroupUsage,
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
        write!(formatter, "{}", self.usage)?;
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
        match observed.scratch.size_kib {
            Some(kib) => writeln!(formatter, "readiness.scratch_size={kib}k")?,
            None => writeln!(formatter, "readiness.scratch_size=unavailable")?,
        }
        match observed.scratch.inodes {
            Some(inodes) => writeln!(formatter, "readiness.scratch_inodes={inodes}")?,
            None => writeln!(formatter, "readiness.scratch_inodes=unavailable")?,
        }
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

/// What the group's `cgroup.procs` listing held at its last read after the stub's normal exit,
/// within `TEARDOWN_GRACE` and the wall deadline.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum SurvivorListing {
    /// No exit was observed before a deadline or a failure, so no listing was read.
    #[default]
    NotChecked,
    /// The listing holds no pid: the group is empty.
    Empty,
    /// The listing holds at least one pid.
    Occupied,
    /// The listing could not be read, was over its bound, was not UTF-8, or had a line that is
    /// not a pid.
    Unreadable,
}

/// Classifies a `cgroup.procs` listing read after the stub's exit. Every non-empty line must be a
/// pid, so a listing that is not understood is never taken for an empty group.
fn survivor_listing(listing: io::Result<Vec<u8>>) -> SurvivorListing {
    let Ok(bytes) = listing else {
        return SurvivorListing::Unreadable;
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return SurvivorListing::Unreadable;
    };
    let mut occupied = false;
    for line in text.lines().filter(|line| !line.is_empty()) {
        if line.parse::<u32>().is_err() {
            return SurvivorListing::Unreadable;
        }
        occupied = true;
    }
    if occupied {
        SurvivorListing::Occupied
    } else {
        SurvivorListing::Empty
    }
}

/// Reads the group's listing until it is empty, unreadable, or still occupied once `grace` has
/// passed since the first read. The listing is read at least once, and once more after the last
/// pause, so a group that empties within `grace` is never reported as occupied. A listing that
/// was occupied and then cannot be read stays occupied: the processes were seen.
fn await_empty_listing(
    mut read: impl FnMut() -> io::Result<Vec<u8>>,
    grace: Duration,
) -> SurvivorListing {
    let started = Instant::now();
    let mut seen_occupied = false;
    loop {
        match survivor_listing(read()) {
            SurvivorListing::Occupied if started.elapsed() < grace => seen_occupied = true,
            SurvivorListing::Unreadable if seen_occupied => return SurvivorListing::Occupied,
            listing => return listing,
        }
        thread::sleep(POLL);
    }
}

/// What one launch holds while its group exists. `settle` ends every part of it.
#[derive(Debug, Default)]
struct Session {
    /// The stub, once it has been spawned.
    child: Option<Child>,
    /// The stub's exit status, once the poll observed it.
    exit: Option<ExitStatus>,
    /// The group's last listing read after the stub's exit, within `TEARDOWN_GRACE`, before the
    /// group is killed.
    survivors: SurvivorListing,
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
    /// Whether the group was seen empty, the stub reaped, both readers finished and the group
    /// removed. Only then is the cleanup observed; otherwise the attempt is quarantined.
    cleanup_observed: bool,
    /// The group's counters, read after it was seen empty and before it was removed.
    usage: Option<GroupUsage>,
    /// The captured standard output, or the read error that ended it. `None` when the reader
    /// did not finish in time.
    stdout: Option<io::Result<Vec<u8>>>,
    /// The captured standard error, with the same meaning as `stdout`.
    stderr: Option<io::Result<Vec<u8>>>,
}

/// Runs one attempt: the admission lock, the quarantine check, the whole host admission, then
/// the attempt group. The group is always ended and removed, or the attempt is quarantined
/// (decision 0042 rule 6). A refusal carries the group's counters whenever the group was
/// created, emptied and read.
///
/// `execution_dir` holds the admission lock only. The group is created beside the admission's
/// `supervisor` child, never in `execution_dir`.
pub(super) fn launch(
    plan: &AttemptPlan,
    execution_dir: &Path,
) -> Result<AttemptEvidence, AttemptRefusal> {
    // The stub and the bootstrap use the fixed tool root, so the roots that are verified here
    // must be the same ones: no caller chooses them.
    let roots = &admission::production_roots();
    // Held until this function returns, so that no second launch can start meanwhile.
    let _lock = AdmissionLock::acquire(execution_dir)
        .map_err(|refusal| without_usage(lock_failure(refusal)))?;
    if QUARANTINED.load(Ordering::SeqCst) {
        return Err(without_usage(AttemptFailure::Quarantined));
    }
    let (report, delegated) = admission::admit_for_attempt(InspectionProfile::fixture(), roots)
        .map_err(|failure| without_usage(AttemptFailure::Refused(failure)))?;
    let filter_sha256 =
        policy::policy_sha256().map_err(|_| without_usage(AttemptFailure::PolicyUnavailable))?;
    let own_pid_namespace =
        fs::read_link(roots.proc_self.join("ns").join("pid")).map_err(|_| {
            without_usage(AttemptFailure::Refused(
                AdmissionRefusal::NamespacesUnavailable,
            ))
        })?;
    let name = format!(
        "attempt-{}-{}",
        process::id(),
        ATTEMPT_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let group = probe_group::create_probe(&delegated, &name)
        .map_err(|failure| without_usage(AttemptFailure::Refused(failure)))?;
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
    let settled = settle(group, &group_path, &mut session);
    let usage = settled.usage;
    let refused = |failure: AttemptFailure| AttemptRefusal { failure, usage };
    let outcome = Outcome {
        cleanup_observed: settled.cleanup_observed,
        output_exceeded: session.exceeded.load(Ordering::SeqCst),
        phase: phase.as_ref().map(|_| ()).map_err(|failure| *failure),
        usage,
        survivors: session.survivors,
        captures_read: matches!(
            (&settled.stdout, &settled.stderr),
            (Some(Ok(_)), Some(Ok(_)))
        ),
    };
    final_verdict(&outcome).map_err(refused)?;
    let (Ok(passed), Some(Ok(stdout)), Some(Ok(stderr)), Some(counters)) =
        (phase, settled.stdout, settled.stderr, usage)
    else {
        return Err(refused(AttemptFailure::Unobserved));
    };
    Ok(AttemptEvidence {
        group: group_path,
        limits: passed.limits,
        exit: passed.exit,
        stdout,
        stderr,
        wall_time: passed.wall_time,
        peak_memory_bytes: counters.memory_peak_bytes,
        usage: counters,
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

/// Records that a cleanup was not observed, so that later launches in this process are refused
/// (decision 0042 rule 6). The attempt's verdict then names `CleanupNotObserved`.
fn quarantine() {
    QUARANTINED.store(true, Ordering::SeqCst);
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
    // `await_exit` records the exit in the session itself, on every path that reaped the stub.
    let (exit, wall_time) = await_exit(plan, session, start)?;
    // Decision 0042 rule 6: after the launcher and init teardown a normal exit also requires an
    // empty group. `populated` is not used: measured on 2026-11-12, it stays 1 for a few
    // milliseconds after ordinary exits while the listing is already empty. `settle` kills
    // survivors as it kills every group.
    // The wait ends at the wall deadline too, so a group never runs past it (0042 rule 5).
    let listing = group_path.join("cgroup.procs");
    let grace = TEARDOWN_GRACE.min(plan.wall_deadline.saturating_sub(start.elapsed()));
    session.survivors = await_empty_listing(|| read_bounded(&listing, LISTING_BYTES), grace);
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
                // The stub is reaped now, so `settle` must know it whatever the verdict below:
                // a kill of a reaped child is not a failed cleanup.
                session.exit = Some(status);
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

/// What the parts of one attempt measured, as plain values, for the one final decision.
#[derive(Clone, Copy, Debug)]
struct Outcome {
    /// Whether the group, the stub, both readers and the group's removal were all observed.
    cleanup_observed: bool,
    /// Whether the output budget was exceeded.
    output_exceeded: bool,
    /// The group phase's result: `Ok` for a run that exited, or the failure that ended the phase.
    phase: Result<(), AttemptFailure>,
    /// The group's counters, when they were read.
    usage: Option<GroupUsage>,
    /// The group's last listing read after the stub's exit, within `TEARDOWN_GRACE`.
    survivors: SurvivorListing,
    /// Whether both captured streams were read to their end.
    captures_read: bool,
}

/// The final verdict of one attempt. The first matching rule decides: cleanup not observed
/// (the group is quarantined), output budget exceeded, wall deadline exceeded, out of memory,
/// survivors, the not-started reasons of the control channel (`ExecFailed`, `ExecUnconfirmed`),
/// success.
///
/// An attempt that the group's OOM killed is `OutOfMemory`, although its stub exited by signal 9
/// and its control channel read end of stream: the counters decide, not the exit status. Counters
/// that cannot be read are `Unobserved` before the OOM rule, because that rule cannot be decided
/// without them. A survivor listing that cannot be read is `Unobserved` at the survivors rule, for
/// the same reason: an unread listing must never count as an empty group.
fn final_verdict(outcome: &Outcome) -> Result<(), AttemptFailure> {
    if !outcome.cleanup_observed {
        return Err(AttemptFailure::CleanupNotObserved);
    }
    if outcome.output_exceeded {
        return Err(AttemptFailure::OutputLimitExceeded);
    }
    if outcome.phase == Err(AttemptFailure::DeadlineExceeded) {
        return Err(AttemptFailure::DeadlineExceeded);
    }
    let Some(usage) = outcome.usage else {
        // Without the counters a run cannot be told from an out-of-memory kill. A launch that
        // failed before or at its start keeps its own reason, which needs no counter.
        outcome.phase?;
        return Err(AttemptFailure::Unobserved);
    };
    if usage.oom_kills > 0 {
        return Err(AttemptFailure::OutOfMemory);
    }
    match outcome.survivors {
        SurvivorListing::Occupied => return Err(AttemptFailure::Survivors),
        SurvivorListing::Unreadable => return Err(AttemptFailure::Unobserved),
        // A listing is read whenever the stub's exit was observed, so a run that ended well
        // without one was not observed.
        SurvivorListing::NotChecked if outcome.phase.is_ok() => {
            return Err(AttemptFailure::Unobserved);
        }
        SurvivorListing::NotChecked | SurvivorListing::Empty => {}
    }
    outcome.phase?;
    if !outcome.captures_read {
        return Err(AttemptFailure::Unobserved);
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
/// the group's counters. The group is removed only when every part was observed, the removal
/// included. Otherwise the attempt is quarantined and the group stays in place. A counter read
/// that fails leaves `usage` empty; the verdict then refuses the attempt, and the group is
/// still removed.
fn settle(group: ProbeGroup, group_path: &Path, session: &mut Session) -> Settled {
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
    // Read after the group was seen empty and before it is removed.
    let usage = if emptied {
        usage::read_usage(group_path).ok()
    } else {
        None
    };
    let readers_finished = stdout.is_some() && stderr.is_some();
    // The removal comes last, so a removed group was observed in every earlier part too.
    let cleanup_observed =
        emptied && reaped && readers_finished && probe_group::remove_probe(group).is_ok();
    if !cleanup_observed {
        quarantine();
    }
    Settled {
        cleanup_observed,
        usage,
        stdout,
        stderr,
    }
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
mod resource_tests;
#[cfg(test)]
mod tests;
