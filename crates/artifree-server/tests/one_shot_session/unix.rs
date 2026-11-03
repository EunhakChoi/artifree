//! The Unix checks of one-shot sessions: signals, modes, the sweep, stopping a
//! real process at an exact startup stage, and what does not carry over.
//!
//! Tests that hold the process at a stage use the debug-only seam of decision
//! 0039 and skip, saying so, in a build without debug assertions.

use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use artifree_server::session_sweep::{MARKER_FILE, marker_text};
use serde_json::{Value, json};

use super::support::{PATIENCE, Root, Server, TestResult, data_dir_of};

const HOLD: &str = "ARTIFREE_TEST_HOLD_AT";
const PREFIX: &str = "artifree-session-";

fn skip(reason: &str) {
    writeln!(std::io::stderr(), "skipped: {reason}").ok();
}

/// Whether the debug-only seam exists in this build.
fn seam_available() -> bool {
    if !cfg!(debug_assertions) {
        skip("the hold seam is compiled out of release builds");
    }
    cfg!(debug_assertions)
}

fn send(server: &Server, signal: &str) -> TestResult {
    let status = Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(server.child.id().to_string())
        .status()?;
    assert!(status.success(), "kill -{signal} failed");
    Ok(())
}

fn one_shot_args<'a>(root: &'a Root, ttl: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec![
        "serve",
        "--one-shot-ttl-seconds",
        ttl,
        "--session-root",
        root.arg(),
        "--api-listen",
        "127.0.0.1:0",
        "--content-listen",
        "127.0.0.1:0",
    ];
    args.extend_from_slice(extra);
    args
}

fn start(root: &Root, ttl: &str) -> Result<Server, std::io::Error> {
    Server::start(&one_shot_args(root, ttl, &[]), &[])
}

/// A directory that looks like a session of `owner`'s pid.
fn session_dir(root: &Root, suffix: &str, marker: &str) -> Result<PathBuf, std::io::Error> {
    let dir = root.path().join(format!("{PREFIX}{suffix}"));
    fs::create_dir(&dir)?;
    fs::write(dir.join(MARKER_FILE), marker)?;
    Ok(dir)
}

/// The pid of a process that has exited and been reaped.
fn dead_pid() -> Result<u32, std::io::Error> {
    let mut child = Command::new("true").spawn()?;
    let pid = child.id();
    child.wait()?;
    Ok(pid)
}

/// A running process that is not the server, killed when dropped.
struct Sleeper(Child);

impl Sleeper {
    fn new() -> Result<Self, std::io::Error> {
        Ok(Self(
            Command::new("sleep")
                .arg("120")
                .stdout(Stdio::null())
                .spawn()?,
        ))
    }
}

