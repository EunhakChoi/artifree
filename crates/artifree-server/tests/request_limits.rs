//! Per-actor request limits through the real router: HTTP and MCP share one
//! budget, refusals come after authentication and before any processing, and
//! a refused request leaves its idempotency key unused.
use std::convert::Infallible;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use artifree_providers::storage::SqliteStore;
use artifree_server::auth::AccessToken;
use artifree_server::changes;
use artifree_server::http::{self, AppState, Settings};
use artifree_server::limits::RequestLimits;
use axum::Router;
use axum::body::{Body, Bytes, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use http_body::Frame;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tower::ServiceExt;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct Harness {
    directory: PathBuf,
    api: Router,
}

impl Harness {
    fn new(limits: RequestLimits) -> Self {
        Self::configured(Settings {
            limits,
            ..Self::settings()
        })
    }

    /// The settings every harness starts from: only the owner's limits vary.
    fn settings() -> Settings {
        Settings {
            content_origin: "http://127.0.0.1:7411".into(),
            preview_ttl: Duration::from_secs(60),
            frame_ancestors: "http://127.0.0.1:*".into(),
            change_streams: changes::DEFAULT_STREAMS_PER_ACTOR,
            ..Settings::default()
        }
    }

    fn configured(settings: Settings) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!(
            "artifree-limits-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory)
            .unwrap_or_else(|error| panic!("cannot create test directory: {error}"));
        let store = SqliteStore::open(&directory.join("db.sqlite3"))
            .unwrap_or_else(|error| panic!("cannot open store: {error}"));
        let token_store =
            artifree_providers::tokens::SqliteTokenStore::open(&directory.join("db.sqlite3"))
                .unwrap_or_else(|error| panic!("cannot open token store: {error}"));
        let token = AccessToken::parse(TOKEN).unwrap_or_else(|| panic!("fixture token is invalid"));
        let state = AppState::new(store, token_store, token, settings)
            .unwrap_or_else(|error| panic!("cannot build state: {error}"));
        Self {
            directory,
            // The real server records the socket peer; with none, the client
            // is unknown and /mcp is refused, so tests connect from loopback.
            api: http::router(state).layer(axum::Extension(ConnectInfo(SocketAddr::from((
                [127, 0, 0, 1],
                40_000,
            ))))),
        }
    }

    async fn send(&self, request: Request<Body>) -> Result<(StatusCode, String, Value), TestError> {
        let response = self.api.clone().oneshot(request).await?;
        let status = response.status();
        let retry = response
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let body = to_bytes(response.into_body(), usize::MAX).await?;
        let body = serde_json::from_slice(&body).unwrap_or(Value::Null);
        Ok((status, retry, body))
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

type TestError = Box<dyn std::error::Error>;

fn list(token: &str) -> Result<Request<Body>, axum::http::Error> {
    Request::get("/v1/artifacts")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
}

fn create(key: &str) -> Result<Request<Body>, axum::http::Error> {
    let body = json!({
        "title": "Limited",
        "files": [{ "path": "index.html", "encoding": "utf8", "content": "<p>once</p>" }]
    });
    Request::post("/v1/artifacts")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header("idempotency-key", key)
        .body(Body::from(body.to_string()))
}

/// A request body whose next frame never arrives: an upload stalled mid-body.
struct Stalled;

impl http_body::Body for Stalled {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        Poll::Pending
    }
}

fn stalled_create(key: &str) -> Result<Request<Body>, axum::http::Error> {
    Request::post("/v1/artifacts")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header("idempotency-key", key)
        .body(Body::new(Stalled))
}

fn mcp_list() -> Result<Request<Body>, axum::http::Error> {
    Request::post("/mcp")
        .header(header::HOST, "127.0.0.1:7410")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-11-25")
        .body(Body::from(
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": "list_artifacts", "arguments": {} }
            })
            .to_string(),
        ))
}

