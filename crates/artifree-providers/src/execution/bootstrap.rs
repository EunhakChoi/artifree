//! The trusted stub and bootstrap of one attempt (decision 0056 rules 3 to 5). Their entry
//! points are the tool root's programs, the test-support program (0056 rule 4) and the server
//! binary's `execution` subcommands (0057 rule 4), chosen by a closed `EntryProgram`. Each takes a
//! profile identity (and the stub the provider's input path) and nothing else, and runs no command
//! it is given.
//!
//! The bootstrap's steps run in this order, with the numbers of their step documents: 1, the
//! identity; 2, the control channel; 3, the leaked descriptors; 4, the scratch in its own mount
//! namespace (the amendment of 2026-11-12, rule 3); 5, the scratch root `/tmp` made the working
//! directory and read back (the amendment of 2026-11-13, crashes); 6, the mount tree made private
//! and `/input` remounted read-only with `nosuid`, `nodev` and `noexec` (the amendment of
//! 2026-11-13, input and output); 7, the mount table read back, with the launcher's empty read-only
//! tmpfs over `/dev/pts` (rule 3 as reworded on 2026-11-13), `/input` and no propagation tag; 8, the
//! limits; 9, the privilege drop; 10, the two seccomp programs, the allowlist last; 11, the
//! confirmation of its own setup. Then it writes the readiness record, waits for the release and
//! replaces itself with the payload.

use std::convert::Infallible;
use std::ffi::{CStr, CString};
use std::fs::{self, File};
use std::io::Read;
use std::mem::ManuallyDrop;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::{fmt, str};

use nix::sched::{self, CloneFlags};
use rustix::mount::{self, MountFlags, MountPropagationFlags};
use rustix::process::{Resource, Rlimit};
use rustix::thread::{CapabilitySet, CapabilitySets};
use rustix::{io, process, stdio, thread};

use super::launcher::EntryProgram;
use super::profile::{self, ExecutionProfile};
use super::{launcher, policy, readiness};

/// The record that starts the launcher; the provider sends it to the stub.
pub(super) const LAUNCH: [u8; 1] = *b"L";
/// The record that the bootstrap writes once every setup step is confirmed.
pub(super) const READY: [u8; 16] = *b"ARTIFREE-READY-1";
/// The record that releases the bootstrap; the provider sends it only after it confirms readiness.
pub(super) const RELEASE: [u8; 1] = *b"R";
/// The record that the bootstrap writes when the payload's `exec` returned.
pub(super) const EXEC_FAILED: [u8; 1] = *b"X";
/// The payload executable inside the tool root, as the bootstrap sees it.
pub(super) const PAYLOAD: &str = "/tools/bin/artifree-execution-payload";

/// The lowest descriptor number for the control channel's close-on-exec copy.
const CONTROL_MIN_DESCRIPTOR: RawFd = 3;
const NULL_DEVICE: &str = "/dev/null";
const PROC_SELF_FD: &str = "/proc/self/fd";
const PROC_STATUS: &str = "/proc/self/status";
const PROC_LIMITS: &str = "/proc/self/limits";
const PROC_MOUNTINFO: &str = "/proc/self/mountinfo";

