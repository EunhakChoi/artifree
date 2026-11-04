//! The committed change feed through the real router: a stream starts at the
//! head or resumes after a cursor, sends each commit once and nothing for
//! replays or conflicts, resets a foreign cursor, and is bounded per actor
//! and ended by shutdown.
use std::future::poll_fn;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use artifree_providers::backup;
use artifree_providers::storage::{DATABASE_FILE, SqliteStore};
use artifree_server::auth::AccessToken;
use artifree_server::http::{self, AppState, Settings};
use artifree_server::limits::RequestLimits;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
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
        let directory = Self::fresh_path();
        std::fs::create_dir_all(&directory)
            .unwrap_or_else(|error| panic!("cannot create test directory: {error}"));
        Self::over(directory, change_streams)
    }

    fn fresh_path() -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!(
            "artifree-feed-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&directory);
        directory
    }

    /// A server over the database in a copy of this one's data directory,
    /// made by backup and restore.
    fn restored(&self, change_streams: u32) -> Self {
        let output = Self::fresh_path();
        backup::backup(&self.directory, &output)
            .unwrap_or_else(|error| panic!("cannot back up: {error}"));
        let directory = Self::fresh_path();
        backup::restore(&output, &directory)
            .unwrap_or_else(|error| panic!("cannot restore: {error}"));
        let _ = std::fs::remove_file(&output);
        Self::over(directory, change_streams)
    }

    fn over(directory: PathBuf, change_streams: u32) -> Self {
        let database = directory.join(DATABASE_FILE);
        let store = SqliteStore::open(&database)
            .unwrap_or_else(|error| panic!("cannot open store: {error}"));
        let token_store = artifree_providers::tokens::SqliteTokenStore::open(&database)
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
                ..Settings::default()
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

    /// A tool call on `/mcp` as the owner, and its JSON-RPC result. The real
    /// server records the socket peer, which `oneshot` has not, so the call
    /// comes from loopback.
    async fn tool(&self, name: &str, arguments: Value) -> Result<Value, TestError> {
        let message = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        });
        let request = Request::post("/mcp")
            .header(header::HOST, "127.0.0.1:7410")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header("mcp-protocol-version", "2025-11-25")
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .body(Body::from(message.to_string()))?;
        let response = self
            .api
            .clone()
            .layer(axum::Extension(ConnectInfo(SocketAddr::from((
                [127, 0, 0, 1],
                40_000,
            )))))
            .oneshot(request)
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let reply: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await?)?;
        Ok(reply["result"].clone())
    }

    async fn put_data(
        &self,
        id: &str,
        key: &str,
        base: u64,
        path: &str,
        value: &Value,
    ) -> Result<Value, TestError> {
        let body = json!({ "path": path, "base_data_version": base, "value": value });
        self.send(Request::put(format!("/v1/artifacts/{id}/data")), key, body)
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

/// A purge is announced once, with the new metadata revision; a client that
/// was away catches up through the earlier changes to it, and after a restore
/// of a database holding the purge the sequence goes on.
#[tokio::test]
async fn a_purge_is_announced_once_and_a_client_behind_catches_up_to_it() -> TestResult {
    let harness = Harness::new(4);
    let created = harness.create("feed-create-1").await?;
    let id = created["artifact"]["id"]
        .as_str()
        .ok_or("no id")?
        .to_owned();
    let mut events = harness.follow(None).await?;
    let ready = events.expect("ready").await?;
    harness.set_deleted(&id, "feed-delete-1", 1, true).await?;
    let deleted = events.expect("change").await?;
    let purge = |key: &'static str, base: u64| {
        harness.send(
            Request::post(format!("/v1/artifacts/{id}/purge")),
            key,
            json!({ "base_metadata_revision": base }),
        )
    };

    assert_eq!(purge("feed-purge-1", 2).await?["replayed"], false);

    let purged = events.expect("change").await?;
    assert_eq!(
        (
            &purged.data["change"],
            &purged.data["artifact_id"],
            &purged.data["revision"],
            &purged.data["metadata_revision"]
        ),
        (&json!("purged"), &json!(id), &json!(1), &json!(3))
    );
    assert_eq!(sequence(&purged.id), sequence(&deleted.id) + 1);
    // A replay, and a purge refused as not found, send nothing.
    assert_eq!(purge("feed-purge-1", 2).await?["replayed"], true);
    assert_eq!(
        purge("feed-purge-2", 3).await?["error"]["code"],
        "not_found"
    );
    assert!(events.quiet().await, "a replay or a refusal was announced");

    // A client that last saw the create is told what happened since, in order.
    let (feed, _) = ready.id.split_once('.').ok_or("not a cursor")?;
    let mut behind = harness.follow(Some(&format!("{feed}.1"))).await?;
    let kinds = [
        behind.expect("change").await?.data["change"].clone(),
        behind.expect("change").await?.data["change"].clone(),
    ];
    assert_eq!(kinds, [json!("deleted"), json!("purged")]);
    assert!(behind.quiet().await);
    // The cursor of the purge is still the feed's head after a restore, which
    // renews the identity and keeps the sequence.
    let restored = harness.restored(4);
    let mut reset = restored.follow(Some(&purged.id)).await?;
    let event = reset.expect("reset").await?;
    assert_eq!(event.id.split_once('.').map(|(_, head)| head), Some("3"));
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

/// A restore gives the feed a new identity (decision 0006): a client that
/// followed the original is told to reload, and the sequence goes on.
#[tokio::test]
async fn a_cursor_of_the_original_database_is_reset_by_its_restore() -> TestResult {
    let original = Harness::new(4);
    let mut events = original.follow(None).await?;
    let ready = events.expect("ready").await?;
    let id = original.create("feed-create-1").await?["artifact"]["id"]
        .as_str()
        .ok_or("no id")?
        .to_owned();
    original.save(&id, "feed-save-2", 1).await?;
    let (old_feed, _) = ready.id.split_once('.').ok_or("not a cursor")?;
    let followed = format!("{old_feed}.2");
    drop(events);
    let mut same = original.follow(Some(&followed)).await?;
    assert!(same.quiet().await, "the original still accepts its cursor");
    drop(same);

    let restored = original.restored(4);

    let mut reset = restored.follow(Some(&followed)).await?;
    let event = reset.expect("reset").await?;
    let (new_feed, head) = event.id.split_once('.').ok_or("not a cursor")?;
    assert_ne!(new_feed, old_feed, "the restored feed has a new identity");
    assert_eq!(head, "2", "the sequence head did not go back");
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

/// A data write that changed the space is announced to the writer's stream at
/// once, over HTTP and over MCP: the stream gets it long before the 15-second
/// reread would find it. The event names the artifact's current positions and
/// the version the write left, and carries no path or value.
#[tokio::test]
async fn a_data_write_over_http_or_mcp_reaches_the_stream_at_once() -> TestResult {
    let harness = Harness::new(4);
    let created = harness.create("feed-create-1").await?;
    let id = created["artifact"]["id"]
        .as_str()
        .ok_or("no id")?
        .to_owned();
    harness.save(&id, "feed-save-2", 1).await?;
    let mut events = harness.follow(None).await?;
    let ready = events.expect("ready").await?;
    let head = sequence(&ready.id);

    let written = harness
        .put_data(
            &id,
            "feed-data-1",
            0,
            "todos/first",
            &json!({ "title": "secret" }),
        )
        .await?;
    assert_eq!(written["data_version"], 1, "{written}");
    let over_http = events.expect("change").await?;
    assert_eq!(sequence(&over_http.id), head + 1);
    let mut shown = over_http.data.clone();
    let committed_at = shown
        .as_object_mut()
        .and_then(|object| object.remove("committed_at_ms"))
        .ok_or("no commit time")?;
    assert!(
        committed_at.as_i64().is_some_and(|ms| ms > 0),
        "{committed_at}"
    );
    assert_eq!(
        shown,
        json!({
            "cursor": over_http.id,
            "artifact_id": id,
            "change": "data_changed",
            "revision": 2,
            "metadata_revision": 1,
            "data_version": 1,
        }),
        "the revisions are the artifact's, and the data version is its own"
    );
    assert!(
        !over_http.data.to_string().contains("secret")
            && !over_http.data.to_string().contains("todos"),
        "an event carries no path and no value: {}",
        over_http.data
    );

    let through_mcp = harness
        .tool(
            "set_data",
            json!({
                "artifact_id": id, "path": "todos/second", "base_data_version": 1,
                "value": "2", "idempotency_key": "feed-mcp-data-1"
            }),
        )
        .await?;
    assert_eq!(through_mcp["isError"], false, "{through_mcp}");
    let over_mcp = events.expect("change").await?;
    assert_eq!(sequence(&over_mcp.id), head + 2);
    assert_eq!(over_mcp.data["change"], "data_changed");
    assert_eq!(over_mcp.data["data_version"], 2);
    assert_eq!(over_mcp.data["revision"], 2);

    // A cursor from before the writes catches up on them, in order.
    let (feed, _) = ready.id.split_once('.').ok_or("not a cursor")?;
    let mut behind = harness.follow(Some(&format!("{feed}.{head}"))).await?;
    let versions = [
        behind.expect("change").await?.data["data_version"].clone(),
        behind.expect("change").await?.data["data_version"].clone(),
    ];
    assert_eq!(versions, [json!(1), json!(2)]);
    Ok(())
}

/// Nothing is announced for a replay of a data write, a write that changed
/// nothing, a stale base, a path through a scalar, or an unknown artifact: they
/// record no row (decision 0040).
#[tokio::test]
async fn a_replayed_unchanged_or_refused_data_write_is_not_announced() -> TestResult {
    let harness = Harness::new(4);
    let created = harness.create("feed-create-1").await?;
    let id = created["artifact"]["id"]
        .as_str()
        .ok_or("no id")?
        .to_owned();
    let mut events = harness.follow(None).await?;
    events.expect("ready").await?;
    let value = json!({ "title": "secret" });
    harness
        .put_data(&id, "feed-data-1", 0, "todos/first", &value)
        .await?;
    events.expect("change").await?;
    harness
        .put_data(&id, "feed-data-2", 1, "todos/second", &json!(2))
        .await?;
    events.expect("change").await?;

    // Nothing is announced for a replay, a write that changed nothing, a stale
    // base, a refused path, or an unknown artifact.
    let replay = harness
        .put_data(
            &id,
            "feed-data-1",
            0,
            "todos/first",
            &json!({ "title": "secret" }),
        )
        .await?;
    assert_eq!(replay["replayed"], true);
    let no_op = harness
        .send(
            Request::delete(format!("/v1/artifacts/{id}/data")),
            "feed-data-noop",
            json!({ "path": "nothing/here", "base_data_version": 2 }),
        )
        .await?;
    assert_eq!(no_op["changed"], false, "{no_op}");
    let stale = harness
        .put_data(&id, "feed-data-stale", 1, "x", &json!(1))
        .await?;
    assert_eq!(stale["error"]["code"], "data_conflict");
    let through = harness
        .put_data(
            &id,
            "feed-data-through",
            2,
            "todos/first/title/deeper",
            &json!(1),
        )
        .await?;
    assert_eq!(through["error"]["code"], "data_path_conflict");
    let missing = harness
        .put_data(
            &format!("art_{}", "0".repeat(32)),
            "feed-data-missing",
            0,
            "x",
            &json!(1),
        )
        .await?;
    assert_eq!(missing["error"]["code"], "not_found");
    assert!(
        events.quiet().await,
        "a replay, a no-op, or a refusal was announced"
    );

    Ok(())
}