#[tokio::test]
async fn http_and_mcp_share_one_budget_that_recovers() -> TestResult {
    let harness = Harness::new(RequestLimits {
        burst: 2,
        per_minute: 60,
        concurrent: 16,
    });
    // Requests without the token are refused first and spend nothing.
    for _ in 0..5 {
        let (status, _, body) = harness.send(list(&"f".repeat(64))?).await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "unauthenticated");
    }
    assert_eq!(harness.send(list(TOKEN)?).await?.0, StatusCode::OK);
    assert_eq!(harness.send(mcp_list()?).await?.0, StatusCode::OK);
    for request in [list(TOKEN)?, mcp_list()?] {
        let (status, retry, body) = harness.send(request).await?;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(retry, "1");
        assert_eq!(body["error"]["code"], "rate_limited");
    }
    // A wrong token is still unauthenticated, not rate limited.
    assert_eq!(
        harness.send(list(&"f".repeat(64))?).await?.0,
        StatusCode::UNAUTHORIZED
    );
    tokio::time::sleep(Duration::from_millis(1050)).await;
    assert_eq!(harness.send(mcp_list()?).await?.0, StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn a_refused_request_leaves_its_key_unused() -> TestResult {
    let harness = Harness::new(RequestLimits {
        burst: 1,
        per_minute: 60,
        concurrent: 16,
    });
    assert_eq!(harness.send(list(TOKEN)?).await?.0, StatusCode::OK);
    let (status, _, _) = harness.send(create("limited-key-1")?).await?;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    tokio::time::sleep(Duration::from_millis(1050)).await;
    let (status, _, body) = harness.send(create("limited-key-1")?).await?;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        body["replayed"], false,
        "the refused request must not commit"
    );
    Ok(())
}