/// Why the stub or the bootstrap stopped before any payload instruction ran (decision 0056 rules 3 and 5).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum BootstrapFailure {
    /// The identity is not the fixture profile's identity.
    UnknownProfile,
    /// The control channel did not carry the record that this step expects.
    ControlChannel,
    /// The stub could not replace itself with the launcher.
    LauncherNotExecuted,
    /// A descriptor other than the standard streams and the control channel is open.
    LeakedDescriptor,
    /// The scratch could not be mounted in the bootstrap's own mount namespace, or its mount table
    /// did not show both bounds and a read-only root and `/dev`.
    Scratch,
    /// The bootstrap could not make the scratch root `/tmp` its working directory, or `getcwd` did
    /// not read it back as exactly `/tmp` (decision 0056 rule 3, as reworded by the amendment of
    /// 2026-11-13, crashes).
    WorkingDirectory,
    /// The mount table did not show the launcher's empty read-only tmpfs as the last mount of
    /// `/dev/pts` (decision 0056 rules 2 and 3, as reworded by the amendments of 2026-11-13).
    Terminals,
    /// The input path given to the stub is not absolute (decision 0056 rule 2, as reworded by the amendment of 2026-11-13, input and output).
    InputPath,
    /// The mount tree could not be made private or `/input` remounted read-only with `nosuid`, `nodev` and `noexec`, or the mount table did not show `/input` with these four flags and no propagation tag (decision 0056 rule 3, as reworded by the amendment of 2026-11-13).
    Input,
    /// A resource limit could not be set, or did not read back as set.
    Limits,
    /// A capability or `no_new_privs` could not be dropped, or did not read back as dropped.
    Privileges,
    /// The seccomp programs could not be built or installed.
    Filter,
    /// The `/proc` text did not show the required state for the named key, or could not be read.
    NotConfirmed(&'static str),
    /// The provider did not release the bootstrap: end of stream, an error or another byte arrived.
    NotReleased,
    /// The payload's `exec` returned, so the payload did not start.
    PayloadNotExecuted,
}

impl fmt::Display for BootstrapFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownProfile => {
                formatter.write_str("the execution profile identity is unknown")
            }
            Self::ControlChannel => {
                formatter.write_str("the control channel did not carry its record")
            }
            Self::LauncherNotExecuted => formatter.write_str("the launcher could not be executed"),
            Self::LeakedDescriptor => formatter.write_str("a leaked descriptor is open"),
            Self::Scratch => {
                formatter.write_str("the scratch could not be mounted with its bounds")
            }
            Self::WorkingDirectory => {
                formatter.write_str("the working directory is not the scratch root")
            }
            Self::Terminals => formatter.write_str("the terminal directory was not covered"),
            Self::InputPath => formatter.write_str("the input path is not absolute"),
            Self::Input => {
                formatter.write_str("the input mount was not made read-only and private")
            }
            Self::Limits => formatter.write_str("a resource limit did not read back as set"),
            Self::Privileges => formatter.write_str("a privilege did not read back as dropped"),
            Self::Filter => formatter.write_str("the seccomp filter could not be installed"),
            Self::NotConfirmed(field) => {
                write!(
                    formatter,
                    "proc field {field} did not show the required state"
                )
            }
            Self::NotReleased => formatter.write_str("the provider did not release the bootstrap"),
            Self::PayloadNotExecuted => formatter.write_str("the payload could not be executed"),
        }
    }
}

impl std::error::Error for BootstrapFailure {}

/// The trusted stub (0056 rule 3): reads the launch record from descriptor 0 and then replaces itself with the launcher.
///
/// The provider passes the staging directory's path, and the stub puts it in the launcher's fixed list as `--ro-bind <path> /input` (the amendment of 2026-11-13, input and output). The path is the provider's, never a request's.
///
/// It returns only on failure. The launcher receives the fixed attempt arguments, that one read-only bind and an empty environment, and nothing else the caller passes reaches either.
///
/// # Errors
///
/// Returns `UnknownProfile` for an identity other than the fixture profile's, `InputPath` for an input path that is not absolute (checked before the launch record is read), `ControlChannel` when the next byte is not the launch record, and `LauncherNotExecuted` when `exec` returns.
pub fn run_stub(identity: &str, input: &Path) -> Result<Infallible, BootstrapFailure> {
    run_stub_for(EntryProgram::Fixture, identity, input)
}