impl Drop for Sleeper {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

#[test]
fn directory_token_and_marker_are_private() -> TestResult {
    let root = Root::new()?;
    let mut server = start(&root, "120")?;
    let ready = server.ready()?;
    let data_dir = data_dir_of(&ready)?;
    let mode = |path: &Path| fs::metadata(path).map(|meta| meta.permissions().mode() & 0o777);

    let text = data_dir.to_str().ok_or("utf-8 path")?;
    let name = text.rsplit('/').next().ok_or("no name")?;
    assert_eq!(text, format!("{}/{name}", root.arg()));
    assert!(
        name.starts_with(PREFIX) && name.len() == PREFIX.len() + 32,
        "{name}"
    );
    assert_eq!(ready["token_file"], format!("{text}/access-token"));
    assert_eq!(mode(&data_dir)?, 0o700);
    assert_eq!(mode(&data_dir.join("access-token"))?, 0o600);
    assert_eq!(mode(&data_dir.join(MARKER_FILE))?, 0o600);

    let marker = fs::read_to_string(data_dir.join(MARKER_FILE))?;
    let lines: Vec<&str> = marker.lines().collect();
    assert_eq!(lines[0], "artifree-session 1");
    assert_eq!(lines[1], format!("pid {}", server.child.id()));
    assert!(lines.len() <= 3 && marker.ends_with('\n'), "{marker:?}");
    if cfg!(target_os = "linux") {
        // The start time is field 22 of /proc/<pid>/stat: the 20th word after
        // the ") " that ends the command name.
        let proc_stat = fs::read_to_string(format!("/proc/{}/stat", server.child.id()))?;
        let (_, after) = proc_stat.rsplit_once(") ").ok_or("no command name")?;
        let began = after.split(' ').nth(19).ok_or("no start time")?;
        assert_eq!(
            lines.get(2).copied(),
            Some(format!("start {began}").as_str())
        );
    }
    server.wait_stderr_contains("nothing is kept")?;
    for signal in ["SIGINT", "SIGTERM", "SIGHUP"] {
        assert!(server.stderr().contains(signal), "{}", server.stderr());
    }
    assert!(server.stderr().contains("no tunnel is implemented"));
    send(&server, "TERM")?;
    assert!(server.wait_exit()?.success());
    Ok(())
}

#[test]
fn every_stop_signal_ends_the_session_and_removes_the_directory() -> TestResult {
    for signal in ["TERM", "INT", "HUP"] {
        let root = Root::new()?;
        let mut server = start(&root, "120")?;
        let data_dir = data_dir_of(&server.ready()?)?;
        assert!(data_dir.is_dir());
        send(&server, signal)?;
        let status = server.wait_exit()?;
        assert_eq!(status.code(), Some(0), "SIG{signal} is a normal end");
        assert!(
            !data_dir.exists(),
            "SIG{signal} left {}",
            data_dir.display()
        );
        assert_eq!(root.entries()?, 0, "SIG{signal}");
        server.wait_stderr_contains("session ended")?;
    }
    Ok(())
}

#[test]
fn a_stalled_client_does_not_keep_the_data_alive() -> TestResult {
    let root = Root::new()?;
    let mut server = start(&root, "120")?;
    let ready = server.ready()?;
    let data_dir = data_dir_of(&ready)?;
    let address = ready["api_url"]
        .as_str()
        .and_then(|url| url.strip_prefix("http://"))
        .ok_or("api_url")?
        .to_owned();
    // A request whose body never arrives: the handler is waiting for bytes
    // that do not come, so a graceful shutdown would wait for it for ever.
    let token = token_of(&ready)?;
    let mut stalled = TcpStream::connect(&address)?;
    write!(
        stalled,
        "POST /v1/artifacts HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\n\
         Idempotency-Key: stalled-key-1\r\nContent-Type: application/json\r\n\
         Content-Length: 5000\r\n\r\n{{\"title\":"
    )?;
    stalled.flush()?;
    // The listener takes connections in order: once this one is answered, the
    // stalled one has been accepted and is being read.
    let api_url = text(&ready, "api_url")?;
    assert_eq!(
        http(api_url, "GET", "/healthz", None, None, None)?.status,
        200
    );
    let begun = Instant::now();
    send(&server, "TERM")?;
    let status = server.wait_exit()?;
    assert!(status.success(), "{status:?}");
    assert!(
        begun.elapsed() < Duration::from_secs(30),
        "the drain is bounded, took {:?}",
        begun.elapsed()
    );
    assert!(!data_dir.exists(), "the stalled client kept the data");
    drop(stalled);
    Ok(())
}

#[test]
fn stopping_at_every_startup_stage_leaves_nothing_and_exits_cleanly() -> TestResult {
    if !seam_available() {
        return Ok(());
    }
    let stages = [
        "create-dir",
        "sweep",
        "open-database",
        "open-token-store",
        "write-token",
        "bind-api",
        "bind-content",
        "bind-loopback",
        "build-state",
        "readiness",
    ];
    // Only Linux sweeps, so only there is there an entry to hold at, and a
    // dead session that is removed once the sweep has run.
    let sweeps = cfg!(target_os = "linux");
    let held_at: Vec<&str> = stages
        .into_iter()
        .chain(sweeps.then_some("sweep-entry"))
        .collect();
    for stage in held_at {
        let root = Root::new()?;
        // A dead session to sweep, so that the sweep has an entry to hold at.
        session_dir(&root, "dead", &marker_text(dead_pid()?, None))?;
        let mut server = Server::start(
            &one_shot_args(&root, "120", &["--loopback-listen", "127.0.0.1:0"]),
            &[(HOLD, stage.as_ref())],
        )?;
        server.wait_stderr_contains(&format!("holding before {stage}"))?;
        send(&server, "TERM")?;
        let status = server.wait_exit()?;
        assert_eq!(status.code(), Some(0), "stop before {stage}");
        assert_eq!(
            server.next_line()?,
            None,
            "no readiness line before {stage}"
        );
        let left: Vec<String> = fs::read_dir(root.path())?
            .map(|entry| entry.map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect::<Result<_, _>>()?;
        if !sweeps || matches!(stage, "create-dir" | "sweep" | "sweep-entry") {
            // The sweep had not finished: the dead session is still there, and
            // only our own directory is gone.
            assert_eq!(left, [format!("{PREFIX}dead")], "stop before {stage}");
        } else {
            // The sweep had run: the dead session is gone, and so is ours.
            assert!(left.is_empty(), "stop before {stage} left {left:?}");
        }
    }
    Ok(())
}

#[test]
fn the_lifetime_counts_startup_time_and_ends_a_startup_that_is_held() -> TestResult {
    if !seam_available() {
        return Ok(());
    }
    let root = Root::new()?;
    let mut server = Server::start(
        &one_shot_args(&root, "1", &[]),
        &[(HOLD, "bind-content".as_ref())],
    )?;
    // No signal: the one-second lifetime alone releases the held startup.
    let status = server.wait_exit()?;
    assert_eq!(status.code(), Some(0));
    assert_eq!(server.next_line()?, None);
    assert_eq!(root.entries()?, 0);
    Ok(())
}

#[test]
fn a_dead_owner_is_swept_and_a_live_one_is_not() -> TestResult {
    if !cfg!(target_os = "linux") {
        skip("the sweep is effective on Linux only");
        return Ok(());
    }
    let root = Root::new()?;
    let sleeper = Sleeper::new()?;
    let dead = session_dir(&root, "dead", &marker_text(dead_pid()?, None))?;
    let live = session_dir(&root, "live", &marker_text(sleeper.0.id(), None))?;
    let mut server = start(&root, "120")?;
    let data_dir = data_dir_of(&server.ready()?)?;
    assert!(!dead.exists(), "the dead owner's directory must be swept");
    assert!(live.exists(), "a live owner's directory must stay");
    assert!(data_dir.exists());
    server.wait_stderr_contains("1 removed, 1 owner still running")?;
    send(&server, "TERM")?;
    assert!(server.wait_exit()?.success());
    assert!(live.exists(), "ending a session never touches another's");
    Ok(())
}

#[test]
fn look_alikes_are_never_swept() -> TestResult {
    if !cfg!(target_os = "linux") {
        skip("the sweep is effective on Linux only");
        return Ok(());
    }
    let root = Root::new()?;
    let dead = marker_text(dead_pid()?, Some(1));
    let wrong_prefix = root.path().join("not-artifree-session-x");
    fs::create_dir(&wrong_prefix)?;
    fs::write(wrong_prefix.join(MARKER_FILE), &dead)?;
    let wrong_format = session_dir(&root, "format", "artifree-session 2\npid 1\n")?;
    let trailing = session_dir(&root, "trailing", &format!("{dead}\n"))?;
    let extra = session_dir(&root, "extra", &format!("{dead}other 1\n"))?;
    // A real, dead-owner session under a name without the prefix, and a link
    // with the prefix that points at it.
    let target = root.path().join("linked-to-session");
    fs::create_dir(&target)?;
    fs::write(target.join(MARKER_FILE), &dead)?;
    let link = root.path().join(format!("{PREFIX}link"));
    symlink(&target, &link)?;
    let no_marker = root.path().join(format!("{PREFIX}nomarker"));
    fs::create_dir(&no_marker)?;

    let mut server = start(&root, "120")?;
    server.ready()?;
    server.wait_stderr_contains("0 removed")?;
    for path in [
        &wrong_prefix,
        &wrong_format,
        &trailing,
        &extra,
        &target,
        &no_marker,
    ] {
        assert!(path.exists(), "{} must be kept", path.display());
    }
    assert!(fs::symlink_metadata(&link)?.file_type().is_symlink());
    send(&server, "TERM")?;
    assert!(server.wait_exit()?.success());
    Ok(())
}

/// The status line and body of one HTTP/1.1 exchange.
struct Reply {
    status: u16,
    body: String,
}

fn http(
    base: &str,
    method: &str,
    path: &str,
    token: Option<&str>,
    key: Option<&str>,
    body: Option<&Value>,
) -> Result<Reply, Box<dyn std::error::Error>> {
    let host = base.strip_prefix("http://").ok_or("not an http url")?;
    let mut stream = TcpStream::connect(host)?;
    stream.set_read_timeout(Some(PATIENCE))?;
    let mut lines = vec![
        format!("{method} {path} HTTP/1.1"),
        format!("Host: {host}"),
        "Connection: close".to_owned(),
    ];
    if let Some(token) = token {
        lines.push(format!("Authorization: Bearer {token}"));
    }
    if let Some(key) = key {
        lines.push(format!("Idempotency-Key: {key}"));
    }
    let payload = body.map(Value::to_string).unwrap_or_default();
    if body.is_some() {
        lines.push("Content-Type: application/json".to_owned());
        lines.push(format!("Content-Length: {}", payload.len()));
    }
    let request = format!("{}\r\n\r\n{payload}", lines.join("\r\n"));
    stream.write_all(request.as_bytes())?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw)?;
    let (head, body) = raw.split_once("\r\n\r\n").ok_or("no header end")?;
    let status = head
        .split_whitespace()
        .nth(1)
        .ok_or("no status")?
        .parse::<u16>()?;
    Ok(Reply {
        status,
        body: body.to_owned(),
    })
}

fn token_of(ready: &Value) -> Result<String, Box<dyn std::error::Error>> {
    let file = ready["token_file"].as_str().ok_or("token_file")?;
    Ok(fs::read_to_string(file)?.trim().to_owned())
}

fn text<'a>(value: &'a Value, name: &str) -> Result<&'a str, String> {
    value[name].as_str().ok_or_else(|| format!("no {name}"))
}

