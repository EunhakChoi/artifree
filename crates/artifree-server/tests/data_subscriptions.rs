//! Application data queries and subscriptions (decision 0038) through the real
//! router, core, and `SQLite` store: the member listing, and the event stream
//! with its snapshot, replay, window, and reset; isolation between users;
//! revocation; and the limits.
//!
//! Absence is proved by order, not by waiting: a write that does touch the
//! watched path follows the writes that do not, and the next event must be it.

use std::future::poll_fn;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use artifree_providers::backup;
use artifree_providers::storage::{DATABASE_FILE, SqliteStore};
use artifree_providers::tokens::SqliteTokenStore;
use artifree_server::auth::AccessToken;
use artifree_server::http::{self, AppState, Settings};
use artifree_server::limits::RequestLimits;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use http_body::Body as _;
use serde_json::{Value, json};
use tower::ServiceExt;

type TestError = Box<dyn std::error::Error>;
type TestResult = Result<(), TestError>;

const OWNER: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// A server over a data directory. `reopened` makes another over the same
/// directory, as a restart does: its windows are empty.
struct Harness {
    directory: PathBuf,
    owns_directory: bool,
    api: Router,
}

impl Harness {
    fn new() -> Self {
        Self::over(Self::fresh_path(), true)
    }

    fn fresh_path() -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        std::env::temp_dir().join(format!(
            "artifree-data-stream-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn over(directory: PathBuf, owns_directory: bool) -> Self {
        std::fs::create_dir_all(&directory)
            .unwrap_or_else(|error| panic!("cannot create test directory: {error}"));
        let database = directory.join(DATABASE_FILE);
        let state = AppState::new(
            SqliteStore::open(&database)
                .unwrap_or_else(|error| panic!("cannot open store: {error}")),
            SqliteTokenStore::open(&database)
                .unwrap_or_else(|error| panic!("cannot open token store: {error}")),
            AccessToken::parse(OWNER).unwrap_or_else(|| panic!("fixture token is invalid")),
            Settings {
                content_origin: "http://127.0.0.1:7411".into(),
                limits: RequestLimits {
                    burst: 100_000,
                    per_minute: 6_000_000,
                    concurrent: 64,
                },
                ..Settings::default()
            },
        )
        .unwrap_or_else(|error| panic!("cannot build state: {error}"));
        Self {
            directory,
            owns_directory,
            // The real server records the socket peer; with none, /mcp refuses.
            api: http::router(state).layer(axum::Extension(ConnectInfo(SocketAddr::from((
                [127, 0, 0, 1],
                40_000,
            ))))),
        }
    }

    /// Another server over the same data: a restart.
    fn reopened(&self) -> Self {
        Self::over(self.directory.clone(), false)
    }

    /// A server over a restored copy of this one's database: another database
    /// with the same data and the same feed identity (a restore gives it a
    /// new one, decision 0006).
    fn restored(&self) -> Self {
        let output = Self::fresh_path();
        backup::backup(&self.directory, &output)
            .unwrap_or_else(|error| panic!("cannot back up: {error}"));
        let directory = Self::fresh_path();
        backup::restore(&output, &directory)
            .unwrap_or_else(|error| panic!("cannot restore: {error}"));
        let _ = std::fs::remove_file(&output);
        Self::over(directory, true)
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        token: &str,
        key: Option<&str>,
        body: Option<&Value>,
        last_event_id: Option<&str>,
    ) -> Result<axum::http::Response<Body>, TestError> {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(header::AUTHORIZATION, format!("Bearer {token}"));
        if let Some(key) = key {
            builder = builder.header("idempotency-key", format!("test-key-{key}"));
        }
        if let Some(cursor) = last_event_id {
            builder = builder.header("last-event-id", cursor);
        }
        let request = match body {
            Some(body) => builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))?,
            None => builder.body(Body::empty())?,
        };
        Ok(self.api.clone().oneshot(request).await?)
    }

    /// A request whose whole answer is read.
    async fn call(
        &self,
        method: &str,
        path: &str,
        token: &str,
        key: Option<&str>,
        body: Option<&Value>,
    ) -> Result<Reply, TestError> {
        let response = self.request(method, path, token, key, body, None).await?;
        let status = response.status();
        let retry_after = response
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let bytes = to_bytes(response.into_body(), usize::MAX).await?;
        Ok(Reply {
            status,
            retry_after,
            text: String::from_utf8_lossy(&bytes).into_owned(),
        })
    }

    async fn ok(
        &self,
        method: &str,
        path: &str,
        token: &str,
        key: Option<&str>,
        body: Option<&Value>,
    ) -> Result<Value, TestError> {
        let reply = self.call(method, path, token, key, body).await?;
        assert!(
            reply.status.is_success(),
            "{method} {path}: {} {}",
            reply.status,
            reply.text
        );
        if reply.text.is_empty() {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_str(&reply.text)?)
    }

    async fn artifact(&self, token: &str, name: &str) -> Result<String, TestError> {
        let created = self
            .ok(
                "POST",
                "/v1/artifacts",
                token,
                Some(&format!("create-{name}")),
                Some(&json!({
                    "title": name,
                    "files": [{ "path": "index.html", "encoding": "utf8", "content": "<p>1</p>" }]
                })),
            )
            .await?;
        Ok(created["artifact"]["id"]
            .as_str()
            .ok_or("no artifact id")?
            .to_owned())
    }

    /// A write of the owner's data; returns the version it left.
    async fn write(
        &self,
        id: &str,
        method: &str,
        key: &str,
        base: u64,
        path: &str,
        field: Option<(&str, Value)>,
    ) -> Result<u64, TestError> {
        let mut body = json!({ "path": path, "base_data_version": base });
        if let Some((name, value)) = field {
            body[name] = value;
        }
        let reply = self
            .ok(
                method,
                &format!("/v1/artifacts/{id}/data"),
                OWNER,
                Some(key),
                Some(&body),
            )
            .await?;
        reply["data_version"]
            .as_u64()
            .ok_or_else(|| "no data_version".into())
    }

    async fn set(
        &self,
        id: &str,
        key: &str,
        base: u64,
        path: &str,
        value: Value,
    ) -> Result<u64, TestError> {
        self.write(id, "PUT", key, base, path, Some(("value", value)))
            .await
    }

    async fn merge(
        &self,
        id: &str,
        key: &str,
        base: u64,
        path: &str,
        document: Value,
    ) -> Result<u64, TestError> {
        self.write(id, "PATCH", key, base, path, Some(("patch", document)))
            .await
    }

    async fn delete(&self, id: &str, key: &str, base: u64, path: &str) -> Result<u64, TestError> {
        self.write(id, "DELETE", key, base, path, None).await
    }

    /// A page of members for the owner.
    async fn members(&self, id: &str, query: &str) -> Result<Value, TestError> {
        self.ok(
            "GET",
            &format!("/v1/artifacts/{id}/data/members{query}"),
            OWNER,
            None,
            None,
        )
        .await
    }

    /// Opens a data stream of the owner, after `cursor` when given.
    async fn watch(&self, id: &str, path: &str, cursor: Option<&str>) -> Result<Events, TestError> {
        self.watching(OWNER, id, path, cursor).await
    }

    async fn watching(
        &self,
        token: &str,
        id: &str,
        path: &str,
        cursor: Option<&str>,
    ) -> Result<Events, TestError> {
        let response = self
            .request(
                "GET",
                &format!("/v1/artifacts/{id}/data/events?path={path}"),
                token,
                None,
                None,
                cursor,
            )
            .await?;
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

    /// A user the owner registers, with one token: `(user id, secret, token id)`.
    async fn person(&self, label: &str) -> Result<(String, String, String), TestError> {
        let user = self
            .ok(
                "POST",
                "/v1/users",
                OWNER,
                None,
                Some(&json!({ "label": label })),
            )
            .await?;
        let id = user["id"].as_str().ok_or("user id")?.to_owned();
        let issued = self
            .ok(
                "POST",
                "/v1/tokens",
                OWNER,
                None,
                Some(&json!({ "name": label, "user_id": id })),
            )
            .await?;
        Ok((
            id,
            issued["secret"].as_str().ok_or("secret")?.to_owned(),
            issued["token"]["id"].as_str().ok_or("token id")?.to_owned(),
        ))
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if self.owns_directory {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }
}

struct Reply {
    status: StatusCode,
    retry_after: Option<String>,
    text: String,
}

impl Reply {
    fn json(&self) -> Result<Value, serde_json::Error> {
        serde_json::from_str(&self.text)
    }
}

/// One server-sent event.
#[derive(Debug, Clone)]
struct Event {
    name: String,
    id: String,
    data: Value,
}

impl Event {
    fn version(&self) -> u64 {
        self.data["data_version"]
            .as_u64()
            .unwrap_or_else(|| panic!("no data_version in {self:?}"))
    }
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

    /// The next event, which must be `name` and carry its id as its cursor.
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

    /// The next `change`, skipping the `position` events that cover writes
    /// beside the watched path: how many of them come depends on how the
    /// writes fall between the stream's wake-ups.
    async fn change(&mut self) -> Result<Event, TestError> {
        loop {
            let event = self.next().await?.ok_or("the stream ended")?;
            assert_eq!(
                event.data["cursor"],
                Value::from(event.id.clone()),
                "{event:?}"
            );
            match event.name.as_str() {
                "position" => {}
                "change" => return Ok(event),
                other => return Err(format!("expected a change, got {other}: {event:?}").into()),
            }
        }
    }

    /// The events up to and including the first at `version` or later.
    async fn until(&mut self, version: u64) -> Result<Vec<Event>, TestError> {
        let mut seen = Vec::new();
        loop {
            let event = self.next().await?.ok_or("the stream ended")?;
            let reached = event.version() >= version;
            seen.push(event);
            if reached {
                return Ok(seen);
            }
        }
    }

    /// Whether the stream has ended, without waiting for more than a close.
    async fn ended(&mut self) -> Result<bool, TestError> {
        Ok(self.next().await?.is_none())
    }
}

fn kinds(events: &[Event]) -> Vec<(String, u64)> {
    events
        .iter()
        .map(|event| (event.name.clone(), event.version()))
        .collect()
}

fn names(page: &Value) -> Vec<String> {
    page["canonical"]["members"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|member| member["name"].as_str().map(str::to_owned))
        .collect()
}

// --- the member listing ----------------------------------------------------

#[tokio::test]
async fn members_are_listed_by_name_bytes_in_bounded_pages() -> TestResult {
    let harness = Harness::new();
    let id = harness.artifact(OWNER, "list").await?;
    let space = json!({ "b": 1, "é": 2, "a": 3, "_": 4, "B": 5, "item-1": 6, "item-2": 7 });
    let version = harness.set(&id, "set-all-000", 0, "todos", space).await?;
    assert_eq!(version, 1);

    let page = harness.members(&id, "?path=todos").await?;
    assert_eq!(
        names(&page),
        ["B", "_", "a", "b", "item-1", "item-2", "é"],
        "byte order, not locale order"
    );
    assert_eq!(page["artifact_id"], id);
    assert_eq!(page["canonical"]["data_version"], 1);
    assert_eq!(page["canonical"]["exists"], true);
    assert!(page["canonical"].get("next_after").is_none(), "{page}");
    // The query is repeated as completed.
    assert_eq!(
        page["query"],
        json!({ "path": "todos", "select": "names", "limit": 50 })
    );
    // Names only: no value member.
    assert!(page["canonical"]["members"][0].get("value").is_none());

    // Pages of two follow the last name, with no repeat and no gap.
    let first = harness.members(&id, "?path=todos&limit=3").await?;
    assert_eq!(names(&first), ["B", "_", "a"]);
    assert_eq!(first["canonical"]["next_after"], "a");
    let second = harness.members(&id, "?path=todos&limit=3&after=a").await?;
    assert_eq!(names(&second), ["b", "item-1", "item-2"]);
    assert_eq!(second["canonical"]["next_after"], "item-2");
    assert_eq!(second["query"]["after"], "a");
    let third = harness
        .members(&id, "?path=todos&limit=3&after=item-2")
        .await?;
    assert_eq!(names(&third), ["é"]);
    assert!(third["canonical"].get("next_after").is_none());

    // A name prefix, with values.
    let items = harness
        .members(&id, "?path=todos&prefix=item-&select=values")
        .await?;
    assert_eq!(
        items["canonical"]["members"],
        json!([{ "name": "item-1", "value": 6 }, { "name": "item-2", "value": 7 }])
    );
    assert_eq!(items["query"]["prefix"], "item-");
    assert_eq!(items["query"]["select"], "values");

    // Nothing, or no object, at the path: a successful, empty answer.
    for path in ["missing", "todos/a", "todos/none/deeper"] {
        let page = harness.members(&id, &format!("?path={path}")).await?;
        assert_eq!(page["canonical"]["exists"], false, "{path}");
        assert_eq!(page["canonical"]["members"], json!([]), "{path}");
    }
    // The root lists the top-level members.
    assert_eq!(names(&harness.members(&id, "").await?), ["todos"]);
    Ok(())
}

#[tokio::test]
async fn a_page_of_values_is_bounded_in_bytes() -> TestResult {
    let harness = Harness::new();
    let id = harness.artifact(OWNER, "bytes").await?;
    let big = "x".repeat(200 * 1024);
    let mut version = 0;
    for name in ["a", "b", "c"] {
        version = harness
            .set(
                &id,
                &format!("set-{name}-0000"),
                version,
                &format!("blobs/{name}"),
                json!(big),
            )
            .await?;
    }
    let page = harness
        .members(&id, "?path=blobs&select=values&limit=100")
        .await?;
    assert_eq!(
        names(&page),
        ["a", "b"],
        "200 KiB twice fits 512 KiB, three do not"
    );
    assert_eq!(page["canonical"]["next_after"], "b");
    let rest = harness
        .members(&id, "?path=blobs&select=values&after=b")
        .await?;
    assert_eq!(names(&rest), ["c"]);
    Ok(())
}

#[tokio::test]
async fn queries_that_break_a_rule_are_refused_before_anything_is_read() -> TestResult {
    let harness = Harness::new();
    let id = harness.artifact(OWNER, "rules").await?;
    let base = format!("/v1/artifacts/{id}/data/members");
    let long_name = "n".repeat(257);
    let long_prefix = "p".repeat(65);
    // 422 `invalid_request`: a value that breaks a rule.
    for query in [
        "?limit=0".to_owned(),
        "?limit=101".to_owned(),
        "?prefix=".to_owned(),
        format!("?prefix={long_prefix}"),
        "?prefix=a.b".to_owned(),
        "?prefix=a%2Fb".to_owned(),
        "?after=".to_owned(),
        format!("?after={long_name}"),
        "?path=a//b".to_owned(),
        "?path=.".to_owned(),
        "?path=a%20b".to_owned(),
    ] {
        let reply = harness
            .call("GET", &format!("{base}{query}"), OWNER, None, None)
            .await?;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{query}: {}",
            reply.text
        );
        assert_eq!(reply.json()?["error"]["code"], "invalid_request", "{query}");
    }
    // 400: a parameter that is not part of the query, repeated, or not typed.
    for query in [
        "?surprise=1",
        "?limit=1&limit=2",
        "?path=a&path=b",
        "?limit=ten",
        "?limit=-1",
        "?select=all",
    ] {
        let reply = harness
            .call("GET", &format!("{base}{query}"), OWNER, None, None)
            .await?;
        assert_eq!(
            reply.status,
            StatusCode::BAD_REQUEST,
            "{query}: {}",
            reply.text
        );
    }
    // The bounds themselves are fine.
    for query in [
        "?limit=1".to_owned(),
        "?limit=100".to_owned(),
        format!("?after={}", "n".repeat(256)),
        format!("?prefix={}", "p".repeat(64)),
    ] {
        let reply = harness
            .call("GET", &format!("{base}{query}"), OWNER, None, None)
            .await?;
        assert_eq!(reply.status, StatusCode::OK, "{query}: {}", reply.text);
    }
    Ok(())
}