/// The trusted stub for `program` (0056 rule 3): reads the launch record from descriptor 0 and then replaces itself with the launcher.
///
/// The provider passes the staging directory's path, and the stub puts it in the launcher's fixed list as `--ro-bind <path> /input` (the amendment of 2026-11-13, input and output). The path is the provider's, never a request's.
///
/// The launcher runs `program`'s bootstrap (decision 0057 rule 4). The stub returns only on failure. The launcher receives the fixed attempt arguments, that one read-only bind and an empty environment, and nothing else the caller passes reaches either.
///
/// # Errors
///
/// Returns `UnknownProfile` for an identity other than the fixture profile's, `InputPath` for an input path that is not absolute (checked before the launch record is read), `ControlChannel` when the next byte is not the launch record, and `LauncherNotExecuted` when `exec` returns.
pub fn run_stub_for(
    program: EntryProgram,
    identity: &str,
    input: &Path,
) -> Result<Infallible, BootstrapFailure> {
    let profile = check_identity(identity)?;
    if !input.is_absolute() {
        return Err(BootstrapFailure::InputPath);
    }
    if !next_record_is(stdio::stdin(), LAUNCH) {
        return Err(BootstrapFailure::ControlChannel);
    }
    let root = Path::new(launcher::TOOL_ROOT);
    // `exec` returns only when the launcher did not start. Its error text is not reported; the failure names this step.
    let _ = Command::new(root.join(launcher::LAUNCHER))
        .args(launcher::attempt_arguments_for(
            root, input, program, profile,
        ))
        .env_clear()
        .exec();
    Err(BootstrapFailure::LauncherNotExecuted)
}

/// The trusted bootstrap (0056 rule 3): mounts its scratch in its own mount namespace, makes the scratch root `/tmp` its working directory, makes its mount tree private and remounts `/input` read-only, sets its limits and privileges, installs the filter, reports readiness, waits for the release and then replaces itself with the payload.
///
/// It returns only on failure, and a failure before `READY` is written sends no record. The steps run in the order that the module documentation gives; the control channel is closed by the payload's `exec`, never by a drop after the filter.
///
/// # Errors
///
/// Returns the first failing step: `UnknownProfile`, `ControlChannel`, `LeakedDescriptor`, `Scratch`, `WorkingDirectory`, `Input`, `Terminals`, `Limits`, `Privileges`, `Filter`, `NotConfirmed` (the proc field), `NotReleased` or `PayloadNotExecuted`.
pub fn run_bootstrap(identity: &str) -> Result<Infallible, BootstrapFailure> {
    let profile = check_identity(identity)?;
    let control = take_control()?;
    refuse_leaked_descriptors(control.as_raw_fd())?;
    mount_scratch()?;
    enter_scratch()?;
    isolate_mounts()?;
    confirm_terminals_and_scratch()?;
    set_limits()?;
    drop_privileges()?;
    // The control descriptor is close-on-exec, so the payload's exec closes it. Nothing is dropped
    // after the filter: the descriptor is wrapped so that no drop runs.
    let control = ManuallyDrop::new(control);
    install_filter(profile)?;
    confirm_setup()?;
    write_record(control.as_fd(), &READY)?;
    if !next_record_is(control.as_fd(), RELEASE) {
        return Err(BootstrapFailure::NotReleased);
    }
    // `exec` returns only when the payload did not start. The error text is not reported; the provider learns of the failure from `EXEC_FAILED`.
    // The profile fixes the payload, its arguments and its whole environment (decision 0058
    // rule 1); nothing else reaches it.
    let _ = Command::new(profile.payload)
        .args(profile.arguments)
        .env_clear()
        .envs(profile.environment.iter().copied())
        .exec();
    // Best effort: when this write fails, the provider reads end of stream and counts the attempt as not started.
    write_record(control.as_fd(), &EXEC_FAILED).ok();
    Err(BootstrapFailure::PayloadNotExecuted)
}

/// The profile whose identity is exactly `identity`; any other identity is refused.
fn check_identity(identity: &str) -> Result<&'static ExecutionProfile, BootstrapFailure> {
    profile::by_identity(identity).ok_or(BootstrapFailure::UnknownProfile)
}

