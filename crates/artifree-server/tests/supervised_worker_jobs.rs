//! The supervised worker of a default `serve`, against the real binary and its
//! job routes (decision 0055, acceptance 21). A job asked of a default server
//! ends with nobody running `worker run`. A worker that is killed, or that exits
//! with status 0 unasked, is started again and runs jobs. A job held by a killed
//! worker ends after its lease through the server's own worker. The body of
//! `GET /v1/background` is exactly the contract and carries no output of the
//! worker, no process identifier and no path.
//!
//! The worker is found through `/proc` and signalled with `kill`, so the tests
//! that need a worker process run on Linux. Every wait is bounded and observes a
//! condition, and every process is killed and reaped when its guard drops, so a
//! failing test leaves no worker behind.

use std::error::Error;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::ExitStatus;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::{Duration, Instant};

use artifree_contracts::api::{
    BackgroundStatus, WorkerExitOutcome, WorkerMode, WorkerState, WorkerStatus,
};
#[cfg(target_os = "linux")]
use artifree_providers::storage::DATABASE_FILE;
use serde_json::{Value, json};

type TestResult = Result<(), Box<dyn Error>>;

const BIN: &str = env!("CARGO_BIN_EXE_artifree-server");
/// The bound of every wait; a healthy run finishes well inside it.
const PATIENCE: Duration = Duration::from_secs(60);
/// How often a bounded wait looks again. The server admits 1200 requests a minute to
/// one client after a burst of 120, so a step of 100 ms polls below that rate.
const STEP: Duration = Duration::from_millis(100);
/// The number of the signal SIGTERM on Linux.
#[cfg(target_os = "linux")]
const SIGTERM: u32 = 15;

/// Creates the directory `dir` for a server. The server refuses a data directory
/// that other users can access, so on Unix it is created with owner-only
/// permissions.
#[cfg(unix)]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(dir)
}

/// Creates the directory `dir`. Other platforms have no owner-only mode to set here.
#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir)
}

