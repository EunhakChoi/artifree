//! The data members query and the data subscription (decision 0040) through
//! the real router, core, and `SQLite` store.

use std::future::poll_fn;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use artifree_contracts::api::CreateTokenResponse;
use artifree_providers::storage::SqliteStore;
use artifree_server::auth::AccessToken;
use artifree_server::data_stream::DataStreamSettings;
use artifree_server::http::{self, AppState, Settings};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, Response, StatusCode, header};
use http_body::Body as _;
use serde_json::{Value, json};
use tower::ServiceExt;

type TestError = Box<dyn std::error::Error>;
type TestResult = Result<(), TestError>;

const OWNER: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct Harness {
    directory: PathBuf,
    state: AppState,
    api: Router,
}

struct Reply {
    status: StatusCode,
    text: String,
}

impl Reply {
    fn json(&self) -> Result<Value, TestError> {
        Ok(serde_json::from_str(&self.text)?)
    }
}

impl Harness {
    fn new() -> Result<Self, TestError> {
        Self::with(DataStreamSettings::default())
    }

    fn with(settings: DataStreamSettings) -> Result<Self, TestError> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!(
            "artifree-data-stream-it-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _removed = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory)?;
        let database = directory.join("db.sqlite3");
        let state = AppState::new(
            SqliteStore::open(&database)?,
            artifree_providers::tokens::SqliteTokenStore::open(&database)?,
            AccessToken::parse(OWNER).ok_or("fixture token")?,
            Settings {
                content_origin: "http://127.0.0.1:7411".into(),
                ..Settings::default()
            },
        )?;
        state.set_data_stream_settings(settings);
        Ok(Self {
            directory,
            api: http::router(state.clone()),
            state,
        })
    }

    async fn send(
        &self,
        method: &str,
        path: &str,
        token: &str,
        key: Option<&str>,
        body: Option<&Value>,
    ) -> Result<Reply, TestError> {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(header::AUTHORIZATION, format!("Bearer {token}"));
        if let Some(key) = key {
            builder = builder.header("idempotency-key", format!("test-key-{key}"));
        }
        let request = match body {
            Some(body) => builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))?,
            None => builder.body(Body::empty())?,
        };
        let response = self.api.clone().oneshot(request).await?;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await?;
        Ok(Reply {
            status,
            text: String::from_utf8_lossy(&bytes).into_owned(),
        })
    }

    async fn artifact_of(&self, token: &str, name: &str) -> Result<String, TestError> {
        let created = self
            .send(
                "POST",
                "/v1/artifacts",
                token,
                Some(&format!("create-{name}")),
                Some(&json!({ "title": name, "files": [
                    { "path": "index.html", "encoding": "utf8", "content": "x" }] })),
            )
            .await?;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
        Ok(created.json()?["artifact"]["id"]
            .as_str()
            .ok_or("artifact id")?
            .to_owned())
    }

    async fn artifact(&self, name: &str) -> Result<String, TestError> {
        self.artifact_of(OWNER, name).await
    }

    /// Sets `path` to `value`, expecting the space to be at `base`.
    async fn set(
        &self,
        token: &str,
        id: &str,
        path: &str,
        base: u64,
        value: &Value,
    ) -> Result<Value, TestError> {
        let reply = self
            .send(
                "PUT",
                &format!("/v1/artifacts/{id}/data"),
                token,
                Some(&format!("set-{id}-{base}")),
                Some(&json!({ "path": path, "base_data_version": base, "value": value })),
            )
            .await?;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
        reply.json()
    }

    async fn members(&self, id: &str, query: &str) -> Result<Reply, TestError> {
        self.send(
            "GET",
            &format!("/v1/artifacts/{id}/data/members{query}"),
            OWNER,
            None,
            None,
        )
        .await
    }

    async fn open(&self, token: &str, id: &str, query: &str) -> Result<Response<Body>, TestError> {
        Ok(self
            .api
            .clone()
            .oneshot(
                Request::get(format!("/v1/artifacts/{id}/data/events{query}"))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())?,
            )
            .await?)
    }

    async fn follow(&self, token: &str, id: &str, query: &str) -> Result<Events, TestError> {
        let response = self.open(token, id, query).await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/event-stream")
        );
        Ok(Events {
            body: response.into_body(),
            buffer: String::new(),
        })
    }

    async fn issue(&self, user: Option<&str>) -> Result<CreateTokenResponse, TestError> {
        let mut body = json!({ "name": "agent" });
        if let Some(user) = user {
            body["user_id"] = json!(user);
        }
        let reply = self
            .send("POST", "/v1/tokens", OWNER, None, Some(&body))
            .await?;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
        Ok(serde_json::from_str(&reply.text)?)
    }

    /// Changes the stored space the way another process could.
    fn sql(&self, statement: &str, id: &str) -> TestResult {
        let connection = rusqlite::Connection::open(self.directory.join("db.sqlite3"))?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute(statement, [id])?;
        Ok(())
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.directory);
    }
}

