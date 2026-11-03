#![cfg(unix)]
//! The one-shot session as a real process (decision 0039): what `serve
//! --one-shot-ttl-seconds` creates, what it deletes and when, and what it
//! refuses to touch.
//!
//! Every wait observes a condition (a line on stdout, a file the start-up seam
//! creates, an exit status) with an upper bound; nothing sleeps for a guessed
//! time. A child's stdout and stderr are drained on threads from the moment it
//! starts, so a chatty child can never block on a full pipe, and a child is
//! killed and reaped when its guard is dropped.

use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const BIN: &str = env!("CARGO_BIN_EXE_artifree-server");
const PATIENCE: Duration = Duration::from_secs(40);
const STEP: Duration = Duration::from_millis(20);
const HOLD_VARIABLE: &str = "ARTIFREE_TEST_HOLD_STARTUP";

/// A scratch directory that is removed afterwards, also when a test fails.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Result<Self, Box<dyn std::error::Error>> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "artifree-oneshot-{}-{}-{name}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    /// A session root inside the scratch directory.
    fn root(&self) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let root = self.join("root");
        fs::create_dir_all(&root)?;
        Ok(root)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Names of the entries of `directory`, sorted.
fn names(directory: &Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(directory)? {
        names.push(entry?.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    Ok(names)
}

fn mode(path: &Path) -> Result<u32, Box<dyn std::error::Error>> {
    Ok(fs::metadata(path)?.permissions().mode() & 0o777)
}

/// A running `artifree-server serve`.
struct Server {
    child: Child,
    lines: Receiver<String>,
    stderr: Option<JoinHandle<String>>,
}

impl Server {
    /// A one-shot session in `root` with the given lifetime, on free ports.
    fn one_shot(root: &Path, ttl: &str) -> Result<Self, Box<dyn std::error::Error>> {
        Self::start(
            &[
                "--one-shot-ttl-seconds",
                ttl,
                "--session-root",
                &root.to_string_lossy(),
            ],
            &[],
        )
    }

    fn start(args: &[&str], env: &[(&str, OsString)]) -> Result<Self, Box<dyn std::error::Error>> {
        Self::start_with_stdout(args, env, Stdio::piped())
    }

    fn start_with_stdout(
        args: &[&str],
        env: &[(&str, OsString)],
        stdout: Stdio,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut command = Command::new(BIN);
        command
            .args(["serve", "--api-listen", "127.0.0.1:0"])
            .args(["--content-listen", "127.0.0.1:0"])
            .args(args)
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(Stdio::piped());
        for (name, value) in env {
            command.env(name, value);
        }
        let mut child = command.spawn()?;
        let (sender, lines) = channel();
        if let Some(out) = child.stdout.take() {
            thread::spawn(move || {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    if sender.send(line).is_err() {
                        break;
                    }
                }
            });
        }
        let err = child.stderr.take().ok_or("stderr is piped")?;
        let stderr = thread::spawn(move || {
            let mut text = String::new();
            let _ = BufReader::new(err).read_to_string(&mut text);
            text
        });
        Ok(Self {
            child,
            lines,
            stderr: Some(stderr),
        })
    }

    /// The readiness line, parsed.
    fn ready(&self) -> Result<Value, Box<dyn std::error::Error>> {
        let line = self
            .lines
            .recv_timeout(PATIENCE)
            .map_err(|error| format!("no readiness line: {error}"))?;
        Ok(serde_json::from_str(&line)?)
    }

    /// Every remaining line of stdout up to the end of the stream.
    fn rest_of_stdout(&self) -> Vec<String> {
        let mut lines = Vec::new();
        while let Ok(line) = self.lines.recv_timeout(PATIENCE) {
            lines.push(line);
        }
        lines
    }

    fn signal(&self, name: &str) -> TestResult {
        let status = Command::new("kill")
            .arg(format!("-{name}"))
            .arg(self.child.id().to_string())
            .status()?;
        assert!(status.success(), "kill -{name} failed");
        Ok(())
    }

    /// Waits for the process to end on its own, at most `limit`.
    fn wait_within(&mut self, limit: Duration) -> Result<ExitStatus, Box<dyn std::error::Error>> {
        let give_up = Instant::now() + limit;
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= give_up {
                return Err(format!("the server did not exit within {limit:?}").into());
            }
            thread::sleep(STEP);
        }
    }

    fn wait(&mut self) -> Result<ExitStatus, Box<dyn std::error::Error>> {
        self.wait_within(PATIENCE)
    }

    /// Everything the process wrote to stderr; call after it exited.
    fn stderr(&mut self) -> String {
        self.stderr
            .take()
            .and_then(|reader| reader.join().ok())
            .unwrap_or_default()
    }

    /// Sends `name`, waits for the exit, and requires a clean one.
    fn stop_cleanly(&mut self, name: &str) -> TestResult {
        self.signal(name)?;
        let status = self.wait()?;
        let stderr = self.stderr();
        assert!(status.success(), "{status:?}: {stderr}");
        Ok(())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Waits until `path` exists, at most [`PATIENCE`].
fn wait_for_file(path: &Path) -> TestResult {
    let give_up = Instant::now() + PATIENCE;
    while !path.exists() {
        if Instant::now() >= give_up {
            return Err(format!("{} never appeared", path.display()).into());
        }
        thread::sleep(STEP);
    }
    Ok(())
}

fn data_dir_of(ready: &Value) -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(PathBuf::from(ready["data_dir"].as_str().ok_or("data_dir")?))
}

fn token_of(ready: &Value) -> Result<String, Box<dyn std::error::Error>> {
    let file = ready["token_file"].as_str().ok_or("token_file")?;
    Ok(fs::read_to_string(file)?.trim().to_owned())
}

/// One HTTP/1.1 request on a fresh connection; the status and the body.
fn http(
    base: &str,
    method: &str,
    path: &str,
    token: Option<&str>,
    key: Option<&str>,
    body: Option<&Value>,
) -> Result<(u16, String), Box<dyn std::error::Error>> {
    let authority = base.strip_prefix("http://").ok_or("an http base URL")?;
    let mut stream = TcpStream::connect(authority)?;
    stream.set_read_timeout(Some(Duration::from_secs(20)))?;
    let body = body.map(Value::to_string).unwrap_or_default();
    let authorization = token
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    let idempotency = key
        .map(|key| format!("Idempotency-Key: {key}\r\n"))
        .unwrap_or_default();
    let framing = if method == "POST" {
        format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        )
    } else {
        String::new()
    };
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n{authorization}{idempotency}{framing}\r\n{body}"
    );
    stream.write_all(request.as_bytes())?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    let status = reply
        .split_whitespace()
        .nth(1)
        .ok_or("no status line")?
        .parse()?;
    let body = reply.split_once("\r\n\r\n").map_or("", |(_, body)| body);
    Ok((status, body.to_owned()))
}

