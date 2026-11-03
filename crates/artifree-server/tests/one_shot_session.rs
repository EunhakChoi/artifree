//! One-shot sessions as real processes (decision 0038): the data directory is
//! private and is removed when the server ends by signal or by its lifetime, a
//! killed session's directory is swept by the next one only once its owner is
//! gone, and a new session never accepts what an old one issued. Persistent
//! mode keeps its data.
//!
//! Every wait observes a condition (a line on stdout, an exit status), never a
//! fixed delay standing in for one. A child is killed and reaped when its guard
//! is dropped, so a failing test leaves no process behind. The tests send
//! signals with `kill` and read Unix file modes, so they are Unix only.

#![cfg(unix)]

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const BIN: &str = env!("CARGO_BIN_EXE_artifree-server");
const PATIENCE: Duration = Duration::from_secs(60);
const STEP: Duration = Duration::from_millis(20);
const LONG: &str = "600";

/// A private directory for one test, removed when the test ends.
struct Workspace {
    dir: PathBuf,
}

impl Workspace {
    fn new() -> Result<Self, std::io::Error> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "artifree-one-shot-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sessions"))?;
        Ok(Self { dir })
    }

    /// The directory that holds session directories.
    fn root(&self) -> PathBuf {
        self.dir.join("sessions")
    }

    /// The names inside the session root.
    fn leftovers(&self) -> Result<Vec<String>, std::io::Error> {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(self.root())? {
            names.push(entry?.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        Ok(names)
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A server process that is killed and reaped when dropped.
struct Server {
    child: Child,
    lines: Receiver<String>,
    stderr: PathBuf,
    ready: Value,
    started: Instant,
}

impl Server {
    /// Starts `serve` on free ports with `extra` arguments and waits for the
    /// readiness line.
    fn start(
        workspace: &Workspace,
        name: &str,
        extra: &[&std::ffi::OsStr],
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let stderr = workspace.dir.join(format!("{name}.stderr"));
        let started = Instant::now();
        let mut child = Command::new(BIN)
            .args([
                "serve",
                "--api-listen",
                "127.0.0.1:0",
                "--content-listen",
                "127.0.0.1:0",
            ])
            .args(extra)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(File::create(&stderr)?))
            .spawn()?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let (sender, lines) = channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let mut server = Self {
            child,
            lines,
            stderr,
            ready: Value::Null,
            started,
        };
        server.ready = match server.lines.recv_timeout(PATIENCE) {
            Ok(line) => serde_json::from_str(&line)?,
            Err(RecvTimeoutError::Timeout) => {
                return Err(format!("no readiness line; stderr: {}", server.diagnostics()).into());
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(format!("exited early; stderr: {}", server.diagnostics()).into());
            }
        };
        Ok(server)
    }

    /// A one-shot session of `ttl` seconds in the workspace's session root.
    fn one_shot(
        workspace: &Workspace,
        name: &str,
        ttl: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let root = workspace.root();
        Self::start(
            workspace,
            name,
            &[
                "--one-shot-ttl-seconds".as_ref(),
                ttl.as_ref(),
                "--session-root".as_ref(),
                root.as_os_str(),
            ],
        )
    }

    fn field(&self, name: &str) -> Result<&str, String> {
        self.ready[name]
            .as_str()
            .ok_or_else(|| format!("readiness line has no {name}: {}", self.ready))
    }

    fn data_dir(&self) -> Result<PathBuf, String> {
        self.field("data_dir").map(PathBuf::from)
    }

    /// The session directory: the parent of the data directory.
    fn session_dir(&self) -> Result<PathBuf, Box<dyn std::error::Error>> {
        Ok(self
            .data_dir()?
            .parent()
            .ok_or("data_dir has no parent")?
            .to_path_buf())
    }

    fn owner_token(&self) -> Result<String, Box<dyn std::error::Error>> {
        Ok(std::fs::read_to_string(self.field("token_file")?)?
            .trim()
            .to_owned())
    }

    fn signal(&self, name: &str) -> TestResult {
        let sent = Command::new("kill")
            .args([format!("-{name}"), self.child.id().to_string()])
            .status()?;
        assert!(sent.success(), "kill -{name} failed");
        Ok(())
    }

    /// Kills the process without any chance to clean up, and reaps it.
    fn kill(&mut self) -> TestResult {
        self.child.kill()?;
        self.child.wait()?;
        Ok(())
    }

    fn wait_exit(&mut self) -> Result<ExitStatus, Box<dyn std::error::Error>> {
        let deadline = Instant::now() + PATIENCE;
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err("the server did not exit within the time limit".into());
            }
            thread::sleep(STEP);
        }
    }

    fn diagnostics(&self) -> String {
        std::fs::read_to_string(&self.stderr).unwrap_or_default()
    }

    /// Ends the server with `signal` and waits for a clean exit.
    fn end_with(&mut self, signal: &str) -> TestResult {
        self.signal(signal)?;
        let status = self.wait_exit()?;
        assert!(status.success(), "{status}: {}", self.diagnostics());
        Ok(())
    }

    fn api(&self) -> Result<&str, String> {
        self.field("api_url")
    }

    fn call(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        key: Option<&str>,
        body: Option<&Value>,
    ) -> Result<(u16, Value), Box<dyn std::error::Error>> {
        http(self.api()?, method, path, token, key, body)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One HTTP/1.1 request to `base` (`http://host:port`), over a connection that
/// closes after the reply. The body of the reply is parsed as JSON when it is.
fn http(
    base: &str,
    method: &str,
    path: &str,
    token: Option<&str>,
    key: Option<&str>,
    body: Option<&Value>,
) -> Result<(u16, Value), Box<dyn std::error::Error>> {
    let authority = base.strip_prefix("http://").ok_or("not an http url")?;
    let mut stream = TcpStream::connect(authority)?;
    stream.set_read_timeout(Some(Duration::from_secs(20)))?;
    let payload = body.map(Value::to_string).unwrap_or_default();
    let mut head = vec![
        format!("{method} {path} HTTP/1.1"),
        format!("Host: {authority}"),
        "Connection: close".to_owned(),
    ];
    if let Some(token) = token {
        head.push(format!("Authorization: Bearer {token}"));
    }
    if let Some(key) = key {
        head.push(format!("Idempotency-Key: {key}"));
    }
    if body.is_some() {
        head.push("Content-Type: application/json".to_owned());
        head.push(format!("Content-Length: {}", payload.len()));
    }
    stream.write_all(format!("{}\r\n\r\n{payload}", head.join("\r\n")).as_bytes())?;
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply)?;
    let text = String::from_utf8_lossy(&reply);
    let status = text
        .split_whitespace()
        .nth(1)
        .ok_or("no status line")?
        .parse()?;
    let body = text.split_once("\r\n\r\n").map_or("", |(_, body)| body);
    Ok((status, serde_json::from_str(body).unwrap_or(Value::Null)))
}

/// What a server that exited by itself printed.
struct Exited {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: String,
}

/// Runs the server with `args` and the extra environment `env`, and returns how
/// it ended. A server that starts instead of exiting is killed after a short
/// wait and reported.
fn run_to_exit(
    args: &[&std::ffi::OsStr],
    env: &[(&str, &std::ffi::OsStr)],
) -> Result<Exited, Box<dyn std::error::Error>> {
    let mut child = Command::new(BIN)
        .args(args)
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait()? {
            let mut stdout = Vec::new();
            if let Some(mut pipe) = child.stdout.take() {
                pipe.read_to_end(&mut stdout)?;
            }
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                pipe.read_to_string(&mut stderr)?;
            }
            return Ok(Exited {
                status,
                stdout,
                stderr,
            });
        }
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            return Err("the server kept running instead of refusing the options".into());
        }
        thread::sleep(STEP);
    }
}

