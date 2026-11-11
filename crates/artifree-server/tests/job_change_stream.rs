//! The change stream of job lifecycles, against the real binary (decision 0055,
//! acceptance 22 and rules 8 and 10).
//!
//! An open `GET /v1/changes` stream carries one `job_changed` event for each
//! committed state of a job: queued, running and succeeded. A commit made by
//! this server wakes the stream at once. A commit made by the server's own
//! worker wakes it when the worker's `claimed` line or outcome line parses. A
//! commit made by a worker in another process is seen on the stream's
//! fifteen-second poll. Each event carries exactly the change fields and the job
//! identifier, and no job state. A worker run once prints its claim and then its
//! outcome, in that order.
//!
//! Every wait is bounded and observes a condition, and every process is killed
//! and reaped when its guard drops, so a failing test leaves no process behind.

use std::collections::BTreeSet;
use std::error::Error;
use std::fs;
use std::future::{self, Future};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::task::{Context, Poll};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use artifree_contracts::api::WorkerState;
use artifree_core::jobs::SystemClock;
use artifree_server::shutdown::StopLatch;
use artifree_server::supervisor::{self, Launcher, Wake, WorkerChild, WorkerStatusHandle};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, ReadBuf};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time;

type TestResult = Result<(), Box<dyn Error>>;

const BIN: &str = env!("CARGO_BIN_EXE_artifree-server");
/// The bound of every wait. A healthy run finishes well inside it.
const PATIENCE: Duration = Duration::from_secs(60);
/// How often a bounded wait looks again.
const STEP: Duration = Duration::from_millis(100);
/// How long joining a supervisor task may take, in paused time.
const JOIN_LIMIT: Duration = Duration::from_secs(120);
/// How many times a wait yields to the runtime before it gives up.
const YIELD_LIMIT: usize = 10_000;
/// Acceptance 22: a commit of this server or of its own worker reaches an open
/// stream in under two seconds.
const PROMPT_MS: i64 = 2_000;
/// Acceptance 22: a commit of a worker in another process reaches an open stream
/// within its fifteen-second reading, with a margin of two seconds.
const POLLED_MS: i64 = 17_000;
/// The fields of a `job_changed` event, as the contract names them (rule 8). The
/// `data_version` field is not among them, because only a data change carries it.
const JOB_CHANGE_FIELDS: [&str; 7] = [
    "artifact_id",
    "cursor",
    "change",
    "revision",
    "metadata_revision",
    "job_id",
    "committed_at_ms",
];

/// Creates the directory `dir` for a server. The server refuses a data directory
/// that other users can access, so on Unix it is created with owner-only
/// permissions.
#[cfg(unix)]
fn create_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(dir)
}