#[tokio::test]
async fn a_cursor_is_stable_while_the_space_is_written_between_pages() -> TestResult {
    let harness = Harness::new();
    let id = harness.artifact(OWNER, "stable").await?;
    let mut version = harness
        .set(
            &id,
            "set-seed-0000",
            0,
            "items",
            json!({ "b": 1, "d": 1, "f": 1, "h": 1 }),
        )
        .await?;
    let first = harness.members(&id, "?path=items&limit=2").await?;
    assert_eq!(names(&first), ["b", "d"]);
    let cursor = first["canonical"]["next_after"]
        .as_str()
        .ok_or("no cursor")?
        .to_owned();
    // A member before the cursor, the cursor member itself removed, one added
    // after it, and one removed after it.
    version = harness
        .set(&id, "set-a-00000", version, "items/a", json!(1))
        .await?;
    version = harness
        .delete(&id, "del-d-00000", version, "items/d")
        .await?;
    version = harness
        .set(&id, "set-g-00000", version, "items/g", json!(1))
        .await?;
    version = harness
        .delete(&id, "del-f-00000", version, "items/f")
        .await?;
    let second = harness
        .members(&id, &format!("?path=items&limit=10&after={cursor}"))
        .await?;
    assert_eq!(
        names(&second),
        ["g", "h"],
        "starts strictly after the cursor name"
    );
    assert_eq!(second["canonical"]["data_version"], version);
    assert!(
        version
            > first["canonical"]["data_version"]
                .as_u64()
                .ok_or("version")?,
        "the page says which version it was read at"
    );
    Ok(())
}