fn mode_of(path: &Path) -> Result<u32, std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    Ok(std::fs::metadata(path)?.permissions().mode() & 0o777)
}

// --- tests -----------------------------------------------------------------

#[test]
fn a_one_shot_session_is_private_marked_and_reports_its_mode() -> TestResult {
    let workspace = Workspace::new()?;
    let mut session = Server::one_shot(&workspace, "a", LONG)?;
    assert_eq!(session.ready["mode"], "one_shot");
    assert_eq!(session.ready["session_ttl_seconds"], 600);
    assert_eq!(session.ready["stale_sessions_removed"], 0);
    let dir = session.session_dir()?;
    assert_eq!(dir.parent(), Some(workspace.root().as_path()));
    assert_eq!(mode_of(&dir)?, 0o700, "the session directory");
    assert_eq!(mode_of(&session.data_dir()?)?, 0o700, "the data directory");
    assert!(dir.join(".artifree-session").is_file(), "the owner marker");
    assert!(std::path::Path::new(session.field("token_file")?).starts_with(session.data_dir()?));
    assert!(
        session
            .diagnostics()
            .contains("deleted when the server stops"),
        "a person is told on stderr that nothing is kept: {}",
        session.diagnostics()
    );
    session.end_with("TERM")
}

#[test]
fn sigterm_ends_a_one_shot_session_and_removes_its_data() -> TestResult {
    let workspace = Workspace::new()?;
    let mut session = Server::one_shot(&workspace, "a", LONG)?;
    let dir = session.session_dir()?;
    assert!(dir.is_dir());
    session.end_with("TERM")?;
    assert!(!dir.exists(), "{} was left behind", dir.display());
    assert_eq!(workspace.leftovers()?, Vec::<String>::new());
    Ok(())
}

