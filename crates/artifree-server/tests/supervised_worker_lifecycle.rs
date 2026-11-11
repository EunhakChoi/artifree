//! The supervised worker of a persistent server, as real processes (decision
//! 0055, rules 1 and 2): a default `serve` starts one worker; `--no-worker`,
//! `ARTIFREE_NO_WORKER` and a one-shot session start none; a server that stops
//! or is killed leaves no worker process; and `worker run --supervised` stops at
//! the end of its standard input and is refused with `--once`.
//!
//! Every wait is bounded and observes a condition: a line on stdout, the end of
//! standard error, an exit status or a worker state. A child is killed and
//! reaped when its guard is dropped, so a failing test leaves no process behind.

use std::error::Error;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::thread;
use std::time::{Duration, Instant};

use artifree_providers::data_dir::create_private_dir;
use artifree_providers::storage::{DATABASE_FILE, SqliteStore};
use serde_json::{Value, json};

type TestResult = Result<(), Box<dyn Error>>;

const BIN: &str = env!("CARGO_BIN_EXE_artifree-server");
/// The bound of every wait; a healthy run finishes well inside it.
const PATIENCE: Duration = Duration::from_secs(60);
/// How often a bounded wait looks again.
const STEP: Duration = Duration::from_millis(20);

/// A directory under the system temporary directory, removed when dropped.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new() -> Result<Self, Box<dyn Error>> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "artifree-supervised-worker-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        // A directory with the same name from an earlier run is not kept. The new one
        // is private, as a data directory must be: a server refuses one that other
        // users can enter.
        let _ = fs::remove_dir_all(&dir);
        create_private_dir(&dir)?;
        Ok(Self { dir })
    }

    /// A new directory holding a database with its schema, as a worker needs one.
    fn with_database() -> Result<Self, Box<dyn Error>> {
        let scratch = Self::new()?;
        SqliteStore::open(&scratch.dir.join(DATABASE_FILE))?;
        Ok(scratch)
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

/// A child process whose stdout lines and whole standard error are read by
/// threads, so a full pipe never blocks it. It is killed and reaped when dropped.
struct Process {
    child: Child,
    lines: Receiver<String>,
    stderr_eof: Receiver<String>,
    input: Option<ChildStdin>,
}

impl Process {
    /// Starts the test binary with `args` and `stdin` as its standard input.
    /// Every `ARTIFREE_*` variable is removed first, so the ambient environment
    /// cannot change what is under test; `variables` are set after that.
    fn spawn(
        args: &[&str],
        stdin: Stdio,
        variables: &[(&str, &str)],
    ) -> Result<Self, Box<dyn Error>> {
        let mut command = Command::new(BIN);
        command
            .args(args)
            .stdin(stdin)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("ARTIFREE_") {
                command.env_remove(name);
            }
        }
        for (name, value) in variables {
            command.env(name, value);
        }
        let mut child = command.spawn()?;
        let input = child.stdin.take();
        let stdout = child.stdout.take().ok_or("the child has no stdout")?;
        let stderr = child.stderr.take().ok_or("the child has no stderr")?;
        let (sender, lines) = channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let (ended, stderr_eof) = channel();
        thread::spawn(move || {
            let mut text = Vec::new();
            // The read ends only when every process that holds the pipe has exited.
            let _ = BufReader::new(stderr).read_to_end(&mut text);
            let _ = ended.send(String::from_utf8_lossy(&text).into_owned());
        });
        Ok(Self {
            child,
            lines,
            stderr_eof,
            input,
        })
    }

    /// Kills the process, if it is still running.
    fn kill(&mut self) -> std::io::Result<()> {
        self.child.kill()
    }

    /// The next stdout line, which must be an `expected` event, parsed as JSON.
    fn next_event(&self, expected: &str) -> Result<Value, Box<dyn Error>> {
        let Ok(line) = self.lines.recv_timeout(PATIENCE) else {
            return Err(format!(
                "timeout waiting for {expected} (no line within 60 s, or stdout ended)"
            )
            .into());
        };
        let event: Value = serde_json::from_str(&line)?;
        if event["event"] != expected {
            return Err(format!("expected {expected}, got {line}").into());
        }
        Ok(event)
    }

    /// Waits until the process exits, for at most `within`.
    fn wait_exit(&mut self, within: Duration) -> Result<ExitStatus, Box<dyn Error>> {
        let deadline = Instant::now() + within;
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err(
                    format!("the process did not exit within {} s", within.as_secs()).into(),
                );
            }
            thread::sleep(STEP);
        }
    }

    /// Whether the process has not exited yet.
    fn is_running(&mut self) -> Result<bool, Box<dyn Error>> {
        Ok(self.child.try_wait()?.is_none())
    }

    /// Every stdout line that is left, read until stdout ends. Stdout ends when
    /// the process exits, so this is called after it.
    fn rest_lines(&self) -> Result<Vec<String>, Box<dyn Error>> {
        let mut lines = Vec::new();
        loop {
            match self.lines.recv_timeout(PATIENCE) {
                Ok(line) => lines.push(line),
                Err(RecvTimeoutError::Disconnected) => return Ok(lines),
                Err(RecvTimeoutError::Timeout) => {
                    return Err("stdout stayed open for 60 s: the process did not end".into());
                }
            }
        }
    }

    /// The whole standard error, once its pipe has ended. The pipe ends only when
    /// every process that inherited it has exited, so this proves none is left.
    fn wait_stderr_eof(&self, within: Duration) -> Result<String, Box<dyn Error>> {
        let Ok(text) = self.stderr_eof.recv_timeout(within) else {
            return Err(format!(
                "standard error stayed open for {} s: a process that inherited it is still running",
                within.as_secs()
            )
            .into());
        };
        Ok(text)
    }

    /// Ends the standard input, which is the stop request of `--supervised`.
    fn close_input(&mut self) {
        self.input = None;
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // Both calls fail for a process that already exited, which changes nothing.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A `GET` with a bearer token over a plain TCP connection. It asks the server to
/// close the connection, so the whole response can be read to its end.
fn get(api_url: &str, path: &str, token: &str) -> Result<(u16, Value), Box<dyn Error>> {
    let address = api_url
        .strip_prefix("http://")
        .ok_or("the ready line gives no http address")?;
    let mut stream = TcpStream::connect(address)?;
    stream.set_read_timeout(Some(PATIENCE))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    let text = String::from_utf8(response)?;
    let Some((head, body)) = text.split_once("\r\n\r\n") else {
        return Err("the response has no end of its headers".into());
    };
    let status: u16 = head
        .lines()
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .ok_or("the response has no status line")?
        .parse()?;
    Ok((status, serde_json::from_str(body)?))
}

/// A string member of a JSON object, or an error that names the member.
fn member(object: &Value, name: &str) -> Result<String, Box<dyn Error>> {
    object[name]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("the ready line has no {name}").into())
}

