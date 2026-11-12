//! Test-only fixture for the Linux enforcement job (decision 0056 rule 4).
//!
//! Built by the tests only and shipped in no release archive. Its single
//! argument selects the mode: `namespaces` prints one `name=target` line per
//! namespace of this process to stdout, and `wait` reads stdin to end of file.
//! Any other argument, none, or more than one exits with status 2. A mode that
//! fails writes its error to stderr and exits with status 1, or with status 3
//! when stderr itself cannot be written.

use std::ffi::OsStr;
use std::fs;
use std::io::{self, Write};
use std::process::ExitCode;

/// Namespaces that `namespaces` reports, in output order.
const NAMESPACES: [&str; 7] = ["user", "pid", "net", "mnt", "ipc", "uts", "cgroup"];

fn main() -> ExitCode {
    let mut arguments = std::env::args_os().skip(1);
    let mode = arguments.next();
    if arguments.next().is_some() {
        return ExitCode::from(2);
    }
    let result = match mode.as_deref().and_then(OsStr::to_str) {
        Some("namespaces") => print_namespaces(),
        Some("wait") => wait_for_end_of_input(),
        _ => return ExitCode::from(2),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => report_failure(&error),
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

/// Writes `error` to stderr and returns the exit status for a failed mode.
fn report_failure(error: &io::Error) -> ExitCode {
    match writeln!(io::stderr().lock(), "artifree-execution-fixture: {error}") {
        Ok(()) => ExitCode::FAILURE,
        Err(_) => ExitCode::from(3),
    }
}