#[tokio::test]
async fn paging_while_writers_run_repeats_nothing_and_skips_no_stable_member() -> TestResult {
    let harness = Harness::new();
    let id = harness.artifact(OWNER, "race").await?;
    let stable: Vec<String> = (0..30).map(|number| format!("m{number:03}")).collect();
    let seed: serde_json::Map<String, Value> =
        stable.iter().map(|name| (name.clone(), json!(1))).collect();
    harness
        .set(&id, "set-seed-0000", 0, "set", Value::Object(seed))
        .await?;

    // Writers add and remove members of their own while a reader pages.
    let writers: Vec<_> = (0..3)
        .map(|writer| {
            let harness = harness.reopened_router();
            let id = id.clone();
            tokio::spawn(async move {
                let run = async {
                    for round in 0..12 {
                        let version = harness.version(&id).await?;
                        let name = format!("w{writer}x{round:02}");
                        // A conflict with another writer is fine: try the next round.
                        let _ = harness
                            .try_set(
                                &id,
                                &format!("w{writer}-set-{round:03}"),
                                version,
                                &format!("set/{name}"),
                            )
                            .await?;
                        let version = harness.version(&id).await?;
                        let _ = harness
                            .try_delete(
                                &id,
                                &format!("w{writer}-del-{round:03}"),
                                version,
                                &format!("set/{name}"),
                            )
                            .await?;
                    }
                    Ok::<(), TestError>(())
                };
                run.await.map_err(|error| error.to_string())
            })
        })
        .collect();
    let mut listed: Vec<String> = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let query = match &after {
            Some(after) => format!("?path=set&limit=7&after={after}"),
            None => "?path=set&limit=7".to_owned(),
        };
        let page = harness.members(&id, &query).await?;
        listed.extend(names(&page));
        match page["canonical"]["next_after"].as_str() {
            Some(next) => after = Some(next.to_owned()),
            None => break,
        }
    }
    for writer in writers {
        writer.await??;
    }
    let mut sorted = listed.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(listed, sorted, "pages repeat a member or are out of order");
    for name in &stable {
        assert!(
            listed.contains(name),
            "{name} was present throughout and is missing"
        );
    }
    Ok(())
}