/// Step 2: copies the control channel to a close-on-exec descriptor, then makes descriptor 0 read end of stream.
fn take_control() -> Result<OwnedFd, BootstrapFailure> {
    let control = io::fcntl_dupfd_cloexec(stdio::stdin(), CONTROL_MIN_DESCRIPTOR)
        .map_err(|_| BootstrapFailure::ControlChannel)?;
    let null = File::open(NULL_DEVICE).map_err(|_| BootstrapFailure::ControlChannel)?;
    stdio::dup2_stdin(&null).map_err(|_| BootstrapFailure::ControlChannel)?;
    Ok(control)
}

/// Step 3: fails when a descriptor other than 0, 1, 2 and the control channel is open (0042 rule 4; MEASURED (k)).
fn refuse_leaked_descriptors(control: RawFd) -> Result<(), BootstrapFailure> {
    for number in open_descriptor_numbers()? {
        if matches!(number, 0..=2) || number == control {
            continue;
        }
        if fs::symlink_metadata(format!("{PROC_SELF_FD}/{number}")).is_ok() {
            return Err(BootstrapFailure::LeakedDescriptor);
        }
    }
    Ok(())
}

/// The numbers listed in `/proc/self/fd`. The listing's own descriptor is closed before this returns.
fn open_descriptor_numbers() -> Result<Vec<RawFd>, BootstrapFailure> {
    let listing = fs::read_dir(PROC_SELF_FD).map_err(|_| BootstrapFailure::LeakedDescriptor)?;
    let mut numbers = Vec::new();
    for entry in listing {
        let name = entry
            .map_err(|_| BootstrapFailure::LeakedDescriptor)?
            .file_name();
        let number: RawFd = name
            .to_str()
            .and_then(|text| text.parse().ok())
            .ok_or(BootstrapFailure::LeakedDescriptor)?;
        numbers.push(number);
    }
    Ok(numbers)
}

/// Step 4 (decision 0056 rule 3, amended on 2026-11-12): makes the bootstrap's own mount namespace and mounts the scratch there as one tmpfs, `nosuid`, `nodev` and `noexec`, owner-only, with the size and the inode count of decision 0042 rule 5. It comes after the identity, control and descriptor steps, which need no capability, and before the limits.
///
/// `unshare` is called with the mount-namespace flag alone, while the bootstrap is a single thread; that flag is not among those that make the call unsound (0056 rule 7). The capabilities it uses belong to the innermost user namespace and reach only this namespace. The table, which `confirm_terminals_and_scratch` reads back, must show the scratch as required and a read-only root and `/dev`; the provider checks the same table with the same parser before the release. This runs before the filter, so ordinary std file reading is allowed.
fn mount_scratch() -> Result<(), BootstrapFailure> {
    sched::unshare(CloneFlags::CLONE_NEWNS).map_err(|_| BootstrapFailure::Scratch)?;
    let data = CString::new(format!(
        "size={},nr_inodes={},mode={:o}",
        readiness::SCRATCH_SIZE_BYTES,
        readiness::SCRATCH_INODES,
        readiness::SCRATCH_MODE
    ))
    .map_err(|_| BootstrapFailure::Scratch)?;
    mount::mount(
        "tmpfs",
        readiness::SCRATCH,
        "tmpfs",
        MountFlags::NOSUID | MountFlags::NODEV | MountFlags::NOEXEC,
        data.as_c_str(),
    )
    .map_err(|_| BootstrapFailure::Scratch)?;
    Ok(())
}

