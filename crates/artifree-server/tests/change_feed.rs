//! The committed change feed through the real router: a stream starts at the
//! head or resumes after a cursor, sends each commit once and nothing for
//! replays or conflicts, resets a foreign cursor, and is bounded per actor
//! and ended by shutdown.
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
use axum::body::{Body, to_bytes};
use axum::http::{HeaderValue, Request, Response, StatusCode, header};
use http_body::Body as _;
use serde_json::{Value, json};
use tower::ServiceExt;

type TestError = Box<dyn std::error::Error>;
type TestResult = Result<(), TestError>;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct Harness {
    directory: PathBuf,
    state: AppState,
    api: Router,
}

impl Harness {
    fn new(change_streams: u32) -> Self {
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
        let token_store =
            artifree_providers::tokens::SqliteTokenStore::open(&directory.join("db.sqlite3"))
                .unwrap_or_else(|error| panic!("cannot open token store: {error}"));
        let token = AccessToken::parse(TOKEN).unwrap_or_else(|| panic!("fixture token is invalid"));
        let state = AppState::new(
            store,
            token_store,
            token,
            Settings {
                content_origin: "http://127.0.0.1:7411".into(),
                preview_ttl: Duration::from_secs(60),
                frame_ancestors: "http://127.0.0.1:*".into(),
                limits: RequestLimits::default(),
                change_streams,
            },
        )
        .unwrap_or_else(|error| panic!("cannot build state: {error}"));
        Self {
            directory,
            api: http::router(state.clone()),
            state,
        }
    }

    /// Opens a stream, resuming after `cursor` when given.
    async fn open(&self, cursor: Option<&str>) -> Result<Response<Body>, TestError> {
        let mut request = Request::get("/v1/changes")
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .header(header::ACCEPT, "text/event-stream");
        if let Some(cursor) = cursor {
            request = request.header("last-event-id", cursor);
        }
        Ok(self
            .api
            .clone()
            .oneshot(request.body(Body::empty())?)
            .await?)
    }