// --- isolation -------------------------------------------------------------

#[tokio::test]
async fn another_user_finds_nothing_and_cannot_tell_it_from_a_missing_artifact() -> TestResult {
    let harness = Harness::new();
    let (_, bob, _) = harness.person("bob").await?;
    let (_, alice, _) = harness.person("alice").await?;
    let id = harness.artifact(&alice, "alice-notes").await?;
    harness
        .ok(
            "PUT",
            &format!("/v1/artifacts/{id}/data"),
            &alice,
            Some("alice-set-0000"),
            Some(&json!({ "path": "notes", "base_data_version": 0, "value": { "secret": 1 } })),
        )
        .await?;
    let nothing = "art_00000000000000000000000000000000";
    for route in ["members?path=notes", "events?path=notes"] {
        let mut answers = Vec::new();
        for (token, artifact) in [
            (bob.as_str(), id.as_str()),
            (bob.as_str(), nothing),
            (OWNER, id.as_str()),
            (alice.as_str(), nothing),
        ] {
            let reply = harness
                .call(
                    "GET",
                    &format!("/v1/artifacts/{artifact}/data/{route}"),
                    token,
                    None,
                    None,
                )
                .await?;
            assert_eq!(
                reply.status,
                StatusCode::NOT_FOUND,
                "{route}: {}",
                reply.text
            );
            answers.push(reply.text.replace(artifact, "<id>"));
        }
        assert!(
            answers.iter().all(|text| text == &answers[0]),
            "{route}: {answers:?}"
        );
        assert!(!answers[0].contains("secret"), "{route}");
    }
    // Alice is not refused.
    let page = harness
        .ok(
            "GET",
            &format!("/v1/artifacts/{id}/data/members?path=notes&select=values"),
            &alice,
            None,
            None,
        )
        .await?;
    assert_eq!(page["canonical"]["members"][0]["value"], 1);
    Ok(())
}

#[tokio::test]
async fn an_artifact_in_the_trash_is_not_listed_or_streamed_and_ends_a_stream() -> TestResult {
    let harness = Harness::new();
    let id = harness.artifact(OWNER, "trashed").await?;
    harness
        .set(&id, "set-seed-0000", 0, "todos", json!({ "a": 1 }))
        .await?;
    let mut stream = harness.watch(&id, "todos", None).await?;
    stream.expect("snapshot").await?;
    harness
        .ok(
            "PATCH",
            &format!("/v1/artifacts/{id}"),
            OWNER,
            Some("trash-0000000"),
            Some(&json!({ "base_metadata_revision": 1, "deleted": true })),
        )
        .await?;
    // A commit of this process makes the stream look: the artifact is gone.
    assert!(stream.ended().await?, "the stream outlived its artifact");
    for route in ["members?path=todos", "events?path=todos"] {
        let reply = harness
            .call(
                "GET",
                &format!("/v1/artifacts/{id}/data/{route}"),
                OWNER,
                None,
                None,
            )
            .await?;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{route}");
    }
    Ok(())
}

// --- the stream: snapshot, changes, position ---------------------------------

#[tokio::test]
async fn a_stream_starts_with_a_snapshot_and_sends_each_touching_write_once_in_order() -> TestResult
{
    let harness = Harness::new();
    let id = harness.artifact(OWNER, "events").await?;
    let v1 = harness
        .set(
            &id,
            "set-seed-0000",
            0,
            "todos",
            json!({ "t1": { "done": false } }),
        )
        .await?;
    let mut stream = harness.watch(&id, "todos", None).await?;
    let snapshot = stream.expect("snapshot").await?;
    assert_eq!(
        snapshot.data,
        json!({
            "cursor": snapshot.id, "data_version": v1, "path": "todos",
            "exists": true, "value": { "t1": { "done": false } }
        })
    );
    assert!(snapshot.id.starts_with("d."), "{}", snapshot.id);

    // A write beside the path, a replay, and a write that changes nothing
    // send nothing; the write below the path follows them.
    let v2 = harness
        .set(&id, "set-beside-00", v1, "settings", json!(1))
        .await?;
    assert_eq!(
        harness
            .set(&id, "set-beside-00", v1, "settings", json!(1))
            .await?,
        v2
    );
    let same = harness
        .set(&id, "set-beside-01", v2, "settings", json!(1))
        .await?;
    assert_eq!(same, v2, "a write of what is there keeps the version");
    let v3 = harness
        .set(&id, "set-done-0000", v2, "todos/t1/done", json!(true))
        .await?;
    let change = stream.change().await?;
    assert_eq!(
        change.data,
        json!({
            "cursor": change.id, "data_version": v3, "path": "todos/t1/done",
            "exists": true, "value": true
        }),
        "the write at version {v2} is beside the path and was not sent"
    );

    // A merge is sent as its result; removing sends a gone value.
    let v4 = harness
        .merge(
            &id,
            "merge-t2-0000",
            v3,
            "todos",
            json!({ "t2": { "done": false }, "t1": null }),
        )
        .await?;
    let merged = stream.change().await?;
    assert_eq!(
        (
            merged.version(),
            merged.data["path"].clone(),
            merged.data["value"].clone()
        ),
        (v4, json!("todos"), json!({ "t2": { "done": false } }))
    );
    let v5 = harness.delete(&id, "del-t2-00000", v4, "todos/t2").await?;
    let gone = stream.change().await?;
    assert_eq!(
        gone.data,
        json!({ "cursor": gone.id, "data_version": v5, "path": "todos/t2", "exists": false })
    );

    // A write above the path is reported at the path with its part of it.
    let v6 = harness
        .set(
            &id,
            "set-root-0000",
            v5,
            "",
            json!({ "todos": { "t9": 9 }, "other": 1 }),
        )
        .await?;
    let above = stream.change().await?;
    assert_eq!(
        (
            above.version(),
            above.data["path"].clone(),
            above.data["value"].clone()
        ),
        (v6, json!("todos"), json!({ "t9": 9 }))
    );
    // Emptying the root removes what is watched.
    let v7 = harness.delete(&id, "del-root-0000", v6, "").await?;
    let emptied = stream.change().await?;
    assert_eq!(
        emptied.data,
        json!({ "cursor": emptied.id, "data_version": v7, "path": "todos", "exists": false })
    );
    Ok(())
}