/// A directory under the system temporary directory, removed when dropped. The
/// server creates its own database in it.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new() -> Result<Self, Box<dyn Error>> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "artifree-supervised-jobs-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        // A directory with the same name from an earlier run is not kept.
        let _ = fs::remove_dir_all(&dir);
        create_private_dir(&dir)?;
        Ok(Self { dir })
    }

    fn text(&self) -> Result<&str, Box<dyn Error>> {
        self.dir
            .to_str()
            .ok_or_else(|| "the temporary directory is not UTF-8".into())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Sends each line of `reader` to a channel, from a thread of its own, until the
/// reader ends. A full pipe therefore never blocks the process that writes it.
fn read_lines_in_thread(reader: impl Read + Send + 'static) -> Receiver<String> {
    let (sender, receiver) = channel();
    thread::spawn(move || {
        for line in BufReader::new(reader).lines().map_while(Result::ok) {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    receiver
}

/// A child process whose standard output and standard error are read by threads.
/// It is killed and reaped when dropped.
struct Process {
    child: Child,
    /// The child's standard input, which stays open until the process is dropped.
    /// A worker treats the end of its input as a stop request (decision 0055, rule 2).
    _stdin: ChildStdin,
    lines: Receiver<String>,
    errors: Receiver<String>,
}

impl Process {
    /// Starts the server binary with `args`. Its standard input is a pipe that stays
    /// open until the process is dropped. Every `ARTIFREE_*` variable is removed
    /// first, so the ambient environment cannot change what is under test.
    fn spawn(args: &[&str]) -> Result<Self, Box<dyn Error>> {
        let mut command = Command::new(BIN);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("ARTIFREE_") {
                command.env_remove(name);
            }
        }
        let mut child = command.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or("the child has no standard input")?;
        let stdout = child.stdout.take().ok_or("the child has no stdout")?;
        let stderr = child
            .stderr
            .take()
            .ok_or("the child has no standard error")?;
        let lines = read_lines_in_thread(stdout);
        let errors = read_lines_in_thread(stderr);
        Ok(Self {
            child,
            _stdin: stdin,
            lines,
            errors,
        })
    }

    /// The next line of standard output, which must be the JSON event `event`.
    fn next_event(&self, event: &str) -> Result<Value, Box<dyn Error>> {
        let Ok(line) = self.lines.recv_timeout(PATIENCE) else {
            return Err(format!(
                "no line within 60 s, or the output ended, while waiting for {event}; \
                 the standard error so far: {}",
                self.diagnostics()
            )
            .into());
        };
        let value: Value = serde_json::from_str(&line)?;
        if value["event"] != event {
            return Err(format!("expected {event}, got {line}").into());
        }
        Ok(value)
    }

    /// The standard error read so far, for a diagnosis.
    fn diagnostics(&self) -> String {
        self.errors.try_iter().collect::<Vec<String>>().join("\n")
    }

    /// Kills the process, as `SIGKILL` does on Linux.
    #[cfg(target_os = "linux")]
    fn kill(&mut self) -> std::io::Result<()> {
        self.child.kill()
    }

    /// Waits until the process has exited, for at most the patience.
    #[cfg(target_os = "linux")]
    fn wait_exit(&mut self) -> Result<ExitStatus, Box<dyn Error>> {
        let deadline = Instant::now() + PATIENCE;
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err("the process did not exit within 60 s".into());
            }
            thread::sleep(STEP);
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // Both calls fail for a process that already exited, which changes nothing.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A request with a bearer token over a plain TCP connection, answered with its
/// status and body text. The server is asked to close the connection, so the
/// whole response can be read to its end.
fn request(
    api_url: &str,
    method: &str,
    path: &str,
    token: &str,
    key: Option<&str>,
    body: Option<&Value>,
) -> Result<(u16, String), Box<dyn Error>> {
    let address = api_url
        .strip_prefix("http://")
        .ok_or("the ready line gives no http address")?;
    let payload = body.map_or_else(String::new, Value::to_string);
    let key_line = key.map_or_else(String::new, |key| format!("Idempotency-Key: {key}\r\n"));
    let type_line = if body.is_some() {
        "Content-Type: application/json\r\n"
    } else {
        ""
    };
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\nContent-Length: {length}\r\n{key_line}{type_line}\r\n",
        length = payload.len(),
    );
    let mut stream = TcpStream::connect(address)?;
    stream.set_read_timeout(Some(PATIENCE))?;
    stream.write_all(head.as_bytes())?;
    stream.write_all(payload.as_bytes())?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    let text = String::from_utf8(response)?;
    let Some((header_text, body_text)) = text.split_once("\r\n\r\n") else {
        return Err("the response has no end of its headers".into());
    };
    let status = header_text
        .lines()
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .ok_or("the response has no status line")?
        .parse::<u16>()?;
    Ok((status, body_text.to_owned()))
}

/// An error unless `status` is `expected`; the body text is shown in the error.
fn answered(status: u16, expected: u16, text: &str) -> Result<(), Box<dyn Error>> {
    if status == expected {
        Ok(())
    } else {
        Err(format!("the server answered {status}, not {expected}: {text}").into())
    }
}

/// A string member of a JSON object, or an error that names the member.
fn member(object: &Value, name: &str) -> Result<String, Box<dyn Error>> {
    object[name]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("the object has no {name}").into())
}

/// The `worker` object of a `GET /v1/background` body. The body must be exactly
/// the contract, with no key the contract does not name at any depth. It must also
/// carry none of the worker's output (`ready`, `event`, a worker identifier such as
/// `worker-1a2b3c4d`), no process identifier and no path of the data directory.
fn checked_worker(text: &str, dir: &str) -> Result<WorkerStatus, Box<dyn Error>> {
    let raw: Value = serde_json::from_str(text)?;
    let parsed: BackgroundStatus = serde_json::from_str(text)?;
    if serde_json::to_value(&parsed)? != raw {
        return Err(format!("the body is not exactly the contract: {text}").into());
    }
    for forbidden in ["ready", "event", "worker-", "pid", dir] {
        if text.contains(forbidden) {
            return Err(format!("the body carries {forbidden:?}: {text}").into());
        }
    }
    Ok(parsed.worker)
}

/// The outcome of the last exit or failed start, or `None` before any.
fn last_outcome(worker: &WorkerStatus) -> Option<WorkerExitOutcome> {
    worker.last_exit.as_ref().map(|exit| exit.outcome)
}

/// A running server: its process, its API address, its administrator's token and
/// its data directory.
struct Server {
    process: Process,
    api_url: String,
    token: String,
    dir: String,
}

impl Server {
    /// Starts a persistent `serve` on the data directory `dir` and reads its ready
    /// line, which the server prints once its listeners are bound and its worker
    /// has been started.
    fn start(dir: &str) -> Result<Self, Box<dyn Error>> {
        let process = Process::spawn(&[
            "serve",
            "--data-dir",
            dir,
            "--api-listen",
            "127.0.0.1:0",
            "--content-listen",
            "127.0.0.1:0",
        ])?;
        let ready = process.next_event("ready")?;
        let token_file = member(&ready, "token_file")?;
        let token = fs::read_to_string(token_file)?.trim().to_owned();
        Ok(Self {
            api_url: member(&ready, "api_url")?,
            process,
            token,
            dir: dir.to_owned(),
        })
    }

    /// Sends one request with the administrator's token. A refusal for the rate limit
    /// did not process the request, so it is sent again after a second, which is at
    /// least the delay that the refusal gives, until the patience runs out.
    fn send(
        &self,
        method: &str,
        path: &str,
        key: Option<&str>,
        body: Option<&Value>,
    ) -> Result<(u16, String), Box<dyn Error>> {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let (status, text) = request(&self.api_url, method, path, &self.token, key, body)?;
            if status != 429 || Instant::now() >= deadline {
                return Ok((status, text));
            }
            thread::sleep(Duration::from_secs(1));
        }
    }

    /// The text of `GET /v1/background`, which must answer `200`.
    fn background_text(&self) -> Result<String, Box<dyn Error>> {
        let (status, text) = self.send("GET", "/v1/background", None, None)?;
        answered(status, 200, &text)?;
        Ok(text)
    }

    /// The worker that `GET /v1/background` reports, after the checks of
    /// `checked_worker`.
    fn background(&self) -> Result<WorkerStatus, Box<dyn Error>> {
        checked_worker(&self.background_text()?, &self.dir)
    }

    /// Polls `GET /v1/background` until the worker satisfies `done`, within the
    /// patience. `what` names the wanted state in the error.
    fn wait_for_worker(
        &self,
        what: &str,
        done: impl Fn(&WorkerStatus) -> bool,
    ) -> Result<WorkerStatus, Box<dyn Error>> {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let worker = self.background()?;
            if done(&worker) {
                return Ok(worker);
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "the worker did not reach {what} within 60 s: {worker:?}; \
                     the server's standard error so far: {}",
                    self.process.diagnostics()
                )
                .into());
            }
            thread::sleep(STEP);
        }
    }

    /// Creates an artifact of two files and returns its identifier.
    fn create_artifact(&self, key: &str) -> Result<String, Box<dyn Error>> {
        let body = json!({
            "title": "Supervised worker",
            "files": [
                { "path": "index.html", "encoding": "utf8", "content": "<h1>supervised</h1>" },
                { "path": "app.js", "encoding": "utf8", "content": "let a = 1;" }
            ]
        });
        let (status, text) = self.send("POST", "/v1/artifacts", Some(key), Some(&body))?;
        answered(status, 201, &text)?;
        let created: Value = serde_json::from_str(&text)?;
        member(&created["artifact"], "id")
    }

    /// Asks for a `verify_revision` job of `revision` of `artifact` and returns its
    /// identifier.
    fn enqueue(&self, artifact: &str, key: &str, revision: u64) -> Result<String, Box<dyn Error>> {
        let body = json!({ "kind": "verify_revision", "revision": revision });
        let path = format!("/v1/artifacts/{artifact}/jobs");
        let (status, text) = self.send("POST", &path, Some(key), Some(&body))?;
        answered(status, 201, &text)?;
        let reply: Value = serde_json::from_str(&text)?;
        member(&reply["job"], "id")
    }

    /// Polls `GET /v1/jobs/{id}` until the job is in `state` and returns it. A job
    /// that ends in another state ends the wait at once.
    fn wait_for_job(&self, job: &str, state: &str) -> Result<Value, Box<dyn Error>> {
        let deadline = Instant::now() + PATIENCE;
        let path = format!("/v1/jobs/{job}");
        loop {
            let (status, text) = self.send("GET", &path, None, None)?;
            answered(status, 200, &text)?;
            let value: Value = serde_json::from_str(&text)?;
            let now = value["state"].as_str().unwrap_or("");
            if now == state {
                return Ok(value);
            }
            let ended = matches!(now, "succeeded" | "failed" | "cancelled");
            if ended || Instant::now() >= deadline {
                return Err(format!("the job did not reach {state} within 60 s: {value}").into());
            }
            thread::sleep(STEP);
        }
    }

    /// Asks a new verification of a new artifact and waits until it succeeds, which
    /// shows that the worker running now runs the work.
    fn verify_through_worker(&self, name: &str) -> Result<Value, Box<dyn Error>> {
        let artifact = self.create_artifact(&format!("supervised-artifact-{name}"))?;
        let job = self.enqueue(&artifact, &format!("supervised-job-{name}"), 1)?;
        self.wait_for_job(&job, "succeeded")
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // The server is killed first, so that no worker is restarted after the
        // kills below. The worker ends by itself when its input closes, unless it
        // is stopped, and the kills end what is still running.
        let _ = self.process.child.kill();
        kill_workers_of(&self.dir);
    }
}

