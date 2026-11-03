//! Data queries and subscriptions (decision 0040) through the real router,
//! core, and `SQLite` store: members are listed in byte order a page at a
//! time, and a stream sends a snapshot and then the newest state, ends when
//! access ends, and is bounded.

use std::future::poll_fn;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use artifree_providers::storage::{DATABASE_FILE, SqliteStore};
use artifree_providers::tokens::SqliteTokenStore;
use artifree_server::auth::AccessToken;
use artifree_server::data_events::DataStreamSettings;
use artifree_server::http::{self, AppState, Settings};
use artifree_server::limits::RequestLimits;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use http_body::Body as _;
use serde_json::{Value, json};
use tower::ServiceExt;

type TestError = Box<dyn std::error::Error>;
type TestResult = Result<(), TestError>;

const OWNER: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// Short intervals: tests wait for events, not for the clock.
fn quick(per_actor: u32, process: u32) -> DataStreamSettings {
    DataStreamSettings {
        per_actor,
        process,
        heartbeat: Duration::from_millis(150),
        min_read_interval: Duration::from_millis(10),
        lifetime: Duration::from_secs(60),
    }
}

struct Harness {
    directory: PathBuf,
    state: AppState,
    api: Router,
    keys: AtomicU32,
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

struct Person {
    id: String,
    secret: String,
}

impl Harness {
    fn new(streams: DataStreamSettings) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!(
            "artifree-data-queries-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory)
            .unwrap_or_else(|error| panic!("cannot create test directory: {error}"));
        let database = directory.join(DATABASE_FILE);
        let state = AppState::new(
            SqliteStore::open(&database).unwrap_or_else(|error| panic!("store: {error}")),
            SqliteTokenStore::open(&database).unwrap_or_else(|error| panic!("tokens: {error}")),
            AccessToken::parse(OWNER).unwrap_or_else(|| panic!("fixture token")),
            Settings {
                content_origin: "http://127.0.0.1:7411".into(),
                limits: RequestLimits {
                    burst: 100_000,
                    per_minute: 6_000_000,
                    concurrent: 64,
                },
                data_streams: streams,
                ..Settings::default()
            },
        )
        .unwrap_or_else(|error| panic!("cannot build state: {error}"));
        Self {
            directory,
            api: http::router(state.clone()),
            state,
            keys: AtomicU32::new(0),
        }
    }

    fn key(&self) -> String {
        format!(
            "data-queries-{:06}",
            self.keys.fetch_add(1, Ordering::Relaxed)
        )
    }

    async fn send(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<&Value>,
    ) -> Result<Reply, TestError> {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        if method != "GET" {
            builder = builder.header("idempotency-key", self.key());
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

    async fn owner(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Reply, TestError> {
        self.send(method, path, Some(OWNER), body).await
    }

    async fn artifact_for(&self, token: &str, title: &str) -> Result<String, TestError> {
        let created = self
            .send(
                "POST",
                "/v1/artifacts",
                Some(token),
                Some(&json!({
                    "title": title,
                    "files": [{ "path": "index.html", "encoding": "utf8", "content": "<p>x</p>" }]
                })),
            )
            .await?;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
        Ok(created.json()?["artifact"]["id"]
            .as_str()
            .ok_or("no id")?
            .to_owned())
    }

    async fn artifact(&self, title: &str) -> Result<String, TestError> {
        self.artifact_for(OWNER, title).await
    }

    /// Writes `value` at `path` (set) on top of `base`, as `token`.
    async fn set_as(
        &self,
        token: &str,
        id: &str,
        path: &str,
        base: u64,
        value: &Value,
    ) -> Result<Reply, TestError> {
        self.send(
            "PUT",
            &format!("/v1/artifacts/{id}/data"),
            Some(token),
            Some(&json!({ "path": path, "base_data_version": base, "value": value })),
        )
        .await
    }

    /// A set by the owner that must succeed; returns the new version.
    async fn set(&self, id: &str, path: &str, base: u64, value: &Value) -> Result<u64, TestError> {
        let reply = self.set_as(OWNER, id, path, base, value).await?;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
        Ok(reply.json()?["data_version"].as_u64().ok_or("version")?)
    }

    async fn members(&self, id: &str, query: &str) -> Result<Reply, TestError> {
        self.owner(
            "GET",
            &format!("/v1/artifacts/{id}/data/members{query}"),
            None,
        )
        .await
    }

    async fn person(&self, label: &str) -> Result<Person, TestError> {
        let user = self
            .owner("POST", "/v1/users", Some(&json!({ "label": label })))
            .await?;
        assert_eq!(user.status, StatusCode::CREATED, "{}", user.text);
        let id = user.json()?["id"].as_str().ok_or("user id")?.to_owned();
        let token = self
            .owner(
                "POST",
                "/v1/tokens",
                Some(&json!({ "name": label, "user_id": id })),
            )
            .await?;
        assert_eq!(token.status, StatusCode::CREATED, "{}", token.text);
        let body = token.json()?;
        Ok(Person {
            id,
            secret: body["secret"].as_str().ok_or("secret")?.to_owned(),
        })
    }

    /// Opens a stream; the response is returned so that a refusal can be read.
    async fn open_as(
        &self,
        token: &str,
        id: &str,
        query: &str,
        extra: Option<(&str, &str)>,
    ) -> Result<axum::http::Response<Body>, TestError> {
        let mut request = Request::get(format!("/v1/artifacts/{id}/data/events{query}"))
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::ACCEPT, "text/event-stream");
        if let Some((name, value)) = extra {
            request = request.header(name, value);
        }
        Ok(self
            .api
            .clone()
            .oneshot(request.body(Body::empty())?)
            .await?)
    }

    /// Opens a stream that must be accepted.
    async fn follow_as(&self, token: &str, id: &str, query: &str) -> Result<Events, TestError> {
        let response = self.open_as(token, id, query, None).await?;
        let status = response.status();
        if status != StatusCode::OK {
            let bytes = to_bytes(response.into_body(), usize::MAX).await?;
            return Err(format!("{status}: {}", String::from_utf8_lossy(&bytes)).into());
        }
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
            transcript: String::new(),
        })
    }