#[tokio::test]
async fn writes_that_do_not_touch_the_path_are_covered_by_a_position() -> TestResult {
    let harness = Harness::new();
    let id = harness.artifact(OWNER, "position").await?;
    let v1 = harness
        .set(&id, "set-seed-0000", 0, "todos", json!({}))
        .await?;
    let mut stream = harness.watch(&id, "todos", None).await?;
    stream.expect("snapshot").await?;
    let v2 = harness.set(&id, "set-a-0000000", v1, "a", json!(1)).await?;
    let v3 = harness.set(&id, "set-b-0000000", v2, "b", json!(1)).await?;
    let covered = stream.until(v3).await?;
    assert!(
        covered.iter().all(|event| event.name == "position"),
        "neither write touches the path: {covered:?}"
    );
    let position = covered.last().ok_or("no position")?.clone();
    assert_eq!(position.version(), v3, "{position:?}");
    assert_eq!(position.data["cursor"], Value::from(position.id.clone()));
    // The cursor of a position resumes: nothing before it is sent again.
    let v4 = harness
        .set(&id, "set-todo-00000", v3, "todos/t", json!(1))
        .await?;
    drop(stream);
    let mut resumed = harness.watch(&id, "todos", Some(&position.id)).await?;
    let events = resumed.until(v4).await?;
    assert_eq!(kinds(&events), [("change".to_owned(), v4)]);
    Ok(())
}

// --- the stream: reconnecting -------------------------------------------------

#[tokio::test]
async fn a_reconnect_continues_after_the_cursor_with_no_gap_and_no_duplicate() -> TestResult {
    let harness = Harness::new();
    let id = harness.artifact(OWNER, "resume").await?;
    let mut version = 0;
    let mut stream = harness.watch(&id, "", None).await?;
    let snapshot = stream.expect("snapshot").await?;
    assert_eq!(snapshot.version(), 0);
    let mut received = Vec::new();
    for number in 1..=3 {
        version = harness
            .set(
                &id,
                &format!("set-a{number}-000000"),
                version,
                &format!("k{number}"),
                json!(number),
            )
            .await?;
    }
    received.extend(stream.until(version).await?);
    let cursor = received.last().ok_or("no event")?.id.clone();
    drop(stream);

    // Writes while nobody listens, including a replay and a no-op.
    for number in 4..=7 {
        version = harness
            .set(
                &id,
                &format!("set-a{number}-000000"),
                version,
                &format!("k{number}"),
                json!(number),
            )
            .await?;
    }
    assert_eq!(
        harness.set(&id, "set-a4-000000", 3, "k4", json!(4)).await?,
        version - 3,
        "a replay answers with the version of the original"
    );

    let mut resumed = harness.watch(&id, "", Some(&cursor)).await?;
    let replay = resumed.until(version).await?;
    assert_eq!(
        kinds(&replay),
        [("change", 4), ("change", 5), ("change", 6), ("change", 7)]
            .map(|(name, version)| (name.to_owned(), version)),
        "exactly the writes after the cursor, in order"
    );
    // Live writes follow the replay.
    let next = harness
        .set(&id, "set-a8-000000", version, "k8", json!(8))
        .await?;
    assert_eq!(
        kinds(&resumed.until(next).await?),
        [("change".to_owned(), next)]
    );

    // Across both connections: every version once, in order.
    received.extend(replay);
    let versions: Vec<u64> = received.iter().map(Event::version).collect();
    assert_eq!(versions, [1, 2, 3, 4, 5, 6, 7]);
    Ok(())
}

#[tokio::test]
async fn a_cursor_older_than_the_window_gets_a_reset_with_a_current_snapshot() -> TestResult {
    let harness = Harness::new();
    let id = harness.artifact(OWNER, "window").await?;
    let mut stream = harness.watch(&id, "", None).await?;
    stream.expect("snapshot").await?;
    let mut version = harness.set(&id, "set-first-0000", 0, "n", json!(0)).await?;
    let first = stream.until(version).await?;
    let cursor = first.last().ok_or("no event")?.id.clone();
    drop(stream);

    // More writes than the window keeps (64).
    for number in 1..=70 {
        version = harness
            .set(
                &id,
                &format!("set-w{number:03}-0000"),
                version,
                "n",
                json!(number),
            )
            .await?;
    }
    let mut resumed = harness.watch(&id, "", Some(&cursor)).await?;
    let reset = resumed.expect("reset").await?;
    assert_eq!(
        reset.data,
        json!({
            "cursor": reset.id, "data_version": version, "path": "",
            "exists": true, "value": { "n": 70 }
        })
    );
    // And the stream goes on from there.
    let next = harness
        .set(&id, "set-after-0000", version, "n", json!(71))
        .await?;
    assert_eq!(
        kinds(&resumed.until(next).await?),
        [("change".to_owned(), next)]
    );

    // A cursor just inside the window continues: no reset.
    let inside = format!("d.{}.{}", feed_of(&reset.id), next - 60);
    let mut near = harness.watch(&id, "", Some(&inside)).await?;
    let replay = near.until(next).await?;
    assert_eq!(replay.len(), 60, "{}", replay.len());
    assert!(replay.iter().all(|event| event.name == "change"));
    Ok(())
}

