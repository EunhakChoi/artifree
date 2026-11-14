//! `artifree-server execution`: the entry point of one attempt of an execution
//! profile (decision 0057 rule 4; decision 0056 rule 4).
//!
//! A build without the `enforcement-fixture` feature knows no profile, so every
//! mode is refused: one line on stderr and status 2, before anything else is
//! touched. With the feature on Linux the modes serve the fixture profile, and
//! the provider's launcher runs them from the tool root's copy of this binary.

#[cfg(all(target_os = "linux", feature = "enforcement-fixture"))]
use std::ffi::OsStr;
use std::ffi::OsString;
#[cfg(all(target_os = "linux", feature = "enforcement-fixture"))]
use std::fmt::Display;
use std::io::{self, Write};
#[cfg(all(target_os = "linux", feature = "enforcement-fixture"))]
use std::num::ParseIntError;
#[cfg(all(target_os = "linux", feature = "enforcement-fixture"))]
use std::path::Path;
use std::process::ExitCode;

#[cfg(all(target_os = "linux", feature = "enforcement-fixture"))]
use artifree_providers::execution::{EntryProgram, run_bootstrap, run_guardian, run_stub_for};

/// Refuses every mode, because this build knows no execution profile.
///
/// The refusal is one line on stderr and status 2. Nothing else is touched.
#[cfg(not(all(target_os = "linux", feature = "enforcement-fixture")))]
pub fn execution_command(_arguments: &[OsString]) -> ExitCode {
    // A closed stderr changes nothing: the refusal and its status stand either way.
    writeln!(
        io::stderr().lock(),
        "Execution unavailable. This build knows no execution profile, so it runs no execution entry point (decision 0057 rule 4)."
    )
    .ok();
    ExitCode::from(2)
}

/// Runs the mode that `arguments` name, for the fixture profile of the provider.
///
/// The modes are `stub <identity> <input>`, `bootstrap <identity>` and
/// `guardian <name> <deadline> <window>`. Any other argument list exits with
/// status 2.
#[cfg(all(target_os = "linux", feature = "enforcement-fixture"))]
pub fn execution_command(arguments: &[OsString]) -> ExitCode {
    match arguments {
        [mode, identity, input] if mode == "stub" => match identity.to_str() {
            Some(value) => stub(value, Path::new(input)),
            None => ExitCode::from(2),
        },
        [mode, identity] if mode == "bootstrap" => match identity.to_str() {
            Some(value) => bootstrap(value),
            None => ExitCode::from(2),
        },
        [mode, name, deadline, window] if mode == "guardian" => guardian(name, deadline, window),
        _ => ExitCode::from(2),
    }
}

/// Runs the launch stub of the server's program with the provider's staged input,
/// which the provider passes as the last argument (decision 0056 rule 3).
///
/// A failure is written to stderr and exits with status 70.
#[cfg(all(target_os = "linux", feature = "enforcement-fixture"))]
fn stub(identity: &str, input: &Path) -> ExitCode {
    match run_stub_for(EntryProgram::Server, identity, input) {
        Ok(never) => match never {},
        Err(failure) => report(&failure, ExitCode::from(70)),
    }
}

/// Runs the bootstrap of the fixture profile (decision 0056 rule 3).
///
/// A failure is written to stderr and exits with status 70.
#[cfg(all(target_os = "linux", feature = "enforcement-fixture"))]
fn bootstrap(identity: &str) -> ExitCode {
    match run_bootstrap(identity) {
        Ok(never) => match never {},
        Err(failure) => report(&failure, ExitCode::from(70)),
    }
}

/// Runs the guardian of the attempt group `name`, with its deadline and renewal
/// window in milliseconds (decision 0056, amendment of 2026-11-13).
///
/// A name that is not UTF-8, or a number that does not parse as `u64`, exits with
/// status 2. A failure is written to stderr and exits with status 70.
#[cfg(all(target_os = "linux", feature = "enforcement-fixture"))]
fn guardian(name: &OsStr, deadline: &OsStr, window: &OsStr) -> ExitCode {
    let (Some(name), Some(deadline), Some(window)) =
        (name.to_str(), whole_number(deadline), whole_number(window))
    else {
        return ExitCode::from(2);
    };
    match run_guardian(name, deadline, window) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => report(&failure, ExitCode::from(70)),
    }
}

/// The text as a `u64`, or `None` when it is not UTF-8 or is not a whole number that fits.
#[cfg(all(target_os = "linux", feature = "enforcement-fixture"))]
fn whole_number(text: &OsStr) -> Option<u64> {
    let digits = text.to_str()?;
    let number: Result<u64, ParseIntError> = digits.parse();
    number.ok()
}

/// Writes `error` to stderr and returns `status`, or status 3 when stderr cannot be written.
#[cfg(all(target_os = "linux", feature = "enforcement-fixture"))]
fn report(error: &impl Display, status: ExitCode) -> ExitCode {
    match writeln!(io::stderr().lock(), "artifree-server execution: {error}") {
        Ok(()) => status,
        Err(_) => ExitCode::from(3),
    }
}