/// Step 5 (decision 0056 rule 3, as reworded by the amendment of 2026-11-13, crashes): "After the scratch, the bootstrap makes `/tmp` its working directory, and its readback requires it." The payload inherits the working directory through `exec`, so a core under file routing would land in the scratch, never on the host and never in the read-only root. It runs before the filter, which refuses `chdir`, `fchdir` and `getpid`.
fn enter_scratch() -> Result<(), BootstrapFailure> {
    process::chdir(readiness::SCRATCH).map_err(|_| BootstrapFailure::WorkingDirectory)?;
    let cwd = process::getcwd(Vec::new()).map_err(|_| BootstrapFailure::WorkingDirectory)?;
    if is_scratch_root(&cwd) {
        Ok(())
    } else {
        Err(BootstrapFailure::WorkingDirectory)
    }
}

/// Whether a working directory that `getcwd` read back is exactly the scratch root `/tmp`: no
/// prefix, no trailing slash and no directory below it.
fn is_scratch_root(cwd: &CStr) -> bool {
    cwd.to_bytes() == readiness::SCRATCH.as_bytes()
}

/// Step 6 (decision 0056 rule 3, as reworded by the amendment of 2026-11-13, input and output; 0042 rule 4): makes the bootstrap's whole mount tree private and remounts `/input` as a read-only bind with `nosuid`, `nodev` and `noexec`. It runs after the scratch, in the same mount namespace. Measured on the runner: both calls succeed and the payload then sees `/input` as `ro,nosuid,nodev,noexec` with no propagation tag on any line.
fn isolate_mounts() -> Result<(), BootstrapFailure> {
    mount::mount_change(
        "/",
        MountPropagationFlags::REC | MountPropagationFlags::PRIVATE,
    )
    .map_err(|_| BootstrapFailure::Input)?;
    mount::mount_remount(
        readiness::INPUT_MOUNT,
        MountFlags::BIND
            | MountFlags::RDONLY
            | MountFlags::NOSUID
            | MountFlags::NODEV
            | MountFlags::NOEXEC,
        "",
    )
    .map_err(|_| BootstrapFailure::Input)
}

/// Step 7 (decision 0056 rule 3, as reworded by the amendments of 2026-11-13): reads the mount table back. It must show the scratch as required, a read-only root and `/dev`, and, as the last mount of `/dev/pts`, the launcher's empty read-only tmpfs (rule 2), which this mount namespace inherited from the launcher's. It must also show `/input` read-only with `nosuid`, `nodev` and `noexec`, and no propagation tag on any line; `readiness::confirm_mounts` requires these. The bootstrap mounts nothing over `/dev/pts`: the launcher's cover is in both namespaces, and the launcher's init, the first process of the PID namespace, stays in the launcher's, which `/proc/1/root` shows. The provider checks the same table with the same parser before the release.
fn confirm_terminals_and_scratch() -> Result<(), BootstrapFailure> {
    let limit = u64::try_from(readiness::PROC_FILE_BYTES).map_err(|_| BootstrapFailure::Scratch)?;
    let bytes = super::read_bounded(Path::new(PROC_MOUNTINFO), limit)
        .map_err(|_| BootstrapFailure::Scratch)?;
    let text = str::from_utf8(&bytes).map_err(|_| BootstrapFailure::Scratch)?;
    let facts = readiness::parse_mounts(text).map_err(|_| BootstrapFailure::Scratch)?;
    readiness::confirm_mounts(&facts).map_err(|failure| match failure {
        readiness::ReadinessFailure::DevPts => BootstrapFailure::Terminals,
        readiness::ReadinessFailure::Input | readiness::ReadinessFailure::Propagation => {
            BootstrapFailure::Input
        }
        _ => BootstrapFailure::Scratch,
    })
}