/// The feed identity part of a data cursor.
fn feed_of(cursor: &str) -> String {
    cursor.split('.').nth(1).unwrap_or_default().to_owned()
}

#[tokio::test]
async fn a_cursor_ahead_of_the_data_or_of_another_database_is_reset() -> TestResult {
    let harness = Harness::new();
    let id = harness.artifact(OWNER, "foreign").await?;
    let mut version = 0;
    for number in 1..=3 {
        version = harness
            .set(
                &id,
                &format!("set-f{number}-000000"),
                version,
                "n",
                json!(number),
            )
            .await?;
    }
    let mut live = harness.watch(&id, "", None).await?;
    let snapshot = live.expect("snapshot").await?;
    let feed = feed_of(&snapshot.id);

    // Ahead of the data.
    let ahead = format!("d.{feed}.{}", version + 5);
    let mut stream = harness.watch(&id, "", Some(&ahead)).await?;
    let reset = stream.expect("reset").await?;
    assert_eq!(reset.version(), version);
    assert_eq!(reset.data["value"], json!({ "n": 3 }));

    // Another database, below its head: the restore has the same data and
    // another feed identity.
    let restored = harness.restored();
    let below = format!("d.{feed}.{}", version - 1);
    let mut stream = restored.watch(&id, "", Some(&below)).await?;
    let reset = stream.expect("reset").await?;
    assert_eq!(reset.version(), version);
    // The restored database's own cursor continues.
    let own = format!("d.{}.{}", feed_of(&reset.id), version - 1);
    let mut stream = restored.watch(&id, "", Some(&own)).await?;
    // The restore's window is empty, and the version before the head is not
    // one it can vouch for: reset again, with the head.
    stream.expect("reset").await?;
    Ok(())
}

#[tokio::test]
async fn a_malformed_cursor_or_a_change_feed_cursor_is_refused() -> TestResult {
    let harness = Harness::new();
    let id = harness.artifact(OWNER, "cursors").await?;
    let feed = "0f".repeat(16);
    for cursor in [
        "nonsense".to_owned(),
        format!("{feed}.3"),
        format!("d.{feed}"),
        format!("d.{feed}.03"),
        format!("d.{feed}.-1"),
        format!("D.{feed}.3"),
    ] {
        let response = harness
            .request(
                "GET",
                &format!("/v1/artifacts/{id}/data/events"),
                OWNER,
                None,
                None,
                Some(&cursor),
            )
            .await?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{cursor}");
    }
    // A change feed event's id is a change cursor, not a data cursor.
    let mut feed_stream = harness
        .request("GET", "/v1/changes", OWNER, None, None, None)
        .await?
        .into_body();
    let frame = poll_fn(|context| Pin::new(&mut feed_stream).poll_frame(context))
        .await
        .ok_or("no frame")??;
    let text = String::from_utf8(frame.into_data().map_err(|_| "no data")?.to_vec())?;
    let change_cursor = text
        .lines()
        .find_map(|line| line.strip_prefix("id: "))
        .ok_or("no id")?
        .to_owned();
    let response = harness
        .request(
            "GET",
            &format!("/v1/artifacts/{id}/data/events"),
            OWNER,
            None,
            None,
            Some(&change_cursor),
        )
        .await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    Ok(())
}

#[tokio::test]
async fn after_a_restart_a_cursor_continues_only_if_nothing_was_written_since() -> TestResult {
    let harness = Harness::new();
    let id = harness.artifact(OWNER, "restart").await?;
    let v1 = harness.set(&id, "set-r1-000000", 0, "n", json!(1)).await?;
    let mut stream = harness.watch(&id, "", None).await?;
    let current = stream.expect("snapshot").await?;
    let v2 = harness.set(&id, "set-r2-000000", v1, "n", json!(2)).await?;
    let latest = stream.until(v2).await?.pop().ok_or("no event")?;
    drop(stream);

    let restarted = harness.reopened();
    // Nothing was written since: the cursor continues, and says so.
    let mut resumed = restarted.watch(&id, "", Some(&latest.id)).await?;
    let position = resumed.expect("position").await?;
    assert_eq!(position.version(), v2);
    // A write after the restart is sent.
    let v3 = restarted
        .set(&id, "set-r3-000000", v2, "n", json!(3))
        .await?;
    assert_eq!(
        kinds(&resumed.until(v3).await?),
        [("change".to_owned(), v3)]
    );
    drop(resumed);

    // A cursor from before the restart that is behind a write is reset: the
    // window was lost with the process.
    let restarted_again = restarted.reopened();
    let mut behind = restarted_again.watch(&id, "", Some(&current.id)).await?;
    let reset = behind.expect("reset").await?;
    assert_eq!(
        (reset.version(), reset.data["value"].clone()),
        (v3, json!({ "n": 3 }))
    );
    Ok(())
}

#[tokio::test]
async fn a_write_this_process_did_not_make_resets_the_streams_behind_it() -> TestResult {
    let harness = Harness::new();
    let id = harness.artifact(OWNER, "foreign-write").await?;
    let v1 = harness.set(&id, "set-p1-000000", 0, "n", json!(1)).await?;
    let mut stream = harness.watch(&id, "", None).await?;
    stream.expect("snapshot").await?;

    // Another process on the same database writes; this one never sees it.
    let other = harness.reopened();
    let v2 = other.set(&id, "set-p2-000000", v1, "n", json!(2)).await?;
    // A commit of this process makes the stream read storage, and the window
    // does not cover the version it finds.
    harness
        .ok(
            "PATCH",
            &format!("/v1/artifacts/{id}"),
            OWNER,
            Some("rename-000000"),
            Some(&json!({ "base_metadata_revision": 1, "title": "Renamed" })),
        )
        .await?;
    let reset = stream.expect("reset").await?;
    assert_eq!(
        (reset.version(), reset.data["value"].clone()),
        (v2, json!({ "n": 2 }))
    );
    Ok(())
}

// --- the stream: writers --------------------------------------------------------