/// The process identifiers of the supervised workers of the data directory `dir`,
/// read from `/proc`.
#[cfg(target_os = "linux")]
fn supervised_workers(dir: &str) -> Result<Vec<u32>, Box<dyn Error>> {
    let mut pids = Vec::new();
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        let name = entry.file_name();
        let Ok(pid) = name.to_string_lossy().parse::<u32>() else {
            continue;
        };
        // The command line of a process that has just exited cannot be read.
        let Ok(command_line) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        if is_supervised_worker_of(&command_line, dir) {
            pids.push(pid);
        }
    }
    Ok(pids)
}

/// Whether a command line (arguments separated by NUL bytes) is
/// `worker run --data-dir <dir> --supervised`. The server's own command line and
/// a worker that was run by hand do not match.
#[cfg(target_os = "linux")]
fn is_supervised_worker_of(command_line: &[u8], dir: &str) -> bool {
    let arguments: Vec<&str> = command_line
        .split(|byte| *byte == 0)
        .filter_map(|argument| std::str::from_utf8(argument).ok())
        .collect();
    let names_dir = arguments
        .windows(2)
        .any(|pair| matches!(pair, ["--data-dir", value] if *value == dir));
    let runs_worker = arguments
        .windows(2)
        .any(|pair| matches!(pair, ["worker", "run"]));
    names_dir && runs_worker && arguments.contains(&"--supervised")
}