#[tokio::test]
async fn finished_requests_free_their_concurrency_slot() -> TestResult {
    let harness = Harness::new(RequestLimits {
        burst: 50,
        per_minute: 3000,
        concurrent: 1,
    });
    for request in 0..20 {
        let next = if request % 2 == 0 {
            list(TOKEN)?
        } else {
            mcp_list()?
        };
        assert_eq!(
            harness.send(next).await?.0,
            StatusCode::OK,
            "request {request}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn an_abandoned_request_keeps_its_slot_until_its_work_ends() -> TestResult {
    let harness = Harness::new(RequestLimits {
        burst: 100,
        per_minute: 6000,
        concurrent: 1,
    });
    // Poll the create once, past admission, then drop it as a client that
    // disconnects would. Its work must keep the actor's only slot.
    let abandoned = harness.api.clone().oneshot(create("abandoned-key-1")?);
    assert!(
        tokio::time::timeout(Duration::ZERO, abandoned)
            .await
            .is_err(),
        "the create must still be running"
    );
    let (status, _, body) = harness.send(list(TOKEN)?).await?;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["error"]["code"], "rate_limited");
    // The work finishes and frees the slot; the create was committed.
    let mut listed = Value::Null;
    for _ in 0..200 {
        let (status, _, body) = harness.send(list(TOKEN)?).await?;
        if status == StatusCode::OK {
            listed = body;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        listed["artifacts"].as_array().map(Vec::len),
        Some(1),
        "{listed}"
    );
    Ok(())
}

#[tokio::test]
async fn refusals_cancelled_mid_body_release_their_reading() -> TestResult {
    let harness = Harness::new(RequestLimits {
        burst: 1,
        per_minute: 60,
        concurrent: 16,
    });
    assert_eq!(harness.send(list(TOKEN)?).await?.0, StatusCode::OK);
    // More refused uploads than may be read at once, each dropped while its
    // body is being read, as when the server drops the connection.
    for index in 0..=http::MAX_DRAINING {
        let cancelled = harness
            .api
            .clone()
            .oneshot(stalled_create(&format!("stalled-key-{index}"))?);
        assert!(
            tokio::time::timeout(Duration::ZERO, cancelled)
                .await
                .is_err(),
            "the refused body must still be read"
        );
    }
    // Their reading was released, so the next refused body is read again and
    // its refusal waits for the drain time instead of being sent at once.
    let started = Instant::now();
    let (status, _, body) = harness.send(stalled_create("stalled-key-last")?).await?;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert!(
        started.elapsed() >= http::MAX_DRAIN_TIME,
        "refused after {:?}, without reading the body",
        started.elapsed()
    );
    Ok(())
}

// Requests refused with 413 after the body was partly read (decision 0048).
// The server reads and drops the rest, within the bounds it shares with the
// 429 drains, before it sends the refusal. The bodies below record what the
// server read, so each test sees whether the rest was read. Their frames are
// queued up front and more than the routes read before they refuse, so the
// first time a body is found empty is the drain reading it.
//
// Waits are on state, with a generous deadline. Upper bounds on elapsed time
// are not asserted: that the drain ends at a bound is shown by what was read
// or by the state afterwards. The slow-body test runs in real time because
// the paused clock would advance while the blocking pool authenticates and
// fire its own guard timer early.

const MIB: usize = 1024 * 1024;
/// A chunk of a body for the routes with small limits.
const SMALL_CHUNK: usize = 4 * 1024;
/// How long a test waits for a state before it fails.
const DEADLINE: Duration = Duration::from_secs(10);

/// What the server has taken from a test body.
#[derive(Debug, Default)]
struct Tally {
    read: AtomicUsize,
    /// Reads that found no frame ready: the body is being waited on.
    waited: AtomicUsize,
    /// Reads after the body returned an error.
    after_error: AtomicUsize,
}

impl Tally {
    fn read(&self) -> usize {
        self.read.load(Ordering::SeqCst)
    }

    fn waited_on(&self) -> bool {
        self.waited.load(Ordering::SeqCst) > 0
    }

    fn read_after_error(&self) -> bool {
        self.after_error.load(Ordering::SeqCst) > 0
    }
}

/// Where the frames of a [`Probe`] come from.
#[derive(Debug)]
enum Source {
    /// Frames the test queues: an error fails the read, dropping the feed
    /// ends the body, and an empty queue with the feed kept stalls it.
    Fed(mpsc::UnboundedReceiver<io::Result<Bytes>>),
    /// Chunks without end. Every few reads it yields, as a socket does, so
    /// that timers can fire while it is read.
    Endless { chunk: Bytes, served: u32 },
}

/// A request body that counts what is read from it.
#[derive(Debug)]
struct Probe {
    source: Source,
    tally: Arc<Tally>,
    failed: bool,
}

impl Probe {
    fn poll_source(&mut self, context: &mut Context<'_>) -> Poll<Option<io::Result<Bytes>>> {
        match &mut self.source {
            Source::Fed(frames) => frames.poll_recv(context),
            Source::Endless { chunk, served } => {
                *served = (*served + 1) % 16;
                if *served == 0 {
                    context.waker().wake_by_ref();
                    return Poll::Pending;
                }
                Poll::Ready(Some(Ok(chunk.clone())))
            }
        }
    }
}

impl http_body::Body for Probe {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        let tally = self.tally.clone();
        if self.failed {
            tally.after_error.fetch_add(1, Ordering::SeqCst);
        }
        let polled = self.poll_source(context);
        match &polled {
            Poll::Ready(Some(Ok(data))) => {
                tally.read.fetch_add(data.len(), Ordering::SeqCst);
            }
            Poll::Ready(Some(Err(_))) => self.failed = true,
            Poll::Pending => {
                tally.waited.fetch_add(1, Ordering::SeqCst);
            }
            Poll::Ready(None) => {}
        }
        polled.map(|ready| ready.map(|item| item.map(Frame::data)))
    }
}

/// The test's side of a [`Probe`] body fed by the test.
struct Upload {
    body: Body,
    tally: Arc<Tally>,
    /// Keeping it stalls the body after its queued chunks; dropping it ends
    /// the body.
    feed: mpsc::UnboundedSender<io::Result<Bytes>>,
}

impl Upload {
    /// A body of `chunks` queued chunks of `size` bytes each, then silence.
    fn new(chunks: usize, size: usize) -> Self {
        let (feed, frames) = mpsc::unbounded_channel();
        let chunk = Bytes::from(vec![b'x'; size]);
        for _ in 0..chunks {
            feed.send(Ok(chunk.clone()))
                .unwrap_or_else(|_| panic!("the body is gone before it is sent"));
        }
        let tally = Arc::new(Tally::default());
        let body = Body::new(Probe {
            source: Source::Fed(frames),
            tally: tally.clone(),
            failed: false,
        });
        Self { body, tally, feed }
    }

    /// As [`Upload::new`], but the body ends after its chunks.
    fn fed_and_closed(chunks: usize, size: usize) -> (Body, Arc<Tally>) {
        let Self { body, tally, feed } = Self::new(chunks, size);
        drop(feed);
        (body, tally)
    }

    /// A body of chunks of `MIB` bytes that never ends.
    fn endless() -> (Body, Arc<Tally>) {
        let tally = Arc::new(Tally::default());
        let body = Body::new(Probe {
            source: Source::Endless {
                chunk: Bytes::from(vec![b'x'; MIB]),
                served: 0,
            },
            tally: tally.clone(),
            failed: false,
        });
        (body, tally)
    }
}

fn upload_to(path: &str, body: Body) -> Result<Request<Body>, axum::http::Error> {
    Request::post(path)
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header("idempotency-key", "drain-key-1")
        .body(body)
}

/// A frame call of a view that does not exist: the body is judged first.
fn frame_upload(body: Body) -> Result<Request<Body>, axum::http::Error> {
    upload_to(
        "/v1/artifacts/a/revisions/1/runtime-views/v/1/metadata",
        body,
    )
}

fn mcp_upload(body: Body) -> Result<Request<Body>, axum::http::Error> {
    Request::post("/mcp")
        .header(header::HOST, "127.0.0.1:7410")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-11-25")
        .body(body)
}

/// Sends the request and reads the whole answer.
async fn answer(api: Router, request: Request<Body>) -> (StatusCode, String) {
    let response = api
        .oneshot(request)
        .await
        .unwrap_or_else(|error| match error {});
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap_or_else(|error| panic!("cannot read the answer: {error}"));
    (status, String::from_utf8_lossy(&body).into_owned())
}

/// Sends the request in its own task, so that it can be left running.
fn start(harness: &Harness, request: Request<Body>) -> JoinHandle<(StatusCode, String)> {
    tokio::spawn(answer(harness.api.clone(), request))
}

/// Waits until `ready`, polling it; fails the test after [`DEADLINE`].
async fn until(what: &str, mut ready: impl FnMut() -> bool) {
    let waited = tokio::time::timeout(DEADLINE, async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await;
    assert!(waited.is_ok(), "timed out waiting for {what}");
}

fn plentiful() -> RequestLimits {
    RequestLimits {
        burst: 100_000,
        per_minute: 6_000_000,
        concurrent: 64,
    }
}

/// Sends `request` and checks that the answer is a 413 sent after the server
/// read the whole body: `total` bytes. Returns the answer's text.
async fn assert_refused_after_reading_all(
    harness: &Harness,
    request: Request<Body>,
    tally: &Tally,
    total: usize,
) -> String {
    let (status, text) = answer(harness.api.clone(), request).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{text}");
    assert_eq!(
        tally.read(),
        total,
        "the 413 was sent before the rest of the body was read"
    );
    text
}

#[tokio::test]
async fn mcp_413_is_sent_after_the_body_is_read_to_its_end() -> TestResult {
    let harness = Harness::new(plentiful());
    let (body, tally) = Upload::fed_and_closed(40, MIB);
    let text =
        assert_refused_after_reading_all(&harness, mcp_upload(body)?, &tally, 40 * MIB).await;
    // The SDK's own refusal is plain text, not the API's JSON error (0004).
    assert!(!text.trim().is_empty(), "the 413 has no body");
    assert!(
        serde_json::from_str::<Value>(&text).is_err(),
        "the SDK's 413 body is plain text, got {text}"
    );
    Ok(())
}

#[tokio::test]
async fn owner_api_413_is_sent_after_the_body_is_read_to_its_end() -> TestResult {
    let harness = Harness::new(plentiful());
    let (body, tally) = Upload::fed_and_closed(40, MIB);
    let text = assert_refused_after_reading_all(
        &harness,
        upload_to("/v1/artifacts", body)?,
        &tally,
        40 * MIB,
    )
    .await;
    assert!(text.contains("payload_too_large"), "{text}");
    Ok(())
}

#[tokio::test]
async fn frame_413_is_sent_after_the_body_is_read_to_its_end() -> TestResult {
    let harness = Harness::new(plentiful());
    // Twice the frame route's 16 KiB limit.
    let (body, tally) = Upload::fed_and_closed(8, SMALL_CHUNK);
    let text =
        assert_refused_after_reading_all(&harness, frame_upload(body)?, &tally, 8 * SMALL_CHUNK)
            .await;
    assert!(text.contains("payload_too_large"), "{text}");
    Ok(())
}

#[tokio::test]
async fn room_413_is_sent_after_the_body_is_read_to_its_end() -> TestResult {
    let harness = Harness::new(plentiful());
    let (status, _, created) = harness.send(create("room-artifact-1")?).await?;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let artifact = created["artifact"]["id"]
        .as_str()
        .ok_or("missing artifact id")?;
    let request = Request::post("/v1/runtime-sessions")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::empty())?;
    let (status, _, session) = harness.send(request).await?;
    assert_eq!(status, StatusCode::OK, "{session}");
    let session = session["session_id"].as_str().ok_or("missing session id")?;
    // Four times the room route's 8 KiB limit.
    let (body, tally) = Upload::fed_and_closed(8, SMALL_CHUNK);
    let path = format!("/v1/runtime-sessions/{session}/artifacts/{artifact}/room/participants");
    let text = assert_refused_after_reading_all(
        &harness,
        upload_to(&path, body)?,
        &tally,
        8 * SMALL_CHUNK,
    )
    .await;
    assert!(text.contains("payload_too_large"), "{text}");
    Ok(())
}

#[tokio::test]
async fn a_413_drain_commits_nothing_and_leaves_the_key_unused() -> TestResult {
    let harness = Harness::new(plentiful());
    let (body, tally) = Upload::fed_and_closed(40, MIB);
    assert_refused_after_reading_all(
        &harness,
        upload_to("/v1/artifacts", body)?,
        &tally,
        40 * MIB,
    )
    .await;
    // The key the refused upload carried is free for the same request within
    // the limit.
    let request = Request::post("/v1/artifacts")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header("idempotency-key", "drain-key-1")
        .body(Body::from(
            json!({
                "title": "After the refusal",
                "files": [{ "path": "index.html", "encoding": "utf8", "content": "<p>fits</p>" }]
            })
            .to_string(),
        ))?;
    let (status, _, body) = harness.send(request).await?;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        body["replayed"], false,
        "the refused request must not commit"
    );
    Ok(())
}