#[test]
fn sigint_ends_a_one_shot_session_and_removes_its_data() -> TestResult {
    let workspace = Workspace::new()?;
    let mut session = Server::one_shot(&workspace, "a", LONG)?;
    let dir = session.session_dir()?;
    session.end_with("INT")?;
    assert!(!dir.exists(), "{} was left behind", dir.display());
    assert_eq!(workspace.leftovers()?, Vec::<String>::new());
    Ok(())
}

#[test]
fn the_session_lifetime_stops_the_server_and_removes_its_data() -> TestResult {
    let workspace = Workspace::new()?;
    let mut session = Server::one_shot(&workspace, "a", "3")?;
    let dir = session.session_dir()?;
    let (status, _) = http(session.api()?, "GET", "/healthz", None, None, None)?;
    assert_eq!(status, 200, "the session serves before its lifetime ends");
    let api = session.api()?.to_owned();

    let exit = session.wait_exit()?;
    assert!(exit.success(), "{exit}: {}", session.diagnostics());
    assert!(
        session.started.elapsed() >= Duration::from_secs(3),
        "it must not stop before its lifetime: {:?}",
        session.started.elapsed()
    );
    assert!(!dir.exists(), "{} was left behind", dir.display());
    assert_eq!(workspace.leftovers()?, Vec::<String>::new());
    assert!(
        http(&api, "GET", "/healthz", None, None, None).is_err(),
        "the API must not answer after the session ended"
    );
    Ok(())
}

#[test]
fn a_killed_session_is_swept_by_the_next_session_only_once_its_owner_is_gone() -> TestResult {
    let workspace = Workspace::new()?;
    let mut first = Server::one_shot(&workspace, "first", LONG)?;
    let first_dir = first.session_dir()?;

    // The owner is alive: a second session leaves its directory alone.
    let mut second = Server::one_shot(&workspace, "second", LONG)?;
    assert_eq!(second.ready["stale_sessions_removed"], 0);
    assert!(first_dir.is_dir(), "a running session must not be swept");

    // The owner is killed and cannot clean up.
    first.kill()?;
    assert!(first_dir.is_dir(), "a killed session leaves its directory");

    let mut third = Server::one_shot(&workspace, "third", LONG)?;
    assert_eq!(third.ready["stale_sessions_removed"], 1);
    assert_eq!(third.ready["stale_sessions_failed"], 0);
    assert!(!first_dir.exists(), "the dead session's data must be swept");
    assert!(second.session_dir()?.is_dir(), "a live session stays");
    assert!(third.session_dir()?.is_dir());

    second.end_with("TERM")?;
    third.end_with("TERM")?;
    assert_eq!(workspace.leftovers()?, Vec::<String>::new());
    Ok(())
}

#[test]
fn a_directory_that_is_not_a_session_is_never_swept() -> TestResult {
    let workspace = Workspace::new()?;
    let unmarked = workspace.root().join("artifree-session-0123456789abcdef");
    std::fs::create_dir(&unmarked)?;
    std::fs::write(unmarked.join("precious"), b"x")?;
    let other = workspace.root().join("notes");
    std::fs::create_dir(&other)?;
    let mut session = Server::one_shot(&workspace, "a", LONG)?;
    assert_eq!(session.ready["stale_sessions_removed"], 0);
    session.end_with("TERM")?;
    assert!(unmarked.join("precious").is_file());
    assert!(other.is_dir());
    Ok(())
}

/// What a session holds that a later session must not inherit.
struct Held {
    job_id: String,
    preview_path: String,
    issued_token: String,
}

