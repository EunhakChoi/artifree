//! Fixed-order Linux host admission (decision 0056 rules 8 and 10). Runs no payload.
//!
//! Each facility runs once, in the order the parent module documents, and the first
//! refusal is returned. Only the standard library and the report helpers, which hash the
//! launcher and the policy, are used here.

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{self, Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use super::report::{HostReport, sha256_file_hex, sha256_hex};
use super::{
    AdmissionRefusal, InspectionProfile, RoutingObservation, cgroup, launcher, probe_group,
    read_bounded,
};

/// The signal number that `cgroup.kill` delivers; a killed probe child must exit by it.
const SIGKILL: i32 = 9;
/// The pause between two polls of a child that has not exited yet.
const REAP_POLL: Duration = Duration::from_millis(10);

/// Keeps probe group names unique within this process, across calls.
static PROBE_COUNTER: AtomicU32 = AtomicU32::new(0);

/// The filesystem and process facts one admission run reads.
///
/// Production uses the constants of `production_roots`; tests point the same fields at
/// temporary directories. No caller outside this module chooses them.
#[derive(Debug)]
pub(super) struct AdmissionRoots<'a> {
    /// The supervisor's own proc directory, `/proc/self` in production.
    pub(super) proc_self: &'a Path,
    /// The kernel release file.
    pub(super) osrelease: &'a Path,
    /// The core routing file.
    pub(super) core_pattern: &'a Path,
    /// The cgroup v2 mount.
    pub(super) cgroup_mount: &'a Path,
    /// The tool root holding the launcher and the fixture.
    pub(super) tool_root: &'a Path,
}

/// The production roots: constants only, so no caller-supplied path reaches the checks.
pub(super) fn production_roots() -> AdmissionRoots<'static> {
    AdmissionRoots {
        proc_self: Path::new("/proc/self"),
        osrelease: Path::new("/proc/sys/kernel/osrelease"),
        core_pattern: Path::new(super::CORE_PATTERN),
        cgroup_mount: Path::new("/sys/fs/cgroup"),
        tool_root: Path::new(launcher::TOOL_ROOT),
    }
}

/// Runs every facility against `roots`, with this process's own pid and uid.
pub(super) fn admit(
    profile: InspectionProfile,
    roots: &AdmissionRoots<'_>,
) -> Result<HostReport, AdmissionRefusal> {
    admit_with(profile, roots, process::id(), || {
        cgroup::own_uid(roots.proc_self)
    })
}

/// Runs facilities 2 to 10 in the documented order and returns the first refusal.
///
/// The pid and the uid come in as values, so tests can run the whole order against
/// temporary roots. Facility 7 checks the group that holds this process: the delegated
/// group on a first call, or its `supervisor` child on a second one.
pub(super) fn admit_with(
    profile: InspectionProfile,
    roots: &AdmissionRoots<'_>,
    own_pid: u32,
    worker_uid: impl FnOnce() -> Result<u32, AdmissionRefusal>,
) -> Result<HostReport, AdmissionRefusal> {
    // Facility 2: the kernel release, the provider's group and its kill support.
    let kernel_release = read_bounded(roots.osrelease, 256)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .map(|text| text.trim_end().to_owned())
        .filter(|text| !text.is_empty())
        .ok_or(AdmissionRefusal::UnsupportedKernel)?;
    let discovered =
        cgroup::discover_own_group(&roots.proc_self.join("cgroup"), roots.cgroup_mount)?;
    cgroup::check_kill_support(&discovered)?;
    // Facility 3: observed as evidence only. Its one-second freshness would lapse during
    // the probes, so it is not rechecked after them.
    let routing =
        RoutingObservation::from_reader(fs::File::open(roots.core_pattern), Instant::now())?;
    // Facility 4.
    launcher::check_tool_files(roots.tool_root)?;
    // Facilities 5 and 6 check the delegated group, facility 7 the group holding this process.
    let delegated = delegated_group(&discovered);
    cgroup::check_delegation(delegated, worker_uid()?)?;
    let delegated_controllers = cgroup::check_controllers(delegated)?;
    cgroup::check_sole_occupant(&discovered, own_pid)?;
    cgroup::enter_supervisor(delegated, own_pid)?;
    // Facility 8.
    launcher::check_namespaces(roots.tool_root, &roots.proc_self.join("ns"))?;
    // Facilities 9 and 10.
    let evidence = run_probe(delegated, roots.tool_root)?;
    let launcher_sha256 = sha256_file_hex(&roots.tool_root.join(launcher::LAUNCHER))
        .map_err(|_| AdmissionRefusal::MissingLauncher)?;
    Ok(HostReport {
        kernel_release,
        architecture: env::consts::ARCH,
        launcher_version: launcher::launcher_version(roots.tool_root),
        launcher_sha256,
        policy_sha256: sha256_hex(profile.manifest()),
        core_routing: String::from_utf8_lossy(&routing.pattern)
            .trim_end()
            .to_owned(),
        delegated_controllers,
        effective_limits: evidence.limits,
        peak_memory_bytes: evidence.peak_memory_bytes,
    })
}