#[tokio::test]
async fn an_endless_413_body_is_read_only_up_to_the_byte_bound() -> TestResult {
    let harness = Harness::new(plentiful());
    let (body, tally) = Upload::endless();
    let (status, text) = answer(harness.api.clone(), frame_upload(body)?).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{text}");
    // One chunk was read to find the body over the route's limit; the drain
    // then reads until a chunk brings its own count above the bound.
    let read = tally.read();
    assert!(
        read > http::MAX_BODY_BYTES,
        "only {read} bytes were read: the rest of the body was not drained"
    );
    assert!(
        read <= http::MAX_BODY_BYTES + 2 * MIB,
        "{read} bytes were read, more than the bound and one chunk after the refusal"
    );
    Ok(())
}

#[tokio::test]
async fn a_slow_413_body_is_refused_when_the_drain_time_is_over() -> TestResult {
    let harness = Harness::new(plentiful());
    // Over the limit, then silence: the sender neither continues nor ends.
    let Upload {
        body,
        tally,
        feed: _stalled,
    } = Upload::new(8, SMALL_CHUNK);
    let started = Instant::now();
    let answered = tokio::time::timeout(DEADLINE, answer(harness.api.clone(), frame_upload(body)?))
        .await
        .map_err(|_| "the refusal was never sent: the drain has no time bound")?;
    assert_eq!(answered.0, StatusCode::PAYLOAD_TOO_LARGE, "{}", answered.1);
    assert!(tally.waited_on(), "the drain never waited for the body");
    assert!(
        started.elapsed() >= http::MAX_DRAIN_TIME,
        "refused after {:?}, before the drain time",
        started.elapsed()
    );
    Ok(())
}