/// Step 8: sets the open-file and core-size limits and reads both back.
fn set_limits() -> Result<(), BootstrapFailure> {
    let open_files = Rlimit {
        current: Some(readiness::OPEN_FILES_LIMIT),
        maximum: Some(readiness::OPEN_FILES_LIMIT),
    };
    let core_size = Rlimit {
        current: Some(readiness::CORE_FILE_LIMIT),
        maximum: Some(readiness::CORE_FILE_LIMIT),
    };
    process::setrlimit(Resource::Nofile, open_files).map_err(|_| BootstrapFailure::Limits)?;
    process::setrlimit(Resource::Core, core_size).map_err(|_| BootstrapFailure::Limits)?;
    if process::getrlimit(Resource::Nofile) != open_files
        || process::getrlimit(Resource::Core) != core_size
    {
        return Err(BootstrapFailure::Limits);
    }
    Ok(())
}

/// Step 9: clears the ambient set, removes every named capability from the bounding set, empties the effective, permitted and inheritable sets, sets `no_new_privs`, and reads all of it back.
///
/// The launcher grants `CAP_SYS_ADMIN` and `CAP_SETPCAP` inside the sandbox's own user namespace (the amendment of 2026-11-12, rule 2), and steps 4 and 6 have used the first. This step removes both from every set, the bounding set included. The bounding set is cleared first, one capability at a time, because removing a bounding capability needs `CAP_SETPCAP` in the effective set; the call after the loop empties the effective, permitted and inheritable sets.
fn drop_privileges() -> Result<(), BootstrapFailure> {
    thread::clear_ambient_capability_set().map_err(|_| BootstrapFailure::Privileges)?;
    for capability in named_capabilities() {
        let in_bounding = thread::capability_is_in_bounding_set(capability)
            .map_err(|_| BootstrapFailure::Privileges)?;
        if in_bounding {
            thread::remove_capability_from_bounding_set(capability)
                .map_err(|_| BootstrapFailure::Privileges)?;
        }
    }
    thread::set_capabilities(None, empty_sets()).map_err(|_| BootstrapFailure::Privileges)?;
    thread::set_no_new_privs(true).map_err(|_| BootstrapFailure::Privileges)?;
    confirm_privileges()
}

/// Every capability that rustix names, one bit each. The unnamed remainder of `CapabilitySet::all()` is skipped, since the kernel refuses a query with more than one bit.
fn named_capabilities() -> impl Iterator<Item = CapabilitySet> {
    CapabilitySet::all()
        .iter()
        .filter(|capability| capability.bits().is_power_of_two())
}

/// The capability sets that the bootstrap leaves: effective, permitted and inheritable all empty.
fn empty_sets() -> CapabilitySets {
    CapabilitySets {
        effective: CapabilitySet::empty(),
        permitted: CapabilitySet::empty(),
        inheritable: CapabilitySet::empty(),
    }
}

/// Reads back what `drop_privileges` set and fails for any capability or flag that is not dropped.
fn confirm_privileges() -> Result<(), BootstrapFailure> {
    let sets = thread::capabilities(None).map_err(|_| BootstrapFailure::Privileges)?;
    if sets != empty_sets() {
        return Err(BootstrapFailure::Privileges);
    }
    for capability in named_capabilities() {
        let in_bounding = thread::capability_is_in_bounding_set(capability)
            .map_err(|_| BootstrapFailure::Privileges)?;
        let in_ambient = thread::capability_is_in_ambient_set(capability)
            .map_err(|_| BootstrapFailure::Privileges)?;
        if in_bounding || in_ambient {
            return Err(BootstrapFailure::Privileges);
        }
    }
    let no_new_privs = thread::no_new_privs().map_err(|_| BootstrapFailure::Privileges)?;
    if !no_new_privs {
        return Err(BootstrapFailure::Privileges);
    }
    Ok(())
}

/// Step 10: installs the profile's two seccomp programs in order (decision 0042 rule 4; 0056 rule 3).
///
/// The clone3 refusal is installed first and the allowlist LAST. Each `apply_filter` call needs `prctl` and `seccomp`, which the allowlist does not admit, so an allowlist installed earlier would kill the process (MEASURED (f)). `confirm_setup` then requires two filters.
fn install_filter(profile: &ExecutionProfile) -> Result<(), BootstrapFailure> {
    let programs = policy::filters(profile.filter).map_err(|_| BootstrapFailure::Filter)?;
    for program in &programs {
        seccompiler::apply_filter(program).map_err(|_| BootstrapFailure::Filter)?;
    }
    Ok(())
}