/// Gives a session an artifact, a job, a preview link, and an issued token,
/// and shows that each works there.
fn fill(session: &Server, owner: &str) -> Result<Held, Box<dyn std::error::Error>> {
    let page = json!({
        "title": "Old",
        "files": [{ "path": "index.html", "encoding": "utf8", "content": "<h1>old</h1>" }]
    });
    let (status, created) = session.call(
        "POST",
        "/v1/artifacts",
        Some(owner),
        Some("old-create-1"),
        Some(&page),
    )?;
    assert_eq!(status, 201, "{created}");
    let id = created["artifact"]["id"].as_str().ok_or("no id")?;
    let (status, job) = session.call(
        "POST",
        &format!("/v1/artifacts/{id}/jobs"),
        Some(owner),
        Some("old-job-key-1"),
        Some(&json!({ "kind": "verify_revision", "revision": 1 })),
    )?;
    assert_eq!(status, 201, "{job}");
    let (status, preview) = session.call(
        "POST",
        &format!("/v1/artifacts/{id}/revisions/1/previews"),
        Some(owner),
        None,
        Some(&json!({})),
    )?;
    assert_eq!(status, 201, "{preview}");
    let url = preview["url"].as_str().ok_or("no preview url")?;
    let content = session.field("content_origin")?;
    let preview_path = url
        .strip_prefix(content)
        .ok_or("the preview is not on the content origin")?
        .to_owned();
    let (status, issued) = session.call(
        "POST",
        "/v1/tokens",
        Some(owner),
        None,
        Some(&json!({ "name": "agent" })),
    )?;
    assert_eq!(status, 201, "{issued}");
    let held = Held {
        job_id: job["job"]["id"].as_str().ok_or("no job id")?.to_owned(),
        preview_path,
        issued_token: issued["secret"].as_str().ok_or("no secret")?.to_owned(),
    };
    // The control for what the caller checks later: all of it works here.
    assert_eq!(
        http(content, "GET", &held.preview_path, None, None, None)?.0,
        200
    );
    assert_eq!(
        session
            .call("GET", "/v1/artifacts", Some(&held.issued_token), None, None)?
            .0,
        200
    );
    assert_eq!(
        session
            .call(
                "GET",
                &format!("/v1/jobs/{}", held.job_id),
                Some(owner),
                None,
                None
            )?
            .0,
        200
    );
    Ok(held)
}

#[test]
fn a_new_session_starts_empty_and_nothing_of_an_old_one_authenticates() -> TestResult {
    let workspace = Workspace::new()?;
    let mut old = Server::one_shot(&workspace, "old", LONG)?;
    let owner = old.owner_token()?;
    let held = fill(&old, &owner)?;

    // The old process dies with no chance to clean up.
    let old_data = old.data_dir()?;
    old.kill()?;
    assert!(old_data.is_dir());

    let new = Server::one_shot(&workspace, "new", LONG)?;
    assert_eq!(new.ready["stale_sessions_removed"], 1);
    assert!(!old_data.exists(), "the old session's data is gone");
    assert_ne!(new.data_dir()?, old_data);
    let fresh = new.owner_token()?;
    assert_ne!(fresh, owner, "a new session has a new owner token");

    for (what, token) in [
        ("owner token", &owner),
        ("issued token", &held.issued_token),
    ] {
        let (status, body) = new.call("GET", "/v1/artifacts", Some(token), None, None)?;
        assert_eq!(status, 401, "the old {what} must not authenticate: {body}");
    }
    let (status, listed) = new.call("GET", "/v1/artifacts", Some(&fresh), None, None)?;
    assert_eq!(status, 200);
    assert_eq!(listed["artifacts"], json!([]), "a new session starts empty");
    let job_path = format!("/v1/jobs/{}", held.job_id);
    let (status, _) = new.call("GET", &job_path, Some(&fresh), None, None)?;
    assert_eq!(status, 404, "an old job is not resumed");
    let (_, jobs) = new.call("GET", "/v1/jobs", Some(&fresh), None, None)?;
    assert_eq!(jobs["jobs"], json!([]));
    let content = new.field("content_origin")?;
    let (status, _) = http(content, "GET", &held.preview_path, None, None, None)?;
    assert_eq!(status, 404, "an old preview address is not served");
    Ok(())
}

