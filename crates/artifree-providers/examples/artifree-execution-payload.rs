//! Test-only payload of R02ab to R02ag (decision 0056 rules 4, 12 and 14).
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
//! `descendant-refusal`, `lower`, `descriptors`, `watch`, `network`, `host`,
//! `proc-dev`, `environment`, `marker`, `copy`, `input-write`, `out-socket`,
//! `out-many`, `out-big-file`, `out-big-total`, `out-bomb`, `out-deep`,
//! `out-invalid-name`, `out-collision`, `out-extension`, `out-no-entry`,
//! `out-fail`, `out-plain`, `crash`, `crash-setter`, `linger` and `fail`. Each prints fixed
//! lines to stdout, flushed, that the job tests parse. A scenario that cannot
//! do its work prints the reason to stderr and exits with status 3. An unknown
//! scenario word exits with status 2. Status 4 means that a call the filter
//! must refuse returned. The `clone3` scenario replaces the payload with the
//! probe `ns_clone3`, so its status is the probe's. The `fail` scenario only
//! writes one fixed line to stderr and exits with status 5, a status no other
//! scenario uses, so that a job test sees a deliberate non-zero exit with a
//! known standard error (decision 0058 rule 18).
//!
//! The input and output scenarios (R02af, decision 0056 amendment of
//! 2026-11-13) read the staged input at `/input` and write the output root
//! `/tmp/out`, which the keeper makes before the release. Each of them exits
//! with status 0 after its lines, except `out-fail`: it writes a valid entry
//! page and then exits with status 3 on purpose, so that the job test sees a
//! failed attempt that still has output. The `out-*` scenarios make only the
//! calls that the filter allows, so none of them makes a link, a rename, a
//! mode change, a device or a mount.
//!
//! The scenarios start this binary again with one child role as its only
//! argument: `hold`, `middle`, `detached`, `ignore`, `descriptors-child`,
//! `hold-briefly`, `crash-child`, `setter-limit` or `setter-dumpable`. Any other
//! argument list exits with status 2. Other systems keep the R02ab behaviour
//! only, because the Linux-only dependencies are not built there.
//!
//! The crash scenarios (R02ag, decision 0056 amendment of 2026-11-13, crashes)
//! print their core limits from `/proc/self/limits`, fill 8 MiB of memory, start
//! their children and then send themselves signals with `kill`. The payload
//! learns its PID from `/proc/self/stat`, since `getpid` is refused. `crash`
//! ends by sending itself `SIGABRT`, and `crash-child` and `crash-setter` end by
//! sending themselves `SIGSEGV` twice. The setter roles are killed by the
//! filter. Status 4 means that a crash or a refused call returned. `linger`
//! sleeps 15 seconds and exits with status 0. Since R02ag the payload starts in
//! `/tmp`, the scratch root; every path the scenarios use is absolute.

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
    use nix::sys::signal::{self, Signal};
    use rustix::process;
    use std::ffi::{OsStr, OsString};
    use std::fmt::Display;
    use std::fs::{self, File, OpenOptions};
    use std::io::{self, Write};
    use std::net::{Ipv4Addr, Ipv6Addr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
    use std::os::fd::AsRawFd;
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::{SocketAddr as UnixSocketAddr, UnixListener, UnixStream};
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
    /// The file that holds the runner's IPv4 address, which the operator's `prepare.sh` writes into the tool root.
    const HOST_ADDRESS_FILE: &str = "/tools/etc/host-address";
    /// The TCP port of the loopback listener that the job test starts on IPv4 and IPv6 loopback.
    const LOOPBACK_TCP_PORT: u16 = 47001;
    /// The TCP port of the listener that the job test starts on the runner's address.
    const HOST_TCP_PORT: u16 = 47002;
    /// The UDP port of the loopback listener that the job test starts.
    const LOOPBACK_UDP_PORT: u16 = 47053;
    /// The Unix socket path of the listener that the job test starts in `/tmp`.
    const HOST_SOCKET_PATH: &str = "/tmp/artifree-enforcement-host.sock";
    /// The abstract Unix socket name of the listener that the job test starts outside the attempt.
    const HOST_ABSTRACT_NAME: &str = "artifree-enforcement-host";
    /// The abstract Unix socket name of the positive control, which the payload listens on inside the attempt.
    const INSIDE_ABSTRACT_NAME: &str = "artifree-enforcement-inside";
    /// How long the `network` scenario keeps its namespace in view after its last line, so that the job test can read it from outside.
    const NETWORK_HOLD: Duration = Duration::from_secs(2);
    /// The canary files that the `host` scenario reads, through the paths that the job test lists. `/tmp` is the scratch inside an attempt, so no canary lives there.
    const HOST_READ_PATHS: [&str; 5] = [
        "/var/tmp/artifree-enforcement-canary/secret",
        "/home/runner/artifree-enforcement-canary/secret",
        "/tools/../var/tmp/artifree-enforcement-canary/secret",
        "/proc/1/root/var/tmp/artifree-enforcement-canary/secret",
        "/proc/self/cwd/../var/tmp/artifree-enforcement-canary/secret",
    ];
    /// The paths that the `host` scenario tries to create files at: inside the canary's drop directory, in `/` and in `/tools`.
    const HOST_CREATE_PATHS: [&str; 3] = [
        "/var/tmp/artifree-enforcement-canary/drop/created",
        "/created",
        "/tools/created",
    ];
    /// The kernel's limit on process numbers, which the `host` scenario reads before it probes every number up to it.
    const PID_MAX_FILE: &str = "/proc/sys/kernel/pid_max";
    /// The kernel controls that the `proc-dev` scenario tries to write, as name, path and text.
    const PROC_WRITES: [(&str, &str, &str); 5] = [
        ("hostname", "/proc/sys/kernel/hostname", "artifree"),
        ("drop-caches", "/proc/sys/vm/drop_caches", "1"),
        ("core-pattern", "/proc/sys/kernel/core_pattern", "artifree"),
        ("sysrq", "/proc/sysrq-trigger", "h"),
        ("ns-last-pid", "/proc/sys/kernel/ns_last_pid", "500"),
    ];
    /// The pseudo-terminal master device that the `proc-dev` scenario opens.
    const DEV_PTMX: &str = "/dev/ptmx";
    /// The pseudo-terminal masters as the launcher's own mount namespace shows them, reached through the root of the PID namespace's first process, which runs in that namespace and not in the bootstrap's.
    const INIT_PTMX: [(&str, &str); 2] = [
        ("init-ptmx", "/proc/1/root/dev/ptmx"),
        ("init-pts-ptmx", "/proc/1/root/dev/pts/ptmx"),
    ];
    /// The directory of the pseudo-terminal slaves, which the `proc-dev` scenario lists.
    const DEV_PTS: &str = "/dev/pts";
    /// The file that the `marker` scenario writes into its scratch, to see whether a previous attempt left it.
    const MARKER: &str = "/tmp/artifree-marker";
    /// The shared-memory file that the `marker` scenario tries to create. The attempt refuses the creation.
    const SHM_MARKER: &str = "/dev/shm/artifree-marker";
    /// The staged input's entry page.
    const INPUT_PAGE: &str = "/input/index.html";
    /// The staged input as the launcher's own mount namespace shows it, reached through the root of the PID namespace's first process, which runs in that namespace and not in the bootstrap's.
    const INIT_INPUT: &str = "/proc/1/root/input";
    /// The output root that the keeper makes before the release.
    const OUT_DIR: &str = "/tmp/out";
    /// A valid entry page for the hostile output scenarios.
    const OUT_PAGE: &[u8] = b"<!doctype html><title>out</title>";
    /// The marker that `copy` appends to the input page.
    const COPIED: &[u8] = b"<!-- copied -->";
    /// The file that `copy` writes in the scratch, so that the scratch's shared memory is large while the payload runs.
    const COPY_BIG_FILE: &str = "/tmp/big";
    /// The number of one-mebibyte writes that `copy` makes to `COPY_BIG_FILE`.
    const COPY_BIG_MIB: usize = 64;
    /// Five mebibytes, which is the largest file that the package accepts.
    const FIVE_MIB: usize = 5 * MIB;
    /// The number of one-byte files that `out-many` adds to its entry page, which makes 257 files: more than the package accepts.
    const MANY_FILES: usize = 256;
    /// The number of five-mebibyte files that `out-big-total` adds to its entry page: 30 MiB, which is more than the package's 25 MiB.
    const BIG_TOTAL_FILES: usize = 6;
    /// The number of empty directories that `out-bomb` makes directly under the output root, which with the entry page is more than the collection's 2,048 entries.
    const BOMB_DIRECTORIES: usize = 2_100;
    /// The memory that each crashing process fills before it crashes: 8 MiB of `0x5a`.
    const CRASH_MEMORY: usize = 8 * MIB;
    /// The core limit, 1 MiB, that the `setter-limit` role asks for.
    const SETTER_LIMIT: u64 = 1_048_576;
    /// How long the `linger` scenario sleeps before it exits with status 0.
    const LINGER: Duration = Duration::from_secs(15);
    /// The process's own status file; its first field is the PID, since `getpid` is refused.
    const PROC_SELF_STAT: &str = "/proc/self/stat";
    /// The process's own limits, whose core line the crash scenarios print.
    const PROC_SELF_LIMITS: &str = "/proc/self/limits";
    /// The name of the core limit's line in `/proc/self/limits`.
    const CORE_LIMIT_LINE: &str = "Max core file size";

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
            "network" => finish(network()),
            "host" => finish(host()),
            "proc-dev" => finish(proc_dev()),
            "environment" => finish(environment()),
            "marker" => finish(marker()),
            "copy" => finish(copy()),
            "input-write" => finish(input_write()),
            "out-socket" => finish(out_socket()),
            "out-many" => finish(out_many()),
            "out-big-file" => finish(out_big_file()),
            "out-big-total" => finish(out_big_total()),
            "out-bomb" => finish(out_bomb()),
            "out-deep" => finish(out_deep()),
            "out-invalid-name" => finish(out_named("out-invalid-name", &[".hidden.txt"])),
            "out-collision" => finish(out_named("out-collision", &["A.txt", "a.txt"])),
            "out-extension" => finish(out_named("out-extension", &["x.exe"])),
            "out-no-entry" => finish(out_no_entry()),
            "out-fail" => out_fail(),
            "out-plain" => finish(out_plain()),
            "crash" => finish_status(crash()),
            "crash-setter" => finish_status(crash_setter()),
            "linger" => finish(linger()),
            "fail" => report(&"fail scenario", 5),
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
            Some("crash-child") => finish_status(crash_child()),
            Some("setter-limit") => finish_status(setter_limit()),
            Some("setter-dumpable") => finish_status(setter_dumpable()),
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

    /// Names the outcome of a call from its error: `ok` when there is none, else `error=<n>` with the raw OS error number (else the kind).
    fn outcome_of_error(error: Option<&io::Error>) -> String {
        match error {
            Some(error) => format!("error={}", error_word(Some(error))),
            None => String::from("ok"),
        }
    }

    /// Names the outcome of a call: `ok`, or `error=<n>` with the raw OS error number (else the kind).
    fn outcome_word(result: io::Result<()>) -> String {
        outcome_of_error(result.err().as_ref())
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

    /// Prints one line that names the outcome of the try `name`.
    fn network_line(name: &str, result: io::Result<()>) -> io::Result<()> {
        say(&format!("network {name} {}", outcome_word(result)))
    }

    /// Opens a TCP connection to `address` with a plain blocking connect, then drops it. Returns the outcome of the connect.
    fn connect_tcp(address: impl ToSocketAddrs) -> io::Result<()> {
        TcpStream::connect(address).map(drop)
    }

    /// Sends one datagram from a socket bound to `local` to `destination`. Returns the outcome of the send.
    fn send_datagram(local: (Ipv4Addr, u16), destination: (Ipv4Addr, u16)) -> io::Result<()> {
        let socket = UdpSocket::bind(local)?;
        socket.send_to(b"artifree", destination).map(drop)
    }

    /// Connects a Unix stream to the abstract socket `name`. Returns the outcome of the connect.
    fn connect_abstract(name: &str) -> io::Result<()> {
        let address = UnixSocketAddr::from_abstract_name(name)?;
        UnixStream::connect_addr(&address).map(drop)
    }

    /// Makes each outside try, in the order that the job test reads them, and prints one line for each.
    fn network_attempts(runner_address: Ipv4Addr) -> io::Result<()> {
        let unspecified = (Ipv4Addr::UNSPECIFIED, 0);
        network_line(
            "tcp-loopback4",
            connect_tcp((Ipv4Addr::LOCALHOST, LOOPBACK_TCP_PORT)),
        )?;
        network_line(
            "tcp-loopback6",
            connect_tcp((Ipv6Addr::LOCALHOST, LOOPBACK_TCP_PORT)),
        )?;
        network_line("tcp-host", connect_tcp((runner_address, HOST_TCP_PORT)))?;
        network_line(
            "udp-loopback",
            send_datagram(
                (Ipv4Addr::LOCALHOST, 0),
                (Ipv4Addr::LOCALHOST, LOOPBACK_UDP_PORT),
            ),
        )?;
        network_line(
            "udp-resolver",
            send_datagram(unspecified, (Ipv4Addr::new(127, 0, 0, 53), 53)),
        )?;
        network_line(
            "udp-azure-dns",
            send_datagram(unspecified, (Ipv4Addr::new(168, 63, 129, 16), 53)),
        )?;
        network_line(
            "tcp-metadata",
            connect_tcp((Ipv4Addr::new(169, 254, 169, 254), 80)),
        )?;
        network_line(
            "tcp-external",
            connect_tcp((Ipv4Addr::new(1, 1, 1, 1), 443)),
        )?;
        network_line(
            "tcp6-external",
            connect_tcp((
                Ipv6Addr::new(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111),
                443,
            )),
        )?;
        network_line("unix-path", UnixStream::connect(HOST_SOCKET_PATH).map(drop))?;
        network_line("unix-abstract", connect_abstract(HOST_ABSTRACT_NAME))
    }

    /// Listens on 127.0.0.1 at a port that the kernel picks, connects to it and accepts the connection.
    fn local_tcp_round_trip() -> io::Result<()> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let address = listener.local_addr()?;
        // The client stays open until the accept returns.
        let _client = TcpStream::connect(address)?;
        listener.accept().map(drop)
    }

    /// Listens on the abstract name inside the attempt, connects to it and accepts the connection.
    fn local_abstract_round_trip() -> io::Result<()> {
        let address = UnixSocketAddr::from_abstract_name(INSIDE_ABSTRACT_NAME)?;
        let listener = UnixListener::bind_addr(&address)?;
        // The client stays open until the accept returns.
        let _client = UnixStream::connect_addr(&address)?;
        listener.accept().map(drop)
    }

    /// Makes the two positive controls, which must succeed inside the attempt, and prints one line for each.
    fn network_controls() -> io::Result<()> {
        network_line("local-tcp", local_tcp_round_trip())?;
        network_line("local-abstract", local_abstract_round_trip())
    }

    /// Runs the `network` scenario: makes the outside tries and the controls, then holds the namespace for `NETWORK_HOLD`.
    fn network() -> io::Result<()> {
        let text = fs::read_to_string(HOST_ADDRESS_FILE)?;
        let runner_address: Ipv4Addr = text.trim().parse().map_err(io::Error::other)?;
        network_attempts(runner_address)?;
        network_controls()?;
        say("network holding")?;
        thread::sleep(NETWORK_HOLD);
        say("network done")
    }

    /// Counts the process numbers from 1 to `pid_max` whose signal-0 probe succeeds, which is every existing process that the caller may signal.
    fn count_answering_pids() -> io::Result<usize> {
        let limit: i32 = fs::read_to_string(PID_MAX_FILE)?
            .trim()
            .parse()
            .map_err(io::Error::other)?;
        let mut answered = 0_usize;
        for raw in 1..=limit {
            if let Some(pid) = process::Pid::from_raw(raw)
                && process::test_kill_process(pid).is_ok()
            {
                answered += 1;
            }
        }
        Ok(answered)
    }

    /// Runs the `host` scenario: tries to read the canaries and to create files outside the scratch, probes every process number, then starts a `hold` child and kills every process it may signal.
    fn host() -> io::Result<()> {
        for path in HOST_READ_PATHS {
            let read = File::open(path).map(drop);
            say(&format!("host read {path} {}", outcome_word(read)))?;
        }
        for path in HOST_CREATE_PATHS {
            let created = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map(drop);
            say(&format!("host create {path} {}", outcome_word(created)))?;
        }
        let answered = count_answering_pids()?;
        say(&format!("host signal-probe answered={answered}"))?;
        let mut child = Command::new(SELF).arg("hold").spawn()?;
        let announced = say(&format!("host hold pid={}", child.id()));
        let killed =
            signal::kill(nix::unistd::Pid::from_raw(-1), Signal::SIGKILL).map_err(io::Error::from);
        let kill_announced = say(&format!("host kill-all {}", outcome_word(killed)));
        // The child is waited for before any announcement's error is reported, so no path leaves it unreaped.
        let status = child.wait()?;
        announced?;
        kill_announced?;
        say(&format!(
            "host hold ended signal={}",
            number_word(status.signal())
        ))
    }

    /// Returns the names in `/dev/pts`, sorted and joined by one space. A name that is not text is shown lossily.
    fn list_dev_pts() -> io::Result<String> {
        let mut names: Vec<String> = Vec::new();
        for entry in fs::read_dir(DEV_PTS)? {
            names.push(entry?.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        Ok(names.join(" "))
    }

    /// Runs the `proc-dev` scenario: tries each kernel-control write, opens `/dev/ptmx` and the launcher's masters through `/proc/1/root`, and lists `/dev/pts`.
    fn proc_dev() -> io::Result<()> {
        for (name, path, text) in PROC_WRITES {
            let written = OpenOptions::new()
                .write(true)
                .open(path)
                .and_then(|mut file| file.write_all(text.as_bytes()));
            say(&format!("proc-dev {name} {}", outcome_word(written)))?;
        }
        // If the open succeeds, the descriptor stays open until this function returns, so its terminal would show in the listing.
        let ptmx = OpenOptions::new().read(true).write(true).open(DEV_PTMX);
        say(&format!(
            "proc-dev ptmx {}",
            outcome_of_error(ptmx.as_ref().err())
        ))?;
        let mut through_init = Vec::new();
        for (name, path) in INIT_PTMX {
            let opened = OpenOptions::new().read(true).write(true).open(path);
            say(&format!(
                "proc-dev {name} {}",
                outcome_of_error(opened.as_ref().err())
            ))?;
            through_init.push(opened);
        }
        // The mount table's `/dev/pts` lines, as evidence of the cover in the job's log.
        if let Ok(table) = fs::read_to_string("/proc/self/mountinfo") {
            for line in table.lines().filter(|line| line.contains(" /dev/pts ")) {
                say(&format!("proc-dev mount {line}"))?;
            }
        }
        match list_dev_pts() {
            Ok(names) => say(&format!("proc-dev pts {names}")),
            Err(error) => say(&format!("proc-dev pts error={}", error_word(Some(&error)))),
        }
    }

    /// Runs the `environment` scenario: prints how many variables the payload has, then each name. No value is printed.
    fn environment() -> io::Result<()> {
        let names: Vec<OsString> = std::env::vars_os().map(|(name, _)| name).collect();
        say(&format!("environment count={}", names.len()))?;
        for name in &names {
            say(&format!("environment var={}", name.to_string_lossy()))?;
        }
        Ok(())
    }

    /// Runs the `marker` scenario: reports whether a marker from an earlier attempt is in the scratch, writes one, and tries to create one in `/dev/shm`.
    fn marker() -> io::Result<()> {
        let found = if Path::new(MARKER).exists() {
            "yes"
        } else {
            "no"
        };
        say(&format!("marker found={found}"))?;
        say(&format!(
            "marker wrote {}",
            outcome_word(fs::write(MARKER, b"x"))
        ))?;
        let shared = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(SHM_MARKER)
            .map(drop);
        say(&format!("marker dev-shm {}", outcome_word(shared)))
    }

    /// Writes `contents` to `name` in the output root, which the keeper makes before the release.
    fn write_output(name: &str, contents: &[u8]) -> io::Result<()> {
        fs::write(format!("{OUT_DIR}/{name}"), contents)
    }

    /// Prints one `input-write` line: the name of the check and its outcome.
    fn input_line(name: &str, result: io::Result<()>) -> io::Result<()> {
        say(&format!("input-write {name} {}", outcome_word(result)))
    }

    /// Creates the file at `path` and fails when it already exists. Returns the outcome of the creation.
    fn create_new_file(path: &str) -> io::Result<()> {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map(drop)
    }

    /// Opens the file at `path` for appending and writes nothing. Returns the outcome of the open.
    fn append_to(path: &str) -> io::Result<()> {
        OpenOptions::new().append(true).open(path).map(drop)
    }

    /// Writes the staged input page into the output with the marker appended, adds an asset, then writes 64 MiB to `/tmp/big`.
    fn copy() -> io::Result<()> {
        let mut copied = fs::read(INPUT_PAGE)?;
        copied.extend_from_slice(COPIED);
        write_output("index.html", &copied)?;
        fs::create_dir(format!("{OUT_DIR}/assets"))?;
        write_output("assets/result.txt", b"collected")?;
        let mut big = File::create(COPY_BIG_FILE)?;
        let chunk = vec![0x5a_u8; MIB];
        for _ in 0..COPY_BIG_MIB {
            big.write_all(&chunk)?;
        }
        say("copy done")
    }

    /// Tries each change to the staged input and prints one line for each. Then prints the `/input` line of the mount table and copies the unchanged input page into the output.
    fn input_write() -> io::Result<()> {
        input_line("create", create_new_file("/input/new.txt"))?;
        input_line("unlink", fs::remove_file(INPUT_PAGE))?;
        input_line("mkdir", fs::create_dir("/input/d"))?;
        input_line("append", append_to(INPUT_PAGE))?;
        input_line(
            "init-append",
            append_to(&format!("{INIT_INPUT}/index.html")),
        )?;
        input_line(
            "init-create",
            create_new_file(&format!("{INIT_INPUT}/new.txt")),
        )?;
        input_line(
            "init-unlink",
            fs::remove_file(format!("{INIT_INPUT}/index.html")),
        )?;
        input_line("init-mkdir", fs::create_dir(format!("{INIT_INPUT}/d")))?;
        let table = fs::read_to_string("/proc/self/mountinfo")?;
        let mount = table
            .lines()
            .rev()
            .find(|line| line.split_whitespace().nth(4) == Some("/input"))
            .unwrap_or("none");
        say(&format!("input-write mount {mount}"))?;
        write_output("index.html", &fs::read(INPUT_PAGE)?)?;
        say("input-write done")
    }

    /// Writes a valid entry page and binds a Unix socket file named `s.txt` in the output root. The listener stays bound until after the announcement.
    fn out_socket() -> io::Result<()> {
        write_output("index.html", OUT_PAGE)?;
        let listener = UnixListener::bind(format!("{OUT_DIR}/s.txt"))?;
        let announced = say("out-socket done");
        drop(listener);
        announced
    }

    /// Writes a valid entry page and 256 one-byte files, which makes 257 files with the page.
    fn out_many() -> io::Result<()> {
        write_output("index.html", OUT_PAGE)?;
        for index in 0..MANY_FILES {
            write_output(&format!("f{index}.txt"), b"x")?;
        }
        say("out-many done")
    }

    /// Writes a valid entry page and one file of 5 MiB plus one byte.
    fn out_big_file() -> io::Result<()> {
        write_output("index.html", OUT_PAGE)?;
        let big = vec![0x5a_u8; FIVE_MIB + 1];
        write_output("big.txt", &big)?;
        say("out-big-file done")
    }

    /// Writes a valid entry page and six files of 5 MiB each, which is 30 MiB in all.
    fn out_big_total() -> io::Result<()> {
        write_output("index.html", OUT_PAGE)?;
        let chunk = vec![0x5a_u8; FIVE_MIB];
        for index in 0..BIG_TOTAL_FILES {
            write_output(&format!("b{index}.txt"), &chunk)?;
        }
        say("out-big-total done")
    }

    /// Writes a valid entry page and empty directories directly under the output root, more than the collection's entry bound.
    fn out_bomb() -> io::Result<()> {
        write_output("index.html", OUT_PAGE)?;
        for index in 0..BOMB_DIRECTORIES {
            fs::create_dir(format!("{OUT_DIR}/d{index}"))?;
        }
        say("out-bomb done")
    }

    /// Writes a valid entry page and eight directories nested one in another, so that the deepest one is below the collection's depth bound.
    fn out_deep() -> io::Result<()> {
        write_output("index.html", OUT_PAGE)?;
        fs::create_dir_all(format!("{OUT_DIR}/d1/d2/d3/d4/d5/d6/d7/d8"))?;
        say("out-deep done")
    }

    /// Writes a valid entry page and one file of one byte for each of `names`, then says that `scenario` is done.
    fn out_named(scenario: &str, names: &[&str]) -> io::Result<()> {
        write_output("index.html", OUT_PAGE)?;
        for name in names {
            write_output(name, b"x")?;
        }
        say(&format!("{scenario} done"))
    }

    /// Writes one file of one byte and no entry page, so that the output has no `index.html`.
    fn out_no_entry() -> io::Result<()> {
        write_output("page.txt", b"x")?;
        say("out-no-entry done")
    }

    /// Writes a valid entry page and exits with status 3 on purpose, so that the job test sees a failed attempt that still has output. A write error is reported with status 3 too.
    fn out_fail() -> ExitCode {
        match write_output("index.html", OUT_PAGE) {
            Ok(()) => {
                // The status already says what happened, so a stdout that cannot be written does not change it.
                drop(say("out-fail done"));
                ExitCode::from(3)
            }
            Err(error) => report(&error, 3),
        }
    }

    /// Writes only a valid entry page.
    fn out_plain() -> io::Result<()> {
        write_output("index.html", OUT_PAGE)?;
        say("out-plain done")
    }

    /// Returns this process's PID, read from the first field of `/proc/self/stat`, since the filter refuses `getpid`.
    fn own_pid() -> io::Result<nix::unistd::Pid> {
        let stat = fs::read_to_string(PROC_SELF_STAT)?;
        let first = stat
            .split_whitespace()
            .next()
            .ok_or_else(|| io::Error::other("the stat file is empty"))?;
        let raw: i32 = first.parse().map_err(io::Error::other)?;
        Ok(nix::unistd::Pid::from_raw(raw))
    }

    /// Returns the soft and hard values of the core limit's line in `/proc/self/limits`, as the kernel prints them.
    fn core_limits() -> io::Result<(String, String)> {
        let text = fs::read_to_string(PROC_SELF_LIMITS)?;
        let rest = text
            .lines()
            .find_map(|line| line.strip_prefix(CORE_LIMIT_LINE))
            .ok_or_else(|| io::Error::other("no core limit line"))?;
        let mut values = rest.split_whitespace();
        match (values.next(), values.next()) {
            (Some(soft), Some(hard)) => Ok((soft.to_owned(), hard.to_owned())),
            _ => Err(io::Error::other(
                "the core limit line has no soft and hard value",
            )),
        }
    }

    /// Fills 8 MiB with `0x5a` and returns it. The caller keeps the result alive until it crashes, so the memory is still mapped at the crash.
    fn touch_memory() -> Vec<u8> {
        std::hint::black_box(vec![0x5a_u8; CRASH_MEMORY])
    }

    /// Sends `sig` to this process with `kill`, since a fault needs `unsafe` code. The kernel takes the same dump path for it as for a fault.
    fn raise(sig: Signal) -> io::Result<()> {
        signal::kill(own_pid()?, sig).map_err(io::Error::from)
    }

    /// Returns the core flag of a wait status as the crash lines print it: 1 when the kernel reports a core, else 0.
    fn core_word(status: std::process::ExitStatus) -> u8 {
        u8::from(status.core_dumped())
    }

    /// Prints the core limits, fills memory and runs `crash-child`, prints its signal and core flag, then sends itself `SIGABRT`. Status 4 means that the signal returned.
    fn crash() -> io::Result<ExitCode> {
        let (soft, hard) = core_limits()?;
        say(&format!("crash limits soft={soft} hard={hard}"))?;
        let _held = touch_memory();
        let status = Command::new(SELF).arg("crash-child").status()?;
        say(&format!(
            "crash child signal={} core={}",
            number_word(status.signal()),
            core_word(status)
        ))?;
        // Rust's runtime does not handle SIGABRT, so its default action ends the process.
        raise(Signal::SIGABRT)?;
        Ok(ExitCode::from(4))
    }

    /// The child of `crash`: prints its core limits, fills memory and sends itself `SIGSEGV` twice. Status 4 means that the signals returned.
    fn crash_child() -> io::Result<ExitCode> {
        let (soft, hard) = core_limits()?;
        say(&format!("crash-child limits soft={soft} hard={hard}"))?;
        let _held = touch_memory();
        // Rust's runtime handles the first SIGSEGV: for a signal that is not a guard-page fault its handler restores the default action and returns, so the second one takes the default action.
        raise(Signal::SIGSEGV)?;
        raise(Signal::SIGSEGV)?;
        Ok(ExitCode::from(4))
    }

    /// Prints the core limits, fills memory, runs the two setter children in turn and prints their signals and core flags, then sends itself `SIGSEGV` twice. Status 4 means that the signals returned.
    fn crash_setter() -> io::Result<ExitCode> {
        let (soft, hard) = core_limits()?;
        say(&format!("crash-setter limits soft={soft} hard={hard}"))?;
        let _held = touch_memory();
        for (name, role) in [("limit", "setter-limit"), ("dumpable", "setter-dumpable")] {
            let status = Command::new(SELF).arg(role).status()?;
            say(&format!(
                "crash-setter child={name} signal={} core={}",
                number_word(status.signal()),
                core_word(status)
            ))?;
        }
        raise(Signal::SIGSEGV)?;
        raise(Signal::SIGSEGV)?;
        Ok(ExitCode::from(4))
    }

    /// The child that asks `prlimit64` to set its core limit to 1 MiB. The filter kills it with `SIGSYS`; a returned call is a failure and the status is 4.
    fn setter_limit() -> io::Result<ExitCode> {
        let result = process::prlimit(
            None,
            process::Resource::Core,
            process::Rlimit {
                current: Some(SETTER_LIMIT),
                maximum: Some(SETTER_LIMIT),
            },
        );
        say(&format!("setter-limit returned {result:?}"))?;
        Ok(ExitCode::from(4))
    }

    /// The child that asks `prctl` to make it dumpable, which is `SUID_DUMP_USER`. The filter kills it with `SIGSYS`; a returned call is a failure and the status is 4.
    fn setter_dumpable() -> io::Result<ExitCode> {
        let result = process::set_dumpable_behavior(process::DumpableBehavior::Dumpable);
        say(&format!("setter-dumpable returned {result:?}"))?;
        Ok(ExitCode::from(4))
    }

    /// Sleeps for `LINGER`, then says that it is done. The scenario exits with status 0.
    fn linger() -> io::Result<()> {
        thread::sleep(LINGER);
        say("linger done")
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
