//! Test-only fixture for the Linux enforcement job (decision 0056 rule 4).
//!
//! Built by the tests only and shipped in no release archive. Its arguments
//! select the mode:
//!
//! - `namespaces` prints one `name=target` line per namespace of this process
//!   to stdout.
//! - `wait` reads stdin to end of file and discards what it read.
//! - `false-ready` writes the READY record on descriptor 0 without limits, a
//!   privilege drop or a filter, then reads descriptor 0 to end of file. It is
//!   a negative control for the provider's readiness check and execs nothing.
//! - `stub <identity>` runs the launch stub and `bootstrap <identity>` runs the
//!   bootstrap (decision 0056 rule 3). On success each one replaces this process
//!   with the next program, so they return only on failure.
//! - `write-probes <directory>` writes the generated probe programs of decision
//!   0056 (amendment of 2026-11-13) into the directory, one file per probe with
//!   mode 0o755, and exits 0. The job's `prepare.sh` runs it while it assembles
//!   the tool root.
//!
//! `false-ready`, `stub`, `bootstrap` and `write-probes` exist on Linux only.
//! Elsewhere, and for any other argument list (including an identity that is
//! not UTF-8), the exit status is 2. A failing `false-ready` writes its error
//! to stderr and exits with status 1. A failing `stub`, `bootstrap` or
//! `write-probes` writes its failure to stderr and exits with status 70. Status
//! 3 means that a failing mode could not write its error to stderr at all.

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

/// Namespaces that `namespaces` reports, in output order.
const NAMESPACES: [&str; 7] = ["user", "pid", "net", "mnt", "ipc", "uts", "cgroup"];

fn main() -> ExitCode {
    let arguments: Vec<OsString> = env::args_os().skip(1).collect();
    match arguments.as_slice() {
        [mode] => run_mode(mode),
        [mode, directory] if mode == "write-probes" => write_probes(Path::new(directory)),
        [mode, identity] => match (mode.to_str(), identity.to_str()) {
            (Some(name), Some(value)) => run_identity_mode(name, value),
            _ => ExitCode::from(2),
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

/// Runs the stub or the bootstrap with its identity.
#[cfg(target_os = "linux")]
fn run_identity_mode(mode: &str, identity: &str) -> ExitCode {
    match mode {
        "stub" => run_child(run_stub, identity),
        "bootstrap" => run_child(run_bootstrap, identity),
        _ => ExitCode::from(2),
    }
}

/// Other systems have no launch stub or bootstrap, so both modes exit with status 2.
#[cfg(not(target_os = "linux"))]
fn run_identity_mode(_mode: &str, _identity: &str) -> ExitCode {
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

/// Runs a launch stub or bootstrap entry point, which returns only on failure.
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