#[test]
fn persistent_mode_keeps_its_data_and_tokens_across_a_restart() -> TestResult {
    let workspace = Workspace::new()?;
    let data = workspace.dir.join("kept");
    let mut first = Server::start(
        &workspace,
        "first",
        &["--data-dir".as_ref(), data.as_os_str()],
    )?;
    assert_eq!(first.ready["mode"], "persistent");
    assert!(first.ready.get("session_ttl_seconds").is_none());
    assert_eq!(first.data_dir()?, data);
    let owner = first.owner_token()?;
    let page = json!({
        "title": "Kept",
        "files": [{ "path": "index.html", "encoding": "utf8", "content": "<h1>kept</h1>" }]
    });
    let (status, _) = first.call(
        "POST",
        "/v1/artifacts",
        Some(&owner),
        Some("kept-create-1"),
        Some(&page),
    )?;
    assert_eq!(status, 201);
    first.end_with("TERM")?;
    assert!(data.join("access-token").is_file(), "persistent data stays");

    let second = Server::start(
        &workspace,
        "second",
        &["--data-dir".as_ref(), data.as_os_str()],
    )?;
    assert_eq!(second.owner_token()?, owner);
    let (status, listed) = second.call("GET", "/v1/artifacts", Some(&owner), None, None)?;
    assert_eq!(status, 200);
    assert_eq!(listed["artifacts"].as_array().map(Vec::len), Some(1));
    Ok(())
}

#[test]
fn contradictory_storage_options_are_refused_before_anything_is_created() -> TestResult {
    let workspace = Workspace::new()?;
    let kept = workspace.dir.join("kept");
    let root = workspace.root();
    let cases: [(&str, Vec<&std::ffi::OsStr>); 3] = [
        (
            "both",
            vec![
                "serve".as_ref(),
                "--data-dir".as_ref(),
                kept.as_os_str(),
                "--one-shot-ttl-seconds".as_ref(),
                "5".as_ref(),
            ],
        ),
        (
            "root without a lifetime",
            vec![
                "serve".as_ref(),
                "--data-dir".as_ref(),
                kept.as_os_str(),
                "--session-root".as_ref(),
                root.as_os_str(),
            ],
        ),
        (
            "no storage at all",
            vec![
                "serve".as_ref(),
                "--session-root".as_ref(),
                root.as_os_str(),
            ],
        ),
    ];
    for (name, args) in cases {
        let exited = run_to_exit(&args, &[]).map_err(|error| format!("{name}: {error}"))?;
        assert!(!exited.status.success(), "{name} must be refused");
        assert!(exited.stdout.is_empty(), "{name}: no readiness line");
    }
    assert!(!kept.exists(), "a refused start creates no data directory");
    assert_eq!(workspace.leftovers()?, Vec::<String>::new());
    Ok(())
}

#[test]
fn a_client_that_never_finishes_a_request_cannot_keep_the_data_alive() -> TestResult {
    let workspace = Workspace::new()?;
    let mut session = Server::one_shot(&workspace, "a", "2")?;
    let dir = session.session_dir()?;
    // The request line and a header arrive, the end of the headers never does.
    let authority = session
        .api()?
        .strip_prefix("http://")
        .ok_or("not an http url")?
        .to_owned();
    let mut stalled = TcpStream::connect(&authority)?;
    stalled.write_all(format!("GET /healthz HTTP/1.1\r\nHost: {authority}\r\n").as_bytes())?;

    let exit = session.wait_exit()?;
    assert!(exit.success(), "{exit}: {}", session.diagnostics());
    assert!(!dir.exists(), "{} was left behind", dir.display());
    drop(stalled);
    Ok(())
}

#[test]
fn a_temporary_directory_that_is_not_utf8_is_refused_before_serving_and_leaves_nothing()
-> TestResult {
    use std::os::unix::ffi::OsStrExt;
    let workspace = Workspace::new()?;
    // The default session root is the system temporary directory, which the
    // environment can set to bytes that are not text.
    let tmp = workspace
        .dir
        .join(std::ffi::OsStr::from_bytes(b"tmp-\xff\xfe"));
    std::fs::create_dir(&tmp)?;
    let args: [&std::ffi::OsStr; 7] = [
        "serve".as_ref(),
        "--api-listen".as_ref(),
        "127.0.0.1:0".as_ref(),
        "--content-listen".as_ref(),
        "127.0.0.1:0".as_ref(),
        "--one-shot-ttl-seconds".as_ref(),
        LONG.as_ref(),
    ];

    let exited = run_to_exit(&args, &[("TMPDIR", tmp.as_os_str())])?;

    assert_eq!(
        exited.status.code(),
        Some(1),
        "a refusal exits 1; a panic would be 101: {}",
        exited.stderr
    );
    assert!(exited.stdout.is_empty(), "no readiness line was printed");
    assert!(
        exited.stderr.contains("not valid UTF-8") && !exited.stderr.contains("panicked"),
        "a clear error and no panic: {}",
        exited.stderr
    );
    let left: Vec<_> = std::fs::read_dir(&tmp)?.collect::<Result<_, _>>()?;
    assert!(
        left.is_empty(),
        "the session directory must be removed: {left:?}"
    );
    Ok(())
}