/// Kills the supervised workers of the data directory `dir`. It is used when a test
/// ends, so that a worker which the test stopped does not outlive it.
#[cfg(target_os = "linux")]
fn kill_workers_of(dir: &str) {
    if let Ok(pids) = supervised_workers(dir) {
        for pid in pids {
            // A worker that has exited already needs no kill.
            let _ = send_signal(pid, "KILL");
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn kill_workers_of(_dir: &str) {}

/// Sends the signal `name`, such as `KILL`, to the process `pid` with `kill`.
#[cfg(target_os = "linux")]
fn send_signal(pid: u32, name: &str) -> Result<(), Box<dyn Error>> {
    let status = Command::new("kill")
        .arg(format!("-{name}"))
        .arg(pid.to_string())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("kill -{name} {pid} failed: {status}").into())
    }
}

/// The value of a `/proc/<pid>/status` field, such as `SigCgt:`, without its name.
#[cfg(target_os = "linux")]
fn status_field(pid: u32, field: &str) -> Result<String, Box<dyn Error>> {
    let status = fs::read_to_string(format!("/proc/{pid}/status"))?;
    status
        .lines()
        .find_map(|line| line.strip_prefix(field))
        .map(|value| value.trim().to_owned())
        .ok_or_else(|| format!("/proc/{pid}/status has no {field}").into())
}

/// Whether the process has a handler for `signal`, which its `SigCgt` mask shows.
/// A process without one ends by the signal, and so does not report status 0.
#[cfg(target_os = "linux")]
fn catches(pid: u32, signal: u32) -> Result<bool, Box<dyn Error>> {
    let mask = u64::from_str_radix(&status_field(pid, "SigCgt:")?, 16)?;
    Ok((mask & (1_u64 << (signal - 1))) != 0)
}

/// Whether the process is stopped, which its `State` field shows as `T`.
#[cfg(target_os = "linux")]
fn is_stopped(pid: u32) -> Result<bool, Box<dyn Error>> {
    Ok(status_field(pid, "State:")?.starts_with('T'))
}

/// Calls `check` until it answers `true`, within the patience. `what` names the
/// condition in the error.
#[cfg(target_os = "linux")]
fn poll(
    what: &str,
    mut check: impl FnMut() -> Result<bool, Box<dyn Error>>,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + PATIENCE;
    loop {
        if check()? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("{what} did not hold within 60 s").into());
        }
        thread::sleep(STEP);
    }
}

