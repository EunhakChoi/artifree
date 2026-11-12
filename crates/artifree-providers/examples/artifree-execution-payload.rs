//! Test-only payload fixture of R02ab (decision 0056 rules 4 and 14): it prints one line and exits.
//!
//! Built by the tests only and shipped in no release archive. It uses only what the fixture profile's seccomp allowlist permits: no file, no thread, no environment read.

use std::env;
use std::io::{self, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    if env::args_os().nth(1).is_some() {
        return ExitCode::from(2);
    }
    match print_line() {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

/// Writes the payload's single line to stdout and flushes it.
fn print_line() -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(b"artifree-execution-payload: ran\n")?;
    stdout.flush()
}
