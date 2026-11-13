//! Test-only fixture for the Linux enforcement job (decision 0056 rule 4).
//!
//! Built by the tests only and shipped in no release archive. Its arguments
//! select the mode:
//!
//! - `namespaces` prints one `name=target` line per namespace of this process
//!   to stdout.
//! - `wait <input>` reads stdin to end of file and discards what it read. The
//!   trailing path is the provider's staged input, which this mode ignores.
//! - `false-ready <input>` writes the READY record on descriptor 0 without
//!   limits, a privilege drop or a filter, then reads descriptor 0 to end of
//!   file. It is a negative control for the provider's readiness check and execs
//!   nothing. Its trailing path is ignored as well.
//! - `stub <identity> <input>` runs the launch stub with the provider's staged
//!   input directory as `<input>` (decision 0056, amendment of 2026-11-13).
//!   `bootstrap <identity>` runs the bootstrap (decision 0056 rule 3). On success
//!   each one replaces this process with the next program, so they return only
//!   on failure.
//! - `write-probes <directory>` writes the generated probe programs of decision
//!   0056 (amendment of 2026-11-13) into the directory, one file per probe with
//!   mode 0o755, and exits 0. The job's `prepare.sh` runs it while it assembles
//!   the tool root.
//! - `core-collector <count-file>` reads standard input to its end and appends
//!   one line `invocation bytes=<n>` to the count file, created with mode 0o600
//!   when it is missing, then exits 0. A run whose read fails appends
//!   `invocation error=<kind>` instead. The job's pipe routing runs a root-owned
//!   copy of this program as the kernel's core collector (decision 0056,
//!   amendment of 2026-11-13, crashes).
//! - `core-listener <socket-path> <count-file>` refuses a socket path that
//!   exists, listens there and appends one line `connection bytes=<n>` for each
//!   connection, read to its end. A connection whose read fails is recorded as
//!   `connection error=<kind>`. It runs until it is killed; the job's socket
//!   routing names its socket.
//!
//! `false-ready`, `core-collector`, `core-listener`, `stub`, `bootstrap` and
//! `write-probes` exist on Linux only. Elsewhere, and for any other argument list
//! (including an identity that is not UTF-8), the exit status is 2. A failing
//! `false-ready`, `core-collector` or `core-listener` writes its error to stderr
//! and exits with status 1. A failing `stub`, `bootstrap` or `write-probes` writes
//! its failure to stderr and exits with status 70. Status 3 means that a failing
//! mode could not write its error to stderr at all. The two counter modes are
//! test support, never part of a release.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt::Display;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

#[cfg(target_os = "linux")]
use artifree_providers::execution::{
    BootstrapFailure, run_bootstrap, run_stub, write_probe_programs,
};
#[cfg(target_os = "linux")]
use std::convert::Infallible;
#[cfg(target_os = "linux")]
use std::os::unix::net::UnixListener;

/// Namespaces that `namespaces` reports, in output order.
const NAMESPACES: [&str; 7] = ["user", "pid", "net", "mnt", "ipc", "uts", "cgroup"];

fn main() -> ExitCode {
    let arguments: Vec<OsString> = env::args_os().skip(1).collect();
    match arguments.as_slice() {
        [mode] => run_mode(mode),
        [mode, directory] if mode == "write-probes" => write_probes(Path::new(directory)),
        // The provider appends the staged input's path to every stub argument list (0056,
        // amendment of 2026-11-13, input and output); the two negative controls ignore it.
        [mode, _input] if mode == "wait" || mode == "false-ready" => run_mode(mode),
        [mode, count] if mode == "core-collector" => core_collector(Path::new(count)),
        [mode, identity] => match (mode.to_str(), identity.to_str()) {
            (Some(name), Some(value)) => run_identity_mode(name, value),
            _ => ExitCode::from(2),
        },
        [mode, socket, count] if mode == "core-listener" => {
            core_listener(Path::new(socket), Path::new(count))
        }
        [mode, identity, input] if mode == "stub" => match identity.to_str() {
            Some(value) => run_stub_mode(value, Path::new(input)),
            None => ExitCode::from(2),
        },
        _ => ExitCode::from(2),
    }
}