/// Creates the directory `dir`. Other platforms have no owner-only mode to set here.
#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> io::Result<()> {
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
            "artifree-job-change-{}-{}",
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
fn lines_in_thread(reader: impl Read + Send + 'static) -> Receiver<String> {
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
        Ok(Self {
            child,
            _stdin: stdin,
            lines: lines_in_thread(stdout),
            errors: lines_in_thread(stderr),
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

    /// Waits until the process has exited, for at most the patience.
    fn wait_exit(&mut self) -> Result<ExitStatus, Box<dyn Error>> {
        let deadline = Instant::now() + PATIENCE;
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "the process did not exit within 60 s; the standard error so far: {}",
                    self.diagnostics()
                )
                .into());
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
    address: &str,
    method: &str,
    path: &str,
    token: &str,
    key: Option<&str>,
    body: Option<&Value>,
) -> Result<(u16, String), Box<dyn Error>> {
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

/// A running server: its process, the address of its API and its administrator's
/// token. Dropping it kills the process.
struct Server {
    /// Held only so that dropping the server kills and reaps its process.
    _process: Process,
    address: String,
    token: String,
}

impl Server {
    /// Starts a persistent `serve` on the data directory `dir`, with the flags `extra`
    /// added, and reads its ready line. The server prints that line once its
    /// listeners are bound and its worker, if it has one, has started.
    fn start(dir: &str, extra: &[&str]) -> Result<Self, Box<dyn Error>> {
        let mut arguments = vec![
            "serve",
            "--data-dir",
            dir,
            "--api-listen",
            "127.0.0.1:0",
            "--content-listen",
            "127.0.0.1:0",
        ];
        arguments.extend_from_slice(extra);
        let process = Process::spawn(&arguments)?;
        let ready = process.next_event("ready")?;
        let token_file = member(&ready, "token_file")?;
        let token = fs::read_to_string(token_file)?.trim().to_owned();
        let api_url = member(&ready, "api_url")?;
        let address = api_url
            .strip_prefix("http://")
            .ok_or("the ready line gives no http address")?
            .to_owned();
        Ok(Self {
            _process: process,
            address,
            token,
        })
    }

    /// Sends one request with the administrator's token. A refusal for the rate limit
    /// did not process the request, so it is sent again after a second, until the
    /// patience runs out.
    fn send(
        &self,
        method: &str,
        path: &str,
        key: Option<&str>,
        body: Option<&Value>,
    ) -> Result<(u16, String), Box<dyn Error>> {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let (status, text) = request(&self.address, method, path, &self.token, key, body)?;
            if status != 429 || Instant::now() >= deadline {
                return Ok((status, text));
            }
            thread::sleep(Duration::from_secs(1));
        }
    }

    /// Creates an artifact of one file and returns its identifier.
    fn create_artifact(&self, key: &str) -> Result<String, Box<dyn Error>> {
        let body = json!({
            "title": "Job change stream",
            "files": [
                { "path": "index.html", "encoding": "utf8", "content": "<h1>job changes</h1>" }
            ]
        });
        let (status, text) = self.send("POST", "/v1/artifacts", Some(key), Some(&body))?;
        answered(status, 201, &text)?;
        let created: Value = serde_json::from_str(&text)?;
        member(&created["artifact"], "id")
    }

    /// Asks for a `verify_revision` job of revision 1 of `artifact` and returns the
    /// job's identifier.
    fn enqueue(&self, artifact: &str, key: &str) -> Result<String, Box<dyn Error>> {
        let body = json!({ "kind": "verify_revision", "revision": 1 });
        let path = format!("/v1/artifacts/{artifact}/jobs");
        let (status, text) = self.send("POST", &path, Some(key), Some(&body))?;
        answered(status, 201, &text)?;
        let reply: Value = serde_json::from_str(&text)?;
        member(&reply["job"], "id")
    }

    /// The state that `GET /v1/jobs/{job}` reports for `job`.
    fn job_state(&self, job: &str) -> Result<String, Box<dyn Error>> {
        let path = format!("/v1/jobs/{job}");
        let (status, text) = self.send("GET", &path, None, None)?;
        answered(status, 200, &text)?;
        let value: Value = serde_json::from_str(&text)?;
        member(&value, "state")
    }
}

/// One server-sent event of a change stream: its name, its `id` when it has one, its
/// data, and the wall-clock time at which the reader took it from the socket.
#[derive(Debug)]
struct Frame {
    name: String,
    id: Option<String>,
    data: String,
    received_at_ms: i64,
}

/// An open `GET /v1/changes` stream. A thread of its own reads the chunked response
/// and sends each complete event, so that waiting for an event never blocks on the
/// socket.
struct Stream {
    receiver: Receiver<Result<Frame, String>>,
}

impl Stream {
    /// Opens `GET /v1/changes` with the server's token and reads the head of the
    /// response, which must be a chunked stream answered with status 200.
    fn open(server: &Server) -> Result<Self, Box<dyn Error>> {
        let mut socket = TcpStream::connect(server.address.as_str())?;
        socket.set_read_timeout(Some(PATIENCE))?;
        let head = format!(
            "GET /v1/changes HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nAccept: text/event-stream\r\n\r\n",
            address = server.address,
            token = server.token,
        );
        socket.write_all(head.as_bytes())?;
        let mut reader = BufReader::new(socket);
        let mut status_line = String::new();
        reader.read_line(&mut status_line)?;
        let mut chunked = false;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                return Err("the stream ended inside its response head".into());
            }
            let header = line.trim_end().to_ascii_lowercase();
            if header.is_empty() {
                break;
            }
            if header == "transfer-encoding: chunked" {
                chunked = true;
            }
        }
        let status = status_line.split(' ').nth(1).unwrap_or_default();
        if status != "200" || !chunked {
            return Err(format!(
                "the change stream must answer 200 and be chunked; it answered {status_line:?}, chunked: {chunked}"
            )
            .into());
        }
        let (sender, receiver) = channel();
        thread::spawn(move || {
            if let Err(message) = read_frames(&mut reader, &sender) {
                // The receiver may be gone already; then nobody reads the message.
                drop(sender.send(Err(message)));
            }
        });
        Ok(Self { receiver })
    }

    /// The next event, which must arrive within the patience.
    fn next_frame(&self) -> Result<Frame, Box<dyn Error>> {
        match self.receiver.recv_timeout(PATIENCE) {
            Ok(Ok(frame)) => Ok(frame),
            Ok(Err(message)) => Err(message.into()),
            Err(_) => Err("no event within 60 s, or the stream ended".into()),
        }
    }

    /// The next event, which must be named `name`.
    fn expect_frame(&self, name: &str) -> Result<Frame, Box<dyn Error>> {
        let frame = self.next_frame()?;
        if frame.name == name {
            return Ok(frame);
        }
        Err(format!(
            "expected a {name} event, got {}: {}",
            frame.name, frame.data
        )
        .into())
    }

    /// The next event, which must be a `change` event.
    fn next_change(&self) -> Result<Frame, Box<dyn Error>> {
        self.expect_frame("change")
    }
}