    async fn follow(&self, id: &str, query: &str) -> Result<Events, TestError> {
        self.follow_as(OWNER, id, query).await
    }

    /// Changes the stored space behind the server's back, as another process
    /// on the same database would. No wake-up is sent.
    fn raw(&self, statement: &str) -> TestResult {
        let connection = rusqlite::Connection::open(self.directory.join(DATABASE_FILE))?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute(statement, [])?;
        Ok(())
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
    data: Value,
}

/// Reads the events of one stream.
struct Events {
    body: Body,
    buffer: String,
    /// Everything received, comments included.
    transcript: String,
}

impl Events {
    async fn frame(&mut self) -> Result<bool, TestError> {
        let frame = tokio::time::timeout(
            Duration::from_secs(5),
            poll_fn(|context| Pin::new(&mut self.body).poll_frame(context)),
        )
        .await
        .map_err(|_| format!("nothing within 5 s; saw {:?}", self.transcript))?;
        match frame {
            None => Ok(false),
            Some(frame) => {
                if let Ok(data) = frame?.into_data() {
                    let text = std::str::from_utf8(&data)?;
                    self.buffer.push_str(text);
                    self.transcript.push_str(text);
                }
                Ok(true)
            }
        }
    }

    /// The next event, skipping comment lines; `None` when the stream ended.
    /// Fails after 10 s in all: heartbeat comments keep frames arriving, so a
    /// stream that never sends the event would otherwise wait for ever.
    async fn next(&mut self) -> Result<Option<Event>, TestError> {
        tokio::time::timeout(Duration::from_secs(10), self.read_event())
            .await
            .map_err(|_| format!("no event within 10 s; saw {:?}", self.transcript))?
    }

    async fn read_event(&mut self) -> Result<Option<Event>, TestError> {
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
                assert!(field("id").is_none(), "a data event has no id: {block:?}");
                return Ok(Some(Event {
                    name,
                    data: serde_json::from_str(&field("data").ok_or("an event without data")?)?,
                }));
            }
            if !self.frame().await? {
                return Ok(None);
            }
        }
    }

    async fn expect(&mut self, name: &str) -> Result<Event, TestError> {
        let event = self
            .next()
            .await?
            .ok_or_else(|| format!("the stream ended; wanted {name}"))?;
        assert_eq!(event.name, name, "{event:?}");
        Ok(event)
    }

    /// Reads until the stream ends; fails if it does not within 10 s in all
    /// (heartbeat comments must not keep a stream that should end waiting).
    /// Returns the events that arrived on the way.
    async fn ends(&mut self) -> Result<Vec<Event>, TestError> {
        let reading = async {
            let mut events = Vec::new();
            while let Some(event) = self.next().await? {
                events.push(event);
            }
            Ok::<_, TestError>(events)
        };
        tokio::time::timeout(Duration::from_secs(10), reading)
            .await
            .map_err(|_| "the stream did not end within 10 s")?
    }

    /// Waits until `text` is in the transcript (a comment line, say).
    async fn until_seen(&mut self, text: &str) -> TestResult {
        while !self.transcript.contains(text) {
            if !self.frame().await? {
                return Err(format!("the stream ended before {text:?}").into());
            }
        }
        Ok(())
    }
}

