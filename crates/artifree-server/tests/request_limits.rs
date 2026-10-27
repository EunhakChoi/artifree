//! Per-actor request limits through the real router: HTTP and MCP share one
//! budget, refusals come after authentication and before any processing, and
//! a refused request leaves its idempotency key unused.
use std::convert::Infallible;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use artifree_providers::storage::SqliteStore;
use artifree_server::auth::AccessToken;
use artifree_server::changes;
use artifree_server::http::{self, AppState, Settings};
use artifree_server::limits::RequestLimits;
use axum::Router;
use axum::body::{Body, Bytes, to_bytes};
use axum::http::{Request, StatusCode, header};
use http_body::Frame;
use serde_json::{Value, json};
use tower::ServiceExt;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct Harness {
    directory: PathBuf,
    api: Router,
}

impl Harness {
    fn new(limits: RequestLimits) -> Self {
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
        let token = AccessToken::parse(TOKEN).unwrap_or_else(|| panic!("fixture token is invalid"));
        let state = AppState::new(
            store,
            token,
            Settings {
                content_origin: "http://127.0.0.1:7411".into(),
                preview_ttl: Duration::from_secs(60),
                frame_ancestors: "http://127.0.0.1:*".into(),
                limits,
                change_streams: changes::DEFAULT_STREAMS_PER_ACTOR,
            },
        )
        .unwrap_or_else(|error| panic!("cannot build state: {error}"));
        Self {
            directory,
            api: http::router(state),
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