/// A running `serve`: its process, its API address and the administrator's token.
struct Server {
    process: Process,
    api_url: String,
    token: String,
}

impl Server {
    /// Starts `serve` with `args` and reads its ready line, which the server
    /// prints only after both listeners are bound and its signal handlers exist.
    fn start(args: &[&str], variables: &[(&str, &str)]) -> Result<Self, Box<dyn Error>> {
        let process = Process::spawn(args, Stdio::null(), variables)?;
        let Ok(ready) = process.next_event("ready") else {
            // A server that fails to start says why on its standard error.
            let stderr = process
                .wait_stderr_eof(Duration::from_secs(5))
                .unwrap_or_default();
            return Err(format!("the server printed no ready line; its stderr: {stderr}").into());
        };
        let api_url = member(&ready, "api_url")?;
        let token_file = member(&ready, "token_file")?;
        let token = fs::read_to_string(token_file)?.trim().to_owned();
        Ok(Self {
            process,
            api_url,
            token,
        })
    }

    /// The `worker` object of `GET /v1/background`, read with the administrator's token.
    fn worker(&self) -> Result<Value, Box<dyn Error>> {
        let (status, body) = get(&self.api_url, "/v1/background", &self.token)?;
        if status != 200 {
            return Err(format!("GET /v1/background answered {status}: {body}").into());
        }
        Ok(body["worker"].clone())
    }