/// Retries `attempt` until it succeeds, for something the server does in its
/// own task (releasing a slot). Fails after 5 s.
async fn eventually<F, Fut>(what: &str, mut attempt: F) -> TestResult
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<bool, TestError>>,
{
    let give_up = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if attempt().await? {
            return Ok(());
        }
        if tokio::time::Instant::now() >= give_up {
            return Err(format!("{what} did not happen within 5 s").into());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Percent-encodes every byte that is not unreserved.
fn encode(text: &str) -> String {
    use std::fmt::Write as _;
    text.bytes().fold(String::new(), |mut out, byte| {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
        out
    })
}

fn names(page: &Value) -> Vec<String> {
    page["members"]
        .as_array()
        .map(|members| {
            members
                .iter()
                .filter_map(|member| member["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

// Queries --------------------------------------------------------------------

#[tokio::test]
async fn members_are_listed_in_byte_order_and_paged_by_name() -> TestResult {
    let harness = Harness::new(quick(8, 64));
    let id = harness.artifact("members").await?;
    // "B" < "a" < U+00E9 < U+FF5E < U+10000 by UTF-8 bytes (not by UTF-16).
    let space = json!({
        "list": { "a": 1, "B": 2, "\u{e9}": 3, "\u{ff5e}": 4, "\u{10000}": 5, "ab": 6 },
        "other": true
    });
    let version = harness.set(&id, "", 0, &space).await?;

    let all = harness.members(&id, "?path=list&limit=100").await?;
    assert_eq!(all.status, StatusCode::OK, "{}", all.text);
    let all = all.json()?;
    assert_eq!(
        names(&all),
        ["B", "a", "ab", "\u{e9}", "\u{ff5e}", "\u{10000}"]
    );
    assert_eq!(all["data_version"], version);
    assert_eq!(all["collection"], true);
    assert!(all.get("next_after").is_none(), "{all}");
    assert_eq!(
        all["query"],
        json!({ "path": "list", "prefix": "", "select": "names", "limit": 100 })
    );
    // Names only: no member has a value or a too_large flag.
    assert_eq!(all["members"][0], json!({ "name": "B" }));

    let mut seen = Vec::new();
    let mut after = None;
    let mut pages = 0;
    loop {
        let query = after.as_ref().map_or_else(
            || "?path=list&limit=4".to_owned(),
            |name: &String| format!("?path=list&limit=4&after={}", encode(name)),
        );
        let page = harness.members(&id, &query).await?.json()?;
        seen.extend(names(&page));
        pages += 1;
        match page.get("next_after").and_then(Value::as_str) {
            Some(next) => after = Some(next.to_owned()),
            None => break,
        }
        assert!(pages < 5, "paging does not end: {seen:?}");
    }
    assert_eq!(seen, names(&all), "the pages are the whole list, once");
    assert_eq!(pages, 2);

    let prefixed = harness
        .members(&id, "?path=list&prefix=a&select=values")
        .await?
        .json()?;
    assert_eq!(
        prefixed["members"],
        json!([{ "name": "a", "value": 1 }, { "name": "ab", "value": 6 }])
    );
    assert_eq!(prefixed["query"]["prefix"], "a");
    assert_eq!(prefixed["query"]["select"], "values");
    assert_eq!(prefixed["query"]["limit"], 25);
    Ok(())
}

#[tokio::test]
async fn a_cursor_survives_writes_between_pages() -> TestResult {
    let harness = Harness::new(quick(8, 64));
    let id = harness.artifact("stable").await?;
    let mut version = harness
        .set(
            &id,
            "",
            0,
            &json!({ "c": { "a": 1, "b": 2, "c": 3, "d": 4 } }),
        )
        .await?;
    let first = harness.members(&id, "?path=c&limit=2").await?.json()?;
    assert_eq!(names(&first), ["a", "b"]);
    assert_eq!(first["next_after"], "b");
    // The cursor's own member is deleted, and members are written before and
    // after the cursor, between the two pages.
    version = harness
        .set(
            &id,
            "c",
            version,
            &json!({ "0": 0, "a": 1, "bb": 5, "c": 3, "d": 4 }),
        )
        .await?;
    let second = harness
        .members(&id, "?path=c&limit=2&after=b")
        .await?
        .json()?;
    assert_eq!(names(&second), ["bb", "c"]);
    assert_eq!(
        second["data_version"], version,
        "a page names its own version"
    );
    let third = harness
        .members(&id, "?path=c&limit=2&after=c")
        .await?
        .json()?;
    assert_eq!(names(&third), ["d"]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paging_during_concurrent_writes_repeats_and_skips_nothing() -> TestResult {
    let harness = std::sync::Arc::new(Harness::new(quick(8, 64)));
    let id = harness.artifact("concurrent").await?;
    let mut seeded = serde_json::Map::new();
    for number in (0..60).step_by(2) {
        seeded.insert(format!("m{number:03}"), json!(number));
    }
    let mut version = harness.set(&id, "", 0, &json!({ "c": seeded })).await?;
    let writer = {
        let (harness, id) = (harness.clone(), id.clone());
        tokio::spawn(async move {
            for number in (1..60).step_by(2) {
                let path = format!("c/m{number:03}");
                let reply = harness
                    .send(
                        "PATCH",
                        &format!("/v1/artifacts/{id}/data"),
                        Some(OWNER),
                        Some(&json!({
                            "path": path, "base_data_version": version, "patch": number
                        })),
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                if reply.status != StatusCode::OK {
                    return Err(reply.text);
                }
                version += 1;
            }
            Ok(())
        })
    };
    let mut seen: Vec<String> = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let query = after.as_ref().map_or_else(
            || "?path=c&limit=7".to_owned(),
            |name| format!("?path=c&limit=7&after={}", encode(name)),
        );
        let page = harness.members(&id, &query).await?.json()?;
        seen.extend(names(&page));
        match page.get("next_after").and_then(Value::as_str) {
            Some(next) => after = Some(next.to_owned()),
            None => break,
        }
    }
    writer.await?.map_err(TestError::from)?;
    assert!(
        seen.windows(2).all(|pair| pair[0] < pair[1]),
        "names repeat or go backwards: {seen:?}"
    );
    for number in (0..60).step_by(2) {
        assert!(
            seen.contains(&format!("m{number:03}")),
            "m{number:03} was present throughout and was skipped: {seen:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_member_has_exactly_one_of_value_and_too_large() -> TestResult {
    let harness = Harness::new(quick(8, 64));
    let id = harness.artifact("bounds").await?;
    let big = "x".repeat(140_000);
    let version = harness
        .set(
            &id,
            "",
            0,
            &json!({ "c": { "big": big, "nul": null, "obj": { "k": [1, 2] }, "z": "z" } }),
        )
        .await?;
    let page = harness
        .members(&id, "?path=c&select=values")
        .await?
        .json()?;
    assert_eq!(page["data_version"], version);
    assert_eq!(
        page["members"],
        json!([
            { "name": "big", "too_large": true },
            { "name": "nul", "value": null },
            { "name": "obj", "value": { "k": [1, 2] } },
            { "name": "z", "value": "z" }
        ])
    );
    let plain = harness.members(&id, "?path=c").await?.json()?;
    for member in plain["members"].as_array().ok_or("members")? {
        assert_eq!(
            member.as_object().map(serde_json::Map::len),
            Some(1),
            "names only: {member}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_page_ends_before_the_member_that_would_pass_the_value_bound() -> TestResult {
    let harness = Harness::new(quick(8, 64));
    let id = harness.artifact("page-bound").await?;
    let part = "y".repeat(60_000);
    harness
        .set(
            &id,
            "",
            0,
            &json!({ "c": { "a": part, "b": part, "c": part, "d": 1 } }),
        )
        .await?;
    let first = harness
        .members(&id, "?path=c&select=values&limit=100")
        .await?
        .json()?;
    assert_eq!(names(&first), ["a", "b"]);
    assert_eq!(first["next_after"], "b");
    let second = harness
        .members(&id, "?path=c&select=values&limit=100&after=b")
        .await?
        .json()?;
    assert_eq!(names(&second), ["c", "d"]);
    assert!(second.get("next_after").is_none());
    Ok(())
}

#[tokio::test]
async fn a_path_that_is_not_a_collection_lists_nothing() -> TestResult {
    let harness = Harness::new(quick(8, 64));
    let id = harness.artifact("scalar").await?;
    harness
        .set(&id, "", 0, &json!({ "n": 7, "list": [1, 2], "o": {} }))
        .await?;
    for path in ["n", "list", "missing", "n/deeper", "list/0"] {
        let page = harness.members(&id, &format!("?path={path}")).await?;
        assert_eq!(page.status, StatusCode::OK, "{path}: {}", page.text);
        let page = page.json()?;
        assert_eq!(page["collection"], false, "{path}");
        assert_eq!(page["members"], json!([]), "{path}");
    }
    let empty = harness.members(&id, "?path=o").await?.json()?;
    assert_eq!(empty["collection"], true);
    assert_eq!(empty["members"], json!([]));
    // The root of a space nobody wrote is an empty collection at version 0.
    let fresh = harness.artifact("unwritten").await?;
    let root = harness.members(&fresh, "").await?.json()?;
    assert_eq!(
        (root["collection"].clone(), root["data_version"].clone()),
        (json!(true), json!(0))
    );
    Ok(())
}

#[tokio::test]
async fn invalid_queries_are_refused_by_rule_and_never_echoed() -> TestResult {
    let harness = Harness::new(quick(8, 64));
    let id = harness.artifact("invalid").await?;
    let long = "p".repeat(257);
    let deep = vec!["s"; 17].join("/");
    let cases: Vec<(String, StatusCode)> = vec![
        ("?limit=0".into(), StatusCode::UNPROCESSABLE_ENTITY),
        ("?limit=101".into(), StatusCode::UNPROCESSABLE_ENTITY),
        ("?limit=-1".into(), StatusCode::BAD_REQUEST),
        ("?limit=ten".into(), StatusCode::BAD_REQUEST),
        ("?limit=5&limit=6".into(), StatusCode::BAD_REQUEST),
        ("?select=keys".into(), StatusCode::BAD_REQUEST),
        (
            "?select=names&select=values".into(),
            StatusCode::BAD_REQUEST,
        ),
        ("?order=desc".into(), StatusCode::BAD_REQUEST),
        (format!("?prefix={long}"), StatusCode::UNPROCESSABLE_ENTITY),
        ("?after=".into(), StatusCode::UNPROCESSABLE_ENTITY),
        (format!("?after={long}"), StatusCode::UNPROCESSABLE_ENTITY),
        ("?path=a//b".into(), StatusCode::UNPROCESSABLE_ENTITY),
        (format!("?path={deep}"), StatusCode::UNPROCESSABLE_ENTITY),
        ("?path=a&path=b".into(), StatusCode::BAD_REQUEST),
    ];
    for (query, status) in &cases {
        let reply = harness.members(&id, query).await?;
        assert_eq!(reply.status, *status, "{query}: {}", reply.text);
        assert!(!reply.text.contains(&long), "{query} echoed the value");
    }
    // The bounds themselves are accepted.
    for query in [
        "?limit=1".to_owned(),
        "?limit=100".to_owned(),
        format!("?prefix={}&after={}", "p".repeat(256), "a".repeat(256)),
        format!("?path={}", vec!["s"; 16].join("/")),
    ] {
        let reply = harness.members(&id, &query).await?;
        assert_eq!(reply.status, StatusCode::OK, "{query}: {}", reply.text);
    }
    Ok(())
}

#[tokio::test]
async fn a_stored_space_that_cannot_be_read_is_a_typed_server_error() -> TestResult {
    let harness = Harness::new(quick(8, 64));
    let id = harness.artifact("corrupt").await?;
    harness.set(&id, "", 0, &json!({ "c": { "a": 1 } })).await?;
    // Valid JSON for SQLite, but a repeated member name breaks the rules of a
    // space: it must not read as a member with neither value nor flag.
    harness.raw("UPDATE app_data SET document = '{\"c\":{\"a\":1,\"a\":2}}'")?;
    let reply = harness.members(&id, "?path=c&select=values").await?;
    assert_eq!(
        reply.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{}",
        reply.text
    );
    assert_eq!(reply.json()?["error"]["code"], "unavailable");
    let stream = harness.open_as(OWNER, &id, "?path=c", None).await?;
    assert_eq!(stream.status(), StatusCode::SERVICE_UNAVAILABLE);
    Ok(())
}

#[tokio::test]
async fn the_routes_need_a_token_and_work_with_an_issued_agent_token() -> TestResult {
    let harness = Harness::new(quick(8, 64));
    let id = harness.artifact("tokens").await?;
    harness.set(&id, "", 0, &json!({ "c": { "a": 1 } })).await?;
    let anonymous = harness
        .send(
            "GET",
            &format!("/v1/artifacts/{id}/data/members"),
            None,
            None,
        )
        .await?;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    let stream = harness
        .api
        .clone()
        .oneshot(Request::get(format!("/v1/artifacts/{id}/data/events")).body(Body::empty())?)
        .await?;
    assert_eq!(stream.status(), StatusCode::UNAUTHORIZED);
    // A token issued to the owner profile is an agent token.
    let issued = harness
        .owner("POST", "/v1/tokens", Some(&json!({ "name": "agent" })))
        .await?
        .json()?;
    let secret = issued["secret"].as_str().ok_or("secret")?;
    let page = harness
        .send(
            "GET",
            &format!("/v1/artifacts/{id}/data/members?path=c"),
            Some(secret),
            None,
        )
        .await?;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    let mut events = harness.follow_as(secret, &id, "?path=c").await?;
    let snapshot = events.expect("snapshot").await?;
    assert_eq!(snapshot.data["value"], json!({ "a": 1 }));
    Ok(())
}

// Isolation ------------------------------------------------------------------

#[tokio::test]
async fn another_user_finds_exactly_what_a_missing_artifact_gives() -> TestResult {
    let harness = Harness::new(quick(1, 64));
    let alice = harness.person("alice").await?;
    let bob = harness.person("bob").await?;
    let hers = harness.artifact_for(&alice.secret, "hers").await?;
    let reply = harness
        .set_as(
            &alice.secret,
            &hers,
            "",
            0,
            &json!({ "c": { "secret": 1 } }),
        )
        .await?;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let missing = "art_00000000000000000000000000000000";
    for route in ["data/members?path=c", "data/events?path=c"] {
        let others = harness
            .send(
                "GET",
                &format!("/v1/artifacts/{hers}/{route}"),
                Some(&bob.secret),
                None,
            )
            .await?;
        let nothing = harness
            .send(
                "GET",
                &format!("/v1/artifacts/{missing}/{route}"),
                Some(&bob.secret),
                None,
            )
            .await?;
        assert_eq!(
            others.status,
            StatusCode::NOT_FOUND,
            "{route}: {}",
            others.text
        );
        assert_eq!(
            (others.status, others.text.replace(&hers, "ID")),
            (nothing.status, nothing.text.replace(missing, "ID")),
            "{route}: another user's artifact is told apart from a missing one"
        );
        assert!(!others.text.contains("secret"));
    }
    // Their own data of the same shape is theirs alone.
    let hers_for_bob = harness.artifact_for(&bob.secret, "his").await?;
    let page = harness
        .send(
            "GET",
            &format!("/v1/artifacts/{hers_for_bob}/data/members?path=c"),
            Some(&bob.secret),
            None,
        )
        .await?;
    assert_eq!(page.json()?["members"], json!([]));
    Ok(())
}

#[tokio::test]
async fn a_missing_or_foreign_artifact_takes_no_slot_and_evicts_nothing() -> TestResult {
    let harness = Harness::new(quick(1, 2));
    let alice = harness.person("alice").await?;
    let bob = harness.person("bob").await?;
    let hers = harness.artifact_for(&alice.secret, "hers").await?;
    let his = harness.artifact_for(&bob.secret, "his").await?;
    // Bob's one place is in use.
    let mut open = harness.follow_as(&bob.secret, &his, "").await?;
    open.expect("snapshot").await?;
    for _ in 0..20 {
        for target in [hers.as_str(), "art_00000000000000000000000000000000"] {
            let response = harness.open_as(&bob.secret, target, "", None).await?;
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
    }
    // Nothing was taken from the process either: Alice still has her place.
    let mut hers_stream = harness.follow_as(&alice.secret, &hers, "").await?;
    hers_stream.expect("snapshot").await?;
    // Bob's stream is still the one he opened, and it still gets changes.
    let written = harness
        .set_as(&bob.secret, &his, "", 0, &json!({ "k": 1 }))
        .await?;
    assert_eq!(written.status, StatusCode::OK);
    let change = open.expect("change").await?;
    assert_eq!(change.data["data_version"], 1);
    Ok(())
}

// Streams --------------------------------------------------------------------

#[tokio::test]
async fn a_stream_sends_a_snapshot_and_then_each_new_state_in_order() -> TestResult {
    let harness = Harness::new(quick(8, 64));
    let id = harness.artifact("stream").await?;
    let mut events = harness.follow(&id, "?path=todo/list").await?;
    let snapshot = events.expect("snapshot").await?;
    assert_eq!(
        snapshot.data,
        json!({ "artifact_id": id, "path": "todo/list", "data_version": 0, "exists": false })
    );
    let v1 = harness.set(&id, "todo", 0, &json!({ "list": [1] })).await?;
    let first = events.expect("change").await?;
    assert_eq!(
        first.data,
        json!({ "artifact_id": id, "path": "todo/list", "data_version": v1,
                "exists": true, "value": [1] })
    );
    // The new full value, not a difference.
    let v2 = harness
        .set(&id, "todo", v1, &json!({ "list": [1, 2], "x": 1 }))
        .await?;
    let second = events.expect("change").await?;
    assert_eq!(second.data["value"], json!([1, 2]));
    assert_eq!(second.data["data_version"], v2);
    // A write elsewhere moves the version; the value at the path is the same.
    let v3 = harness.set(&id, "other", v2, &json!(true)).await?;
    let third = events.expect("change").await?;
    assert_eq!(third.data["data_version"], v3);
    assert_eq!(third.data["value"], json!([1, 2]));
    // Removing the watched value is a state too.
    let removed = harness
        .owner(
            "DELETE",
            &format!("/v1/artifacts/{id}/data"),
            Some(&json!({ "path": "todo", "base_data_version": v3 })),
        )
        .await?;
    assert_eq!(removed.status, StatusCode::OK, "{}", removed.text);
    let gone = events.expect("change").await?;
    assert_eq!(gone.data["exists"], false);
    assert!(gone.data.get("value").is_none());
    assert_eq!(gone.data["data_version"], v3 + 1);
    Ok(())
}

#[tokio::test]
async fn a_burst_of_writes_is_coalesced_and_versions_never_go_back() -> TestResult {
    let harness = Harness::new(DataStreamSettings {
        min_read_interval: Duration::from_millis(400),
        ..quick(8, 64)
    });
    let id = harness.artifact("burst").await?;
    let mut events = harness.follow(&id, "?path=n").await?;
    events.expect("snapshot").await?;
    let mut version = 0;
    for number in 1..=30 {
        version = harness.set(&id, "n", version, &json!(number)).await?;
    }
    assert_eq!(version, 30);
    let mut versions = Vec::new();
    let mut last = Value::Null;
    while versions.last() != Some(&30) {
        let event = events.expect("change").await?;
        versions.push(event.data["data_version"].as_u64().ok_or("version")?);
        last = event.data["value"].clone();
    }
    assert_eq!(last, json!(30), "the newest state arrives");
    assert!(
        versions.windows(2).all(|pair| pair[0] < pair[1]),
        "versions repeat or go back: {versions:?}"
    );
    assert!(
        versions.len() < 10,
        "30 writes were sent one by one: {versions:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_write_in_this_process_is_sent_without_waiting_for_the_heartbeat() -> TestResult {
    // The heartbeat read is far off, so only the wake-up after the commit can
    // deliver the change within the wait of `Events::next`.
    let harness = Harness::new(DataStreamSettings {
        heartbeat: Duration::from_secs(30),
        ..quick(8, 64)
    });
    let id = harness.artifact("wake").await?;
    let mut events = harness.follow(&id, "?path=n").await?;
    events.expect("snapshot").await?;
    let version = harness.set(&id, "n", 0, &json!("now")).await?;
    let change = events.expect("change").await?;
    assert_eq!(change.data["data_version"], version);
    assert_eq!(change.data["value"], json!("now"));
    Ok(())
}

#[tokio::test]
async fn storage_is_read_no_more_often_than_the_minimum_interval() -> TestResult {
    let interval = Duration::from_millis(600);
    let harness = Harness::new(DataStreamSettings {
        min_read_interval: interval,
        ..quick(8, 64)
    });
    let id = harness.artifact("spacing").await?;
    let mut events = harness.follow(&id, "?path=n").await?;
    events.expect("snapshot").await?;
    // The producer read at the connection and again for the first change, and
    // each read waits out the interval after the one before, however fast the
    // writes come. A slow machine only lengthens the wait, so the lower bound
    // below cannot fail because of one.
    let v1 = harness.set(&id, "n", 0, &json!(1)).await?;
    events.expect("change").await?;
    let v2 = harness.set(&id, "n", v1, &json!(2)).await?;
    let written = tokio::time::Instant::now();
    assert_eq!(events.expect("change").await?.data["data_version"], v2);
    let waited = written.elapsed();
    assert!(
        waited >= interval / 2,
        "a write was read {waited:?} after the previous read; the interval is {interval:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_new_connection_starts_with_a_fresh_snapshot() -> TestResult {
    let harness = Harness::new(quick(8, 64));
    let id = harness.artifact("reconnect").await?;
    let mut first = harness.follow(&id, "?path=n").await?;
    first.expect("snapshot").await?;
    let v1 = harness.set(&id, "n", 0, &json!(1)).await?;
    first.expect("change").await?;
    drop(first);
    // Writes while nobody is connected are not replayed.
    let v2 = harness.set(&id, "n", v1, &json!(2)).await?;
    let v3 = harness.set(&id, "n", v2, &json!(3)).await?;
    let mut second = harness.follow(&id, "?path=n").await?;
    let snapshot = second.expect("snapshot").await?;
    assert_eq!(snapshot.data["data_version"], v3);
    assert_eq!(snapshot.data["value"], json!(3));
    harness.set(&id, "n", v3, &json!(4)).await?;
    assert_eq!(second.expect("change").await?.data["value"], json!(4));
    Ok(())
}

#[tokio::test]
async fn a_resume_cursor_is_refused() -> TestResult {
    let harness = Harness::new(quick(8, 64));
    let id = harness.artifact("cursor").await?;
    for cursor in ["", "0123456789abcdef0123456789abcdef.7", "3"] {
        let response = harness
            .open_as(OWNER, &id, "", Some(("last-event-id", cursor)))
            .await?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{cursor:?}");
    }
    Ok(())
}

#[tokio::test]
async fn invalid_stream_requests_are_refused_before_a_stream_opens() -> TestResult {
    let harness = Harness::new(quick(1, 64));
    let id = harness.artifact("bad-stream").await?;
    let deep = vec!["s"; 17].join("/");
    for (query, status) in [
        ("?path=a//b".to_owned(), StatusCode::UNPROCESSABLE_ENTITY),
        (format!("?path={deep}"), StatusCode::UNPROCESSABLE_ENTITY),
        ("?path=a&path=b".to_owned(), StatusCode::BAD_REQUEST),
        ("?prefix=a".to_owned(), StatusCode::BAD_REQUEST),
    ] {
        let response = harness.open_as(OWNER, &id, &query, None).await?;
        assert_eq!(response.status(), status, "{query}");
    }
    // None of them took the one place the actor has.
    let mut events = harness
        .follow(&id, &format!("?path={}", vec!["s"; 16].join("/")))
        .await?;
    events.expect("snapshot").await?;
    Ok(())
}

#[tokio::test]
async fn a_value_over_the_bound_ends_the_stream_with_a_typed_event() -> TestResult {
    let harness = Harness::new(quick(8, 64));
    let id = harness.artifact("too-large").await?;
    let version = harness
        .set(
            &id,
            "",
            0,
            &json!({ "big": "x".repeat(140_000), "small": 1 }),
        )
        .await?;
    // Opening on the large value: no snapshot with a value, one typed event.
    let mut events = harness.follow(&id, "?path=big").await?;
    let event = events.expect("too_large").await?;
    assert_eq!(
        event.data,
        json!({ "artifact_id": id, "path": "big", "data_version": version, "max_bytes": 131_072 })
    );
    assert!(events.ends().await?.is_empty(), "nothing follows");
    assert!(
        !events.transcript.contains("xxxxxxxx"),
        "the value was sent"
    );

    // A value that grows past the bound while the stream is open.
    let mut events = harness.follow(&id, "?path=small").await?;
    events.expect("snapshot").await?;
    harness
        .set(&id, "small", version, &json!("y".repeat(140_000)))
        .await?;
    let event = events.expect("too_large").await?;
    assert_eq!(event.data["data_version"], version + 1);
    assert!(events.ends().await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn the_stream_limits_refuse_instead_of_growing() -> TestResult {
    let harness = Harness::new(quick(2, 3));
    let alice = harness.person("alice").await?;
    let bob = harness.person("bob").await?;
    let hers = harness.artifact_for(&alice.secret, "hers").await?;
    let his = harness.artifact_for(&bob.secret, "his").await?;
    let mut one = harness.follow_as(&alice.secret, &hers, "").await?;
    let _two = harness.follow_as(&alice.secret, &hers, "?path=a").await?;
    one.expect("snapshot").await?;
    // The actor's own limit: 429 with Retry-After.
    let refused = harness
        .open_as(&alice.secret, &hers, "?path=b", None)
        .await?;
    assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        refused
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok()),
        Some("5")
    );
    // The process limit: another user with room of their own is refused too.
    let _three = harness.follow_as(&bob.secret, &his, "").await?;
    let full = harness.open_as(&bob.secret, &his, "?path=a", None).await?;
    assert_eq!(full.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        full.headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok()),
        Some("10")
    );
    let body = to_bytes(full.into_body(), usize::MAX).await?;
    let body: Value = serde_json::from_slice(&body)?;
    assert_eq!(body["error"]["code"], "unavailable");
    // Closing a stream frees its places, whichever limit was full.
    drop(one);
    eventually("a place freed by a closed stream", || async {
        let again = harness
            .open_as(&alice.secret, &hers, "?path=c", None)
            .await?;
        Ok(again.status() == StatusCode::OK)
    })
    .await
}

#[tokio::test]
async fn trashing_or_purging_ends_the_stream_even_when_the_reader_is_slow() -> TestResult {
    let harness = Harness::new(quick(1, 64));
    let id = harness.artifact("slow").await?;
    // A reader that never reads: its first event waits in the channel.
    let slow = harness.follow(&id, "?path=n").await?;
    let v1 = harness.set(&id, "n", 0, &json!(1)).await?;
    // A second state cannot be sent while the reader is behind.
    let v2 = harness.set(&id, "n", v1, &json!(2)).await?;
    let in_use = harness.open_as(OWNER, &id, "?path=n", None).await?;
    assert_eq!(
        in_use.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "the place is in use"
    );
    let meta = harness
        .owner("GET", &format!("/v1/artifacts/{id}"), None)
        .await?
        .json()?;
    let base = meta["metadata_revision"].as_u64().ok_or("metadata")?;
    let trashed = harness
        .owner(
            "PATCH",
            &format!("/v1/artifacts/{id}"),
            Some(&json!({ "base_metadata_revision": base, "deleted": true })),
        )
        .await?;
    assert_eq!(trashed.status, StatusCode::OK, "{}", trashed.text);
    // The producer ends without the reader reading anything: the place is
    // free again for another stream.
    let other = harness.artifact("other").await?;
    eventually(
        "the place of a stream whose artifact was trashed",
        || async { Ok(harness.open_as(OWNER, &other, "", None).await?.status() == StatusCode::OK) },
    )
    .await?;
    drop(slow);
    assert!(v2 > v1);
    Ok(())
}

#[tokio::test]
async fn a_purge_ends_the_stream_and_a_reader_sees_no_value_after_it() -> TestResult {
    let harness = Harness::new(quick(8, 64));
    let id = harness.artifact("purged").await?;
    let version = harness.set(&id, "", 0, &json!({ "n": "private" })).await?;
    let mut events = harness.follow(&id, "?path=n").await?;
    events.expect("snapshot").await?;
    let meta = harness
        .owner("GET", &format!("/v1/artifacts/{id}"), None)
        .await?
        .json()?;
    let base = meta["metadata_revision"].as_u64().ok_or("metadata")?;
    let trashed = harness
        .owner(
            "PATCH",
            &format!("/v1/artifacts/{id}"),
            Some(&json!({ "base_metadata_revision": base, "deleted": true })),
        )
        .await?;
    let base = trashed.json()?["artifact"]["metadata_revision"]
        .as_u64()
        .ok_or("revision")?;
    assert!(
        events.ends().await?.is_empty(),
        "nothing is sent after the trash"
    );
    let purged = harness
        .owner(
            "POST",
            &format!("/v1/artifacts/{id}/purge"),
            Some(&json!({ "base_metadata_revision": base })),
        )
        .await?;
    assert_eq!(purged.status, StatusCode::OK, "{}", purged.text);
    assert_eq!(
        events.transcript.matches("private").count(),
        1,
        "the value was sent more than once: {:?}",
        events.transcript
    );
    assert_eq!(version, 1);
    let after = harness.members(&id, "").await?;
    assert_eq!(after.status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn disabling_a_user_or_revoking_a_token_ends_its_streams() -> TestResult {
    let harness = Harness::new(quick(8, 64));
    let alice = harness.person("alice").await?;
    let bob = harness.person("bob").await?;
    let hers = harness.artifact_for(&alice.secret, "hers").await?;
    let his = harness.artifact_for(&bob.secret, "his").await?;
    let mut alices = harness.follow_as(&alice.secret, &hers, "").await?;
    let mut bobs = harness.follow_as(&bob.secret, &his, "").await?;
    alices.expect("snapshot").await?;
    bobs.expect("snapshot").await?;
    let disabled = harness
        .owner("POST", &format!("/v1/users/{}/disable", alice.id), None)
        .await?;
    assert_eq!(disabled.status, StatusCode::OK, "{}", disabled.text);
    assert!(alices.ends().await?.is_empty());
    // Bob's stream is untouched, and a revoked token ends only its own.
    let second = harness
        .owner(
            "POST",
            "/v1/tokens",
            Some(&json!({ "name": "second", "user_id": bob.id })),
        )
        .await?
        .json()?;
    let second_secret = second["secret"].as_str().ok_or("secret")?.to_owned();
    let mut with_second = harness.follow_as(&second_secret, &his, "?path=x").await?;
    with_second.expect("snapshot").await?;
    let revoked = harness
        .owner(
            "DELETE",
            &format!("/v1/tokens/{}", second["token"]["id"].as_str().ok_or("id")?),
            None,
        )
        .await?;
    assert!(revoked.status.is_success(), "{}", revoked.text);
    assert!(with_second.ends().await?.is_empty());
    harness
        .set_as(&bob.secret, &his, "", 0, &json!({ "k": 1 }))
        .await?;
    assert_eq!(bobs.expect("change").await?.data["data_version"], 1);
    // A request for a disabled user is refused like any other.
    let refused = harness.open_as(&alice.secret, &hers, "", None).await?;
    assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn a_write_by_another_process_arrives_by_the_heartbeat_read() -> TestResult {
    let harness = Harness::new(quick(8, 64));
    let id = harness.artifact("elsewhere").await?;
    harness.set(&id, "", 0, &json!({ "n": 1 })).await?;
    let mut events = harness.follow(&id, "?path=n").await?;
    events.expect("snapshot").await?;
    harness.raw("UPDATE app_data SET data_version = 5, document = '{\"n\":9}'")?;
    let change = events.expect("change").await?;
    assert_eq!(change.data["data_version"], 5);
    assert_eq!(change.data["value"], 9);
    // Comment lines keep an idle connection open.
    events.until_seen(":").await?;
    Ok(())
}

#[tokio::test]
async fn a_stream_ends_rather_than_skip_a_state_it_cannot_read_or_send_a_lower_version()
-> TestResult {
    let harness = Harness::new(quick(8, 64));
    let id = harness.artifact("faults").await?;
    let first = harness.set(&id, "", 0, &json!({ "n": 1 })).await?;
    let version = harness
        .set(&id, "", first, &json!({ "n": 1, "m": 1 }))
        .await?;
    // A version that goes back ends the stream, with nothing sent for it.
    let mut events = harness.follow(&id, "?path=n").await?;
    events.expect("snapshot").await?;
    harness.raw("UPDATE app_data SET data_version = 1")?;
    assert!(events.ends().await?.is_empty());
    // A stored space that cannot be read back ends the stream without an event.
    harness.raw("UPDATE app_data SET data_version = 7, document = '{\"n\":{\"a\":1,\"a\":2}}'")?;
    // (It cannot be opened either.)
    let refused = harness.open_as(OWNER, &id, "?path=n", None).await?;
    assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
    harness.raw(&format!(
        "UPDATE app_data SET data_version = {version}, document = '{{\"n\":1}}'"
    ))?;
    let mut events = harness.follow(&id, "?path=n").await?;
    events.expect("snapshot").await?;
    harness.raw("UPDATE app_data SET data_version = 9, document = '{\"n\":{\"a\":1,\"a\":2}}'")?;
    assert!(
        events.ends().await?.is_empty(),
        "an unreadable state was skipped or sent"
    );
    Ok(())
}

#[tokio::test]
async fn a_stream_ends_at_the_end_of_its_lifetime_and_at_shutdown() -> TestResult {
    let harness = Harness::new(DataStreamSettings {
        lifetime: Duration::from_millis(400),
        ..quick(8, 64)
    });
    let id = harness.artifact("lifetime").await?;
    let mut events = harness.follow(&id, "").await?;
    events.expect("snapshot").await?;
    assert!(events.ends().await?.is_empty());

    let harness = Harness::new(quick(8, 64));
    let id = harness.artifact("shutdown").await?;
    let mut events = harness.follow(&id, "").await?;
    events.expect("snapshot").await?;
    harness.state.close_streams();
    assert!(events.ends().await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn a_write_succeeds_whatever_the_subscribers_do() -> TestResult {
    let harness = Harness::new(quick(8, 64));
    let id = harness.artifact("indifferent").await?;
    // Subscribers that never read, and ones that are gone.
    let mut open = Vec::new();
    for number in 0..4 {
        open.push(harness.follow(&id, &format!("?path=p{number}")).await?);
    }
    let gone = harness.follow(&id, "?path=gone").await?;
    drop(gone);
    let mut version = 0;
    for number in 0..20 {
        let reply = harness
            .set_as(OWNER, &id, "p0", version, &json!(number))
            .await?;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
        version = reply.json()?["data_version"].as_u64().ok_or("version")?;
    }
    assert_eq!(version, 20);
    // The unread streams are still open, and the data is as written.
    assert_eq!(open.len(), 4);
    let page = harness.members(&id, "").await?.json()?;
    assert_eq!(page["data_version"], 20);
    Ok(())
}
