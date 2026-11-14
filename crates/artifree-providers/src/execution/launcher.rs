//! The trusted namespace check and the attempt's fixed argument list through the tool root's launcher (decisions 0042 rule 1, 0056 rule 2 with its amendment of 2026-11-12). The check and the attempt share one namespace and bind prefix; the attempt's scratch is mounted by its bootstrap rather than by the launcher. Runs no payload.

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
/// The fixture's path inside the sandbox, where the tool root is bound as `/tools`.
const FIXTURE_COMMAND: &str = "/tools/bin/artifree-execution-fixture";
/// The server binary inside the tool root (decision 0057 rule 4).
#[cfg(any(test, feature = "enforcement-fixture"))]
pub(super) const SERVER_FILE: &str = "bin/artifree-server";
/// The server binary's path inside the sandbox.
const SERVER_COMMAND: &str = "/tools/bin/artifree-server";

/// The program of the tool root that runs an attempt's stub, bootstrap and guardian (decision 0057 rule 4). Admission's namespace check and probe child always use the fixture program.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryProgram {
    /// `bin/artifree-execution-fixture`, the test-support program, whose mode is its first argument.
    Fixture,
    /// `bin/artifree-server`, whose modes follow its `execution` subcommand.
    Server,
}

impl EntryProgram {
    /// The program's file inside the tool root.
    #[cfg(any(test, feature = "enforcement-fixture"))]
    #[must_use]
    pub(super) fn file(self) -> &'static str {
        match self {
            Self::Fixture => FIXTURE_FILE,
            Self::Server => SERVER_FILE,
        }
    }

    /// The program's path inside the sandbox.
    #[must_use]
    pub(super) fn command(self) -> &'static str {
        match self {
            Self::Fixture => FIXTURE_COMMAND,
            Self::Server => SERVER_COMMAND,
        }
    }

    /// The arguments that the program takes before the mode word `bootstrap`.
    #[must_use]
    pub(super) fn prefix(self) -> &'static [&'static str] {
        match self {
            Self::Fixture => &[],
            Self::Server => &["execution"],
        }
    }
}

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

/// The shared prefix of the namespace check and the attempt, in launch order: every namespace unshared, the tool root bound read-only, and a private `/proc` and `/dev`.
///
/// Each namespace has its own explicit `--unshare-*` flag. `--unshare-all` is not used because its user and cgroup entries are `-try` forms that skip a namespace silently when the kernel refuses it; the namespace comparison would refuse a skipped namespace too, but an explicit flag makes the launcher fail instead of continuing without that namespace. `--disable-userns` stops a further user namespace, which was measured to be possible without it. The root is bound read-only as `/tools`, and its `lib` and `lib64` directories are bound as `/lib` and `/lib64`.
#[must_use]
fn sandbox_arguments(root: &Path) -> Vec<OsString> {
    vec![
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
    ]
}

/// The fixed argument list for the namespace check, in launch order: the shared prefix, then the check's 1 MiB scratch tmpfs at `/tmp` and the fixture's `namespaces` mode.
///
/// The check still mounts its scratch through the launcher. Only the attempt moves the scratch into its bootstrap (0056 rule 2 with its amendment of 2026-11-12), so the check keeps its own `--size` and `--tmpfs` pair.
#[must_use]
pub(super) fn launcher_arguments(root: &Path) -> Vec<OsString> {
    let mut arguments = sandbox_arguments(root);
    arguments.extend(
        [
            "--size",
            CHECK_SCRATCH_BYTES,
            "--tmpfs",
            "/tmp",
            "--chdir",
            "/",
            FIXTURE_COMMAND,
            "namespaces",
        ]
        .map(OsString::from),
    );
    arguments
}

/// The permission bits of the empty tmpfs that the attempt's launcher mounts over `/dev/pts`: read and search only (0056, amendment of 2026-11-13, the launcher's `/dev/pts`).
const TERMINALS_MODE: &str = "0555";
/// The size bound of that tmpfs in bytes, the smallest the kernel rounds to: one page.
const TERMINALS_BYTES: &str = "4096";

/// The attempt's fixed argument list, in launch order (0056 rule 2 with its amendments of 2026-11-12 and 2026-11-13): the shared prefix, the staged input bound read-only at `/input`, the two capabilities, the scratch's mount point, an empty tmpfs over the minimal `/dev`'s `/dev/pts`, the read-only remounts, then `program`'s `bootstrap` mode with the fixture profile's identity.
///
/// The fixed argument list binds the attempt's staging directory read-only at `/input`. Its path comes from the provider (the amendment of 2026-11-13, input and output), and it is absolute, since the stub refuses any other path before this list is built. It is never a request's.
///
/// The launcher's init is the first process of the attempt's PID namespace and stays in the launcher's mount namespace, so `/proc/1/root` shows that namespace to the payload. The bootstrap covers `/dev/pts` in its own namespace; this tmpfs covers the launcher's, so neither path opens a pseudo-terminal master (measured on the runner on 2026-11-13).
///
/// The launcher no longer mounts the scratch. It makes the mount point, remounts the sandbox's root and `/dev` read-only, and hands the bootstrap two capabilities inside the sandbox's user namespace, `CAP_SYS_ADMIN` and `CAP_SETPCAP`. The bootstrap mounts the scratch in its own mount namespace with the size and inode bounds of 0042 rule 5, and it removes both capabilities from every set, the bounding set included, before the seccomp filter (0056 rule 3). The capabilities are over the sandbox's own namespaces only: they belong to the user namespace that `--unshare-user` creates, which owns the other namespaces of the sandbox, and they grant nothing over the host.
///
/// The `--remount-ro` options come after every bind, `--proc`, `--dev` and `--dir` option, because a mount point created after a read-only remount would land on a read-only tree and fail.
///
/// Nothing a request carries reaches this list, and the scratch's size and inode bounds are set by the bootstrap, not here.
///
/// `program` names the bootstrap's program (decision 0057 rule 4).
#[must_use]
pub(super) fn attempt_arguments_for(
    root: &Path,
    input: &Path,
    program: EntryProgram,
) -> Vec<OsString> {
    let mut arguments = sandbox_arguments(root);
    // The staged input, read-only at `/input` (0056 rule 2, as reworded by the amendment of 2026-11-13, input and output).
    arguments.extend([
        OsString::from("--ro-bind"),
        input.as_os_str().to_owned(),
        OsString::from("/input"),
    ]);
    arguments.extend(
        [
            "--cap-add",
            "CAP_SYS_ADMIN",
            "--cap-add",
            "CAP_SETPCAP",
            "--dir",
            "/tmp",
            "--perms",
            TERMINALS_MODE,
            "--size",
            TERMINALS_BYTES,
            "--tmpfs",
            "/dev/pts",
            "--remount-ro",
            "/",
            "--remount-ro",
            "/dev",
            "--remount-ro",
            "/dev/pts",
            "--chdir",
            "/",
        ]
        .map(OsString::from),
    );
    arguments.push(program.command().into());
    arguments.extend(program.prefix().iter().map(OsString::from));
    arguments.push("bootstrap".into());
    arguments.push(InspectionProfile::fixture().identity().into());
    arguments
}

/// The fixture's attempt argument list, for the tests that pin it (decision 0056 rule 2).
#[cfg(test)]
#[must_use]
pub(super) fn attempt_arguments(root: &Path, input: &Path) -> Vec<OsString> {
    attempt_arguments_for(root, input, EntryProgram::Fixture)
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
mod attempt_tests;
#[cfg(test)]
mod tests;