#[test]
fn a_new_session_inherits_no_token_job_preview_or_artifact() -> TestResult {
    let root = Root::new()?;

    let mut first = start(&root, "120")?;
    let ready = first.ready()?;
    let (api, content) = (text(&ready, "api_url")?, text(&ready, "content_origin")?);
    let old_token = token_of(&ready)?;
    let created = http(
        api,
        "POST",
        "/v1/artifacts",
        Some(&old_token),
        Some("create-key-1"),
        Some(&json!({"title": "old", "files": [
            {"path": "index.html", "encoding": "utf8", "content": "<h1>old</h1>"}]})),
    )?;
    assert_eq!(created.status, 201, "{}", created.body);
    let artifact = serde_json::from_str::<Value>(&created.body)?["artifact"]["id"]
        .as_str()
        .ok_or("artifact id")?
        .to_owned();
    let job = http(
        api,
        "POST",
        &format!("/v1/artifacts/{artifact}/jobs"),
        Some(&old_token),
        Some("job-key-1"),
        Some(&json!({"kind": "verify_revision", "revision": 1})),
    )?;
    assert_eq!(job.status, 201, "{}", job.body);
    let job = serde_json::from_str::<Value>(&job.body)?["job"]["id"]
        .as_str()
        .ok_or("job id")?
        .to_owned();
    let preview = http(
        api,
        "POST",
        &format!("/v1/artifacts/{artifact}/revisions/1/previews"),
        Some(&old_token),
        None,
        Some(&json!({})),
    )?;
    assert_eq!(preview.status, 201, "{}", preview.body);
    let url = serde_json::from_str::<Value>(&preview.body)?["url"]
        .as_str()
        .ok_or("preview url")?
        .to_owned();
    let preview_path = &url[url.find("/p/").ok_or("preview path")?..];
    let before = http(content, "GET", preview_path, None, None, None)?;
    assert_eq!(before.status, 200, "the preview works in its own session");
    send(&first, "TERM")?;
    assert!(first.wait_exit()?.success());

    let mut second = start(&root, "120")?;
    let ready = second.ready()?;
    let (api, content) = (text(&ready, "api_url")?, text(&ready, "content_origin")?);
    let new_token = token_of(&ready)?;
    assert_ne!(new_token, old_token, "a new session has a new owner token");
    let stale = http(api, "GET", "/v1/artifacts", Some(&old_token), None, None)?;
    assert_eq!(stale.status, 401, "{}", stale.body);
    let listed = http(api, "GET", "/v1/artifacts", Some(&new_token), None, None)?;
    assert_eq!(listed.status, 200, "{}", listed.body);
    assert!(!listed.body.contains(&artifact), "{}", listed.body);
    for path in [
        format!("/v1/jobs/{job}"),
        format!("/v1/artifacts/{artifact}"),
    ] {
        let reply = http(api, "GET", &path, Some(&new_token), None, None)?;
        assert_eq!(reply.status, 404, "{path}: {}", reply.body);
    }
    let gone = http(content, "GET", preview_path, None, None, None)?;
    assert_eq!(gone.status, 404, "the old preview address is not served");
    send(&second, "TERM")?;
    assert!(second.wait_exit()?.success());
    Ok(())
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::fs::OpenOptions;

    fn full() -> Result<Stdio, std::io::Error> {
        Ok(Stdio::from(
            OpenOptions::new().write(true).open("/dev/full")?,
        ))
    }

    #[test]
    fn a_failed_readiness_write_ends_a_one_shot_session_cleanly() -> TestResult {
        let root = Root::new()?;
        let mut server = Server::start_with(&one_shot_args(&root, "120", &[]), &[], full()?)?;
        let status = server.wait_exit()?;
        assert_eq!(status.code(), Some(1), "{}", server.stderr());
        assert_eq!(root.entries()?, 0, "no directory may be left");
        server.wait_stderr_contains("Could not write the readiness line")?;
        Ok(())
    }

    fn free_port() -> Result<u16, std::io::Error> {
        Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
    }

    #[test]
    fn persistent_mode_keeps_serving_after_a_failed_readiness_write() -> TestResult {
        let root = Root::new()?;
        let data = root.path().join("data");
        let port = free_port()?;
        let api = format!("127.0.0.1:{port}");
        let mut server = Server::start_with(
            &[
                "serve",
                "--data-dir",
                data.to_str().ok_or("data path")?,
                "--api-listen",
                &api,
                "--content-listen",
                "127.0.0.1:0",
            ],
            &[],
            full()?,
        )?;
        server.wait_stderr_contains("Could not write the readiness line")?;
        assert!(server.child.try_wait()?.is_none(), "it must keep serving");
        let health = http(
            &format!("http://{api}"),
            "GET",
            "/healthz",
            None,
            None,
            None,
        )?;
        assert_eq!(health.status, 200, "{}", health.body);
        send(&server, "TERM")?;
        assert!(server.wait_exit()?.success());
        assert!(data.join("access-token").is_file(), "persistent data stays");
        Ok(())
    }

    #[test]
    fn a_temporary_directory_that_is_not_utf8_is_refused_cleanly() -> TestResult {
        let root = Root::new()?;
        let odd = root.path().join(OsString::from_vec(vec![b't', 0xff, b'm']));
        if fs::create_dir(&odd).is_err() {
            skip("this file system refuses non-UTF-8 names");
            return Ok(());
        }
        let mut server = Server::start(
            &[
                "serve",
                "--one-shot-ttl-seconds",
                "120",
                "--api-listen",
                "127.0.0.1:0",
                "--content-listen",
                "127.0.0.1:0",
            ],
            &[("TMPDIR", odd.as_os_str())],
        )?;
        let status = server.wait_exit()?;
        assert_eq!(status.code(), Some(1), "{}", server.stderr());
        assert_eq!(server.next_line()?, None, "no readiness line");
        server.wait_stderr_contains("UTF-8")?;
        assert_eq!(fs::read_dir(&odd)?.count(), 0, "no leftover directory");
        Ok(())
    }
}
