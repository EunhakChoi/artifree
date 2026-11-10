//! Frame call refusals (decisions 0019, 0032, 0033 and 0045). A frame route
//! refuses a bad path before a bad body, a malformed body, a body over its
//! limit, and any method other than POST. Every refusal happens before an
//! artifact is looked up, so this harness creates no artifact.
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use artifree_providers::storage::SqliteStore;
use artifree_providers::tokens::SqliteTokenStore;
use artifree_server::auth::AccessToken;
use artifree_server::changes;
use artifree_server::http::{self, AppState, Settings};
use artifree_server::limits::RequestLimits;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use serde_json::Value;
use tower::ServiceExt;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const CONTENT_ORIGIN: &str = "http://127.0.0.1:7411";
/// A well-formed view identifier, so that only the case's own fault is refused.
const VIEW: &str = "0123456789abcdef0123456789abcdef";
/// Every frame route, by its path after the view and the generation.
const TAILS: [&str; 12] = [
    "metadata",
    "source/list",
    "source/read",
    "source/offer",
    "source/download",
    "source/write",
    "assets/list",
    "assets/read",
    "assets/offer",
    "assets/download",
    "assets/upload",
    "assets/delete",
];
/// The two routes with the larger write limit (1.5 MiB). A body over the frame
/// limit is accepted there and refused for the fields it lacks.
const WRITE_TAILS: [&str; 2] = ["source/write", "assets/upload"];
/// Above the 16 KiB frame body limit and below the 1.5 MiB write limit.
const OVERSIZED_INITIALIZATION_BYTES: usize = 20_000;
const INVALID_FRAME_CALL: &str = "the frame call is not a valid request";
const FRAME_CALL_TOO_LARGE: &str = "the frame call is too large";

/// Frame limits far above this file's traffic, so that no refusal is a budget
/// refusal.
fn plentiful() -> RequestLimits {
    RequestLimits {
        burst: 100_000,
        per_minute: 6_000_000,
        concurrent: 64,
    }
}

/// A server over a new `SQLite` store in a temporary directory, removed on drop.
struct Harness {
    directory: PathBuf,
    api: Router,
}

