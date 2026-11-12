//! Test-only payload of R02ab and R02ac (decision 0056 rules 4, 12 and 14).
//!
//! Built by the tests only and shipped in no release archive. The Linux
//! enforcement job runs it inside the attempt's sandbox; no local run is
//! allowed (rule 12). Its scenario is the one word in `/tools/etc/scenario`,
//! which the operator's `scripts/enforcement/prepare.sh` writes. A missing
//! file or the word `exit` selects the R02ab behaviour: print one line and exit
//! with status 0.
//!
//! On Linux the scenarios are `threads`, `processes`, `memory`,
//! `scratch-bytes`, `scratch-inodes`, `flood`, `hang`, `cpu`, `survivors` and
//! `survivors-hang`. Each prints fixed lines to stdout, flushed, that the job
//! tests parse. A scenario that cannot do its work prints the reason to stderr
//! and exits with status 3. An unknown scenario word exits with status 2.
//!
//! The scenarios start this binary again with one child role as its only
//! argument: `hold`, `middle`, `detached` or `ignore`. Any other argument list
//! exits with status 2. Other systems keep the R02ab behaviour only, because
//! the Linux-only dependencies are not built there.

use std::env;
use std::io::{self, Write};
use std::process::ExitCode;

/// Writes one line to stdout and flushes it, so that the job tests see it before the payload blocks or exits.
fn say(line: &str) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{line}")?;
    stdout.flush()
}

/// Prints the R02ab line. The scenario `exit` and a missing scenario file both select it.
fn print_ran() -> io::Result<()> {
    say("artifree-execution-payload: ran")
}

