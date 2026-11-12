//! The trusted namespace check and the attempt's fixed argument list through the tool root's launcher (decisions 0042 rule 1, 0056 rule 2). Runs no payload.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::str;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use super::{AdmissionRefusal, InspectionProfile};

/// One directory per version, operator-installed and not writable by the provider (0056 rule 6). Must equal `ROOT` in `scripts/enforcement/*.sh`.
pub(super) const TOOL_ROOT: &str = "/opt/artifree/tools/v1";
/// The launcher inside the tool root: a copy of bubblewrap at a fixed path.
pub(super) const LAUNCHER: &str = "bin/bwrap";
/// The fixture inside the tool root, run inside the launcher for the namespace check and as the probe child.
pub(super) const FIXTURE_FILE: &str = "bin/artifree-execution-fixture";
/// The seven namespaces compared with the supervisor's own (0042 rule 1).
pub(super) const NAMESPACES: [&str; 7] = ["user", "pid", "net", "mnt", "ipc", "uts", "cgroup"];
/// Bound on the fixture's namespace report (this slice's choice; not an attempt bound).
pub(super) const MAX_OUTPUT_BYTES: u64 = 4096;
/// Bound on the launcher's `--version` output.
const VERSION_OUTPUT_BYTES: u64 = 256;
/// Wall deadline for one trusted launcher run; expiry kills it (this slice's choice; not an attempt bound).
pub(super) const RUN_DEADLINE: Duration = Duration::from_secs(10);
/// How long a killed launcher may take to exit before the check gives up on it (this slice's choice).
const REAP_DEADLINE: Duration = Duration::from_secs(5);
/// How often the supervisor polls a running launcher.
const POLL_INTERVAL: Duration = Duration::from_millis(10);
/// How long the output may take to arrive after the launcher has exited (this slice's choice).
const OUTPUT_GRACE: Duration = Duration::from_secs(1);
/// The namespace check's scratch tmpfs size in bytes (R02aa's choice; not an attempt bound).
const CHECK_SCRATCH_BYTES: &str = "1048576";
/// The attempt's scratch tmpfs size in bytes: decision 0042 rule 5, "One 128 MiB tmpfs" for scratch and output.
const ATTEMPT_SCRATCH_BYTES: &str = "134217728";

/// Checks that the tool root holds a regular, executable launcher and then a regular, executable fixture.
///
/// # Errors
///
/// Returns `MissingLauncher` or `MissingFixture` for the first file that is missing, is a link, is not a regular file, or has no execute bit.
pub(super) fn check_tool_files(root: &Path) -> Result<(), AdmissionRefusal> {
    if !is_regular_executable(&root.join(LAUNCHER)) {
        return Err(AdmissionRefusal::MissingLauncher);
    }
    if !is_regular_executable(&root.join(FIXTURE_FILE)) {
        return Err(AdmissionRefusal::MissingFixture);
    }
    Ok(())
}

/// Whether `path` is a regular file with an execute bit. A symbolic link is not followed, so it is never a regular file.
#[must_use]
fn is_regular_executable(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        metadata.file_type().is_file() && (metadata.permissions().mode() & 0o111) != 0
    })
}

/// The fixed argument list for the namespace check, in launch order.
///
/// Each namespace has its own explicit `--unshare-*` flag. `--unshare-all` is not used because its user and cgroup entries are `-try` forms that skip a namespace silently when the kernel refuses it; the namespace comparison would refuse a skipped namespace too, but an explicit flag makes the launcher fail instead of continuing without that namespace. `--disable-userns` stops a further user namespace, which was measured to be possible without it. The root is bound read-only as `/tools`, and its `lib` and `lib64` directories are bound as `/lib` and `/lib64`.
#[must_use]
pub(super) fn launcher_arguments(root: &Path) -> Vec<OsString> {
    fixed_arguments(root, CHECK_SCRATCH_BYTES, &["namespaces"])
}

/// The attempt's fixed argument list, in launch order (0056 rule 2): the namespace check's list with the 128 MiB scratch tmpfs, ending in the fixture's `bootstrap` mode with the fixture profile's identity.
///
/// Nothing a request carries reaches this list. The scratch's inode bound is R02ac's (0056 rule 17).
#[must_use]
pub(super) fn attempt_arguments(root: &Path) -> Vec<OsString> {
    fixed_arguments(
        root,
        ATTEMPT_SCRATCH_BYTES,
        &["bootstrap", InspectionProfile::fixture().identity()],
    )
}

