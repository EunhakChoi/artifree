//! The committed change feed, `GET /v1/changes`, through the real router:
//! events for commits only, resumption by cursor, a reset for a cursor the
//! server cannot continue from, authentication, the per-actor stream cap,
//! and shutdown.
use std::future::poll_fn;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use artifree_providers::storage::SqliteStore;
use artifree_server::auth::AccessToken;
use artifree_server::http::{self, AppState, Settings};
use artifree_server::limits::RequestLimits;
use axum::Router;
use axum::body::{Body, HttpBody, to_bytes};
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

type TestResult = Result<(), Box<dyn std::error::Error>>;
type TestError = Box<dyn std::error::Error>;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct Harness {
    directory: PathBuf,
    state: AppState,
    api: Router,
}

impl Harness {
    fn new(limits: RequestLimits) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!(
            "artifree-feed-{}-{}",
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
            },
        )
        .unwrap_or_else(|error| panic!("cannot build state: {error}"));
        Self {
            directory,
            api: http::router(state.clone()),
            state,
        }
    }

    async fn json(&self, request: Request<Body>) -> Result<(StatusCode, Value), TestError> {
        let response = self.api.clone().oneshot(request).await?;
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await?;
        Ok((status, serde_json::from_slice(&body).unwrap_or(Value::Null)))
    }

    /// Opens the feed, optionally after `cursor`, and returns its events.
    async fn open(&self, cursor: Option<&str>) -> Result<Events, TestError> {
        let mut request = Request::get("/v1/changes")
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .header(header::ACCEPT, "text/event-stream");
        if let Some(cursor) = cursor {
            request = request.header("last-event-id", cursor);
        }
        let response = self
            .api
            .clone()
            .oneshot(request.body(Body::empty())?)
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE),
            Some(&header::HeaderValue::from_static("text/event-stream"))
        );
        Ok(Events {
            body: response.into_body(),
            buffer: String::new(),
        })
    }

    async fn create(&self, key: &str) -> Result<(StatusCode, Value), TestError> {
        self.json(
            Request::post("/v1/artifacts")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/json")
                .header("idempotency-key", key)
                .body(Body::from(
                    json!({
                        "title": "Feed",
                        "files": [{ "path": "index.html", "encoding": "utf8", "content": "<p>1</p>" }]
                    })
                    .to_string(),
                ))?,
        )
        .await
    }

    async fn save(&self, id: &str, base: u64, key: &str) -> Result<(StatusCode, Value), TestError> {
        self.json(
            Request::post(format!("/v1/artifacts/{id}/revisions"))
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/json")
                .header("idempotency-key", key)
                .body(Body::from(
                    json!({
                        "base_revision": base,
                        "changes": [{ "op": "put", "path": "index.html", "encoding": "utf8", "content": key }]
                    })
                    .to_string(),
                ))?,
        )
        .await
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// One received server-sent event.
#[derive(Debug, PartialEq)]
struct Event {
    name: String,
    id: String,
    data: Value,
}

struct Events {
    body: Body,
    buffer: String,
}

impl Events {
    /// The next event, skipping comments; `None` when the stream ended.
    async fn next(&mut self) -> Result<Option<Event>, TestError> {
        loop {
            if let Some(end) = self.buffer.find("\n\n") {
                let block: String = self.buffer.drain(..end + 2).collect();
                let mut event = Event {
                    name: String::new(),
                    id: String::new(),
                    data: Value::Null,
                };
                for line in block.lines() {
                    if let Some(name) = line.strip_prefix("event: ") {
                        name.clone_into(&mut event.name);
                    } else if let Some(id) = line.strip_prefix("id: ") {
                        id.clone_into(&mut event.id);
                    } else if let Some(data) = line.strip_prefix("data: ") {
                        event.data = serde_json::from_str(data)?;
                    }
                }
                if !event.name.is_empty() {
                    return Ok(Some(event));
                }
                continue;
            }
            let frame = tokio::time::timeout(
                Duration::from_secs(5),
                poll_fn(|context| Pin::new(&mut self.body).poll_frame(context)),
            )
            .await
            .map_err(|_| "no event within 5 s")?;
            match frame {
                None => return Ok(None),
                Some(frame) => {
                    if let Ok(data) = frame?.into_data() {
                        self.buffer.push_str(std::str::from_utf8(&data)?);
                    }
                }
            }
        }
    }

    /// Whether nothing arrives for a while.
    async fn quiet(&mut self) -> bool {
        tokio::time::timeout(Duration::from_millis(300), self.next())
            .await
            .is_err()
    }
}

fn position(name: &str, cursor: u64) -> Event {
    Event {
        name: name.into(),
        id: cursor.to_string(),
        data: json!({ "cursor": cursor }),
    }
}

/// `(event, id, artifact, change, revision)` of a change event.
fn change(event: &Event) -> (&str, &str, &str, &str, u64) {
    (
        event.name.as_str(),
        event.id.as_str(),
        event.data["artifact_id"].as_str().unwrap_or(""),
        event.data["change"].as_str().unwrap_or(""),
        event.data["revision"].as_u64().unwrap_or(0),
    )
}

#[tokio::test]
async fn commits_appear_once_in_order_and_retries_or_conflicts_do_not() -> TestResult {
    let harness = Harness::new(RequestLimits::default());
    let mut events = harness.open(None).await?;
    assert_eq!(events.next().await?, Some(position("ready", 0)));

    let (status, created) = harness.create("feed-create-1").await?;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["artifact"]["id"].as_str().ok_or("no id")?;
    let first = events.next().await?.ok_or("stream ended")?;
    assert_eq!(change(&first), ("change", "1", id, "created", 1));
    assert_eq!(
        first.data["committed_at_ms"],
        created["artifact"]["updated_at_ms"]
    );

    // A replayed create and a stale save write nothing and send nothing.
    assert_eq!(harness.create("feed-create-1").await?.1["replayed"], true);
    assert_eq!(
        harness.save(id, 2, "feed-stale").await?.0,
        StatusCode::CONFLICT
    );
    assert!(events.quiet().await, "a retry or conflict sent an event");

    assert_eq!(
        harness.save(id, 1, "feed-save-2").await?.0,
        StatusCode::CREATED
    );
    let second = events.next().await?.ok_or("stream ended")?;
    assert_eq!(change(&second), ("change", "2", id, "revised", 2));
    assert_eq!(
        harness.save(id, 1, "feed-save-2").await?.1["replayed"],
        true
    );
    assert!(events.quiet().await, "a replayed save sent an event");
    Ok(())
}

#[tokio::test]
async fn a_stream_resumes_after_its_cursor_with_changes_made_meanwhile() -> TestResult {
    let harness = Harness::new(RequestLimits::default());
    let (_, created) = harness.create("resume-create").await?;
    let id = created["artifact"]["id"].as_str().ok_or("no id")?;
    harness.save(id, 1, "resume-2").await?;
    harness.save(id, 2, "resume-3").await?;

    // Resume after cursor 1: the two saves made while away arrive in order.
    let mut events = harness.open(Some("1")).await?;
    assert_eq!(
        change(&events.next().await?.ok_or("ended")?),
        ("change", "2", id, "revised", 2)
    );
    assert_eq!(
        change(&events.next().await?.ok_or("ended")?),
        ("change", "3", id, "revised", 3)
    );
    assert!(events.quiet().await);
    // Resuming at the head sends nothing until the next commit.
    let mut caught_up = harness.open(Some("3")).await?;
    assert!(caught_up.quiet().await);
    harness.save(id, 3, "resume-4").await?;
    assert_eq!(
        change(&caught_up.next().await?.ok_or("ended")?),
        ("change", "4", id, "revised", 4)
    );
    Ok(())
}

#[tokio::test]
async fn a_cursor_beyond_the_head_is_reset_to_the_head() -> TestResult {
    let harness = Harness::new(RequestLimits::default());
    let (_, created) = harness.create("reset-create").await?;
    let id = created["artifact"]["id"].as_str().ok_or("no id")?;
    // A cursor from another database: this one only reached 1.
    let mut events = harness.open(Some("40")).await?;
    assert_eq!(events.next().await?, Some(position("reset", 1)));
    assert_eq!(
        harness.save(id, 1, "reset-save-2").await?.0,
        StatusCode::CREATED
    );
    assert_eq!(
        change(&events.next().await?.ok_or("ended")?),
        ("change", "2", id, "revised", 2)
    );
    Ok(())
}

#[tokio::test]
async fn the_feed_requires_the_token_and_a_valid_cursor() -> TestResult {
    let harness = Harness::new(RequestLimits::default());
    let (status, body) = harness
        .json(Request::get("/v1/changes").body(Body::empty())?)
        .await?;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "unauthenticated");
    // The token is never read from the URL.
    let (status, _) = harness
        .json(Request::get(format!("/v1/changes?access_token={TOKEN}")).body(Body::empty())?)
        .await?;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    for cursor in ["", "-1", "1.5", "x", "99999999999999999999999"] {
        let (status, body) = harness
            .json(
                Request::get("/v1/changes")
                    .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                    .header("last-event-id", cursor)
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "cursor {cursor:?}");
        assert_eq!(body["error"]["code"], "invalid_request");
    }
    Ok(())
}