/// Runs a mode that takes no identity argument.
fn run_mode(mode: &OsStr) -> ExitCode {
    match mode.to_str() {
        Some("namespaces") => finish(print_namespaces()),
        Some("wait") => finish(wait_for_end_of_input()),
        #[cfg(target_os = "linux")]
        Some("false-ready") => finish(false_ready()),
        _ => ExitCode::from(2),
    }
}

/// Runs the bootstrap with its identity. The `stub` mode takes the staged input as well, so it
/// is handled by `run_stub_mode`.
#[cfg(target_os = "linux")]
fn run_identity_mode(mode: &str, identity: &str) -> ExitCode {
    if mode == "bootstrap" {
        run_child(run_bootstrap, identity)
    } else {
        ExitCode::from(2)
    }
}

/// Other systems have no bootstrap, so the mode exits with status 2.
#[cfg(not(target_os = "linux"))]
fn run_identity_mode(_mode: &str, _identity: &str) -> ExitCode {
    ExitCode::from(2)
}

/// Runs the launch stub with its identity and the provider's staged input, which the stub
/// receives as its last argument.
///
/// A failure is written to stderr and exits with status 70.
#[cfg(target_os = "linux")]
fn run_stub_mode(identity: &str, input: &Path) -> ExitCode {
    match run_stub(identity, input) {
        Ok(never) => match never {},
        Err(failure) => report_failure(&failure, ExitCode::from(70)),
    }
}

/// Other systems have no launch stub, so the mode exits with status 2.
#[cfg(not(target_os = "linux"))]
fn run_stub_mode(_identity: &str, _input: &Path) -> ExitCode {
    ExitCode::from(2)
}

/// Writes the generated probe programs into `directory`.
///
/// A failure is written to stderr and exits with status 70.
#[cfg(target_os = "linux")]
fn write_probes(directory: &Path) -> ExitCode {
    match write_probe_programs(directory) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => report_failure(&error, ExitCode::from(70)),
    }
}

/// Other systems have no generated probe programs, so the mode exits with status 2.
#[cfg(not(target_os = "linux"))]
fn write_probes(_directory: &Path) -> ExitCode {
    ExitCode::from(2)
}

/// Runs the bootstrap entry point, which returns only on failure.
///
/// A failure is written to stderr and exits with status 70.
#[cfg(target_os = "linux")]
fn run_child(run: fn(&str) -> Result<Infallible, BootstrapFailure>, identity: &str) -> ExitCode {
    match run(identity) {
        Ok(never) => match never {},
        Err(failure) => report_failure(&failure, ExitCode::from(70)),
    }
}

/// Prints one `name=target` line per namespace of this process.
///
/// The whole text is built before anything is written, so a failure prints nothing.
fn print_namespaces() -> io::Result<()> {
    let mut text = String::new();
    for name in NAMESPACES {
        let link = fs::read_link(format!("/proc/self/ns/{name}"))?;
        let target = link.to_str().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "namespace target is not UTF-8")
        })?;
        text.push_str(name);
        text.push('=');
        text.push_str(target);
        text.push('\n');
    }
    let mut stdout = io::stdout().lock();
    stdout.write_all(text.as_bytes())?;
    stdout.flush()
}

/// Reads standard input to end of file and discards what it read.
fn wait_for_end_of_input() -> io::Result<()> {
    io::copy(&mut io::stdin().lock(), &mut io::sink()).map(drop)
}

