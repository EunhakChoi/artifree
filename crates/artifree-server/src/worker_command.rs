//! `artifree-server worker run`: argument parsing, the signal handler, and the
//! JSON lines. The loop is in `artifree_server::worker`.

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use artifree_core::jobs::SystemClock;
use artifree_ports::execution::Execution;
use artifree_server::worker::{self, Stopped, WorkerEvent, WorkerOptions};

const WORKER_HELP: &str = "Usage: artifree-server worker run --data-dir <dir> [options]

Runs the persistent jobs (decision 0030) of the server whose data directory is
<dir>: it claims queued jobs one at a time, re-reads every file of the pinned
revision, compares its SHA-256 digest and size with the stored listing, and
records the result. The worker is trusted in-repository code: it has no network
access of its own and is not a sandbox. It runs no untrusted code unless it is
given --execution-dir (decision 0057). It opens the existing database directly
(the server need not run) and never creates one. Run it as the user that runs
the server.

Options:
  --data-dir <dir>     Data directory of the server (required)
  --once               Stop when no job is due instead of waiting for more
  --supervised         For a parent process: the end of standard input is a
                       stop request, handled like SIGTERM; not with --once
  --poll-ms <n>        Milliseconds between looks at an empty queue
                       (10-60000, default 1000)
  --lease-secs <n>     Seconds a lease lasts without a renewal; it is renewed
                       every third of it (1-3600, default 30)
  --hold-file <path>   Testing only: after each claim, wait until this file
                       exists before reading the job's files
  --execution-dir <dir>  The private directory of the admission lock, the
                       attempt registry and the staged input (decision 0057
                       rule 18). A build that knows no execution profile
                       refuses it with exit status 2.

