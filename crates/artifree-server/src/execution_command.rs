//! `artifree-server execution`: the entry points of one attempt of an execution
//! profile (decision 0057 rule 4; decision 0056 rule 4) and of admission's
//! namespace check and probe child (decision 0058, note "R03fa").
//!
//! A build that has the launch, on Linux with the `enforcement-fixture` feature
//! or on `x86_64` Linux (decision 0058 rule 5, R03fa), serves the profiles the
//! build knows, and the provider's launcher and admission run its modes from the
//! tool root's copy of this binary. Any other build knows no profile, so every
//! mode is refused: one line on stderr and status 2, before anything else is
//! touched.

#[cfg(all(
    target_os = "linux",
    any(feature = "enforcement-fixture", target_arch = "x86_64")
))]
use std::ffi::OsStr;
use std::ffi::OsString;
#[cfg(all(
    target_os = "linux",
    any(feature = "enforcement-fixture", target_arch = "x86_64")
))]
use std::fmt::Display;
use std::io::{self, Write};
#[cfg(all(
    target_os = "linux",
    any(feature = "enforcement-fixture", target_arch = "x86_64")
))]
use std::num::ParseIntError;
#[cfg(all(
    target_os = "linux",
    any(feature = "enforcement-fixture", target_arch = "x86_64")
))]
use std::path::Path;
use std::process::ExitCode;

#[cfg(all(
    target_os = "linux",
    any(feature = "enforcement-fixture", target_arch = "x86_64")
))]
use artifree_providers::execution::{
    EntryProgram, print_namespaces, run_bootstrap, run_guardian, run_stub_for,
    wait_for_end_of_input,
};

/// Refuses every mode, because this build knows no execution profile.
///
/// The refusal is one line on stderr and status 2. Nothing else is touched.
#[cfg(not(all(
    target_os = "linux",
    any(feature = "enforcement-fixture", target_arch = "x86_64")
)))]
pub fn execution_command(_arguments: &[OsString]) -> ExitCode {
    // A closed stderr changes nothing: the refusal and its status stand either way.
    writeln!(
        io::stderr().lock(),
        "Execution unavailable. This build knows no execution profile, so it runs no execution entry point (decision 0057 rule 4)."
    )
    .ok();
    ExitCode::from(2)
}

/// Runs the mode that `arguments` name, for the profiles the provider knows.
///
/// The attempt's modes are `stub <identity> <input>`, `bootstrap <identity>` and
/// `guardian <name> <deadline> <window>`; an identity the build does not know
/// fails with status 70. Admission's modes are `namespaces` and `wait` (decision
/// 0058, note "R03f split"). Any other argument list exits with status 2.
#[cfg(all(
    target_os = "linux",
    any(feature = "enforcement-fixture", target_arch = "x86_64")
))]
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
        [mode] if mode == "namespaces" => finish(print_namespaces()),
        [mode] if mode == "wait" => finish(wait_for_end_of_input()),
        _ => ExitCode::from(2),
    }
}

/// Runs the launch stub of the server's program with the provider's staged input,
/// which the provider passes as the last argument (decision 0056 rule 3).
///
/// A failure is written to stderr and exits with status 70.
#[cfg(all(
    target_os = "linux",
    any(feature = "enforcement-fixture", target_arch = "x86_64")
))]
fn stub(identity: &str, input: &Path) -> ExitCode {
    match run_stub_for(EntryProgram::Server, identity, input) {
        Ok(never) => match never {},
        Err(failure) => report(&failure, ExitCode::from(70)),
    }
}

/// Runs the bootstrap of the profile whose identity is `identity` (decision 0056 rule 3).
///
/// A failure is written to stderr and exits with status 70.
#[cfg(all(
    target_os = "linux",
    any(feature = "enforcement-fixture", target_arch = "x86_64")
))]
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
#[cfg(all(
    target_os = "linux",
    any(feature = "enforcement-fixture", target_arch = "x86_64")
))]
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

/// Exits with status 0 after an admission mode, or reports its failure with status 70.
#[cfg(all(
    target_os = "linux",
    any(feature = "enforcement-fixture", target_arch = "x86_64")
))]
fn finish(result: io::Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => report(&error, ExitCode::from(70)),
    }
}

/// The text as a `u64`, or `None` when it is not UTF-8 or is not a whole number that fits.
#[cfg(all(
    target_os = "linux",
    any(feature = "enforcement-fixture", target_arch = "x86_64")
))]
fn whole_number(text: &OsStr) -> Option<u64> {
    let digits = text.to_str()?;
    let number: Result<u64, ParseIntError> = digits.parse();
    number.ok()
}

/// Writes `error` to stderr and returns `status`, or status 3 when stderr cannot be written.
#[cfg(all(
    target_os = "linux",
    any(feature = "enforcement-fixture", target_arch = "x86_64")
))]
fn report(error: &impl Display, status: ExitCode) -> ExitCode {
    match writeln!(io::stderr().lock(), "artifree-server execution: {error}") {
        Ok(()) => status,
        Err(_) => ExitCode::from(3),
    }
}