/// Builds the launcher's fixed argument list for `root` with `scratch_bytes` as the tmpfs size, followed by the inner fixture command and `fixture_arguments`. Both the namespace check and the attempt use this one list, so they cannot drift apart.
#[must_use]
fn fixed_arguments(root: &Path, scratch_bytes: &str, fixture_arguments: &[&str]) -> Vec<OsString> {
    let mut arguments: Vec<OsString> = vec![
        "--unshare-user".into(),
        "--unshare-pid".into(),
        "--unshare-net".into(),
        "--unshare-ipc".into(),
        "--unshare-uts".into(),
        "--unshare-cgroup".into(),
        "--disable-userns".into(),
        "--die-with-parent".into(),
        "--new-session".into(),
        "--clearenv".into(),
        "--ro-bind".into(),
        root.as_os_str().to_os_string(),
        "/tools".into(),
        "--ro-bind".into(),
        root.join("lib").into_os_string(),
        "/lib".into(),
        "--ro-bind".into(),
        root.join("lib64").into_os_string(),
        "/lib64".into(),
        "--proc".into(),
        "/proc".into(),
        "--dev".into(),
        "/dev".into(),
        "--size".into(),
        scratch_bytes.into(),
        "--tmpfs".into(),
        "/tmp".into(),
        "--chdir".into(),
        "/".into(),
        "/tools/bin/artifree-execution-fixture".into(),
    ];
    arguments.extend(
        fixture_arguments
            .iter()
            .map(|argument| OsString::from(*argument)),
    );
    arguments
}

/// Why a bounded run of a trusted command produced no usable output.
#[derive(Debug, Eq, PartialEq)]
pub(super) enum RunFailure {
    /// The command, or the thread that reads its output, could not be started.
    Spawn,
    /// The exit status could not be polled, the output could not be read, or the child could not be killed.
    Wait,
    /// The wall deadline expired, or the output did not arrive shortly after the exit.
    Deadline,
    /// The output was longer than its bound.
    Oversize,
    /// The command exited unsuccessfully.
    Exit,
}

/// Runs a trusted command with a cleared environment, no standard input and a bounded standard output, and kills it at `deadline`.
///
/// The command is spawned and waited for on the calling thread: a parent-death signal follows the thread that created the child, so that thread must stay alive until the child is reaped. The output is read on a helper thread that is never joined: a descendant that inherited the pipe must not hang the check, so a helper still blocked after the exit is reported as `RunFailure::Deadline`.
///
/// # Errors
///
/// Returns `RunFailure::Spawn` when the command or the reader cannot start, `Wait` when the status or the output cannot be read, `Deadline` when the wall deadline expires or the output does not arrive shortly after the exit, `Oversize` when the output is longer than `limit`, and `Exit` when the command exits unsuccessfully. A child still running is killed and reaped before a failure is returned, unless the kill itself fails.
pub(super) fn run_bounded(
    mut command: Command,
    limit: u64,
    deadline: Duration,
) -> Result<Vec<u8>, RunFailure> {
    command
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn().map_err(|_| RunFailure::Spawn)?;
    let Some(stdout) = child.stdout.take() else {
        kill_and_reap(&mut child)?;
        return Err(RunFailure::Spawn);
    };
    let (sender, receiver) = mpsc::channel::<io::Result<Vec<u8>>>();
    // Detached on purpose: dropping the handle at the end of the call leaves the reader running.
    let Ok(_reader) = thread::Builder::new().spawn(move || {
        let mut bytes = Vec::new();
        let read = stdout
            .take(limit.saturating_add(1))
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        sender.send(read)
    }) else {
        kill_and_reap(&mut child)?;
        return Err(RunFailure::Spawn);
    };
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(_) => {
                kill_and_reap(&mut child)?;
                return Err(RunFailure::Wait);
            }
        }
        if start.elapsed() >= deadline {
            kill_and_reap(&mut child)?;
            return Err(RunFailure::Deadline);
        }
        thread::sleep(POLL_INTERVAL);
    };
    match receiver.recv_timeout(OUTPUT_GRACE) {
        Err(_) => Err(RunFailure::Deadline),
        Ok(Err(_)) => Err(RunFailure::Wait),
        Ok(Ok(bytes)) => {
            let within_limit = u64::try_from(bytes.len()).is_ok_and(|length| length <= limit);
            if !within_limit {
                return Err(RunFailure::Oversize);
            }
            if !status.success() {
                return Err(RunFailure::Exit);
            }
            Ok(bytes)
        }
    }
}