/// Negative control for the provider's readiness check.
///
/// Writes the READY record on descriptor 0 without any limit, privilege drop or
/// filter, then reads descriptor 0 to end of file. It execs nothing.
#[cfg(target_os = "linux")]
fn false_ready() -> io::Result<()> {
    use std::io::Read;
    use std::os::fd::AsFd;

    let mut launch = [0_u8; 1];
    io::stdin().lock().read_exact(&mut launch)?;
    let mut socket = fs::File::from(io::stdin().as_fd().try_clone_to_owned()?);
    // The same 16 bytes as the provider's READY record; a drift makes the attempt test see no readiness and fail.
    socket.write_all(b"ARTIFREE-READY-1")?;
    drop(socket);
    io::copy(&mut io::stdin().lock(), &mut io::sink()).map(drop)
}

/// Runs the kernel's pipe collector of the job's counter (decision 0056, amendment of
/// 2026-11-13, crashes): reads standard input to its end and appends one line to `count`.
///
/// A failed read is written to stderr and exits with status 1.
#[cfg(target_os = "linux")]
fn core_collector(count: &Path) -> ExitCode {
    finish(collect_core(count))
}

/// Counts the bytes of one dump on standard input and appends `invocation bytes=<n>` to `count`.
///
/// A run whose read fails still appends `invocation error=<kind>`, so that a broken read cannot
/// hide a dump, and then reports its error.
#[cfg(target_os = "linux")]
fn collect_core(count: &Path) -> io::Result<()> {
    match io::copy(&mut io::stdin().lock(), &mut io::sink()) {
        Ok(bytes) => append_count(count, &format!("invocation bytes={bytes}\n")),
        Err(error) => {
            append_count(count, &format!("invocation error={:?}\n", error.kind()))?;
            Err(error)
        }
    }
}

/// Other systems have no pipe collector, so the mode exits with status 2.
#[cfg(not(target_os = "linux"))]
fn core_collector(_count: &Path) -> ExitCode {
    ExitCode::from(2)
}

/// Runs the socket listener of the job's counter (decision 0056, amendment of 2026-11-13,
/// crashes). It runs until it is killed.
///
/// A failure is written to stderr and exits with status 1.
#[cfg(target_os = "linux")]
fn core_listener(socket: &Path, count: &Path) -> ExitCode {
    finish(serve_cores(socket, count))
}

/// Binds a Unix stream listener at `socket` and counts the bytes of each connection in `count`.
///
/// Refuses a socket path that exists, including a dangling symbolic link.
#[cfg(target_os = "linux")]
fn serve_cores(socket: &Path, count: &Path) -> io::Result<()> {
    if fs::symlink_metadata(socket).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "the socket path already exists",
        ));
    }
    let listener = UnixListener::bind(socket)?;
    for stream in listener.incoming() {
        let mut stream = stream?;
        // A connection counts even when its read fails, so that a broken read cannot hide a dump.
        let line = match io::copy(&mut stream, &mut io::sink()) {
            Ok(bytes) => format!("connection bytes={bytes}\n"),
            Err(error) => format!("connection error={:?}\n", error.kind()),
        };
        append_count(count, &line)?;
    }
    Ok(())
}

/// Other systems have no socket listener, so the mode exits with status 2.
#[cfg(not(target_os = "linux"))]
fn core_listener(_socket: &Path, _count: &Path) -> ExitCode {
    ExitCode::from(2)
}

/// Appends `line` to the count file, creating the file with mode 0o600 when it is missing.
#[cfg(target_os = "linux")]
fn append_count(count: &Path, line: &str) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;

    fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(count)?
        .write_all(line.as_bytes())
}

/// Returns success, or writes the error of a failed mode and returns status 1.
fn finish(result: io::Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => report_failure(&error, ExitCode::FAILURE),
    }
}

/// Writes `error` to stderr and returns `status`, or status 3 when stderr cannot be written.
fn report_failure(error: &impl Display, status: ExitCode) -> ExitCode {
    match writeln!(io::stderr().lock(), "artifree-execution-fixture: {error}") {
        Ok(()) => status,
        Err(_) => ExitCode::from(3),
    }
}