#[derive(Debug)]
struct Event {
    name: String,
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
        Ok(event)
    }

    async fn ended(&mut self) -> TestResult {
        let next = self.next().await?;
        assert!(next.is_none(), "the stream should have ended: {next:?}");
        Ok(())
    }
}

fn quick() -> DataStreamSettings {
    DataStreamSettings {
        throttle: Duration::from_millis(10),
        ..DataStreamSettings::default()
    }
}

// Members ------------------------------------------------------------------

#[tokio::test]
async fn members_page_in_byte_order_with_a_cursor_and_the_normalized_query() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("pages").await?;
    harness
        .set(
            OWNER,
            &id,
            "list",
            0,
            &json!({ "é": 1, "a": 2, "Z": 3, "b": 4, "c": 5 }),
        )
        .await?;

    let first = harness.members(&id, "?path=list&limit=2").await?;
    assert_eq!(first.status, StatusCode::OK, "{}", first.text);
    assert_eq!(
        first.json()?,
        json!({
            "artifact_id": id, "path": "list", "select": "names", "limit": 2,
            "data_version": 1, "exists": true,
            "members": [{ "name": "Z" }, { "name": "a" }], "next_after": "a"
        })
    );
    let second = harness
        .members(&id, "?path=list&limit=2&after=a")
        .await?
        .json()?;
    assert_eq!(second["members"], json!([{ "name": "b" }, { "name": "c" }]));
    assert_eq!(second["after"], "a");
    assert_eq!(second["next_after"], "c");
    let last = harness
        .members(&id, "?path=list&limit=2&after=c")
        .await?
        .json()?;
    assert_eq!(last["members"], json!([{ "name": "é" }]));
    assert!(last.get("next_after").is_none(), "{last}");
    Ok(())
}

#[tokio::test]
async fn values_are_sent_unless_a_member_is_over_its_bound() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("values").await?;
    let big = "x".repeat(40_000);
    harness
        .set(
            OWNER,
            &id,
            "",
            0,
            &json!({ "big": big, "small": { "k": [1, null] } }),
        )
        .await?;
    let reply = harness.members(&id, "?select=values").await?.json()?;
    assert_eq!(
        reply["members"],
        json!([{ "name": "big", "too_large": true },
               { "name": "small", "value": { "k": [1, null] } }])
    );
    let names = harness.members(&id, "?select=names").await?.json()?;
    assert_eq!(
        names["members"],
        json!([{ "name": "big" }, { "name": "small" }])
    );
    Ok(())
}

#[tokio::test]
async fn a_missing_path_or_a_scalar_has_no_members_and_does_not_exist() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("scalars").await?;
    harness.set(OWNER, &id, "n", 0, &json!(5)).await?;
    for query in ["?path=nothing", "?path=n"] {
        let reply = harness.members(&id, query).await?.json()?;
        assert_eq!(reply["exists"], false, "{query}");
        assert_eq!(reply["members"], json!([]), "{query}");
    }
    Ok(())
}