    /// Polls `GET /v1/background` until the worker is in `state`, within the patience.
    fn wait_for_worker(&self, state: &str) -> Result<Value, Box<dyn Error>> {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let worker = self.worker()?;
            if worker["state"] == state {
                return Ok(worker);
            }
            if Instant::now() >= deadline {
                return Err(
                    format!("the worker did not reach {state} within 60 s: {worker}").into(),
                );
            }
            thread::sleep(STEP);
        }
    }
}

/// Arguments of a persistent server whose data is in `dir`, with both listeners
/// on free ports, so tests that run at once never collide.
fn persistent_args(dir: &str) -> Vec<&str> {
    vec![
        "serve",
        "--data-dir",
        dir,
        "--api-listen",
        "127.0.0.1:0",
        "--content-listen",
        "127.0.0.1:0",
    ]
}

/// The ways a server starts without its worker (decision 0055, rule 1).
#[derive(Clone, Copy, Debug)]
enum NoWorker {
    /// `--no-worker` on the command line.
    Flag,
    /// `ARTIFREE_NO_WORKER=1` in the environment.
    EnvOne,
    /// `ARTIFREE_NO_WORKER=true` in the environment.
    EnvTrue,
    /// A one-shot session, which never has a worker and no data directory.
    OneShot,
}

/// The arguments and variables that start a server of `kind`. A one-shot session
/// keeps its directory inside `dir`, so the test removes it with `dir`.
fn no_worker_start(kind: NoWorker, dir: &str) -> (Vec<&str>, Vec<(&'static str, &'static str)>) {
    let mut args = vec!["serve"];
    let mut variables = Vec::new();
    match kind {
        NoWorker::Flag => args.extend(["--data-dir", dir, "--no-worker"]),
        NoWorker::EnvOne => {
            args.extend(["--data-dir", dir]);
            variables.push(("ARTIFREE_NO_WORKER", "1"));
        }
        NoWorker::EnvTrue => {
            args.extend(["--data-dir", dir]);
            variables.push(("ARTIFREE_NO_WORKER", "true"));
        }
        NoWorker::OneShot => {
            args.extend(["--one-shot-ttl-seconds", "60", "--session-root", dir]);
        }
    }
    args.extend([
        "--api-listen",
        "127.0.0.1:0",
        "--content-listen",
        "127.0.0.1:0",
    ]);
    (args, variables)
}

#[test]
fn a_default_serve_starts_one_supervised_worker() -> TestResult {
    let scratch = Scratch::new()?;
    let server = Server::start(&persistent_args(scratch.text()?), &[])?;
    let worker = server.wait_for_worker("running")?;
    assert_eq!(
        (
            worker["mode"].as_str(),
            worker["state"].as_str(),
            worker["restarts"].as_u64()
        ),
        (Some("supervised"), Some("running"), Some(0)),
        "a default serve runs one supervised worker that has not restarted: {worker}"
    );
    Ok(())
}

#[test]
fn a_server_that_starts_no_worker_reports_none_and_stopped() -> TestResult {
    for kind in [
        NoWorker::Flag,
        NoWorker::EnvOne,
        NoWorker::EnvTrue,
        NoWorker::OneShot,
    ] {
        let scratch = Scratch::new()?;
        let (args, variables) = no_worker_start(kind, scratch.text()?);
        let server = Server::start(&args, &variables)?;
        let worker = server.worker()?;
        assert_eq!(
            (worker["mode"].as_str(), worker["state"].as_str()),
            (Some("none"), Some("stopped")),
            "{kind:?} starts no worker: {worker}"
        );
    }
    Ok(())
}