On SIGINT or SIGTERM (Ctrl-C on Windows) the worker gives the job it holds back
to the queue at its next checkpoint, without using an attempt, and exits 0. With
--supervised the end of standard input (a closed or empty input, or the end of a
terminal's input) stops it the same way, and the stopped reason is \"signal\". A
worker that is killed leaves its job running until the lease expires; another
worker then runs it again, and a result is written at most once.

stdout is JSON lines: {\"event\":\"ready\",\"worker\":...}, one
{\"event\":\"claimed\",\"job\":...} after each claim commits, one
{\"event\":\"job\",\"job\":...,\"state\":...,\"attempts\":...} per job ended or
given back, {\"event\":\"lease_lost\",\"job\":...} when a lease was lost, and
{\"event\":\"stopped\",\"reason\":\"drained\"|\"signal\"} last. Diagnostics go to
stderr. Exit status 1: the database is missing, refused, or failed repeatedly.
Exit status 2: --execution-dir was refused (no execution profile, or not the
only process of a delegated group).
See docs/decisions/0030-persistent-jobs-and-worker.md.";

/// What a parsed command line asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Parsed {
    /// Print the help text.
    Help,
    /// Run with these options.
    Run {
        /// The worker's options.
        options: WorkerOptions,
        /// `--supervised`: the end of standard input is a stop request
        /// (decision 0055, rule 2).
        supervised: bool,
    },
}

const POLL_MS: std::ops::RangeInclusive<u64> = 10..=60_000;
const LEASE_SECS: std::ops::RangeInclusive<u64> = 1..=3_600;

/// Parses the arguments after `worker`: `run` and its flags, or a lone
/// `--help`. A value is never repeated in a message, because it is operator
/// input.
///
/// # Errors
///
/// A message starting with `Unknown arguments.`
pub fn parse(arguments: &[OsString]) -> Result<Parsed, String> {
    if let [only] = arguments
        && (only == "--help" || only == "-h")
    {
        return Ok(Parsed::Help);
    }
    let Some((subcommand, rest)) = arguments.split_first() else {
        return Err("Unknown arguments. worker needs a subcommand (run); use --help.".into());
    };
    if subcommand != "run" {
        return Err("Unknown arguments. The only worker subcommand is run; use --help.".into());
    }
    if let [only] = rest
        && (only == "--help" || only == "-h")
    {
        return Ok(Parsed::Help);
    }
    let mut data_dir: Option<PathBuf> = None;
    let mut hold_file: Option<PathBuf> = None;
    let mut execution_dir: Option<PathBuf> = None;
    let mut once = false;
    let mut supervised = false;
    let mut poll_ms: Option<u64> = None;
    let mut lease_secs: Option<u64> = None;
    let mut remaining = rest.iter();
    while let Some(argument) = remaining.next() {
        if argument == "--once" {
            if std::mem::replace(&mut once, true) {
                return Err(twice("--once"));
            }
            continue;
        }
        if argument == "--supervised" {
            if std::mem::replace(&mut supervised, true) {
                return Err(twice("--supervised"));
            }
            continue;
        }
        let flag = [
            "--data-dir",
            "--hold-file",
            "--execution-dir",
            "--poll-ms",
            "--lease-secs",
        ]
        .into_iter()
        .find(|flag| argument == flag)
        .ok_or_else(|| "Unknown arguments. An option is not recognized; use --help.".to_owned())?;
        let value = remaining
            .next()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                format!("Unknown arguments. {flag} requires a non-empty value; use --help.")
            })?;
        let taken = match flag {
            "--data-dir" => data_dir.replace(PathBuf::from(value)).is_some(),
            "--hold-file" => hold_file.replace(PathBuf::from(value)).is_some(),
            "--execution-dir" => execution_dir.replace(PathBuf::from(value)).is_some(),
            "--poll-ms" => poll_ms.replace(number(flag, value, &POLL_MS)?).is_some(),
            _ => lease_secs
                .replace(number(flag, value, &LEASE_SECS)?)
                .is_some(),
        };
        if taken {
            return Err(twice(flag));
        }
    }
    let data_dir = data_dir
        .ok_or_else(|| "Unknown arguments. --data-dir is required; use --help.".to_owned())?;
    if supervised && once {
        return Err(
            "Unknown arguments. --supervised cannot be used with --once; use --help.".into(),
        );
    }
    Ok(Parsed::Run {
        options: WorkerOptions {
            data_dir,
            once,
            poll: Duration::from_millis(poll_ms.unwrap_or(1_000)),
            lease: Duration::from_secs(lease_secs.unwrap_or(30)),
            hold_file,
            execution_dir,
        },
        supervised,
    })
}

fn twice(flag: &str) -> String {
    format!("Unknown arguments. {flag} was given twice; use --help.")
}

/// A canonical decimal number inside `range`: digits only, no sign, no leading
/// zero.
fn number(
    flag: &str,
    value: &OsString,
    range: &std::ops::RangeInclusive<u64>,
) -> Result<u64, String> {
    let parsed = value
        .to_str()
        .filter(|text| {
            !text.is_empty()
                && text.bytes().all(|byte| byte.is_ascii_digit())
                && (text.len() == 1 || !text.starts_with('0'))
        })
        .and_then(|text| text.parse::<u64>().ok())
        .filter(|number| range.contains(number));
    parsed.ok_or_else(|| {
        format!(
            "Unknown arguments. {flag} needs a whole number from {} to {}; use --help.",
            range.start(),
            range.end()
        )
    })
}

/// Why `worker` failed: `Fatal` exits with status 1, `Unsupported` with status 2
/// (decision 0057 rule 18).
#[derive(Debug)]
pub enum WorkerFailure {
    /// A bad command line, a database that cannot be used, or a store that
    /// failed repeatedly.
    Fatal(String),
    /// A request this build cannot serve: `--execution-dir` without an execution
    /// profile, or outside a delegated group.
    Unsupported(String),
}

impl From<String> for WorkerFailure {
    fn from(message: String) -> Self {
        Self::Fatal(message)
    }
}

/// Runs `worker` and returns its exit status; the message of a failure goes to
/// stderr.
pub fn worker_exit(arguments: &[OsString]) -> ExitCode {
    match run_worker(arguments) {
        Ok(()) => ExitCode::SUCCESS,
        Err(WorkerFailure::Fatal(message)) => {
            stderr_line(&message);
            ExitCode::FAILURE
        }
        Err(WorkerFailure::Unsupported(message)) => {
            stderr_line(&message);
            ExitCode::from(2)
        }
    }
}

/// Runs `worker`. Prints help, or JSON lines until the worker stops.
fn run_worker(arguments: &[OsString]) -> Result<(), WorkerFailure> {
    let (options, supervised) = match parse(arguments)? {
        Parsed::Help => {
            print_line(WORKER_HELP)?;
            return Ok(());
        }
        Parsed::Run {
            options,
            supervised,
        } => (options, supervised),
    };
    // Refused before any line is printed and before the database is opened.
    let execution = open_execution(options.execution_dir.as_deref())?;
    let shutdown = Arc::new(AtomicBool::new(false));
    // The runtime exists only to wait for signals; the loop is synchronous.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .map_err(|error| format!("cannot start the signal handler: {error}"))?;
    // Installed before the worker says it is ready, so a signal sent after
    // that line is always handled and never takes the default action.
    install_signal_handlers(&runtime, &shutdown)?;
    if supervised {
        // Armed before the worker says it is ready, like the signal handlers.
        watch_stdin(&shutdown)?;
    }
    let stopped = worker::run(
        &options,
        execution.as_deref(),
        &SystemClock,
        &shutdown,
        &mut |event| {
            // A closed stdout must not stop a worker that holds a lease.
            if let Some(line) = event_line(event) {
                drop(print_line(&line.to_string()));
            }
            match event {
                WorkerEvent::Warning(error) => stderr_line(&format!("warning: {error}")),
                WorkerEvent::Attempt { job, detail } => {
                    stderr_line(&format!("attempt of job {}: {detail}", job.as_str()));
                }
                WorkerEvent::Unavailable(detail) => {
                    stderr_line(&format!("execution unavailable: {detail}"));
                }
                _ => {}
            }
        },
    )?;
    let reason = match stopped {
        Stopped::Drained => "drained",
        Stopped::Signal => "signal",
    };
    drop(print_line(
        &serde_json::json!({"event": "stopped", "reason": reason}).to_string(),
    ));
    runtime.shutdown_background();
    Ok(())
}

/// Opens the execution port that `--execution-dir` asks for, or returns `None`
/// when the option is absent. A worker refuses to start with the option outside a
/// delegated group, naming the missing facility (decision 0057 rule 18).
///
/// # Errors
///
/// [`WorkerFailure::Unsupported`] when the directory or the group is refused.
#[cfg(all(target_os = "linux", feature = "enforcement-fixture"))]
fn open_execution(dir: Option<&Path>) -> Result<Option<Box<dyn Execution>>, WorkerFailure> {
    let Some(dir) = dir else { return Ok(None) };
    match artifree_providers::execution::FixtureExecution::open(dir) {
        Ok(execution) => Ok(Some(Box::new(execution))),
        Err(refusal) => Err(WorkerFailure::Unsupported(format!(
            "Execution unavailable. {refusal}; --execution-dir needs the only process \
             of a delegated group and a private execution directory (decision 0057 \
             rule 18)."
        ))),
    }
}

/// Opens the execution port that `--execution-dir` asks for, or returns `None`
/// when the option is absent. This build knows no execution profile, so it
/// refuses the option before the database is opened (decision 0057 rule 18).
///
/// # Errors
///
/// [`WorkerFailure::Unsupported`] when `--execution-dir` was given.
#[cfg(not(all(target_os = "linux", feature = "enforcement-fixture")))]
fn open_execution(dir: Option<&Path>) -> Result<Option<Box<dyn Execution>>, WorkerFailure> {
    match dir {
        Some(_) => Err(WorkerFailure::Unsupported(
            "Execution unavailable. This build knows no execution profile, so \
             --execution-dir is refused (decision 0057 rule 18)."
                .to_owned(),
        )),
        None => Ok(None),
    }
}

/// Sets `flag` when the process is interrupted (Ctrl-C) or, on Unix, asked to
/// terminate (SIGTERM). Both handlers exist when this returns.
#[cfg(unix)]
fn install_signal_handlers(
    runtime: &tokio::runtime::Runtime,
    flag: &Arc<AtomicBool>,
) -> Result<(), String> {
    use tokio::signal::unix::{SignalKind, signal};
    let _inside = runtime.enter();
    let register =
        |kind| signal(kind).map_err(|error| format!("cannot install a signal handler: {error}"));
    let mut interrupt = register(SignalKind::interrupt())?;
    let mut terminate = register(SignalKind::terminate())?;
    let flag = Arc::clone(flag);
    runtime.spawn(async move {
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = terminate.recv() => {}
        }
        flag.store(true, Ordering::Release);
    });
    Ok(())
}

/// Sets `flag` when the process is interrupted (Ctrl-C). Windows has no
/// SIGTERM; a process that is terminated outright holds its lease until it
/// expires.
#[cfg(windows)]
fn install_signal_handlers(
    runtime: &tokio::runtime::Runtime,
    flag: &Arc<AtomicBool>,
) -> Result<(), String> {
    let _inside = runtime.enter();
    let mut interrupt = tokio::signal::windows::ctrl_c()
        .map_err(|error| format!("cannot install a signal handler: {error}"))?;
    let flag = Arc::clone(flag);
    runtime.spawn(async move {
        interrupt.recv().await;
        flag.store(true, Ordering::Release);
    });
    Ok(())
}

/// Sets `flag` at the end of standard input or on a read error, which is the
/// stop request of a supervised worker (decision 0055, rule 2). The stop is
/// handled as a signal is; the bytes read are discarded.
fn watch_stdin(flag: &Arc<AtomicBool>) -> Result<(), String> {
    use std::io::Read;
    let flag = Arc::clone(flag);
    std::thread::Builder::new()
        .name("stdin-stop".into())
        .spawn(move || {
            let mut input = io::stdin().lock();
            let mut sink = [0_u8; 256];
            // Reads until the end or an error; an interrupted read is retried.
            loop {
                match input.read(&mut sink) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            flag.store(true, Ordering::Release);
        })
        .map(drop)
        .map_err(|error| format!("cannot watch standard input: {error}"))
}

/// The stdout line of an event, or `None` for an event that prints none. An
/// attempt's end is diagnostics only, so it has no stdout line (decision 0030
/// rule 13).
fn event_line(event: &WorkerEvent<'_>) -> Option<serde_json::Value> {
    match event {
        WorkerEvent::Ready { worker } => {
            Some(serde_json::json!({"event": "ready", "worker": worker.as_str()}))
        }
        WorkerEvent::Claimed(job) => {
            Some(serde_json::json!({"event": "claimed", "job": job.as_str()}))
        }
        WorkerEvent::Job(record) | WorkerEvent::Released(record) => Some(serde_json::json!({
            "event": "job",
            "job": record.id.as_str(),
            "state": record.state.as_str(),
            "attempts": record.attempts,
            "failure": record.failure.map(artifree_domain::job::JobFailure::as_str),
            "verified": record.report.as_ref().map(artifree_domain::job::VerifyReport::is_ok),
        })),
        WorkerEvent::LeaseLost(job) => {
            Some(serde_json::json!({"event": "lease_lost", "job": job.as_str()}))
        }
        WorkerEvent::Warning(_) => Some(serde_json::json!({"event": "warning"})),
        WorkerEvent::Attempt { .. } | WorkerEvent::Unavailable(_) => None,
    }
}

fn print_line(text: &str) -> Result<(), String> {
    let mut out = io::stdout().lock();
    match writeln!(out, "{text}").and_then(|()| out.flush()) {
        Err(error) if error.kind() != io::ErrorKind::BrokenPipe => {
            Err(format!("cannot write to stdout: {error}"))
        }
        _ => Ok(()),
    }
}

/// One line on stderr. A closed stderr is not a reason to panic or to stop: the
/// worker may hold a lease or an unsettled attempt (decision 0057 rule 10).
fn stderr_line(message: &str) {
    writeln!(io::stderr().lock(), "{message}").ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    fn run_options(list: &[&str]) -> Result<WorkerOptions, String> {
        match parse(&args(list))? {
            Parsed::Run { options, .. } => Ok(options),
            Parsed::Help => Err("help".into()),
        }
    }

    #[test]
    fn defaults_and_every_flag_are_read() -> Result<(), String> {
        let plain = run_options(&["run", "--data-dir", "d"])?;
        assert_eq!(plain.poll, Duration::from_secs(1));
        assert_eq!(plain.lease, Duration::from_secs(30));
        assert!(!plain.once && plain.hold_file.is_none());
        let full = run_options(&[
            "run",
            "--once",
            "--poll-ms",
            "10",
            "--lease-secs",
            "3600",
            "--hold-file",
            "h",
            "--data-dir",
            "d",
        ])?;
        assert!(full.once);
        assert_eq!(full.poll, Duration::from_millis(10));
        assert_eq!(full.lease, Duration::from_secs(3_600));
        assert_eq!(full.hold_file, Some(PathBuf::from("h")));
        Ok(())
    }

    #[test]
    fn help_is_a_lone_flag_before_or_after_run() {
        assert_eq!(parse(&args(&["--help"])), Ok(Parsed::Help));
        assert_eq!(parse(&args(&["run", "-h"])), Ok(Parsed::Help));
        assert!(parse(&args(&["run", "--help", "--once"])).is_err());
    }

    #[test]
    fn help_names_each_stdout_event_of_a_job() {
        // Decision 0055, rule 10: a parent reads these stdout lines, and the
        // help is their reference, so it names each of them.
        for event in ["ready", "claimed", "job", "lease_lost", "stopped"] {
            let needle = format!("{{\"event\":\"{event}\"");
            assert!(
                WORKER_HELP.contains(needle.as_str()),
                "the help does not name the {event} line: {needle}"
            );
        }
    }

    #[test]
    fn malformed_command_lines_are_refused_without_echoing_values() {
        for bad in [
            &[][..],
            &["start", "--data-dir", "d"],
            &["run"],
            &["run", "--data-dir"],
            &["run", "--data-dir", ""],
            &["run", "--data-dir", "a", "--data-dir", "b"],
            &["run", "--data-dir", "d", "--once", "--once"],
            &["run", "--data-dir", "d", "--force"],
            &["run", "--data-dir", "d", "--poll-ms", "9"],
            &["run", "--data-dir", "d", "--poll-ms", "60001"],
            &["run", "--data-dir", "d", "--poll-ms", "0100"],
            &["run", "--data-dir", "d", "--poll-ms", "+100"],
            &["run", "--data-dir", "d", "--poll-ms", "1e3"],
            &["run", "--data-dir", "d", "--lease-secs", "0"],
            &["run", "--data-dir", "d", "--lease-secs", "3601"],
            &["run", "--data-dir", "d", "--lease-secs", "-1"],
            &["run", "--data-dir", "d", "--lease-secs", "secret-value"],
        ] {
            let Err(error) = parse(&args(bad)) else {
                panic!("accepted {bad:?}");
            };
            assert!(error.starts_with("Unknown arguments."), "{bad:?}: {error}");
            assert!(!error.contains("secret-value"), "{error}");
        }
    }
}