/// The group that facilities 5 and 6 check and that the probes run under.
///
/// A process already in its group's `supervisor` child, as after an earlier call in the
/// same process, delegates from the parent. Any other group delegates from itself.
#[must_use]
fn delegated_group(discovered: &Path) -> &Path {
    if discovered.file_name() == Some(OsStr::new(cgroup::SUPERVISOR_GROUP)) {
        discovered.parent().unwrap_or(discovered)
    } else {
        discovered
    }
}

/// What a successful probe run measured.
struct ProbeEvidence {
    /// The limits read back from the probe group, as control file and value.
    limits: Vec<(&'static str, String)>,
    /// The probe group's peak memory in bytes, from `memory.peak`, when it could be read.
    peak_memory_bytes: Option<u64>,
}

/// Facilities 9 and 10: applies the probe limits, then runs the probe child in its own
/// group until the group is killed and seen empty.
///
/// The child is ended and reaped on every failure after it starts. Once the group exists,
/// a failed attempt leaves it in place whatever the failure (0042 rule 6). Only a group that
/// `kill_and_await_empty` saw empty is removed: `rmdir` would also accept an emptied group
/// whose emptiness was never read.
fn run_probe(group: &Path, tool_root: &Path) -> Result<ProbeEvidence, AdmissionRefusal> {
    let name = format!(
        "probe-{}-{}",
        process::id(),
        PROBE_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let probe = probe_group::create_probe(group, &name)?;
    // An error returns here; dropping `probe` leaves the group in place.
    let evidence = probe_in(&probe, tool_root)?;
    probe_group::remove_probe(probe)?;
    Ok(evidence)
}

/// The steps of `run_probe` that run inside an existing probe group.
fn probe_in(
    probe: &probe_group::ProbeGroup,
    tool_root: &Path,
) -> Result<ProbeEvidence, AdmissionRefusal> {
    let limits = probe_group::apply_limits(probe)?;
    let mut child = spawn_probe_child(tool_root)?;
    if let Err(refusal) = observe_kill(probe, &mut child) {
        return Err(end_after_failure(&mut child, refusal));
    }
    Ok(ProbeEvidence {
        limits,
        peak_memory_bytes: probe_group::read_peak(probe),
    })
}

/// Starts the trusted probe child: the fixture in `wait` mode, with a cleared environment,
/// no output, and a standard input that stays open until the group is killed.
fn spawn_probe_child(tool_root: &Path) -> Result<Child, AdmissionRefusal> {
    Command::new(tool_root.join(launcher::FIXTURE_FILE))
        .arg("wait")
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| AdmissionRefusal::ProbeNotStarted)
}

/// Places the started child in the probe group, kills the group, waits until the group is
/// seen empty and reaps the child.
fn observe_kill(
    probe: &probe_group::ProbeGroup,
    child: &mut Child,
) -> Result<(), AdmissionRefusal> {
    probe_group::add_process(probe, child.id())?;
    // An earlier exit would leave an empty group that proves nothing about the kill.
    if !matches!(child.try_wait(), Ok(None)) {
        return Err(AdmissionRefusal::CleanupNotObserved);
    }
    probe_group::kill_and_await_empty(probe, probe_group::CLEANUP_DEADLINE, Instant::now)?;
    reap_within(child, probe_group::CLEANUP_DEADLINE)
}

/// Ends a probe child that is still held after a failed observation. The first refusal is
/// kept unless the child cannot be ended.
fn end_after_failure(child: &mut Child, refusal: AdmissionRefusal) -> AdmissionRefusal {
    match end_child(child) {
        Ok(()) => refusal,
        Err(cleanup) => cleanup,
    }
}

/// Kills a probe child that may still run, then waits for it within the cleanup deadline.
fn end_child(child: &mut Child) -> Result<(), AdmissionRefusal> {
    child
        .kill()
        .map_err(|_| AdmissionRefusal::CleanupNotObserved)?;
    wait_within(child, probe_group::CLEANUP_DEADLINE).map(drop)
}

/// Waits for a killed child and accepts only a `SIGKILL` exit, the status `cgroup.kill`
/// leaves behind.
fn reap_within(child: &mut Child, deadline: Duration) -> Result<(), AdmissionRefusal> {
    match wait_within(child, deadline)?.signal() {
        Some(SIGKILL) => Ok(()),
        _ => Err(AdmissionRefusal::CleanupNotObserved),
    }
}

/// Polls `child` until it exits, for at most `deadline`. Any other outcome is
/// `CleanupNotObserved`.
fn wait_within(child: &mut Child, deadline: Duration) -> Result<ExitStatus, AdmissionRefusal> {
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if started.elapsed() < deadline => thread::sleep(REAP_POLL),
            Ok(None) | Err(_) => return Err(AdmissionRefusal::CleanupNotObserved),
        }
    }
}

#[cfg(test)]
mod tests;