#[tokio::test]
async fn a_bad_query_is_refused_before_anything_is_read() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("bad").await?;
    let cases = [
        ("?limit=0", 400),
        ("?limit=101", 400),
        ("?limit=x", 400),
        ("?after=", 400),
        ("?select=keys", 400),
        ("?unknown=1", 400),
        ("?limit=1&limit=2", 400),
        ("?path=a//b", 422),
        ("?path=a%20b", 422),
    ];
    for (query, status) in cases {
        let reply = harness.members(&id, query).await?;
        assert_eq!(reply.status.as_u16(), status, "{query}: {}", reply.text);
    }
    let missing = harness
        .members("art_00000000000000000000000000000000", "")
        .await?;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn a_stored_space_that_does_not_parse_is_a_typed_server_error() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("corrupt").await?;
    harness.set(OWNER, &id, "", 0, &json!({ "a": 1 })).await?;
    harness.sql(
        "UPDATE app_data SET document = '{\"a\":1,\"a\":2}' WHERE artifact_id = ?1",
        &id,
    )?;
    let reply = harness.members(&id, "").await?;
    assert_eq!(
        reply.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "{}",
        reply.text
    );
    assert_eq!(reply.json()?["error"]["code"], "internal");
    Ok(())
}

// Events -------------------------------------------------------------------

#[tokio::test]
async fn a_stream_starts_with_a_snapshot_then_sends_each_new_full_value() -> TestResult {
    let harness = Harness::with(quick())?;
    let id = harness.artifact("follow").await?;
    harness
        .set(OWNER, &id, "todo", 0, &json!({ "a": 1 }))
        .await?;
    let mut events = harness.follow(OWNER, &id, "?path=todo").await?;
    let snapshot = events.expect("snapshot").await?;
    assert_eq!(
        snapshot.data,
        json!({ "artifact_id": id, "path": "todo", "data_version": 1,
                "exists": true, "value": { "a": 1 } })
    );
    harness.set(OWNER, &id, "todo/b", 1, &json!(2)).await?;
    let change = events.expect("change").await?;
    assert_eq!(
        change.data,
        json!({ "artifact_id": id, "path": "todo", "data_version": 2,
                "exists": true, "value": { "a": 1, "b": 2 } })
    );
    // A removal is a change too, with no value.
    let removed = harness
        .send(
            "DELETE",
            &format!("/v1/artifacts/{id}/data"),
            OWNER,
            Some("remove-todo"),
            Some(&json!({ "path": "todo", "base_data_version": 2 })),
        )
        .await?;
    assert_eq!(removed.status, StatusCode::OK, "{}", removed.text);
    let gone = events.expect("change").await?;
    assert_eq!(
        gone.data,
        json!({ "artifact_id": id, "path": "todo", "data_version": 3, "exists": false })
    );
    Ok(())
}

#[tokio::test]
async fn a_burst_of_writes_during_the_throttle_is_one_more_read() -> TestResult {
    let harness = Harness::with(DataStreamSettings {
        // Long enough that even five slow commits (a Windows runner fsyncs
        // for a long time) finish inside one interval.
        throttle: Duration::from_secs(3),
        ..DataStreamSettings::default()
    })?;
    let id = harness.artifact("burst").await?;
    harness.set(OWNER, &id, "n", 0, &json!(0)).await?;
    let mut events = harness.follow(OWNER, &id, "?path=n").await?;
    events.expect("snapshot").await?;
    let before = harness.state.data_stream_reads();
    for version in 1..=5_u64 {
        harness
            .set(OWNER, &id, "n", version, &json!(version))
            .await?;
    }
    let change = events.expect("change").await?;
    assert_eq!(change.data["data_version"], 6, "{change:?}");
    assert_eq!(change.data["value"], 5);
    assert_eq!(
        harness.state.data_stream_reads() - before,
        1,
        "five writes in one throttle interval are read once"
    );
    // Nothing was announced since, so nothing is read again. (This can only
    // pass too early, never fail on a slow machine.)
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(
        harness.state.data_stream_reads() - before,
        1,
        "a stream that read everything announced must not keep reading"
    );
    Ok(())
}

#[tokio::test]
async fn a_value_over_the_event_bound_ends_the_stream_with_a_typed_event() -> TestResult {
    let harness = Harness::with(quick())?;
    let id = harness.artifact("huge").await?;
    let mut events = harness.follow(OWNER, &id, "?path=blob").await?;
    events.expect("snapshot").await?;
    let big = "x".repeat(130 * 1024);
    harness.set(OWNER, &id, "blob", 0, &json!(big)).await?;
    let event = events.expect("too_large").await?;
    assert_eq!(
        event.data,
        json!({ "artifact_id": id, "path": "blob", "data_version": 1, "limit_bytes": 131_072 })
    );
    events.ended().await
}