/// The state a reader holds for a watched path after applying events.
fn apply(state: &mut Option<Value>, watched: &str, event: &Event) {
    let path = event.data["path"].as_str().unwrap_or_default();
    let value = event.data.get("value").cloned();
    let relative: Vec<&str> = match (watched.is_empty(), path == watched) {
        (_, true) => Vec::new(),
        (true, false) => path.split('/').collect(),
        (false, false) => path
            .strip_prefix(&format!("{watched}/"))
            .unwrap_or_else(|| panic!("{path} is not within {watched}"))
            .split('/')
            .collect(),
    };
    if relative.is_empty() {
        *state = value;
        return;
    }
    if let Some(value) = value {
        let mut node = state.get_or_insert_with(|| json!({}));
        for name in &relative[..relative.len() - 1] {
            if !node.is_object() {
                *node = json!({});
            }
            node = &mut node[*name];
            if node.is_null() {
                *node = json!({});
            }
        }
        if !node.is_object() {
            *node = json!({});
        }
        node[relative[relative.len() - 1]] = value;
    } else {
        let mut node = state.as_mut();
        for name in &relative[..relative.len() - 1] {
            node = node.and_then(|node| node.get_mut(*name));
        }
        if let Some(object) = node.and_then(Value::as_object_mut) {
            object.remove(relative[relative.len() - 1]);
        }
    }
}

#[tokio::test]
async fn a_subscriber_converges_on_the_state_while_writers_run_concurrently() -> TestResult {
    let harness = Harness::new();
    let id = harness.artifact(OWNER, "converge").await?;
    let mut root = harness.watch(&id, "", None).await?;
    let mut todos = harness.watch(&id, "todos", None).await?;
    let mut root_state = root.expect("snapshot").await?.data.get("value").cloned();
    let mut todos_state = todos.expect("snapshot").await?.data.get("value").cloned();

    let writers: Vec<_> = (0..4)
        .map(|writer| {
            let harness = harness.reopened_router();
            let id = id.clone();
            tokio::spawn(async move {
                let run = async {
                    let mut written = 0;
                    for round in 0..10 {
                        let version = harness.version(&id).await?;
                        let key = format!("c{writer}-{round:02}");
                        let path = format!("todos/w{writer}/i{round}");
                        let outcome = match round % 5 {
                            0 | 1 => harness.try_set(&id, &key, version, &path).await?,
                            2 => {
                                harness
                                    .try_merge(&id, &key, version, &format!("todos/w{writer}"))
                                    .await?
                            }
                            3 => {
                                harness
                                    .try_set(&id, &key, version, "settings/theme")
                                    .await?
                            }
                            _ => {
                                harness
                                    .try_delete(&id, &key, version, &format!("todos/w{writer}/i0"))
                                    .await?
                            }
                        };
                        if outcome {
                            written += 1;
                        }
                    }
                    Ok::<u32, TestError>(written)
                };
                run.await.map_err(|error| error.to_string())
            })
        })
        .collect();
    let mut changed = 0;
    for writer in writers {
        changed += writer.await??;
    }
    assert!(changed > 0, "no writer wrote anything");
    let last = harness.version(&id).await?;

    // The root watcher sees every version once and in order.
    let events = root.until(last).await?;
    let versions: Vec<u64> = events.iter().map(Event::version).collect();
    assert_eq!(
        versions,
        (1..=last).collect::<Vec<_>>(),
        "a gap or a duplicate"
    );
    for event in &events {
        apply(&mut root_state, "", event);
    }
    for event in todos.until(last).await? {
        if event.name == "change" {
            apply(&mut todos_state, "todos", &event);
        }
    }
    let read = harness
        .ok(
            "GET",
            &format!("/v1/artifacts/{id}/data"),
            OWNER,
            None,
            None,
        )
        .await?;
    assert_eq!(root_state, read.get("value").cloned(), "the root diverged");
    let read = harness
        .ok(
            "GET",
            &format!("/v1/artifacts/{id}/data?path=todos"),
            OWNER,
            None,
            None,
        )
        .await?;
    assert_eq!(todos_state, read.get("value").cloned(), "todos diverged");
    Ok(())
}

impl Harness {
    /// A handle on this server for another task; it shares nothing but the
    /// database and the process-wide hub of the original.
    fn reopened_router(&self) -> Self {
        Self {
            directory: self.directory.clone(),
            owns_directory: false,
            api: self.api.clone(),
        }
    }

    async fn version(&self, id: &str) -> Result<u64, TestError> {
        let read = self
            .ok(
                "GET",
                &format!("/v1/artifacts/{id}/data"),
                OWNER,
                None,
                None,
            )
            .await?;
        read["data_version"]
            .as_u64()
            .ok_or_else(|| "no version".into())
    }

    /// A set of a small value, or `false` when another writer won the version.
    async fn try_set(&self, id: &str, key: &str, base: u64, path: &str) -> Result<bool, TestError> {
        self.attempt(
            "PUT",
            id,
            key,
            &json!({ "path": path, "base_data_version": base, "value": { "at": key } }),
        )
        .await
    }

    async fn try_merge(
        &self,
        id: &str,
        key: &str,
        base: u64,
        path: &str,
    ) -> Result<bool, TestError> {
        self.attempt("PATCH", id, key, &json!({ "path": path, "base_data_version": base, "patch": { "merged": key, "i1": null } }))
            .await
    }

    async fn try_delete(
        &self,
        id: &str,
        key: &str,
        base: u64,
        path: &str,
    ) -> Result<bool, TestError> {
        self.attempt(
            "DELETE",
            id,
            key,
            &json!({ "path": path, "base_data_version": base }),
        )
        .await
    }

    async fn attempt(
        &self,
        method: &str,
        id: &str,
        key: &str,
        body: &Value,
    ) -> Result<bool, TestError> {
        let reply = self
            .call(
                method,
                &format!("/v1/artifacts/{id}/data"),
                OWNER,
                Some(key),
                Some(body),
            )
            .await?;
        match reply.status {
            StatusCode::OK => Ok(reply.json()?["changed"] == true),
            StatusCode::CONFLICT => Ok(false),
            other => Err(format!("{method} {key}: {other} {}", reply.text).into()),
        }
    }
}