/// Reads the chunks of a chunked response and sends each complete event with the time
/// it arrived, until the response ends. A chunk that breaks the framing is returned as
/// an error. A receiver that has gone ends the reading without an error.
fn read_frames(
    reader: &mut impl BufRead,
    sender: &Sender<Result<Frame, String>>,
) -> Result<(), String> {
    let mut pending: Vec<u8> = Vec::new();
    loop {
        let mut size_line = String::new();
        let read = reader
            .read_line(&mut size_line)
            .map_err(|error| format!("reading a chunk size: {error}"))?;
        if read == 0 {
            return Ok(());
        }
        let digits = size_line.trim_end().split(';').next().unwrap_or_default();
        let size = usize::from_str_radix(digits, 16)
            .map_err(|error| format!("a chunk size is not hexadecimal: {size_line:?}: {error}"))?;
        if size == 0 {
            return Ok(());
        }
        let mut chunk = vec![0_u8; size];
        reader
            .read_exact(&mut chunk)
            .map_err(|error| format!("reading a chunk of {size} bytes: {error}"))?;
        let mut terminator = [0_u8; 2];
        reader
            .read_exact(&mut terminator)
            .map_err(|error| format!("reading the end of a chunk: {error}"))?;
        pending.extend_from_slice(&chunk);
        while let Some(end) = pending.windows(2).position(|pair| pair == b"\n\n") {
            let block: Vec<u8> = pending.drain(..end + 2).collect();
            let received_at_ms = unix_ms_now()?;
            let Some(frame) = parse_frame(&block, received_at_ms)? else {
                continue;
            };
            if sender.send(Ok(frame)).is_err() {
                return Ok(());
            }
        }
    }
}

/// Reads one server-sent event from `block`, the text up to and including its blank
/// line. A block without an event name is a comment, such as the keep-alive, and
/// yields nothing.
fn parse_frame(block: &[u8], received_at_ms: i64) -> Result<Option<Frame>, String> {
    let text =
        std::str::from_utf8(block).map_err(|error| format!("an event is not UTF-8: {error}"))?;
    let mut name = None;
    let mut id = None;
    let mut data = Vec::new();
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("event:") {
            name = Some(value.trim_start().to_owned());
        } else if let Some(value) = line.strip_prefix("id:") {
            id = Some(value.trim_start().to_owned());
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push(value.trim_start());
        }
    }
    Ok(name.map(|name| Frame {
        name,
        id,
        data: data.join("\n"),
        received_at_ms,
    }))
}

/// The wall clock in Unix milliseconds, which is the clock that the server stamps its
/// commits with.
fn unix_ms_now() -> Result<i64, String> {
    let since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("the clock is before 1970: {error}"))?;
    i64::try_from(since_epoch.as_millis())
        .map_err(|error| format!("the clock is out of range: {error}"))
}

/// Checks that `change` is the `job_changed` event of `job` on `artifact`. Its fields
/// are exactly the contract's, its `id` is its cursor, its revisions are those of a
/// new artifact, and it reached the stream less than `bound_ms` after its commit.
fn check_job_change(
    change: &Frame,
    artifact: &str,
    job: &str,
    bound_ms: i64,
    label: &str,
) -> TestResult {
    let value: Value = serde_json::from_str(&change.data)?;
    let fields: BTreeSet<&str> = value.as_object().map_or_else(BTreeSet::new, |object| {
        object.keys().map(String::as_str).collect()
    });
    let expected: BTreeSet<&str> = JOB_CHANGE_FIELDS.into_iter().collect();
    assert_eq!(
        fields, expected,
        "the {label} event must carry exactly the change fields and job_id, got {}",
        change.data
    );
    assert_eq!(
        value["change"], "job_changed",
        "the {label} event must be a job_changed change, got {}",
        change.data
    );
    assert_eq!(
        value["artifact_id"], artifact,
        "the {label} event must name the artifact, got {}",
        change.data
    );
    assert_eq!(
        value["job_id"], job,
        "the {label} event must name the job, got {}",
        change.data
    );
    assert_eq!(
        value["revision"], 1_u64,
        "a job change names the current revision of a new artifact, got {}",
        change.data
    );
    assert_eq!(
        value["metadata_revision"], 1_u64,
        "a job change names the current metadata revision of a new artifact, got {}",
        change.data
    );
    assert_eq!(
        change.id.as_deref(),
        value["cursor"].as_str(),
        "the id of the {label} event must be its cursor, got {}",
        change.data
    );
    let Some(committed_at_ms) = value["committed_at_ms"].as_i64() else {
        return Err(format!("the {label} event has no committed_at_ms: {}", change.data).into());
    };
    let latency_ms = change.received_at_ms - committed_at_ms;
    assert!(
        latency_ms < bound_ms,
        "the {label} change reached the stream {latency_ms} ms after its commit, not within {bound_ms} ms: {}",
        change.data
    );
    Ok(())
}