/// A request left running with its body stalled: the test ends it by letting
/// go of the feed or by breaking the connection.
struct Held {
    answer: JoinHandle<(StatusCode, String)>,
    tally: Arc<Tally>,
    feed: mpsc::UnboundedSender<io::Result<Bytes>>,
}

impl Held {
    /// Whether the server has read the body up to its end and is waiting for
    /// more. For these bodies only the drain gets that far: the 429 bodies
    /// have no frame at all, and the 413 bodies are refused by the route
    /// before their queue is empty.
    fn drained_up_to_the_stall(&self) -> bool {
        self.tally.waited_on()
    }

    /// Ends the body and returns the answer.
    async fn finish(self) -> Result<(StatusCode, String), TestError> {
        drop(self.feed);
        Ok(self.answer.await?)
    }
}

/// Owner budget with no request left after the first, so an owner upload is
/// refused with 429; the frame budget is large.
fn refusing_owner() -> Harness {
    Harness::configured(Settings {
        limits: RequestLimits {
            burst: 1,
            per_minute: 1,
            concurrent: 16,
        },
        frame_limits: plentiful(),
        ..Harness::settings()
    })
}

/// An owner upload refused with 429 whose body stalls.
fn hold_429(harness: &Harness) -> Result<Held, TestError> {
    let Upload { body, tally, feed } = Upload::new(0, 0);
    let answer = start(harness, upload_to("/v1/artifacts", body)?);
    Ok(Held {
        answer,
        tally,
        feed,
    })
}