/// Step 11: confirms the bootstrap's own status and limits with the functions the provider uses (decision 0042 rule 6).
fn confirm_setup() -> Result<(), BootstrapFailure> {
    let mut status_buffer = [0_u8; readiness::PROC_FILE_BYTES];
    let status = readiness::parse_status(read_proc_text(PROC_STATUS, &mut status_buffer)?)
        .map_err(not_confirmed)?;
    readiness::confirm_status(&status).map_err(not_confirmed)?;
    let mut limits_buffer = [0_u8; readiness::PROC_FILE_BYTES];
    let limits = readiness::parse_limits(read_proc_text(PROC_LIMITS, &mut limits_buffer)?)
        .map_err(not_confirmed)?;
    readiness::confirm_limits(&limits).map_err(not_confirmed)
}

/// Reads the text of one `/proc` file into `buffer` with `open` and `read` only.
///
/// The descriptor is never closed here: it is close-on-exec, so the payload's exec closes it, and nothing is dropped after the filter. A text that fills the buffer is refused, since it may be larger than the bound.
fn read_proc_text<'buffer>(
    path: &str,
    buffer: &'buffer mut [u8; readiness::PROC_FILE_BYTES],
) -> Result<&'buffer str, BootstrapFailure> {
    let mut file = ManuallyDrop::new(File::open(path).map_err(|_| read_failure())?);
    let mut filled = 0_usize;
    loop {
        let Some(rest) = buffer.get_mut(filled..) else {
            return Err(read_failure());
        };
        if rest.is_empty() {
            return Err(read_failure());
        }
        let count = file.read(rest).map_err(|_| read_failure())?;
        if count == 0 {
            break;
        }
        filled += count;
    }
    let text = buffer.get(..filled).ok_or_else(read_failure)?;
    str::from_utf8(text).map_err(|_| read_failure())
}

/// The failure of a `/proc` read. No proc key is concerned, so the field is named `read`.
fn read_failure() -> BootstrapFailure {
    BootstrapFailure::NotConfirmed("read")
}

/// Maps a readiness failure to the failure that names its proc key.
fn not_confirmed(failure: readiness::ReadinessFailure) -> BootstrapFailure {
    BootstrapFailure::NotConfirmed(failure.field())
}

/// Reads one byte from `descriptor`, retrying interrupted reads. `None` means end of stream or an error.
fn read_byte(descriptor: BorrowedFd<'_>) -> Option<u8> {
    let mut buffer = [0_u8; 1];
    let count = io::retry_on_intr(|| io::read(descriptor, &mut buffer)).ok()?;
    let [byte] = buffer;
    (count == 1).then_some(byte)
}

/// Whether the next byte on `descriptor` is the only byte of `record`. End of stream and errors are not.
fn next_record_is(descriptor: BorrowedFd<'_>, record: [u8; 1]) -> bool {
    let [expected] = record;
    read_byte(descriptor) == Some(expected)
}

/// Writes all of `record` to `descriptor`, retrying interrupted writes.
fn write_record(descriptor: BorrowedFd<'_>, record: &[u8]) -> Result<(), BootstrapFailure> {
    let mut rest = record;
    while !rest.is_empty() {
        let count = io::retry_on_intr(|| io::write(descriptor, rest))
            .map_err(|_| BootstrapFailure::ControlChannel)?;
        if count == 0 {
            return Err(BootstrapFailure::ControlChannel);
        }
        let Some(sent) = rest.get(count..) else {
            return Err(BootstrapFailure::ControlChannel);
        };
        rest = sent;
    }
    Ok(())
}

#[cfg(test)]
mod working_directory_tests;
