//! Spawning the server and observing it without fixed delays.

use std::ffi::OsStr;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

pub type TestResult = Result<(), Box<dyn std::error::Error>>;

pub const BIN: &str = env!("CARGO_BIN_EXE_artifree-server");
/// The longest any single wait may take.
pub const PATIENCE: Duration = Duration::from_secs(60);

/// A scratch directory used as `--session-root`; removed on drop.
pub struct Root {
    path: PathBuf,
}

impl Root {
    pub fn new() -> Result<Self, std::io::Error> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "artifree-one-shot-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The path as a command-line argument.
    pub fn arg(&self) -> &str {
        self.path.to_str().unwrap_or_default()
    }

    /// How many entries the root holds.
    pub fn entries(&self) -> Result<usize, std::io::Error> {
        Ok(std::fs::read_dir(&self.path)?.count())
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.path).ok();
    }
}

/// The `data_dir` field of a readiness line.
pub fn data_dir_of(ready: &Value) -> Result<PathBuf, String> {
    ready["data_dir"]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| format!("no data_dir in {ready}"))
}

/// A running `artifree-server`. Its stdout lines and stderr are drained by
/// threads, so a full pipe never blocks it.
pub struct Server {
    pub child: Child,
    lines: Receiver<String>,
    stderr: Arc<Mutex<String>>,
}

impl Server {
    pub fn start(args: &[&str], env: &[(&str, &OsStr)]) -> Result<Self, std::io::Error> {
        Self::start_with(args, env, Stdio::piped())
    }

    /// Like [`Server::start`] with a chosen stdout.
    pub fn start_with(
        args: &[&str],
        env: &[(&str, &OsStr)],
        stdout: Stdio,
    ) -> Result<Self, std::io::Error> {
        let mut command = Command::new(BIN);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(Stdio::piped());
        // The ambient environment must not change what is under test.
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("ARTIFREE_") {
                command.env_remove(name);
            }
        }
        for (name, value) in env {
            command.env(name, value);
        }
        let mut child = command.spawn()?;
        let (send, lines) = channel();
        if let Some(out) = child.stdout.take() {
            thread::spawn(move || {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    if send.send(line).is_err() {
                        break;
                    }
                }
            });
        }
        let stderr = Arc::new(Mutex::new(String::new()));
        if let Some(err) = child.stderr.take() {
            let sink = Arc::clone(&stderr);
            thread::spawn(move || {
                for line in BufReader::new(err).lines().map_while(Result::ok) {
                    if let Ok(mut text) = sink.lock() {
                        text.push_str(&line);
                        text.push('\n');
                    }
                }
            });
        }
        Ok(Self {
            child,
            lines,
            stderr,
        })
    }

    /// Everything the server wrote to stderr so far.
    pub fn stderr(&self) -> String {
        self.stderr
            .lock()
            .map(|text| text.clone())
            .unwrap_or_default()
    }

    /// Waits until stderr contains `needle`. The process may still be running.
    pub fn wait_stderr_contains(&self, needle: &str) -> Result<(), String> {
        let deadline = Instant::now() + PATIENCE;
        while !self.stderr().contains(needle) {
            if Instant::now() >= deadline {
                return Err(format!("no {needle:?} on stderr: {}", self.stderr()));
            }
            thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }

    /// The next stdout line, or `None` when the process closed stdout first.
    pub fn next_line(&self) -> Result<Option<String>, String> {
        match self.lines.recv_timeout(PATIENCE) {
            Ok(line) => Ok(Some(line)),
            Err(RecvTimeoutError::Disconnected) => Ok(None),
            Err(RecvTimeoutError::Timeout) => Err(format!(
                "no output within {PATIENCE:?}; stderr: {}",
                self.stderr()
            )),
        }
    }

    /// Waits for the readiness line and returns it as JSON.
    pub fn ready(&mut self) -> Result<Value, String> {
        let line = self
            .next_line()?
            .ok_or_else(|| format!("exited before readiness; stderr: {}", self.stderr()))?;
        let json: Value =
            serde_json::from_str(&line).map_err(|error| format!("{line}: {error}"))?;
        if json["event"] == "ready" {
            Ok(json)
        } else {
            Err(format!("not a readiness line: {line}"))
        }
    }

    /// Waits for the process to exit.
    pub fn wait_exit(&mut self) -> Result<ExitStatus, String> {
        let deadline = Instant::now() + PATIENCE;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return Ok(status),
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                Ok(None) => {
                    return Err(format!("still running; stderr: {}", self.stderr()));
                }
                Err(error) => return Err(error.to_string()),
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}