#[tokio::test]
async fn a_stored_space_that_stops_parsing_sends_an_error_event_and_ends() -> TestResult {
    let harness = Harness::with(DataStreamSettings {
        throttle: Duration::from_millis(10),
        heartbeat: Duration::from_millis(100),
        ..DataStreamSettings::default()
    })?;
    let id = harness.artifact("rot").await?;
    harness.set(OWNER, &id, "", 0, &json!({ "a": 1 })).await?;
    let mut events = harness.follow(OWNER, &id, "").await?;
    events.expect("snapshot").await?;
    harness.sql(
        "UPDATE app_data SET document = '{\"a\":1,\"a\":2}' WHERE artifact_id = ?1",
        &id,
    )?;
    let event = events.expect("error").await?;
    assert_eq!(event.data["error"]["code"], "internal", "{event:?}");
    events.ended().await?;
    // And a stream cannot start over such a space.
    let refused = harness.open(OWNER, &id, "").await?;
    assert_eq!(refused.status(), StatusCode::INTERNAL_SERVER_ERROR);
    Ok(())
}

#[tokio::test]
async fn a_write_by_another_process_is_found_by_the_idle_read() -> TestResult {
    let harness = Harness::with(DataStreamSettings {
        throttle: Duration::from_millis(10),
        heartbeat: Duration::from_millis(100),
        ..DataStreamSettings::default()
    })?;
    let id = harness.artifact("elsewhere").await?;
    harness.set(OWNER, &id, "", 0, &json!({ "k": 1 })).await?;
    let mut events = harness.follow(OWNER, &id, "").await?;
    events.expect("snapshot").await?;
    harness.sql(
        "UPDATE app_data SET data_version = 2, document = '{\"k\":2}' WHERE artifact_id = ?1",
        &id,
    )?;
    let change = events.expect("change").await?;
    assert_eq!(change.data["data_version"], 2);
    assert_eq!(change.data["value"], json!({ "k": 2 }));
    Ok(())
}

#[tokio::test]
async fn trashing_the_artifact_ends_the_stream() -> TestResult {
    let harness = Harness::with(quick())?;
    let id = harness.artifact("bin").await?;
    let mut events = harness.follow(OWNER, &id, "").await?;
    events.expect("snapshot").await?;
    let trashed = harness
        .send(
            "PATCH",
            &format!("/v1/artifacts/{id}"),
            OWNER,
            Some("trash-it"),
            Some(&json!({ "base_metadata_revision": 1, "deleted": true })),
        )
        .await?;
    assert_eq!(trashed.status, StatusCode::OK, "{}", trashed.text);
    events.ended().await?;
    let again = harness.open(OWNER, &id, "").await?;
    assert_eq!(again.status(), StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn a_stream_ends_cleanly_at_the_end_of_its_lifetime() -> TestResult {
    let harness = Harness::with(DataStreamSettings {
        lifetime: Duration::from_millis(200),
        ..quick()
    })?;
    let id = harness.artifact("short").await?;
    let mut events = harness.follow(OWNER, &id, "").await?;
    events.expect("snapshot").await?;
    events.ended().await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        while harness.state.data_stream_reads() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    Ok(())
}

#[tokio::test]
async fn resuming_is_refused_and_a_bad_path_is_refused() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("resume").await?;
    let response = harness
        .api
        .clone()
        .oneshot(
            Request::get(format!("/v1/artifacts/{id}/data/events"))
                .header(header::AUTHORIZATION, format!("Bearer {OWNER}"))
                .header("last-event-id", "1")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bad = harness.open(OWNER, &id, "?path=a//b").await?;
    assert_eq!(bad.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let repeated = harness.open(OWNER, &id, "?path=a&path=b").await?;
    assert_eq!(repeated.status(), StatusCode::BAD_REQUEST);
    Ok(())
}

// Limits and revocation ------------------------------------------------------

#[tokio::test]
async fn an_actor_is_limited_with_a_retry_after_and_a_refusal_reads_nothing() -> TestResult {
    let harness = Harness::with(DataStreamSettings {
        streams_per_actor: 2,
        ..quick()
    })?;
    let id = harness.artifact("limits").await?;
    let mut first = harness.follow(OWNER, &id, "").await?;
    first.expect("snapshot").await?;
    let mut second = harness.follow(OWNER, &id, "").await?;
    second.expect("snapshot").await?;

    let reads = harness.state.data_stream_reads();
    let third = harness.open(OWNER, &id, "").await?;
    assert_eq!(third.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        third
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok()),
        Some("5")
    );
    assert_eq!(
        harness.state.data_stream_reads(),
        reads,
        "a refusal read storage"
    );

    // Closing one frees its place.
    drop(first);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(response) = harness.open(OWNER, &id, "").await
                && response.status() == StatusCode::OK
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    Ok(())
}

#[tokio::test]
async fn the_process_is_limited_with_a_longer_retry_after() -> TestResult {
    let harness = Harness::with(DataStreamSettings {
        streams_per_process: 1,
        ..quick()
    })?;
    let id = harness.artifact("full").await?;
    let mut only = harness.follow(OWNER, &id, "").await?;
    only.expect("snapshot").await?;
    let reads = harness.state.data_stream_reads();
    let refused = harness.open(OWNER, &id, "").await?;
    assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        refused
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok()),
        Some("10")
    );
    assert_eq!(harness.state.data_stream_reads(), reads);
    Ok(())
}