fn json_body(body: &str) -> Result<Value, serde_json::Error> {
    serde_json::from_str(body)
}

fn page() -> Value {
    json!({"title": "Demo", "files": [
        {"path": "index.html", "encoding": "utf8", "content": "<h1>one</h1>"}
    ]})
}

// ---------------------------------------------------------------------------
// The directory, its modes, and what a person is told.

#[test]
fn a_session_runs_in_a_new_private_directory_and_says_so() -> TestResult {
    let scratch = Scratch::new("private")?;
    let root = scratch.root()?;
    let mut server = Server::one_shot(&root, "600")?;
    let ready = server.ready()?;

    assert_eq!(ready["event"], "ready");
    assert_eq!(ready["mode"], "one_shot");
    assert_eq!(ready["session_ttl_seconds"], 600);
    assert_eq!(ready["sweep"]["removed"], 0);
    let data_dir = data_dir_of(&ready)?;
    assert_eq!(data_dir.parent(), Some(root.as_path()));
    let name = data_dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("name")?;
    assert!(name.starts_with("artifree-session-"), "{name}");
    assert_eq!(mode(&data_dir)?, 0o700, "the session directory is private");
    let token_file = PathBuf::from(ready["token_file"].as_str().ok_or("token_file")?);
    assert_eq!(token_file, data_dir.join("access-token"));
    assert_eq!(mode(&token_file)?, 0o600);
    assert_eq!(mode(&data_dir.join(".artifree-session"))?, 0o600);
    let marker = fs::read_to_string(data_dir.join(".artifree-session"))?;
    assert!(
        marker.starts_with(&format!("artifree-session v1\npid {}\n", server.child.id())),
        "{marker:?}"
    );
    assert_eq!(names(&root)?, [name.to_owned()], "exactly one directory");

    server.stop_cleanly("TERM")?;
    Ok(())
}