#[tokio::test]
async fn open_streams_are_capped_per_actor_and_free_their_slot_when_closed() -> TestResult {
    let harness = Harness::new(RequestLimits {
        concurrent: 1,
        streams: 1,
        ..RequestLimits::default()
    });
    let mut open = harness.open(None).await?;
    assert_eq!(open.next().await?, Some(position("ready", 0)));
    // An open stream is not a running request: other requests still run.
    let (status, _) = harness.create("cap-create").await?;
    assert_eq!(status, StatusCode::CREATED);

    let refused = harness
        .api
        .clone()
        .oneshot(
            Request::get("/v1/changes")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        refused.headers().get(header::RETRY_AFTER),
        Some(&header::HeaderValue::from_static("5"))
    );
    let body: Value = serde_json::from_slice(&to_bytes(refused.into_body(), usize::MAX).await?)?;
    assert_eq!(body["error"]["code"], "rate_limited");

    // Closing the stream frees its slot.
    drop(open);
    let mut reopened = None;
    for _ in 0..100 {
        let response = harness
            .api
            .clone()
            .oneshot(
                Request::get("/v1/changes")
                    .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                    .body(Body::empty())?,
            )
            .await?;
        if response.status() == StatusCode::OK {
            reopened = Some(response);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(reopened.is_some(), "the closed stream kept its slot");
    Ok(())
}

#[tokio::test]
async fn closing_the_streams_ends_them_for_shutdown() -> TestResult {
    let harness = Harness::new(RequestLimits::default());
    let mut events = harness.open(None).await?;
    assert_eq!(events.next().await?, Some(position("ready", 0)));
    harness.state.close_streams();
    assert_eq!(events.next().await?, None, "the stream must end");
    Ok(())
}