#[tokio::test]
async fn revoking_a_token_or_disabling_its_user_ends_its_streams() -> TestResult {
    let harness = Harness::with(quick())?;
    let id = harness.artifact("revoke").await?;
    let agent = harness.issue(None).await?;
    let mut by_token = harness.follow(&agent.secret, &id, "").await?;
    by_token.expect("snapshot").await?;
    let revoked = harness
        .send(
            "DELETE",
            &format!("/v1/tokens/{}", agent.token.id),
            OWNER,
            None,
            None,
        )
        .await?;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT, "{}", revoked.text);
    by_token.ended().await?;
    let refused = harness.open(&agent.secret, &id, "").await?;
    assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);

    // Rotating the secret ends them too.
    let second = harness.issue(None).await?;
    let mut by_secret = harness.follow(&second.secret, &id, "").await?;
    by_secret.expect("snapshot").await?;
    let rotated = harness
        .send(
            "POST",
            &format!("/v1/tokens/{}/rotate", second.token.id),
            OWNER,
            None,
            None,
        )
        .await?;
    assert_eq!(rotated.status, StatusCode::OK, "{}", rotated.text);
    by_secret.ended().await?;

    let user = harness
        .send(
            "POST",
            "/v1/users",
            OWNER,
            None,
            Some(&json!({ "label": "alice" })),
        )
        .await?
        .json()?;
    let user_id = user["id"].as_str().ok_or("user id")?.to_owned();
    let alice = harness.issue(Some(&user_id)).await?;
    let own = harness.artifact_of(&alice.secret, "alices").await?;
    let mut by_user = harness.follow(&alice.secret, &own, "").await?;
    by_user.expect("snapshot").await?;
    let disabled = harness
        .send(
            "POST",
            &format!("/v1/users/{user_id}/disable"),
            OWNER,
            None,
            None,
        )
        .await?;
    assert!(disabled.status.is_success(), "{}", disabled.text);
    by_user.ended().await
}

#[tokio::test]
async fn another_user_finds_no_stream_where_alice_has_data() -> TestResult {
    let harness = Harness::with(quick())?;
    let alice = harness.issue(None).await?;
    let user = harness
        .send(
            "POST",
            "/v1/users",
            OWNER,
            None,
            Some(&json!({ "label": "bob" })),
        )
        .await?
        .json()?;
    let bob = harness
        .issue(Some(user["id"].as_str().ok_or("user id")?))
        .await?;
    let id = harness.artifact_of(&alice.secret, "private").await?;
    for uri in [
        format!("/v1/artifacts/{id}/data/events"),
        format!("/v1/artifacts/{id}/data/members"),
    ] {
        let reply = harness.send("GET", &uri, &bob.secret, None, None).await?;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{uri}: {}", reply.text);
    }
    Ok(())
}