    async fn follow(&self, cursor: Option<&str>) -> Result<Events, TestError> {
        let response = self.open(cursor).await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .map(HeaderValue::as_bytes),
            Some(b"text/event-stream".as_slice())
        );
        Ok(Events {
            body: response.into_body(),
            buffer: String::new(),
        })
    }

    async fn commit(&self, uri: &str, key: &str, body: Value) -> Result<Value, TestError> {
        self.send(Request::post(uri), key, body).await
    }

    async fn send(
        &self,
        request: axum::http::request::Builder,
        key: &str,
        body: Value,
    ) -> Result<Value, TestError> {
        let request = request
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header("idempotency-key", key)
            .body(Body::from(body.to_string()))?;
        let response = self.api.clone().oneshot(request).await?;
        Ok(serde_json::from_slice(
            &to_bytes(response.into_body(), usize::MAX).await?,
        )?)
    }

    async fn create(&self, key: &str) -> Result<Value, TestError> {
        let page = json!({
            "title": "Followed",
            "files": [{ "path": "index.html", "encoding": "utf8", "content": "<p>1</p>" }]
        });
        self.commit("/v1/artifacts", key, page).await
    }

    async fn save(&self, id: &str, key: &str, base: u64) -> Result<Value, TestError> {
        let changes = json!({
            "base_revision": base,
            "changes": [{ "op": "put", "path": "index.html", "encoding": "utf8", "content": key }]
        });
        self.commit(&format!("/v1/artifacts/{id}/revisions"), key, changes)
            .await
    }

    async fn rename(
        &self,
        id: &str,
        key: &str,
        base: u64,
        title: &str,
    ) -> Result<Value, TestError> {
        let change = json!({ "base_metadata_revision": base, "title": title });
        self.send(Request::patch(format!("/v1/artifacts/{id}")), key, change)
            .await
    }

    async fn set_deleted(
        &self,
        id: &str,
        key: &str,
        base: u64,
        deleted: bool,
    ) -> Result<Value, TestError> {
        let change = json!({ "base_metadata_revision": base, "deleted": deleted });
        self.send(Request::patch(format!("/v1/artifacts/{id}")), key, change)
            .await
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// One server-sent event.
#[derive(Debug)]
struct Event {
    name: String,
    id: String,
    data: Value,
}

/// Reads the events of one stream.
struct Events {
    body: Body,
    buffer: String,
}

impl Events {
    /// The next event, skipping comment lines; `None` when the stream ended.
    async fn next(&mut self) -> Result<Option<Event>, TestError> {
        loop {
            if let Some(end) = self.buffer.find("\n\n") {
                let block: String = self.buffer.drain(..end + 2).collect();
                let field = |name: &str| {
                    block
                        .lines()
                        .find_map(|line| line.strip_prefix(name)?.strip_prefix(": "))
                        .map(str::to_owned)
                };
                let Some(name) = field("event") else {
                    continue;
                };
                return Ok(Some(Event {
                    name,
                    id: field("id").ok_or("an event without id")?,
                    data: serde_json::from_str(&field("data").ok_or("an event without data")?)?,
                }));
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

    async fn expect(&mut self, name: &str) -> Result<Event, TestError> {
        let event = self.next().await?.ok_or("the stream ended")?;
        assert_eq!(event.name, name, "{event:?}");
        assert_eq!(
            event.data["cursor"],
            Value::from(event.id.clone()),
            "{event:?}"
        );
        Ok(event)
    }

    /// Whether no event arrives for a moment.
    async fn quiet(&mut self) -> bool {
        tokio::time::timeout(Duration::from_millis(300), self.next())
            .await
            .is_err()
    }
}

fn sequence(cursor: &str) -> u64 {
    cursor
        .rsplit_once('.')
        .and_then(|(_, number)| number.parse().ok())
        .unwrap_or_else(|| panic!("not a cursor: {cursor}"))
}

#[tokio::test]
async fn a_stream_starts_at_the_head_and_sends_each_commit_once() -> TestResult {
    let harness = Harness::new(4);
    let mut events = harness.follow(None).await?;
    let ready = events.expect("ready").await?;
    assert_eq!(sequence(&ready.id), 0);

    let created = harness.create("feed-create-1").await?;
    let id = created["artifact"]["id"].as_str().ok_or("no id")?;
    let change = events.expect("change").await?;
    assert_eq!(sequence(&change.id), 1);
    assert_eq!(
        change.data,
        json!({
            "cursor": change.id,
            "artifact_id": id,
            "change": "created",
            "revision": 1,
            "metadata_revision": 1,
            "committed_at_ms": created["revision"]["created_at_ms"],
        })
    );

    // A replay and a conflict commit nothing and send nothing.
    assert_eq!(harness.create("feed-create-1").await?["replayed"], true);
    assert_eq!(
        harness.save(id, "feed-stale-1", 2).await?["error"]["code"],
        "revision_conflict"
    );
    assert!(events.quiet().await, "a replay or conflict was announced");

    harness.save(id, "feed-save-2", 1).await?;
    let revised = events.expect("change").await?;
    assert_eq!(
        (sequence(&revised.id), revised.data["change"].clone()),
        (2, json!("revised"))
    );
    assert_eq!(revised.data["revision"], 2);

    // A metadata change is announced with both revisions; one that changes
    // nothing, and a stale one, are not.
    harness.rename(id, "feed-rename-1", 1, "Renamed").await?;
    let renamed = events.expect("change").await?;
    assert_eq!(renamed.data["change"], "metadata_updated");
    assert_eq!(renamed.data["revision"], 2);
    assert_eq!(renamed.data["metadata_revision"], 2);
    harness.rename(id, "feed-rename-2", 2, "Renamed").await?;
    assert_eq!(
        harness.rename(id, "feed-rename-3", 1, "Stale").await?["error"]["code"],
        "metadata_conflict"
    );
    assert!(
        events.quiet().await,
        "an update without effect was announced"
    );
    Ok(())
}

#[tokio::test]
async fn deletes_and_recoveries_are_announced_as_such() -> TestResult {
    let harness = Harness::new(4);
    let created = harness.create("feed-create-1").await?;
    let id = created["artifact"]["id"].as_str().ok_or("no id")?;
    let mut events = harness.follow(None).await?;
    events.expect("ready").await?;

    harness.set_deleted(id, "feed-delete-1", 1, true).await?;
    let deleted = events.expect("change").await?;
    assert_eq!(
        (
            &deleted.data["change"],
            &deleted.data["revision"],
            &deleted.data["metadata_revision"]
        ),
        (&json!("deleted"), &json!(1), &json!(2))
    );
    // A write refused in the trash commits nothing and sends nothing.
    assert_eq!(
        harness.save(id, "feed-save-1", 1).await?["error"]["code"],
        "not_found"
    );
    assert!(events.quiet().await, "a refused write was announced");

    harness.set_deleted(id, "feed-recover-1", 2, false).await?;
    let recovered = events.expect("change").await?;
    assert_eq!(
        (
            &recovered.data["change"],
            &recovered.data["metadata_revision"]
        ),
        (&json!("recovered"), &json!(3))
    );
    Ok(())
}

#[tokio::test]
async fn a_stream_resumes_after_its_cursor_without_ready() -> TestResult {
    let harness = Harness::new(4);
    let mut first = harness.follow(None).await?;
    first.expect("ready").await?;
    let id = harness.create("feed-create-1").await?["artifact"]["id"]
        .as_str()
        .ok_or("no id")?
        .to_owned();
    let seen = first.expect("change").await?;
    drop(first);
    harness.save(&id, "feed-save-2", 1).await?;
    harness.save(&id, "feed-save-3", 2).await?;

    let mut resumed = harness.follow(Some(&seen.id)).await?;
    let missed = [
        resumed.expect("change").await?,
        resumed.expect("change").await?,
    ];
    assert_eq!(missed.map(|event| sequence(&event.id)), [2, 3]);
    assert!(resumed.quiet().await);
    Ok(())
}

#[tokio::test]
async fn a_cursor_of_another_feed_or_beyond_the_head_is_reset() -> TestResult {
    let harness = Harness::new(4);
    let mut events = harness.follow(None).await?;
    let ready = events.expect("ready").await?;
    harness.create("feed-create-1").await?;
    let (feed, _) = ready.id.split_once('.').ok_or("not a cursor")?;

    let other_feed = format!("{}.1", "0".repeat(32));
    let beyond = format!("{feed}.9");
    assert_ne!(feed, "0".repeat(32));
    for cursor in [other_feed, beyond] {
        let mut reset = harness.follow(Some(&cursor)).await?;
        let event = reset.expect("reset").await?;
        assert_eq!(
            event.id,
            format!("{feed}.1"),
            "reset to the head for {cursor}"
        );
        assert!(reset.quiet().await);
    }
    Ok(())
}

#[tokio::test]
async fn an_invalid_cursor_or_missing_header_token_is_refused() -> TestResult {
    let harness = Harness::new(1);
    for cursor in ["7", "", "not-a-cursor"] {
        let response = harness.open(Some(cursor)).await?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{cursor:?}");
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await?)?;
        assert_eq!(body["error"]["code"], "invalid_request");
    }
    // A token in the URL is not a credential.
    let request = Request::get(format!("/v1/changes?access_token={TOKEN}")).body(Body::empty())?;
    let response = harness.api.clone().oneshot(request).await?;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    // Refused requests took no stream slot.
    let mut events = harness.follow(None).await?;
    events.expect("ready").await?;
    Ok(())
}

#[tokio::test]
async fn open_streams_are_limited_per_actor_until_one_closes() -> TestResult {
    let harness = Harness::new(2);
    let mut first = harness.follow(None).await?;
    let mut second = harness.follow(None).await?;
    first.expect("ready").await?;
    second.expect("ready").await?;

    let refused = harness.open(None).await?;
    assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        refused
            .headers()
            .get(header::RETRY_AFTER)
            .map(HeaderValue::as_bytes),
        Some(b"5".as_slice())
    );
    let body: Value = serde_json::from_slice(&to_bytes(refused.into_body(), usize::MAX).await?)?;
    assert_eq!(body["error"]["code"], "rate_limited");

    // The reader leaves; its slot is free once the producer notices.
    drop(first);
    for _ in 0..50 {
        let response = harness.open(None).await?;
        if response.status() == StatusCode::OK {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Err("the closed stream kept its slot".into())
}

#[tokio::test]
async fn closing_ends_open_streams_and_later_ones() -> TestResult {
    let harness = Harness::new(4);
    let mut open = harness.follow(None).await?;
    open.expect("ready").await?;
    harness.state.close_streams();
    assert!(
        open.next().await?.is_none(),
        "an open stream outlived the close"
    );

    let mut later = harness.follow(None).await?;
    assert!(
        later.next().await?.is_none(),
        "a stream opened after the close sent an event"
    );
    Ok(())
}