/// A fake worker process. Its standard output is `stdout`, and it exits with status 0
/// once its standard input closes, as a worker does when the server stops it.
struct FakeChild {
    stdout: Option<Vec<u8>>,
    ended: Arc<AtomicBool>,
    closed: watch::Sender<bool>,
}

impl WorkerChild for FakeChild {
    fn take_stdout(&mut self) -> Option<Box<dyn AsyncRead + Send + Unpin>> {
        let bytes = self.stdout.take()?;
        let output: Box<dyn AsyncRead + Send + Unpin> = Box::new(Output {
            bytes: io::Cursor::new(bytes),
            ended: Arc::clone(&self.ended),
        });
        Some(output)
    }

    fn close_stdin(&mut self) {
        self.closed.send_modify(|closed| *closed = true);
    }

    async fn wait(&mut self) -> io::Result<bool> {
        let mut closed = self.closed.subscribe();
        closed
            .wait_for(|value| *value)
            .await
            .map_err(io::Error::other)?;
        Ok(true)
    }

    fn kill(&mut self) -> impl Future<Output = io::Result<()>> + Send {
        future::ready(Ok(()))
    }
}

/// The standard output of a fake worker. It sets `ended` when a reader reaches its end,
/// which happens only after the reader has taken every line before it.
struct Output {
    bytes: io::Cursor<Vec<u8>>,
    ended: Arc<AtomicBool>,
}

impl AsyncRead for Output {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let polled = Pin::new(&mut this.bytes).poll_read(cx, buf);
        if matches!(polled, Poll::Ready(Ok(()))) && buf.filled().len() == before {
            this.ended.store(true, Ordering::SeqCst);
        }
        polled
    }
}

/// Starts fake worker processes, each with the standard output `stdout`.
struct FakeLauncher {
    stdout: Vec<u8>,
    ended: Arc<AtomicBool>,
}

impl Launcher for FakeLauncher {
    type Child = FakeChild;

    fn launch(&self) -> io::Result<FakeChild> {
        Ok(FakeChild {
            stdout: Some(self.stdout.clone()),
            ended: Arc::clone(&self.ended),
            closed: watch::channel(false).0,
        })
    }
}

/// Yields to the runtime until `ready` holds, for at most `YIELD_LIMIT` yields. Yielding
/// does not move the paused clock, so a wait here never depends on time passing.
async fn yield_until(mut ready: impl FnMut() -> bool) -> bool {
    for _ in 0..YIELD_LIMIT {
        if ready() {
            return true;
        }
        tokio::task::yield_now().await;
    }
    ready()
}

/// Waits for a supervisor task, and fails rather than waiting forever.
async fn join(task: JoinHandle<()>) -> TestResult {
    time::timeout(JOIN_LIMIT, task).await??;
    Ok(())
}

/// Acceptance 22 and rule 10: with the server's own worker, the `job_changed` events of
/// a job's queued, running and succeeded states reach an open stream within two seconds
/// of their commits.
#[test]
fn the_servers_own_worker_wakes_the_stream_for_each_job_change() -> TestResult {
    let scratch = Scratch::new()?;
    let server = Server::start(scratch.text()?, &[])?;
    let artifact = server.create_artifact("stream-artifact-served")?;
    let stream = Stream::open(&server)?;
    stream.expect_frame("ready")?;
    let job = server.enqueue(&artifact, "stream-job-served")?;
    for label in ["queued", "running", "succeeded"] {
        let change = stream.next_change()?;
        check_job_change(&change, &artifact, &job, PROMPT_MS, label)?;
    }
    assert_eq!(
        server.job_state(&job)?,
        "succeeded",
        "the job must end succeeded once its succeeded change has arrived"
    );
    Ok(())
}