/// A connection to the database of the data directory `dir`. A lock held by the
/// server is waited for, up to the patience.
#[cfg(target_os = "linux")]
fn database(dir: &str) -> Result<rusqlite::Connection, Box<dyn Error>> {
    let connection = rusqlite::Connection::open(PathBuf::from(dir).join(DATABASE_FILE))?;
    connection.busy_timeout(PATIENCE)?;
    Ok(connection)
}

#[cfg(target_os = "linux")]
impl Server {
    /// The process identifiers of this server's supervised workers, once they
    /// satisfy `done`. `what` names the wanted state in the error.
    fn wait_for_workers(
        &self,
        what: &str,
        done: impl Fn(&[u32]) -> bool,
    ) -> Result<Vec<u32>, Box<dyn Error>> {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let pids = supervised_workers(&self.dir)?;
            if done(&pids) {
                return Ok(pids);
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "did not see {what} within 60 s; the supervised workers are {pids:?}"
                )
                .into());
            }
            thread::sleep(STEP);
        }
    }

    /// The process identifier of the one supervised worker, once there is one.
    fn the_worker(&self) -> Result<u32, Box<dyn Error>> {
        let pids = self.wait_for_workers("one supervised worker", |pids| pids.len() == 1)?;
        pids.first()
            .copied()
            .ok_or_else(|| "no supervised worker".into())
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_killed_worker_is_started_again_and_the_new_worker_runs_jobs() -> TestResult {
    let scratch = Scratch::new()?;
    let server = Server::start(scratch.text()?)?;
    server.wait_for_worker("running", |status| status.state == WorkerState::Running)?;
    let first = server.the_worker()?;
    send_signal(first, "KILL")?;
    let worker =
        server.wait_for_worker("running after one restart of a failed exit", |status| {
            status.state == WorkerState::Running
                && status.restarts == 1
                && last_outcome(status) == Some(WorkerExitOutcome::Failed)
        })?;
    assert_eq!(
        (worker.restarts, last_outcome(&worker)),
        (1, Some(WorkerExitOutcome::Failed)),
        "the killed worker is one restart and a failed exit: {worker:?}"
    );
    let pids = server.wait_for_workers("a new worker process", |pids| {
        pids.len() == 1 && pids.first() != Some(&first)
    })?;
    assert!(
        !pids.contains(&first),
        "the restarted worker is a new process: {pids:?}"
    );
    server.verify_through_worker("b")?;
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn a_job_held_by_a_killed_worker_ends_after_its_lease_through_the_servers_own_worker() -> TestResult
{
    let scratch = Scratch::new()?;
    let dir = scratch.text()?;
    let server = Server::start(dir)?;
    server.wait_for_worker("running", |status| status.state == WorkerState::Running)?;
    let own = server.the_worker()?;
    // A finished job shows that the worker has started up, so it is not inside its
    // start-up when it is stopped. A worker stopped inside a write keeps the database
    // locked, and then the server's own writes fail.
    server.verify_through_worker("c0")?;
    // The server's worker is stopped next, so that it cannot claim the job before
    // the other worker does.
    send_signal(own, "STOP")?;
    poll("the server's worker is stopped", || is_stopped(own))?;
    let artifact = server.create_artifact("supervised-artifact-c")?;
    let job = server.enqueue(&artifact, "supervised-job-c", 1)?;
    // Another worker claims the job and holds it, since its hold file never appears.
    let hold_file = scratch.dir.join("hold-that-never-appears");
    let hold = hold_file
        .to_str()
        .ok_or("the hold file path is not UTF-8")?;
    let mut other = Process::spawn(&[
        "worker",
        "run",
        "--data-dir",
        dir,
        "--lease-secs",
        "1",
        "--hold-file",
        hold,
    ])?;
    let holder = member(&other.next_event("ready")?, "worker")?;
    server.wait_for_job(&job, "running")?;
    let (claimed_by, attempts): (Option<String>, i64) = database(dir)?.query_row(
        "SELECT worker, attempts FROM jobs WHERE id = ?1",
        rusqlite::params![job],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(
        (claimed_by.as_deref(), attempts),
        (Some(holder.as_str()), 1),
        "the other worker holds the job at its first claim"
    );
    // Killed while it holds the job, it gives nothing back.
    other.kill()?;
    let status = other.wait_exit()?;
    assert!(
        !status.success(),
        "the killed worker did not end by itself: {status}"
    );
    // Its lease runs out at once instead of after a second. worker_process.rs:259
    // does the same.
    let changed = database(dir)?.execute(
        "UPDATE jobs SET lease_expires_at = 0 WHERE id = ?1",
        rusqlite::params![job],
    )?;
    assert_eq!(changed, 1, "the lease of the one job is made to run out");
    // The server's own worker resumes and takes the job over.
    send_signal(own, "CONT")?;
    let ended = server.wait_for_job(&job, "succeeded")?;
    assert_eq!(
        ended["attempt"], 2,
        "the takeover is a second claim: {ended}"
    );
    assert_eq!(
        server.the_worker()?,
        own,
        "the server's own worker, not a new one, ended the job"
    );
    assert_eq!(
        server.background()?.restarts,
        0,
        "that worker was never restarted"
    );
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn a_worker_that_exits_with_status_zero_unasked_is_started_again() -> TestResult {
    let scratch = Scratch::new()?;
    let server = Server::start(scratch.text()?)?;
    server.wait_for_worker("running", |status| status.state == WorkerState::Running)?;
    let first = server.the_worker()?;
    // Without a handler, SIGTERM would end the worker by the signal and its exit
    // would not be clean. The handler is installed before the worker prints its
    // ready line, and the wait below covers a worker that is still starting.
    poll("the worker catches SIGTERM", || catches(first, SIGTERM))?;
    send_signal(first, "TERM")?;
    let worker = server.wait_for_worker("running after one restart of a clean exit", |status| {
        status.state == WorkerState::Running
            && status.restarts == 1
            && last_outcome(status) == Some(WorkerExitOutcome::Clean)
    })?;
    assert_eq!(
        (worker.restarts, last_outcome(&worker)),
        (1, Some(WorkerExitOutcome::Clean)),
        "the stopped worker is one restart after a clean exit: {worker:?}"
    );
    let pids = server.wait_for_workers("a new worker process", |pids| {
        pids.len() == 1 && pids.first() != Some(&first)
    })?;
    assert!(
        !pids.contains(&first),
        "the restarted worker is a new process: {pids:?}"
    );
    server.verify_through_worker("d")?;
    Ok(())
}

#[test]
fn a_default_serve_ends_a_job_with_no_worker_started_by_hand() -> TestResult {
    let scratch = Scratch::new()?;
    let server = Server::start(scratch.text()?)?;
    let worker =
        server.wait_for_worker("running", |status| status.state == WorkerState::Running)?;
    assert_eq!(
        worker.mode,
        WorkerMode::Supervised,
        "a default serve supervises its worker: {worker:?}"
    );
    let ended = server.verify_through_worker("a")?;
    assert_eq!(
        ended["result"]["verdict"], "ok",
        "the revision verified: {ended}"
    );
    assert_eq!(
        ended["attempt"], 1,
        "the server's own worker claimed the job once: {ended}"
    );
    Ok(())
}

#[test]
fn the_background_body_carries_none_of_the_workers_output() -> TestResult {
    let scratch = Scratch::new()?;
    let server = Server::start(scratch.text()?)?;
    server.wait_for_worker("running", |status| status.state == WorkerState::Running)?;
    // The worker has printed its ready line and a line for the job before the
    // body is read.
    server.verify_through_worker("e")?;
    let text = server.background_text()?;
    let worker = checked_worker(&text, scratch.text()?)?;
    assert_eq!(
        (
            worker.mode,
            worker.state,
            worker.restarts,
            worker.dropped_lines
        ),
        (WorkerMode::Supervised, WorkerState::Running, 0, 0),
        "a worker that ran a job and was never restarted: {text}"
    );
    assert!(
        last_outcome(&worker).is_none(),
        "no exit is reported before one happens: {text}"
    );
    Ok(())
}