#[test]
fn a_killed_server_leaves_no_worker_process() -> TestResult {
    let scratch = Scratch::new()?;
    let mut server = Server::start(&persistent_args(scratch.text()?), &[])?;
    server.wait_for_worker("running")?;
    server.process.kill()?;
    // The worker's standard input ends with the server, so the worker stops, and
    // the standard error it inherited ends with it.
    server.process.wait_stderr_eof(PATIENCE)?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_stopped_server_leaves_no_worker_process() -> TestResult {
    let scratch = Scratch::new()?;
    let mut server = Server::start(&persistent_args(scratch.text()?), &[])?;
    server.wait_for_worker("running")?;
    let pid = server.process.child.id().to_string();
    let sent = Command::new("kill").args(["-TERM", &pid]).status()?;
    assert!(sent.success(), "kill -TERM {pid} failed: {sent}");
    let status = server.process.wait_exit(Duration::from_secs(15))?;
    assert!(
        status.success(),
        "a stopped server exits with status 0: {status}"
    );
    server.process.wait_stderr_eof(Duration::from_secs(15))?;
    Ok(())
}

#[test]
fn supervised_is_refused_with_once_and_when_given_twice() -> TestResult {
    let scratch = Scratch::new()?;
    let dir = scratch.text()?;
    for words in [
        ["--supervised", "--once"],
        ["--once", "--supervised"],
        ["--supervised", "--supervised"],
    ] {
        let mut args = vec!["worker", "run", "--data-dir", dir];
        args.extend(words);
        let mut worker = Process::spawn(&args, Stdio::null(), &[])?;
        let status = worker.wait_exit(PATIENCE)?;
        let stderr = worker.wait_stderr_eof(PATIENCE)?;
        let stdout = worker.rest_lines()?;
        assert_eq!(
            status.code(),
            Some(1),
            "{words:?} is a usage error: {stderr}"
        );
        assert!(
            stdout.is_empty(),
            "{words:?} prints nothing on stdout: {stdout:?}"
        );
        assert!(
            stderr.starts_with("Unknown arguments."),
            "{words:?} is named on stderr: {stderr}"
        );
    }
    Ok(())
}

#[test]
fn a_supervised_worker_with_no_input_stops_at_once() -> TestResult {
    let scratch = Scratch::with_database()?;
    let mut worker = Process::spawn(
        &[
            "worker",
            "run",
            "--data-dir",
            scratch.text()?,
            "--supervised",
        ],
        Stdio::null(),
        &[],
    )?;
    worker.next_event("ready")?;
    let stopped = worker.next_event("stopped")?;
    assert_eq!(
        stopped,
        json!({"event": "stopped", "reason": "signal"}),
        "the end of an empty input is a signal stop"
    );
    let status = worker.wait_exit(PATIENCE)?;
    assert!(
        status.success(),
        "a stopped worker exits with status 0: {status}"
    );
    let after = worker.rest_lines()?;
    assert!(
        after.is_empty(),
        "nothing follows the stopped line: {after:?}"
    );
    Ok(())
}

#[test]
fn a_supervised_worker_stops_when_its_input_ends() -> TestResult {
    let scratch = Scratch::with_database()?;
    let mut worker = Process::spawn(
        &[
            "worker",
            "run",
            "--data-dir",
            scratch.text()?,
            "--supervised",
        ],
        Stdio::piped(),
        &[],
    )?;
    worker.next_event("ready")?;
    // While its input stays open, the worker keeps running.
    thread::sleep(Duration::from_millis(500));
    assert!(
        worker.is_running()?,
        "the worker stopped while its input was still open"
    );
    worker.close_input();
    let stopped = worker.next_event("stopped")?;
    assert_eq!(
        stopped,
        json!({"event": "stopped", "reason": "signal"}),
        "the end of the input is a signal stop"
    );
    let status = worker.wait_exit(PATIENCE)?;
    assert!(
        status.success(),
        "a stopped worker exits with status 0: {status}"
    );
    Ok(())
}