/// Acceptance 22 and rule 10: a commit made by a worker in another process reaches an
/// open stream on the stream's poll, within seventeen seconds, while the commit of the
/// queued state, made by this server, reaches it at once.
#[test]
fn a_worker_in_another_process_is_seen_on_the_stream_within_seventeen_seconds() -> TestResult {
    let scratch = Scratch::new()?;
    let server = Server::start(scratch.text()?, &["--no-worker"])?;
    let artifact = server.create_artifact("stream-artifact-other")?;
    let stream = Stream::open(&server)?;
    stream.expect_frame("ready")?;
    let job = server.enqueue(&artifact, "stream-job-other")?;
    let queued = stream.next_change()?;
    check_job_change(&queued, &artifact, &job, PROMPT_MS, "queued")?;
    let mut worker = Process::spawn(&["worker", "run", "--data-dir", scratch.text()?, "--once"])?;
    let status = worker.wait_exit()?;
    assert!(
        status.success(),
        "the worker run --once must exit with status 0, got {status}"
    );
    let running = stream.next_change()?;
    check_job_change(&running, &artifact, &job, POLLED_MS, "running")?;
    let succeeded = stream.next_change()?;
    check_job_change(&succeeded, &artifact, &job, POLLED_MS, "succeeded")?;
    assert_eq!(
        server.job_state(&job)?,
        "succeeded",
        "the worker must have run the job to its end"
    );
    Ok(())
}

/// Rule 10: a worker run once prints its claim of the job, then the job's end, then
/// that it stopped because the queue had drained, and exits with status 0.
#[test]
fn a_worker_run_once_prints_its_claim_then_its_job_then_stops() -> TestResult {
    let scratch = Scratch::new()?;
    let server = Server::start(scratch.text()?, &["--no-worker"])?;
    let artifact = server.create_artifact("worker-artifact")?;
    let job = server.enqueue(&artifact, "worker-job")?;
    // The server is stopped first, so the worker alone runs the queued job.
    drop(server);
    let mut worker = Process::spawn(&["worker", "run", "--data-dir", scratch.text()?, "--once"])?;
    worker.next_event("ready")?;
    let claimed = worker.next_event("claimed")?;
    assert_eq!(
        claimed["job"],
        job.as_str(),
        "the claimed line must name the queued job, got {claimed}"
    );
    let settled = worker.next_event("job")?;
    assert_eq!(
        settled["job"],
        job.as_str(),
        "the job line must name the claimed job, got {settled}"
    );
    assert_eq!(
        settled["state"], "succeeded",
        "the job line must report the job as succeeded, got {settled}"
    );
    assert_eq!(
        settled["verified"].as_bool(),
        Some(true),
        "the verification of the job must pass, got {settled}"
    );
    let stopped = worker.next_event("stopped")?;
    assert_eq!(
        stopped["reason"], "drained",
        "a worker run once stops because the queue drained, got {stopped}"
    );
    let status = worker.wait_exit()?;
    assert!(
        status.success(),
        "the worker run --once must exit with status 0, got {status}"
    );
    Ok(())
}

/// Decision 0055, rule 10: a line of a worker's standard output that parses wakes the
/// server's change streams once, and a line that does not parse is only counted as
/// dropped.
#[tokio::test(start_paused = true)]
async fn a_parsed_worker_line_wakes_the_streams_and_a_dropped_line_does_not() -> TestResult {
    let stdout =
        b"not json\n{\"event\":\"claimed\",\"job\":\"job_0123456789abcdef0123456789abcdef\"}\n"
            .to_vec();
    let ended = Arc::new(AtomicBool::new(false));
    let launcher = FakeLauncher {
        stdout,
        ended: Arc::clone(&ended),
    };
    let wakes = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&wakes);
    let wake: Wake = Arc::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
    });
    let status = WorkerStatusHandle::supervised();
    let draining = StopLatch::new();
    let stop = StopLatch::new();
    let task = supervisor::start_with_wake(
        launcher,
        status.clone(),
        Arc::new(SystemClock),
        draining.clone(),
        stop.clone(),
        wake,
    );
    assert!(
        yield_until(|| ended.load(Ordering::SeqCst)).await,
        "the reader did not reach the end of the worker's standard output"
    );
    assert_eq!(
        wakes.load(Ordering::SeqCst),
        1,
        "only the line that parses may wake the streams; the other line is dropped"
    );
    assert_eq!(
        status.current().dropped_lines,
        1,
        "the line that does not parse must be counted as dropped"
    );
    stop.set();
    assert!(
        yield_until(|| status.current().state == WorkerState::Stopped).await,
        "the stop was not observed"
    );
    join(task).await?;
    Ok(())
}