#[test]
fn stderr_tells_a_person_that_nothing_is_kept_and_that_a_tunnel_is_separate() -> TestResult {
    let scratch = Scratch::new("notice")?;
    let root = scratch.root()?;
    let mut server = Server::one_shot(&root, "600")?;
    let ready = server.ready()?;
    let data_dir = data_dir_of(&ready)?;
    server.signal("TERM")?;
    let status = server.wait()?;
    let stderr = server.stderr();
    assert!(status.success(), "{stderr}");
    assert!(stderr.contains("nothing is kept"), "{stderr}");
    assert!(
        stderr.contains(&data_dir.to_string_lossy().into_owned()),
        "{stderr}"
    );
    assert!(stderr.contains("600 seconds"), "{stderr}");
    assert!(
        stderr.contains("tunnel") && stderr.contains("does not implement"),
        "{stderr}"
    );
    assert!(stderr.contains("data directory was deleted"), "{stderr}");
    assert!(
        server.rest_of_stdout().is_empty(),
        "stdout holds only the readiness line"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Every way out removes the directory.

#[test]
fn sigterm_and_sigint_remove_the_directory_and_exit_cleanly() -> TestResult {
    for signal in ["TERM", "INT"] {
        let scratch = Scratch::new("signals")?;
        let root = scratch.root()?;
        let mut server = Server::one_shot(&root, "600")?;
        let ready = server.ready()?;
        let data_dir = data_dir_of(&ready)?;
        assert!(data_dir.join("access-token").exists());
        // The signal comes right after the readiness line.
        server.stop_cleanly(signal)?;
        assert!(
            !data_dir.exists(),
            "SIG{signal} left {}",
            data_dir.display()
        );
        assert_eq!(names(&root)?, Vec::<String>::new(), "SIG{signal}");
    }
    Ok(())
}

#[test]
fn the_lifetime_ends_the_session_and_removes_its_data() -> TestResult {
    let scratch = Scratch::new("ttl")?;
    let root = scratch.root()?;
    let mut server = Server::one_shot(&root, "2")?;
    // No signal is sent: the process ends by itself.
    let status = server.wait()?;
    let stderr = server.stderr();
    assert!(status.success(), "{status:?}: {stderr}");
    assert_eq!(names(&root)?, Vec::<String>::new());
    let lines = server.rest_of_stdout();
    // A start slower than the lifetime ends it before readiness, also cleanly.
    if let Some(line) = lines.first() {
        let ready: Value = serde_json::from_str(line)?;
        assert_eq!(ready["session_ttl_seconds"], 2);
    }
    Ok(())
}

#[test]
fn a_stalled_client_cannot_keep_the_data_alive() -> TestResult {
    let scratch = Scratch::new("stalled")?;
    let root = scratch.root()?;
    let mut server = Server::one_shot(&root, "600")?;
    let ready = server.ready()?;
    let api = ready["api_url"].as_str().ok_or("api_url")?.to_owned();
    let authority = api.strip_prefix("http://").ok_or("http")?;
    // A request that promises 100 bytes, sends 7, and then says nothing more.
    let mut stalled = TcpStream::connect(authority)?;
    stalled.write_all(
        format!(
            "POST /v1/artifacts HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{{\"title\""
        )
        .as_bytes(),
    )?;
    // And one that never finishes its request line.
    let mut half = TcpStream::connect(authority)?;
    half.write_all(b"GET /healthz HT")?;
    // The server has accepted both: this request is answered after them.
    let (status, _) = http(&api, "GET", "/healthz", None, None, None)?;
    assert_eq!(status, 200);

    server.signal("TERM")?;
    // The drain is five seconds; the bound here is far above it.
    let status = server.wait_within(Duration::from_secs(30))?;
    let stderr = server.stderr();
    assert!(status.success(), "{status:?}: {stderr}");
    assert_eq!(names(&root)?, Vec::<String>::new());
    drop((stalled, half));
    Ok(())
}

#[test]
fn a_signal_at_each_start_up_step_still_removes_the_directory() -> TestResult {
    for (stage, directory_exists) in [
        ("before-directory", false),
        ("after-directory", true),
        ("after-database", true),
    ] {
        let scratch = Scratch::new("startup")?;
        let root = scratch.root()?;
        let held = scratch.join("held");
        let spec = OsString::from(format!("{stage}={}", held.display()));
        let mut server = Server::start(
            &[
                "--one-shot-ttl-seconds",
                "600",
                "--session-root",
                &root.to_string_lossy(),
            ],
            &[(HOLD_VARIABLE, spec)],
        )?;
        wait_for_file(&held)?;
        assert_eq!(
            names(&root)?.len(),
            usize::from(directory_exists),
            "{stage}: the directory exists exactly after it is created"
        );
        // SIGINT at one step and SIGTERM at the others.
        server.signal(if stage == "after-directory" {
            "INT"
        } else {
            "TERM"
        })?;
        let status = server.wait()?;
        let stderr = server.stderr();
        assert!(status.success(), "{stage}: {status:?}: {stderr}");
        assert_eq!(names(&root)?, Vec::<String>::new(), "{stage}");
        assert!(
            server.rest_of_stdout().is_empty(),
            "{stage}: a stopped start-up prints no readiness line"
        );
    }
    Ok(())
}

#[test]
fn a_start_up_failure_removes_the_directory_and_fails() -> TestResult {
    let scratch = Scratch::new("bind")?;
    let root = scratch.root()?;
    let taken = TcpListener::bind("127.0.0.1:0")?;
    let address = taken.local_addr()?.to_string();
    let mut server = Server::start(
        &[
            "--one-shot-ttl-seconds",
            "600",
            "--session-root",
            &root.to_string_lossy(),
            "--api-listen",
            &address,
        ],
        &[],
    )?;
    let status = server.wait()?;
    let stderr = server.stderr();
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("cannot listen on"), "{stderr}");
    assert_eq!(names(&root)?, Vec::<String>::new());
    assert!(server.rest_of_stdout().is_empty());
    Ok(())
}

// ---------------------------------------------------------------------------
// The readiness line never panics.

#[test]
fn a_session_root_that_is_not_utf8_fails_cleanly_and_leaves_nothing() -> TestResult {
    let scratch = Scratch::new("utf8")?;
    let bad = scratch.0.join(OsString::from_vec(b"tmp-\xff\xfe".to_vec()));
    fs::create_dir(&bad)?;
    let mut server = Server::start(
        &["--one-shot-ttl-seconds", "600"],
        &[("TMPDIR", bad.clone().into_os_string())],
    )?;
    let status = server.wait()?;
    let stderr = server.stderr();
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("UTF-8"), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert_eq!(
        names(&bad)?,
        Vec::<String>::new(),
        "no session directory left"
    );
    assert!(server.rest_of_stdout().is_empty());
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn a_standard_output_that_cannot_be_written_fails_and_leaves_nothing() -> TestResult {
    let scratch = Scratch::new("full")?;
    let root = scratch.root()?;
    // Every write to /dev/full fails with "no space left on device".
    let full = fs::OpenOptions::new().write(true).open("/dev/full")?;
    let mut server = Server::start_with_stdout(
        &[
            "--one-shot-ttl-seconds",
            "600",
            "--session-root",
            &root.to_string_lossy(),
        ],
        &[],
        Stdio::from(full),
    )?;
    let status = server.wait()?;
    let stderr = server.stderr();
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("cannot write the readiness line"),
        "{stderr}"
    );
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert_eq!(
        names(&root)?,
        Vec::<String>::new(),
        "the failed start left a directory"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// A new session is empty: nothing carries over.

/// What a session made: the addresses a later session must not know.
struct Made {
    artifact: String,
    job: String,
    preview_path: String,
}

/// Creates an artifact, a job, and a preview in the running session and
/// checks that each address works there.
fn make_things(ready: &Value, token: &str) -> Result<Made, Box<dyn std::error::Error>> {
    let api = ready["api_url"].as_str().ok_or("api_url")?;
    let call = |method: &str, path: &str, key: Option<&str>, body: Option<&Value>| {
        http(api, method, path, Some(token), key, body)
    };
    let (status, body) = call("POST", "/v1/artifacts", Some("create-1"), Some(&page()))?;
    assert_eq!(status, 201, "{body}");
    let artifact = json_body(&body)?["artifact"]["id"]
        .as_str()
        .ok_or("id")?
        .to_owned();
    let (status, body) = call(
        "POST",
        &format!("/v1/artifacts/{artifact}/jobs"),
        Some("job-key-1"),
        Some(&json!({"kind": "verify_revision", "revision": 1})),
    )?;
    assert_eq!(status, 201, "{body}");
    let job = json_body(&body)?["job"]["id"]
        .as_str()
        .ok_or("job id")?
        .to_owned();
    let (status, body) = call(
        "POST",
        &format!("/v1/artifacts/{artifact}/revisions/1/previews"),
        None,
        Some(&json!({})),
    )?;
    assert_eq!(status, 201, "{body}");
    let url = json_body(&body)?["url"].as_str().ok_or("url")?.to_owned();
    let origin = ready["content_origin"].as_str().ok_or("origin")?;
    let preview_path = url.strip_prefix(origin).ok_or("preview origin")?.to_owned();
    assert_eq!(
        call("GET", &format!("/v1/artifacts/{artifact}"), None, None)?.0,
        200
    );
    assert_eq!(call("GET", &format!("/v1/jobs/{job}"), None, None)?.0, 200);
    assert_eq!(http(origin, "GET", &preview_path, None, None, None)?.0, 200);
    Ok(Made {
        artifact,
        job,
        preview_path,
    })
}

#[test]
fn tokens_jobs_and_previews_of_an_earlier_session_do_not_exist_in_the_next() -> TestResult {
    let scratch = Scratch::new("carry")?;
    let root = scratch.root()?;

    let mut first = Server::one_shot(&root, "600")?;
    let ready = first.ready()?;
    let old_token = token_of(&ready)?;
    let made = make_things(&ready, &old_token)?;
    let first_dir = data_dir_of(&ready)?;
    first.stop_cleanly("TERM")?;
    assert!(!first_dir.exists());

    let mut second = Server::one_shot(&root, "600")?;
    let ready = second.ready()?;
    let api = ready["api_url"].as_str().ok_or("api_url")?;
    let new_token = token_of(&ready)?;
    assert_ne!(new_token, old_token, "a session never reuses a token");
    assert_ne!(data_dir_of(&ready)?, first_dir);
    let get = |path: &str, token: &str| http(api, "GET", path, Some(token), None, None);
    assert_eq!(get("/v1/artifacts", &old_token)?.0, 401, "the old token");
    let (status, body) = get("/v1/artifacts", &new_token)?;
    assert_eq!(status, 200);
    assert_eq!(
        json_body(&body)?["artifacts"].as_array().map(Vec::len),
        Some(0),
        "{body}"
    );
    let artifact = format!("/v1/artifacts/{}", made.artifact);
    assert_eq!(get(&artifact, &new_token)?.0, 404, "the old artifact");
    let job = format!("/v1/jobs/{}", made.job);
    assert_eq!(get(&job, &new_token)?.0, 404, "the old job");
    let origin = ready["content_origin"].as_str().ok_or("origin")?;
    let (status, _) = http(origin, "GET", &made.preview_path, None, None, None)?;
    assert_eq!(status, 404, "the old preview address");
    second.stop_cleanly("TERM")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Persistent mode is what it was.

#[test]
fn persistent_mode_keeps_its_data_and_token_and_reports_its_mode() -> TestResult {
    let scratch = Scratch::new("persistent")?;
    let data = scratch.join("kept");
    let args = ["--data-dir", &data.to_string_lossy()].map(str::to_owned);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    let mut first = Server::start(&args, &[])?;
    let ready = first.ready()?;
    assert_eq!(ready["mode"], "persistent");
    assert_eq!(ready["data_dir"], data.to_string_lossy().as_ref());
    assert_eq!(
        ready["token_file"],
        data.join("access-token").to_string_lossy().as_ref()
    );
    let object = ready.as_object().ok_or("object")?;
    for absent in ["session_ttl_seconds", "sweep"] {
        assert!(
            !object.contains_key(absent),
            "{absent} in a persistent line"
        );
    }
    let token = token_of(&ready)?;
    // Persistent mode installs its signal handlers when it starts serving
    // (unchanged since decision 0003): a first answer proves it has.
    let api = ready["api_url"].as_str().ok_or("api_url")?;
    assert_eq!(http(api, "GET", "/healthz", None, None, None)?.0, 200);
    first.signal("TERM")?;
    let status = first.wait()?;
    let stderr = first.stderr();
    assert!(status.success(), "{stderr}");
    assert!(
        !stderr.contains("one-shot") && !stderr.contains("deleted"),
        "{stderr}"
    );
    assert!(
        data.join("access-token").exists(),
        "persistent data is kept"
    );

    let mut second = Server::start(&args, &[])?;
    let ready = second.ready()?;
    assert_eq!(token_of(&ready)?, token, "the same data, the same token");
    let api = ready["api_url"].as_str().ok_or("api_url")?;
    assert_eq!(http(api, "GET", "/healthz", None, None, None)?.0, 200);
    second.stop_cleanly("TERM")?;
    Ok(())
}

#[test]
fn contradictory_options_are_refused_before_anything_is_created() -> TestResult {
    let scratch = Scratch::new("refused")?;
    let root = scratch.root()?;
    let data = scratch.join("data");
    let root_text = root.to_string_lossy().into_owned();
    let data_text = data.to_string_lossy().into_owned();
    let cases: Vec<(Vec<&str>, &str)> = vec![
        (
            vec![
                "--data-dir",
                &data_text,
                "--one-shot-ttl-seconds",
                "5",
                "--session-root",
                &root_text,
            ],
            "cannot be combined",
        ),
        (
            vec!["--data-dir", &data_text, "--session-root", &root_text],
            "--session-root only applies",
        ),
        (
            vec!["--session-root", &root_text],
            "--session-root only applies",
        ),
        (vec![], "serve requires --data-dir"),
        (
            vec!["--one-shot-ttl-seconds", "0", "--session-root", &root_text],
            "from 1 to 604800",
        ),
        (
            vec![
                "--one-shot-ttl-seconds",
                "604801",
                "--session-root",
                &root_text,
            ],
            "from 1 to 604800",
        ),
        (
            vec![
                "--one-shot-ttl-seconds",
                "soon",
                "--session-root",
                &root_text,
            ],
            "from 1 to 604800",
        ),
    ];
    for (args, expected) in cases {
        let mut server = Server::start(&args, &[])?;
        let status = server.wait()?;
        let stderr = server.stderr();
        assert_eq!(status.code(), Some(1), "{args:?}: {stderr}");
        assert!(stderr.contains(expected), "{args:?}: {stderr}");
        assert!(server.rest_of_stdout().is_empty(), "{args:?}");
        assert_eq!(
            names(&root)?,
            Vec::<String>::new(),
            "{args:?} created something"
        );
        assert!(!data.exists(), "{args:?} created the data directory");
    }
    Ok(())
}

#[test]
fn a_session_root_that_does_not_exist_is_refused_and_not_created() -> TestResult {
    let scratch = Scratch::new("missing-root")?;
    let missing = scratch.join("absent");
    let mut server = Server::one_shot(&missing, "600")?;
    let status = server.wait()?;
    let stderr = server.stderr();
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("session root"), "{stderr}");
    assert!(!missing.exists());
    Ok(())
}

// ---------------------------------------------------------------------------
// The sweep of sessions that were killed (Linux: it reads /proc).

/// A `sleep` process the test owns, killed and reaped when dropped.
#[cfg(target_os = "linux")]
struct Sleeper(Child);

#[cfg(target_os = "linux")]
impl Sleeper {
    fn start() -> Result<Self, Box<dyn std::error::Error>> {
        let child = Command::new("sleep")
            .arg("600")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(Self(child))
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }

    /// The start time field of `/proc/<pid>/stat`.
    fn start_time(&self) -> Result<u64, Box<dyn std::error::Error>> {
        let stat = fs::read_to_string(format!("/proc/{}/stat", self.pid()))?;
        let after_name = &stat[stat.rfind(')').ok_or("no name")? + 1..];
        Ok(after_name
            .split_whitespace()
            .nth(19)
            .ok_or("no start field")?
            .parse()?)
    }

    /// Ends the process and returns its pid and start time, now unowned.
    fn kill(mut self) -> Result<(u32, u64), Box<dyn std::error::Error>> {
        let (pid, start) = (self.pid(), self.start_time()?);
        self.0.kill()?;
        self.0.wait()?;
        Ok((pid, start))
    }
}

#[cfg(target_os = "linux")]
impl Drop for Sleeper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(target_os = "linux")]
fn fake_session(
    root: &Path,
    digit: char,
    marker: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let dir = root.join(format!("artifree-session-{}", digit.to_string().repeat(16)));
    fs::create_dir(&dir)?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    fs::write(dir.join(".artifree-session"), marker)?;
    fs::write(dir.join("artifree.sqlite3"), b"data of an earlier session")?;
    Ok(dir)
}

#[cfg(target_os = "linux")]
fn marker(pid: u32, start: Option<u64>) -> String {
    match start {
        Some(start) => format!("artifree-session v1\npid {pid}\nstart {start}\n"),
        None => format!("artifree-session v1\npid {pid}\n"),
    }
}

#[cfg(target_os = "linux")]
#[test]
fn the_next_start_removes_the_directory_of_a_dead_owner_and_keeps_a_live_one() -> TestResult {
    let scratch = Scratch::new("sweep")?;
    let root = scratch.root()?;

    let live = Sleeper::start()?;
    let doomed = Sleeper::start()?;
    let reused = Sleeper::start()?;
    let live_dir = fake_session(&root, '1', &marker(live.pid(), Some(live.start_time()?)))?;
    let live_unnoted = fake_session(&root, '2', &marker(live.pid(), None))?;
    let reused_dir = fake_session(
        &root,
        '3',
        &marker(reused.pid(), Some(reused.start_time()? + 1)),
    )?;
    let (dead_pid, dead_start) = doomed.kill()?;
    let dead_dir = fake_session(&root, '4', &marker(dead_pid, Some(dead_start)))?;
    let dead_unnoted = fake_session(&root, '5', &marker(dead_pid, None))?;

    // Things that only look like sessions, each with a dead owner's marker.
    let dead_marker = marker(dead_pid, Some(dead_start));
    let precious = scratch.join("precious");
    fs::create_dir(&precious)?;
    fs::write(precious.join(".artifree-session"), &dead_marker)?;
    fs::write(precious.join("keep.txt"), b"keep")?;
    let link = root.join(format!("artifree-session-{}", "a".repeat(16)));
    symlink(&precious, &link)?;
    let wrong_prefix = root.join(format!("artifree-sessions-{}", "b".repeat(16)));
    fs::create_dir(&wrong_prefix)?;
    fs::write(wrong_prefix.join(".artifree-session"), &dead_marker)?;
    let extra_line = fake_session(&root, 'c', &format!("{dead_marker}\n"))?;
    let other_version = fake_session(&root, 'd', &dead_marker.replace("v1", "v2"))?;
    let no_marker = fake_session(&root, 'e', "")?;
    fs::remove_file(no_marker.join(".artifree-session"))?;
    let upper = root.join(format!("artifree-session-{}", "F".repeat(16)));
    fs::create_dir(&upper)?;
    fs::write(upper.join(".artifree-session"), &dead_marker)?;
    let plain_file = root.join(format!("artifree-session-{}", "9".repeat(16)));
    fs::write(&plain_file, &dead_marker)?;

    let mut server = Server::one_shot(&root, "600")?;
    let ready = server.ready()?;
    let own = data_dir_of(&ready)?;

    assert!(
        !dead_dir.exists(),
        "a dead owner with a matching start time"
    );
    assert!(!dead_unnoted.exists(), "a dead owner without a start time");
    assert!(
        !reused_dir.exists(),
        "a pid that now belongs to a later process"
    );
    for (name, kept) in [
        ("live owner", &live_dir),
        ("live owner, no start time", &live_unnoted),
        ("symbolic link", &link),
        ("wrong prefix", &wrong_prefix),
        ("extra line in the marker", &extra_line),
        ("other marker version", &other_version),
        ("no marker", &no_marker),
        ("uppercase digits", &upper),
        ("plain file", &plain_file),
    ] {
        assert!(
            fs::symlink_metadata(kept).is_ok(),
            "{name} was swept: {}",
            kept.display()
        );
    }
    assert!(
        live_dir.join("artifree.sqlite3").exists(),
        "its data is intact"
    );
    assert!(
        precious.join("keep.txt").exists(),
        "a link is never followed"
    );
    assert!(own.exists());
    assert_eq!(ready["sweep"]["performed"], true);
    assert_eq!(ready["sweep"]["removed"], 3);
    // Prefix-named entries that stayed: live x2, link, extra line, other
    // version, no marker, uppercase, plain file (the wrong prefix is not counted).
    assert_eq!(ready["sweep"]["kept"], 8);
    assert_eq!(ready["sweep"]["failed"], 0);
    server.stop_cleanly("TERM")?;
    assert!(live_dir.exists(), "stopping a session sweeps nothing");
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn a_session_that_was_killed_is_swept_by_the_next_start_but_not_before() -> TestResult {
    let scratch = Scratch::new("killed")?;
    let root = scratch.root()?;

    let mut first = Server::one_shot(&root, "600")?;
    let ready = first.ready()?;
    let left = data_dir_of(&ready)?;
    first.signal("KILL")?;
    let status = first.wait()?;
    assert!(!status.success());
    assert!(left.exists(), "a killed process cannot clean up");

    let mut second = Server::one_shot(&root, "600")?;
    let ready = second.ready()?;
    assert!(!left.exists(), "the next start sweeps the dead session");
    assert_eq!(ready["sweep"]["removed"], 1);
    // A third session starting while the second runs must not touch it.
    let running = data_dir_of(&ready)?;
    let mut third = Server::one_shot(&root, "600")?;
    let third_ready = third.ready()?;
    assert!(
        running.join("access-token").exists(),
        "a live session is not swept"
    );
    assert_eq!(third_ready["sweep"]["removed"], 0);
    assert_eq!(third_ready["sweep"]["kept"], 1);
    third.stop_cleanly("TERM")?;
    second.stop_cleanly("TERM")?;
    Ok(())
}