/// Runs the payload on a system without the Linux scenarios: no argument prints the R02ab line, any argument exits with status 2.
#[cfg(not(target_os = "linux"))]
fn main() -> ExitCode {
    if env::args_os().nth(1).is_some() {
        return ExitCode::from(2);
    }
    match print_ran() {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

/// Runs the payload on Linux: no argument selects the scenario, one argument selects a child role.
#[cfg(target_os = "linux")]
fn main() -> ExitCode {
    let arguments: Vec<std::ffi::OsString> = env::args_os().skip(1).collect();
    linux::run(&arguments)
}

/// The Linux scenarios and child roles. Their names, paths and limits are the contract the job tests parse.
#[cfg(target_os = "linux")]
mod linux {
    use rustix::process;
    use std::ffi::{OsStr, OsString};
    use std::fmt::Display;
    use std::fs::{self, File, OpenOptions};
    use std::io::{self, Write};
    use std::path::Path;
    use std::process::{Command, ExitCode};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::{self, Builder};
    use std::time::{Duration, Instant};

    use super::{print_ran, say};

    /// The file that names the scenario. The operator's `prepare.sh` writes it.
    const SCENARIO_FILE: &str = "/tools/etc/scenario";
    /// The payload binary as the scenarios start it, so every child is this same program.
    const SELF: &str = "/tools/bin/artifree-execution-payload";
    const MIB: usize = 1024 * 1024;
    /// Thread and process creation stop at the first refusal or after this many successes.
    const SPAWN_LIMIT: usize = 500;
    const THREAD_STACK_BYTES: usize = 64 * 1024;
    const MEMORY_STEP_MIB: usize = 16;
    const MEMORY_REPORT_MIB: usize = 128;
    const MEMORY_LIMIT_MIB: usize = 4096;
    const FILL_FILE: &str = "/tmp/fill";
    const FILL_CHUNK: usize = MIB;
    const FILL_LIMIT: usize = 512 * MIB;
    const OUTSIDE_PATHS: [&str; 5] = ["/x", "/dev/x", "/dev/shm/x", "/tools/x", "/lib/x"];
    const INODE_LIMIT: usize = 20_000;
    const FLOOD_CHUNK: usize = 64 * 1024;
    const SPINNERS: usize = 4;
    const SPIN_TIME: Duration = Duration::from_secs(3);
    const SLEEP_STEP: Duration = Duration::from_secs(1);
    const POLL: Duration = Duration::from_millis(20);
    const WAIT_BUDGET: Duration = Duration::from_secs(2);
    const SETTLE: Duration = Duration::from_millis(100);
    const BACKGROUND_PERIOD: Duration = Duration::from_millis(100);
    const WRITER_FILE: &str = "/tmp/w";
    const IGNORE_READY: &str = "/tmp/ignore-ready";
    const IGNORE_GOT: &str = "/tmp/ignore-got";

    /// Runs the payload for its arguments. None selects the scenario, one selects a child role, and any other count exits with status 2.
    pub(super) fn run(arguments: &[OsString]) -> ExitCode {
        match arguments {
            [] => run_scenario(),
            [role] => run_role(role),
            _ => ExitCode::from(2),
        }
    }

    /// Reads the scenario word from its file and runs that scenario.
    fn run_scenario() -> ExitCode {
        match read_scenario_word() {
            Ok(word) => dispatch(&word),
            Err(error) => report(&error, 3),
        }
    }

    /// Returns the scenario word without one trailing newline. A missing file gives `exit`.
    fn read_scenario_word() -> io::Result<String> {
        match fs::read_to_string(SCENARIO_FILE) {
            Ok(text) => Ok(String::from(text.strip_suffix('\n').unwrap_or(&text))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::from("exit")),
            Err(error) => Err(error),
        }
    }

    /// Runs the scenario that `word` names.
    fn dispatch(word: &str) -> ExitCode {
        match word {
            "exit" => finish(print_ran()),
            "threads" => finish(threads()),
            "processes" => finish(processes()),
            "memory" => finish(memory()),
            "scratch-bytes" => finish(scratch_bytes()),
            "scratch-inodes" => finish(scratch_inodes()),
            "flood" => flood(),
            "hang" => sleep_forever(),
            "cpu" => finish(cpu()),
            "survivors" => finish(survivors()),
            "survivors-hang" => finish(survivors_hang()),
            other => report(&format!("unknown scenario {other}"), 2),
        }
    }

    /// Runs the child role that the scenarios start. An unknown role exits with status 2.
    fn run_role(role: &OsStr) -> ExitCode {
        match role.to_str() {
            Some("hold") => sleep_forever(),
            Some("middle") => finish(middle()),
            Some("detached") => finish(detached()),
            Some("ignore") => finish(ignore()),
            _ => ExitCode::from(2),
        }
    }

    /// Returns success, or reports the error and returns status 3.
    fn finish(result: io::Result<()>) -> ExitCode {
        match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => report(&error, 3),
        }
    }

    /// Writes `message` to stderr with the payload's name and returns `status`.
    fn report(message: &impl Display, status: u8) -> ExitCode {
        // The status already says what happened, so a stderr that cannot be written does not change it.
        drop(writeln!(
            io::stderr().lock(),
            "artifree-execution-payload: {message}"
        ));
        ExitCode::from(status)
    }

    /// Sleeps for ever. The group kill or the wall deadline ends the process.
    fn sleep_forever() -> ! {
        loop {
            thread::sleep(SLEEP_STEP);
        }
    }

    /// The body of each thread of the `threads` scenario.
    fn sleep_in_thread() {
        sleep_forever()
    }

    /// Calls `attempt` until it fails or has succeeded `limit` times. Returns the successes and the kind of the failure, if there was one.
    fn until_refused<T>(
        limit: usize,
        mut attempt: impl FnMut() -> io::Result<T>,
    ) -> (usize, Option<io::ErrorKind>) {
        let mut made = 0_usize;
        while made < limit {
            match attempt() {
                Ok(_) => made += 1,
                Err(error) => return (made, Some(error.kind())),
            }
        }
        (made, None)
    }

    /// Names the error kind that stopped a loop, or `none` when the loop reached its own limit.
    fn refusal_text(refusal: Option<io::ErrorKind>) -> String {
        match refusal {
            Some(kind) => format!("{kind:?}"),
            None => String::from("none"),
        }
    }

    /// Starts threads with small stacks that sleep until one is refused.
    fn threads() -> io::Result<()> {
        let (made, refusal) = until_refused(SPAWN_LIMIT, || {
            Builder::new()
                .stack_size(THREAD_STACK_BYTES)
                .spawn(sleep_in_thread)
        });
        say(&format!(
            "threads made={made} refused={}",
            refusal_text(refusal)
        ))
    }

    /// Starts `hold` children, which stay alive, until one is refused.
    fn processes() -> io::Result<()> {
        let (made, refusal) = until_refused(SPAWN_LIMIT, || Command::new(SELF).arg("hold").spawn());
        say(&format!(
            "processes made={made} refused={}",
            refusal_text(refusal)
        ))
    }

    /// Holds memory in 16 MiB blocks, reports every 128 MiB, and fails at 4096 MiB.
    fn memory() -> io::Result<()> {
        let mut blocks: Vec<Vec<u8>> = Vec::new();
        let mut held_mib = 0_usize;
        loop {
            blocks.push(vec![1_u8; MEMORY_STEP_MIB * MIB]);
            held_mib += MEMORY_STEP_MIB;
            if held_mib >= MEMORY_LIMIT_MIB {
                say("memory unbounded")?;
                return Err(io::Error::other("memory unbounded"));
            }
            if held_mib.is_multiple_of(MEMORY_REPORT_MIB) {
                say(&format!("holding {held_mib}"))?;
            }
        }
    }

    /// Fills a file in `/tmp` with 1 MiB writes, then tries to create a file at each of `OUTSIDE_PATHS`.
    fn scratch_bytes() -> io::Result<()> {
        let mut fill = File::create(FILL_FILE)?;
        let chunk = vec![0_u8; FILL_CHUNK];
        let mut written = 0_usize;
        let mut refusal = None;
        while written < FILL_LIMIT {
            match fill.write(&chunk) {
                Ok(0) => break,
                Ok(count) => written += count,
                Err(error) => {
                    refusal = Some(error.kind());
                    break;
                }
            }
        }
        say(&format!(
            "scratch bytes={written} refused={}",
            refusal_text(refusal)
        ))?;
        for path in OUTSIDE_PATHS {
            say(&format!("outside {path}={}", try_outside(path)?))?;
        }
        Ok(())
    }

    /// Creates `path` and removes it again. Returns `created`, or the kind of the error that refused the creation.
    fn try_outside(path: &str) -> io::Result<String> {
        match File::create(path) {
            Ok(file) => {
                drop(file);
                fs::remove_file(path)?;
                Ok(String::from("created"))
            }
            Err(error) => Ok(format!("{:?}", error.kind())),
        }
    }

    /// Creates empty files in `/tmp` until one is refused, then frees one inode and makes a directory.
    fn scratch_inodes() -> io::Result<()> {
        let mut next = 0_usize;
        let (files, refusal) = until_refused(INODE_LIMIT, || {
            let path = format!("/tmp/f{next}");
            next += 1;
            File::create(path)
        });
        say(&format!(
            "scratch files={files} refused={}",
            refusal_text(refusal)
        ))?;
        fs::remove_file("/tmp/f0")?;
        let mkdir = match fs::create_dir("/tmp/d") {
            Ok(()) => String::from("ok"),
            Err(error) => format!("{:?}", error.kind()),
        };
        say(&format!("after-remove mkdir={mkdir}"))
    }

    /// Writes 64 KiB chunks alternately to stdout and stderr for ever, ignoring write errors.
    fn flood() -> ! {
        let chunk = vec![b'x'; FLOOD_CHUNK];
        let mut to_stdout = true;
        loop {
            if to_stdout {
                drop(io::stdout().lock().write_all(&chunk));
            } else {
                drop(io::stderr().lock().write_all(&chunk));
            }
            to_stdout = !to_stdout;
        }
    }

    /// Runs `SPINNERS` threads that spin for `SPIN_TIME`, then reports how many finished.
    fn cpu() -> io::Result<()> {
        let mut spinners = Vec::with_capacity(SPINNERS);
        for _ in 0..SPINNERS {
            spinners.push(Builder::new().spawn(spin)?);
        }
        let mut done = 0_usize;
        for spinner in spinners {
            if spinner.join().is_ok() {
                done += 1;
            }
        }
        say(&format!("spinners done={done}"))
    }

    /// Spins until `SPIN_TIME` has passed on the monotonic clock.
    fn spin() {
        let start = Instant::now();
        while start.elapsed() < SPIN_TIME {
            std::hint::spin_loop();
        }
    }

    /// Runs the survivors scenario and then exits with its children alive.
    fn survivors() -> io::Result<()> {
        let middle_status = Command::new(SELF).arg("middle").status()?;
        if !middle_status.success() {
            return Err(io::Error::other(
                "the middle role did not exit successfully",
            ));
        }
        let mut ignorer = Command::new(SELF).arg("ignore").spawn()?;
        if !wait_for_path(IGNORE_READY, WAIT_BUDGET) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the ignore role did not become ready",
            ));
        }
        process::kill_process(process::Pid::from_child(&ignorer), process::Signal::TERM)?;
        // The ignorer reports the signal through the flag file. The job reads the liveness below, so a missing flag is not an error here.
        wait_for_path(IGNORE_GOT, WAIT_BUDGET);
        thread::sleep(SETTLE);
        let stat = format!("/proc/{}/stat", ignorer.id());
        let alive = Path::new(&stat).exists() && matches!(ignorer.try_wait(), Ok(None));
        say(&format!("ignorer alive-after-term={alive}"))?;
        say(&format!("writer bytes={}", writer_length()?))?;
        say(&format!("processes={}", count_process_entries()?))
    }

    /// Runs the survivors scenario, then sleeps for ever.
    fn survivors_hang() -> io::Result<()> {
        survivors()?;
        sleep_forever()
    }

    /// Starts the `detached` role without waiting for it, so that the child outlives this role.
    fn middle() -> io::Result<()> {
        Command::new(SELF).arg("detached").spawn()?;
        Ok(())
    }

    /// Leaves the session, then appends to the writer file and prints `background` every 100 ms for ever.
    fn detached() -> io::Result<()> {
        match process::setsid() {
            Ok(_) => say("detached setsid=ok")?,
            Err(error) => say(&format!("detached setsid={:?}", error.kind()))?,
        }
        let mut writer = OpenOptions::new()
            .append(true)
            .create(true)
            .open(WRITER_FILE)?;
        loop {
            writer.write_all(b"background\n")?;
            say("background")?;
            thread::sleep(BACKGROUND_PERIOD);
        }
    }

    /// Ignores SIGTERM by setting a flag, announces readiness and the flag, and then stays alive for ever.
    fn ignore() -> io::Result<()> {
        let terminated = Arc::new(AtomicBool::new(false));
        signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&terminated))?;
        File::create(IGNORE_READY)?;
        let mut announced = false;
        loop {
            thread::sleep(POLL);
            if !announced && terminated.load(Ordering::SeqCst) {
                File::create(IGNORE_GOT)?;
                announced = true;
            }
        }
    }

    /// Polls for `path` every `POLL` until it exists or `budget` has passed. Returns whether it exists.
    fn wait_for_path(path: &str, budget: Duration) -> bool {
        let deadline = Instant::now() + budget;
        while !Path::new(path).exists() {
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(POLL);
        }
        true
    }

    /// Returns the length of the writer file, or 0 when it does not exist.
    fn writer_length() -> io::Result<u64> {
        match fs::metadata(WRITER_FILE) {
            Ok(metadata) => Ok(metadata.len()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(error),
        }
    }

    /// Counts the entries of `/proc` whose names are all digits.
    fn count_process_entries() -> io::Result<usize> {
        let mut count = 0_usize;
        for entry in fs::read_dir("/proc")? {
            let name = entry?.file_name();
            let is_pid = name.to_str().is_some_and(|text| {
                !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
            });
            if is_pid {
                count += 1;
            }
        }
        Ok(count)
    }
}