/// Kills a child that may still be running, then reaps it within `REAP_DEADLINE`. A failed kill is returned without waiting, because waiting on a live child could block the check; a killed child that does not exit in time (a kernel wait that the signal cannot interrupt) is `RunFailure::Deadline`.
fn kill_and_reap(child: &mut Child) -> Result<(), RunFailure> {
    child.kill().map_err(|_| RunFailure::Wait)?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) if start.elapsed() < REAP_DEADLINE => thread::sleep(POLL_INTERVAL),
            Ok(None) => return Err(RunFailure::Deadline),
            Err(_) => return Err(RunFailure::Wait),
        }
    }
}

/// Reads the supervisor's seven namespace links under `proc_self_ns` (normally `/proc/self/ns`), in `NAMESPACES` order.
///
/// # Errors
///
/// Returns `NamespacesUnavailable` when a link cannot be read or its target is not valid text.
pub(super) fn supervisor_namespaces(
    proc_self_ns: &Path,
) -> Result<Vec<(&'static str, String)>, AdmissionRefusal> {
    let mut namespaces = Vec::with_capacity(NAMESPACES.len());
    for name in NAMESPACES {
        let link = fs::read_link(proc_self_ns.join(name))
            .map_err(|_| AdmissionRefusal::NamespacesUnavailable)?;
        let target = link
            .into_os_string()
            .into_string()
            .map_err(|_| AdmissionRefusal::NamespacesUnavailable)?;
        namespaces.push((name, target));
    }
    Ok(namespaces)
}

/// Compares the launcher's `name=target` lines with the supervisor's own namespace targets.
///
/// Each of the seven namespaces must appear exactly once, with a non-empty target that differs from the supervisor's. Any other line, name or repeat is refused.
///
/// # Errors
///
/// Returns `NamespacesUnavailable` for output that does not meet those rules.
pub(super) fn compare_namespaces(
    own: &[(&'static str, String)],
    output: &[u8],
) -> Result<(), AdmissionRefusal> {
    let text = str::from_utf8(output).map_err(|_| AdmissionRefusal::NamespacesUnavailable)?;
    let mut seen: Vec<&str> = Vec::with_capacity(NAMESPACES.len());
    for line in text.lines() {
        let (name, target) = line
            .split_once('=')
            .ok_or(AdmissionRefusal::NamespacesUnavailable)?;
        if !NAMESPACES.contains(&name) || seen.contains(&name) {
            return Err(AdmissionRefusal::NamespacesUnavailable);
        }
        let own_target = own
            .iter()
            .find(|(own_name, _)| *own_name == name)
            .map(|(_, recorded)| recorded.as_str())
            .ok_or(AdmissionRefusal::NamespacesUnavailable)?;
        if target.is_empty() || target == own_target {
            return Err(AdmissionRefusal::NamespacesUnavailable);
        }
        seen.push(name);
    }
    if seen.len() == NAMESPACES.len() {
        Ok(())
    } else {
        Err(AdmissionRefusal::NamespacesUnavailable)
    }
}

/// Runs the launcher's fixed namespace check and compares its namespaces with the supervisor's own.
///
/// # Errors
///
/// Returns `NamespacesUnavailable` when the supervisor's links cannot be read, the launcher cannot run, exceeds its bounds or exits unsuccessfully, or its output is not seven separate namespaces that each differ from the supervisor's.
pub(super) fn check_namespaces(root: &Path, proc_self_ns: &Path) -> Result<(), AdmissionRefusal> {
    let own = supervisor_namespaces(proc_self_ns)?;
    let mut command = Command::new(root.join(LAUNCHER));
    command.args(launcher_arguments(root));
    let output = run_bounded(command, MAX_OUTPUT_BYTES, RUN_DEADLINE)
        .map_err(|_| AdmissionRefusal::NamespacesUnavailable)?;
    compare_namespaces(&own, &output)
}

/// The launcher's `--version` text, trimmed, when it is non-empty and made only of printable ASCII and spaces; otherwise `None`.
///
/// The launcher runs under the same cleared environment and wall deadline as the namespace check, with a smaller output bound.
#[must_use]
pub(super) fn launcher_version(root: &Path) -> Option<String> {
    let mut command = Command::new(root.join(LAUNCHER));
    command.arg("--version");
    let bytes = run_bounded(command, VERSION_OUTPUT_BYTES, RUN_DEADLINE).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let version = text.trim();
    let printable = !version.is_empty()
        && version
            .chars()
            .all(|character| character.is_ascii_graphic() || character == ' ');
    printable.then(|| version.to_owned())
}

#[cfg(test)]
mod tests;