impl Harness {
    fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!(
            "artifree-runtime-frame-rejections-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        // The directory may be left by an earlier process with the same id.
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory)
            .unwrap_or_else(|error| panic!("cannot create test directory: {error}"));
        let store = SqliteStore::open(&directory.join("db.sqlite3"))
            .unwrap_or_else(|error| panic!("cannot open store: {error}"));
        let token_store = SqliteTokenStore::open(&directory.join("db.sqlite3"))
            .unwrap_or_else(|error| panic!("cannot open token store: {error}"));
        let token = AccessToken::parse(TOKEN).unwrap_or_else(|| panic!("fixture token is invalid"));
        let state = AppState::new(
            store,
            token_store,
            token,
            Settings {
                content_origin: CONTENT_ORIGIN.into(),
                preview_ttl: Duration::from_secs(60),
                frame_ancestors: "http://127.0.0.1:*".into(),
                limits: RequestLimits::default(),
                frame_limits: plentiful(),
                change_streams: changes::DEFAULT_STREAMS_PER_ACTOR,
                ..Settings::default()
            },
        )
        .unwrap_or_else(|error| panic!("cannot build state: {error}"));
        Self {
            directory,
            api: http::router(state),
        }
    }

    async fn send(&self, request: Request<Body>) -> Result<Reply, Box<dyn std::error::Error>> {
        let response: Response = self.api.clone().oneshot(request).await?;
        Reply::read(response).await
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// A response's status and body text. Assertions read the error object from
/// the text, so a body that is not an error body fails them with the body
/// shown.
struct Reply {
    status: StatusCode,
    text: String,
}

impl Reply {
    async fn read(response: Response) -> Result<Self, Box<dyn std::error::Error>> {
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await?;
        Ok(Self {
            status,
            text: String::from_utf8_lossy(&bytes).into_owned(),
        })
    }

    /// The `error` object of the contract's error body, or `null` for any
    /// other body.
    fn error(&self) -> Value {
        serde_json::from_str::<Value>(&self.text).map_or(Value::Null, |body| body["error"].clone())
    }
}

fn authorized(builder: axum::http::request::Builder) -> axum::http::request::Builder {
    builder.header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
}

/// A JSON POST with the account's bearer token.
fn post_raw(path: &str, body: String) -> Result<Request<Body>, axum::http::Error> {
    authorized(Request::post(path))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
}

/// Asserts that `reply` is a refusal with `status`, error `code` and error
/// `message`.
fn assert_refused(context: &str, reply: &Reply, status: StatusCode, code: &str, message: &str) {
    let error = reply.error();
    let body = &reply.text;
    assert_eq!(reply.status, status, "{context}: status; body {body}");
    assert_eq!(error["code"], code, "{context}: error code; body {body}");
    assert_eq!(
        error["message"], message,
        "{context}: error message; body {body}"
    );
}

/// The frame route at `tail` for artifact `a` at `revision`, view generation 1.
fn frame_path(revision: &str, tail: &str) -> String {
    format!("/v1/artifacts/a/revisions/{revision}/runtime-views/{VIEW}/1/{tail}")
}

/// An unparsable revision with an unparsable body is refused with the path
/// rejection, because the path is checked first.
async fn bad_path_is_refused_before_body(harness: &Harness, tail: &str) -> TestResult {
    let reply = harness
        .send(post_raw(&frame_path("x", tail), "{".to_owned())?)
        .await?;
    let error = reply.error();
    let body = &reply.text;
    assert_eq!(
        reply.status,
        StatusCode::BAD_REQUEST,
        "{tail}: status for a bad path; body {body}"
    );
    assert_eq!(
        error["code"], "invalid_request",
        "{tail}: code for a bad path; body {body}"
    );
    let message = error["message"].as_str().unwrap_or("");
    assert_ne!(
        message, INVALID_FRAME_CALL,
        "{tail}: the path is checked before the body, so the path rejection is the message"
    );
    assert!(
        message.contains("u64"),
        "{tail}: the path rejection must name the expected type u64, got {message:?}"
    );
    Ok(())
}

/// A body that is not JSON, on a well-formed path, is refused as an invalid
/// frame call.
async fn malformed_body_is_refused(harness: &Harness, tail: &str) -> TestResult {
    let reply = harness
        .send(post_raw(&frame_path("1", tail), "{".to_owned())?)
        .await?;
    assert_refused(
        tail,
        &reply,
        StatusCode::BAD_REQUEST,
        "invalid_request",
        INVALID_FRAME_CALL,
    );
    Ok(())
}

/// A body over the frame limit is refused as too large, except on the write
/// routes. There the limit accepts it, and it lacks fields they require, so it
/// is refused as an invalid frame call.
async fn oversized_body_is_refused(harness: &Harness, tail: &str) -> TestResult {
    let initialization = "0".repeat(OVERSIZED_INITIALIZATION_BYTES);
    let body = format!("{{\"initialization\":\"{initialization}\"}}");
    let reply = harness
        .send(post_raw(&frame_path("1", tail), body)?)
        .await?;
    if WRITE_TAILS.contains(&tail) {
        assert_refused(
            tail,
            &reply,
            StatusCode::BAD_REQUEST,
            "invalid_request",
            INVALID_FRAME_CALL,
        );
    } else {
        assert_refused(
            tail,
            &reply,
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            FRAME_CALL_TOO_LARGE,
        );
    }
    Ok(())
}

/// A GET on a frame route is refused with 405, as the route accepts only POST.
async fn other_methods_are_refused(harness: &Harness, tail: &str) -> TestResult {
    let request = authorized(Request::get(frame_path("1", tail))).body(Body::empty())?;
    let reply = harness.send(request).await?;
    let error = reply.error();
    let body = &reply.text;
    assert_eq!(
        reply.status,
        StatusCode::METHOD_NOT_ALLOWED,
        "{tail}: GET status; body {body}"
    );
    assert_eq!(
        error["code"], "invalid_request",
        "{tail}: GET error code; body {body}"
    );
    Ok(())
}

#[tokio::test]
async fn frame_routes_refuse_as_before() -> TestResult {
    let harness = Harness::new();
    for tail in TAILS {
        bad_path_is_refused_before_body(&harness, tail).await?;
        malformed_body_is_refused(&harness, tail).await?;
        oversized_body_is_refused(&harness, tail).await?;
        other_methods_are_refused(&harness, tail).await?;
    }
    Ok(())
}