#[tokio::test]
async fn writes_over_mcp_reach_a_stream_through_the_same_path() -> TestResult {
    let harness = Harness::new();
    let id = harness.artifact(OWNER, "mcp").await?;
    let mut stream = harness.watch(&id, "notes", None).await?;
    stream.expect("snapshot").await?;
    let response = harness
        .api
        .clone()
        .oneshot(
            Request::post("/mcp")
                .header(header::HOST, "127.0.0.1:7410")
                .header(header::AUTHORIZATION, format!("Bearer {OWNER}"))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "application/json, text/event-stream")
                .header("mcp-protocol-version", "2025-11-25")
                .body(Body::from(
                    json!({
                        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                        "params": { "name": "set_data", "arguments": {
                            "artifact_id": id, "path": "notes", "base_data_version": 0,
                            "value": "{\"from\":\"agent\"}", "idempotency_key": "mcp-set-0000000"
                        } }
                    })
                    .to_string(),
                ))?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let text = String::from_utf8(to_bytes(response.into_body(), usize::MAX).await?.to_vec())?;
    assert!(text.contains("\"data_version\":1"), "{text}");
    let change = stream.expect("change").await?;
    assert_eq!(
        (change.version(), change.data["value"].clone()),
        (1, json!({ "from": "agent" }))
    );
    Ok(())
}

// --- revocation and limits -----------------------------------------------------

#[tokio::test]
async fn disabling_a_user_ends_their_data_streams_and_no_one_elses() -> TestResult {
    let harness = Harness::new();
    let (alice_id, alice, _) = harness.person("alice").await?;
    let (_, bob, _) = harness.person("bob").await?;
    let alices = harness.artifact(&alice, "alice-data").await?;
    let bobs = harness.artifact(&bob, "bob-data").await?;
    let mut hers = harness.watching(&alice, &alices, "", None).await?;
    let mut his = harness.watching(&bob, &bobs, "", None).await?;
    hers.expect("snapshot").await?;
    his.expect("snapshot").await?;

    harness
        .ok(
            "POST",
            &format!("/v1/users/{alice_id}/disable"),
            OWNER,
            None,
            None,
        )
        .await?;
    assert!(hers.ended().await?, "the stream outlived the user");
    // Her token no longer opens one: the same 401 as any unknown token.
    let reply = harness
        .call(
            "GET",
            &format!("/v1/artifacts/{alices}/data/events"),
            &alice,
            None,
            None,
        )
        .await?;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    // Bob's stream goes on.
    harness
        .ok(
            "PUT",
            &format!("/v1/artifacts/{bobs}/data"),
            &bob,
            Some("bob-set-000000"),
            Some(&json!({ "path": "n", "base_data_version": 0, "value": 1 })),
        )
        .await?;
    assert_eq!(his.expect("change").await?.version(), 1);
    Ok(())
}

#[tokio::test]
async fn revoking_a_token_ends_the_streams_it_opened() -> TestResult {
    let harness = Harness::new();
    let (_, secret, token) = harness.person("carol").await?;
    let id = harness.artifact(&secret, "carol-data").await?;
    let mut stream = harness.watching(&secret, &id, "", None).await?;
    stream.expect("snapshot").await?;
    harness
        .ok("DELETE", &format!("/v1/tokens/{token}"), OWNER, None, None)
        .await?;
    assert!(stream.ended().await?, "the stream outlived its token");
    Ok(())
}

#[tokio::test]
async fn data_streams_are_limited_per_actor_and_a_closed_one_frees_its_place() -> TestResult {
    let harness = Harness::new();
    let id = harness.artifact(OWNER, "limit").await?;
    let mut open = Vec::new();
    for _ in 0..8 {
        let mut stream = harness.watch(&id, "", None).await?;
        stream.expect("snapshot").await?;
        open.push(stream);
    }
    // The refusal is a plain response: its body ends.
    let refused = harness
        .call(
            "GET",
            &format!("/v1/artifacts/{id}/data/events"),
            OWNER,
            None,
            None,
        )
        .await?;
    assert_eq!(
        refused.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        refused.text
    );
    assert_eq!(refused.retry_after.as_deref(), Some("5"));
    assert_eq!(refused.json()?["error"]["code"], "rate_limited");
    // Another user is unaffected, and the change feed has its own slots.
    let (_, other, _) = harness.person("other").await?;
    let theirs = harness.artifact(&other, "theirs").await?;
    harness
        .watching(&other, &theirs, "", None)
        .await?
        .expect("snapshot")
        .await?;
    harness.ok_stream("/v1/changes", OWNER).await?;

    // A reader that goes away frees its place, without any further event.
    drop(open.pop());
    let mut reopened = false;
    for _ in 0..500 {
        let response = harness
            .request(
                "GET",
                &format!("/v1/artifacts/{id}/data/events"),
                OWNER,
                None,
                None,
                None,
            )
            .await?;
        if response.status() == StatusCode::OK {
            reopened = true;
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(reopened, "the dropped stream still holds its place");
    Ok(())
}

impl Harness {
    /// Opens a stream of another route and checks that it is accepted.
    async fn ok_stream(&self, route: &str, token: &str) -> TestResult {
        let response = self.request("GET", route, token, None, None, None).await?;
        assert_eq!(response.status(), StatusCode::OK);
        Ok(())
    }
}

#[tokio::test]
async fn a_stream_for_a_missing_path_grammar_or_an_unknown_parameter_is_refused() -> TestResult {
    let harness = Harness::new();
    let id = harness.artifact(OWNER, "bad-stream").await?;
    for (query, status) in [
        ("?path=a//b", StatusCode::UNPROCESSABLE_ENTITY),
        ("?surprise=1", StatusCode::BAD_REQUEST),
        ("?path=a&path=b", StatusCode::BAD_REQUEST),
    ] {
        let reply = harness
            .call(
                "GET",
                &format!("/v1/artifacts/{id}/data/events{query}"),
                OWNER,
                None,
                None,
            )
            .await?;
        assert_eq!(reply.status, status, "{query}: {}", reply.text);
    }
    // No token, no stream.
    let response = harness
        .api
        .clone()
        .oneshot(Request::get(format!("/v1/artifacts/{id}/data/events")).body(Body::empty())?)
        .await?;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}
