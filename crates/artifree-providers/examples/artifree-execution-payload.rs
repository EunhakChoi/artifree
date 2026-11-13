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
//! `scratch-bytes`, `scratch-inodes`, `flood`, `hang`, `cpu`, `survivors`,
//! `survivors-hang`, `probes`, `clone3`, `thread-refusal`,
//! `descendant-refusal`, `lower`, `descriptors` and `watch`. Each prints fixed
//! lines to stdout, flushed, that the job tests parse. A scenario that cannot
//! do its work prints the reason to stderr and exits with status 3. An unknown
//! scenario word exits with status 2. Status 4 means that a call the filter
//! must refuse returned. The `clone3` scenario replaces the payload with the
//! probe `ns_clone3`, so its status is the probe's.
//!
//! The scenarios start this binary again with one child role as its only
//! argument: `hold`, `middle`, `detached`, `ignore`, `descriptors-child` or
//! `hold-briefly`. Any other argument list exits with status 2. Other systems
//! keep the R02ab behaviour only, because the Linux-only dependencies are not
//! built there.

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
    use nix::sched::{self, CloneFlags};
    use rustix::process;
    use std::ffi::{OsStr, OsString};
    use std::fmt::Display;
    use std::fs::{self, File, OpenOptions};
    use std::io::{self, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::path::Path;
    use std::process::{Child, Command, ExitCode};
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
    /// The directory of the generated probe programs. The operator's `prepare.sh` writes it before the launch.
    const PROBES_DIR: &str = "/tools/probes";
    /// The probe that the `clone3` scenario replaces the payload with.
    const CLONE3_PROBE: &str = "/tools/probes/ns_clone3";
    /// The soft descriptor limit that the `lower` scenario sets before it restores the limit.
    const LOWER_SOFT: u64 = 128;
    /// The device that the descriptor scenarios open again and again.
    const DEVICE_NULL: &str = "/dev/null";
    /// The bound on each descriptor loop, so that a broken ceiling still ends the scenario. Reaching it prints `error=none`.
    const DESCRIPTOR_LIMIT: usize = 4096;
    /// How long the `watch` scenario keeps its thread and its child alive.
    const BRIEF_HOLD: Duration = Duration::from_secs(2);

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
            "probes" => finish(probes()),
            "clone3" => clone3(),
            "thread-refusal" => finish_status(thread_refusal()),
            "descendant-refusal" => finish_status(descendant_refusal()),
            "lower" => finish_status(lower()),
            "descriptors" => finish(descriptors()),
            "watch" => finish(watch()),
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
            Some("descriptors-child") => finish(descriptor_report("descriptors-child")),
            Some("hold-briefly") => {
                thread::sleep(BRIEF_HOLD);
                ExitCode::SUCCESS
            }
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

    /// Returns the scenario's own status on success, or reports the error and returns status 3.
    fn finish_status(result: io::Result<ExitCode>) -> ExitCode {
        match result {
            Ok(code) => code,
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

    /// Calls `attempt` until it fails or has succeeded `limit` times, and keeps every success. Returns the successes and the error that stopped the loop, if there was one.
    fn keep_until_refused<T>(
        limit: usize,
        mut attempt: impl FnMut() -> io::Result<T>,
    ) -> (Vec<T>, Option<io::Error>) {
        let mut kept: Vec<T> = Vec::new();
        while kept.len() < limit {
            match attempt() {
                Ok(value) => kept.push(value),
                Err(error) => return (kept, Some(error)),
            }
        }
        (kept, None)
    }

    /// Names the error kind that stopped a loop, or `none` when the loop reached its own limit.
    fn refusal_text(refusal: Option<io::ErrorKind>) -> String {
        match refusal {
            Some(kind) => format!("{kind:?}"),
            None => String::from("none"),
        }
    }

    /// Names an exit code or a signal number: the number, or `none` when there is none.
    fn number_word(value: Option<i32>) -> String {
        match value {
            Some(number) => number.to_string(),
            None => String::from("none"),
        }
    }

    /// Names a descriptor limit: the number, or `unlimited` when the limit has no value.
    fn limit_word(value: Option<u64>) -> String {
        match value {
            Some(number) => number.to_string(),
            None => String::from("unlimited"),
        }
    }

    /// Names the error that stopped a loop: its raw OS error number when it has one, else its kind, or `none` when the loop reached its bound without an error.
    fn error_word(error: Option<&io::Error>) -> String {
        match error {
            Some(error) => match error.raw_os_error() {
                Some(number) => number.to_string(),
                None => format!("{:?}", error.kind()),
            },
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

    /// Starts each file in `/tools/probes` as a child, in file-name order, with inherited stdio and an empty environment. Prints one line per probe, then a count.
    fn probes() -> io::Result<()> {
        let mut names: Vec<OsString> = Vec::new();
        for entry in fs::read_dir(PROBES_DIR)? {
            names.push(entry?.file_name());
        }
        names.sort();
        for name in &names {
            // `status` keeps the stdio inherited: captured pipes need pipe2, which the filter refuses.
            let status = Command::new(Path::new(PROBES_DIR).join(name))
                .env_clear()
                .status()?;
            say(&format!(
                "probe name={} code={} signal={}",
                name.to_string_lossy(),
                number_word(status.code()),
                number_word(status.signal())
            ))?;
        }
        say(&format!("probes done count={}", names.len()))
    }

    /// Replaces this process with the `ns_clone3` probe, so that the probe's exit status is the payload's. Returns only when the exec fails.
    fn clone3() -> ExitCode {
        let error = Command::new(CLONE3_PROBE).env_clear().exec();
        report(&error, 3)
    }

    /// Starts a thread that makes a call the filter must refuse, then waits for it. The filter kills the whole process, so a returned call is a failure and the status is 4.
    fn thread_refusal() -> io::Result<ExitCode> {
        let handle = Builder::new().spawn(unshare_in_thread)?;
        let joined = handle
            .join()
            .map_err(|_| io::Error::other("the refusing thread panicked"))?;
        let text = joined?;
        drop(say(&format!("thread-refusal returned {text}")));
        Ok(ExitCode::from(4))
    }

    /// The body of the refusing thread: it announces itself, then calls `unshare` with `CLONE_NEWNS`, which the filter refuses.
    fn unshare_in_thread() -> io::Result<String> {
        say("thread-refusal started")?;
        Ok(format!("{:?}", sched::unshare(CloneFlags::CLONE_NEWNS)))
    }

    /// Starts a `hold` child and sets its descriptor limit. The filter kills the process for that call, so a returned call is a failure and the status is 4.
    fn descendant_refusal() -> io::Result<ExitCode> {
        let mut child = Command::new(SELF).arg("hold").spawn()?;
        let reported = set_descendant_limit(&child);
        // Reached only when the filter did not kill the payload, or when a report failed: stop and reap the child either way before returning.
        let killed = child.kill();
        let reaped = child.wait();
        reported?;
        killed?;
        reaped?;
        Ok(ExitCode::from(4))
    }

    /// Reports the child, then sets its descriptor limit, which the filter refuses, and reports the result if the call returns.
    fn set_descendant_limit(child: &Child) -> io::Result<()> {
        say(&format!("descendant pid={}", child.id()))?;
        let result = process::prlimit(
            Some(process::Pid::from_child(child)),
            process::Resource::Nofile,
            process::Rlimit {
                current: Some(128),
                maximum: Some(256),
            },
        );
        say(&format!("descendant-refusal returned {result:?}"))
    }

    /// Lowers the soft descriptor limit within the hard limit, then restores it. Under the filter the lowering is refused by a kill, since rustix's `setrlimit` makes a `prlimit64` call (syscall 302). This is the setter guard's lower-and-restore case. A returned call is a failure and the status is 4.
    fn lower() -> io::Result<ExitCode> {
        let before = process::getrlimit(process::Resource::Nofile);
        say(&format!(
            "lower before soft={} hard={}",
            limit_word(before.current),
            limit_word(before.maximum)
        ))?;
        let set = process::setrlimit(
            process::Resource::Nofile,
            process::Rlimit {
                current: Some(LOWER_SOFT),
                maximum: before.maximum,
            },
        );
        say(&format!("lower set {set:?}"))?;
        let restored = process::setrlimit(process::Resource::Nofile, before);
        say(&format!("lower restored {restored:?}"))?;
        Ok(ExitCode::from(4))
    }

    /// Returns the numbers of the open descriptors in ascending order, joined with commas. The list may contain the descriptor of the `/proc/self/fd` directory being read, which is normally 3.
    fn open_descriptors() -> io::Result<String> {
        let mut numbers: Vec<u32> = Vec::new();
        for entry in fs::read_dir("/proc/self/fd")? {
            let name = entry?.file_name();
            let text = name.to_str().ok_or_else(|| {
                io::Error::other(format!(
                    "a descriptor name is not text: {}",
                    name.to_string_lossy()
                ))
            })?;
            let number: u32 = text.parse().map_err(io::Error::other)?;
            numbers.push(number);
        }
        numbers.sort_unstable();
        let texts: Vec<String> = numbers.iter().map(u32::to_string).collect();
        Ok(texts.join(","))
    }

    /// Prints the three descriptor lines of `prefix`: the limits with the open numbers, the opens up to the ceiling, and the duplications of half of those files. Every file is dropped when this returns.
    fn descriptor_report(prefix: &str) -> io::Result<()> {
        let nofile = process::getrlimit(process::Resource::Nofile);
        let core_limit = process::getrlimit(process::Resource::Core);
        let open = open_descriptors()?;
        say(&format!(
            "{prefix} limits soft={} hard={} core-soft={} core-hard={} open={open}",
            limit_word(nofile.current),
            limit_word(nofile.maximum),
            limit_word(core_limit.current),
            limit_word(core_limit.maximum),
        ))?;
        let (mut files, open_refusal) =
            keep_until_refused(DESCRIPTOR_LIMIT, || File::open(DEVICE_NULL));
        say(&format!(
            "{prefix} opened={} highest={} error={}",
            files.len(),
            number_word(files.iter().map(AsRawFd::as_raw_fd).max()),
            error_word(open_refusal.as_ref())
        ))?;
        // The ceiling is reached with the files still open, so half of them are dropped to leave descriptors for the duplications.
        files.truncate(files.len() / 2);
        let first = files
            .first()
            .ok_or_else(|| io::Error::other("no descriptor was opened"))?;
        let (clones, duplicate_refusal) =
            keep_until_refused(DESCRIPTOR_LIMIT, || first.try_clone());
        say(&format!(
            "{prefix} duplicated={} highest={} error={}",
            clones.len(),
            number_word(clones.iter().map(AsRawFd::as_raw_fd).max()),
            error_word(duplicate_refusal.as_ref())
        ))
    }

    /// Runs the descriptor report in this process, then runs the `descriptors-child` role, which starts with the same limits. The child's stdout is inherited.
    fn descriptors() -> io::Result<()> {
        descriptor_report("descriptors")?;
        let status = Command::new(SELF).arg("descriptors-child").status()?;
        say(&format!(
            "descriptors child code={}",
            number_word(status.code())
        ))
    }

    /// Keeps a thread and a `hold-briefly` child alive for `BRIEF_HOLD`, so that the job test can read every task of the group while both run.
    fn watch() -> io::Result<()> {
        let sleeper = Builder::new().spawn(|| thread::sleep(BRIEF_HOLD))?;
        let mut child = Command::new(SELF).arg("hold-briefly").spawn()?;
        // The child is waited for before the announcement's error is reported, so no path leaves it unreaped.
        let announced = say(&format!("watch child={}", child.id()));
        child.wait()?;
        announced?;
        sleeper
            .join()
            .map_err(|_| io::Error::other("the sleeping thread panicked"))?;
        say("watch done")
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