/// A frame call refused with 413 because its body is over the route's limit,
/// which then stalls.
fn hold_413(harness: &Harness) -> Result<Held, TestError> {
    let Upload { body, tally, feed } = Upload::new(8, SMALL_CHUNK);
    let answer = start(harness, frame_upload(body)?);
    Ok(Held {
        answer,
        tally,
        feed,
    })
}

/// Holds `count` refusals of the given kind and waits until each is being
/// drained.
async fn saturate(
    harness: &Harness,
    count: usize,
    hold: fn(&Harness) -> Result<Held, TestError>,
) -> Result<Vec<Held>, TestError> {
    let mut drains = Vec::new();
    for _ in 0..count {
        drains.push(hold(harness)?);
    }
    until("the refused bodies to be read", || {
        drains.iter().all(Held::drained_up_to_the_stall)
    })
    .await;
    Ok(drains)
}

/// Checks that a refusal is sent without the server reading the body that
/// stalls, as when all places are in use: the answer arrives although the
/// body never ends, and the body was never found empty.
async fn assert_refused_without_reading(
    harness: &Harness,
    hold: fn(&Harness) -> Result<Held, TestError>,
    status: StatusCode,
) -> Result<(), TestError> {
    let extra = hold(harness)?;
    let (got, text) = tokio::time::timeout(DEADLINE, extra.answer)
        .await
        .map_err(|_| "the refusal waited for the body: no place was free")??;
    assert_eq!(got, status, "{text}");
    assert!(
        !extra.tally.waited_on(),
        "the server read the body although {} drains were running",
        http::MAX_DRAINING
    );
    Ok(())
}

#[tokio::test]
async fn drains_of_429_and_413_share_the_places_and_the_33rd_is_not_read() -> TestResult {
    let harness = refusing_owner();
    assert_eq!(harness.send(list(TOKEN)?).await?.0, StatusCode::OK);
    let half = http::MAX_DRAINING / 2;
    let mut drains = saturate(&harness, half, hold_429).await?;
    drains.extend(saturate(&harness, half, hold_413).await?);
    // Every place is in use: a refusal of either kind is sent without
    // reading, and a 413 stops reading at what the route itself read.
    assert_refused_without_reading(&harness, hold_429, StatusCode::TOO_MANY_REQUESTS).await?;
    assert_refused_without_reading(&harness, hold_413, StatusCode::PAYLOAD_TOO_LARGE).await?;
    for drain in drains {
        let (status, text) = drain.finish().await?;
        assert!(
            [StatusCode::TOO_MANY_REQUESTS, StatusCode::PAYLOAD_TOO_LARGE].contains(&status),
            "{status}: {text}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_413_keeps_its_permit_until_its_body_is_drained() -> TestResult {
    let harness = Harness::configured(Settings {
        frame_limits: RequestLimits {
            burst: 100,
            per_minute: 6000,
            concurrent: 1,
        },
        ..Harness::settings()
    });
    let refused = hold_413(&harness)?;
    until("the 413 body to be drained", || {
        refused.drained_up_to_the_stall()
    })
    .await;
    // The same actor has no other slot while the body is drained.
    let (status, _, body) = harness.send(frame_upload(Body::from("{}"))?).await?;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["error"]["code"], "rate_limited");
    let (status, text) = refused.finish().await?;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{text}");
    // The drain ended and released the slot: the next call is judged.
    let (status, _, body) = harness.send(frame_upload(Body::from("{}"))?).await?;
    assert_ne!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    Ok(())
}

#[tokio::test]
async fn a_dropped_connection_ends_a_413_drain_and_frees_its_place_and_permit() -> TestResult {
    let harness = Harness::configured(Settings {
        limits: RequestLimits {
            burst: 1,
            per_minute: 1,
            concurrent: 16,
        },
        frame_limits: RequestLimits {
            burst: 100,
            per_minute: 6000,
            concurrent: 1,
        },
        ..Harness::settings()
    });
    assert_eq!(harness.send(list(TOKEN)?).await?.0, StatusCode::OK);
    // The other places are taken, so the 413 drain holds the last one.
    let others = saturate(&harness, http::MAX_DRAINING - 1, hold_429).await?;
    let refused = hold_413(&harness)?;
    until("the 413 body to be drained", || {
        refused.drained_up_to_the_stall()
    })
    .await;
    // The server drops the connection's request: the drain, which runs in the
    // request's own task, goes on and keeps its place and its permit.
    refused.answer.abort();
    let (status, _, body) = harness.send(frame_upload(Body::from("{}"))?).await?;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    // The body then fails, as a dropped connection's does. The drain ends on
    // the error instead of waiting for its time limit, and frees the permit.
    refused
        .feed
        .send(Err(io::Error::from(io::ErrorKind::ConnectionReset)))?;
    tokio::time::timeout(DEADLINE, async {
        loop {
            let (status, _, _) = harness.send(frame_upload(Body::from("{}"))?).await?;
            if status != StatusCode::TOO_MANY_REQUESTS {
                return Ok::<(), TestError>(());
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .map_err(|_| "the permit was never freed after the read error")??;
    assert!(
        !refused.tally.read_after_error(),
        "the drain went on reading a body that had failed"
    );
    // Its place is free too: the next refusal is read again.
    let again = hold_429(&harness)?;
    until("a new refused body to be read", || {
        again.drained_up_to_the_stall()
    })
    .await;
    for held in others.into_iter().chain([again]) {
        held.finish().await?;
    }
    Ok(())
}
