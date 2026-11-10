//! Runtime data writes over HTTP (decision 0053, R14b), exercised in process
//! through the real routers, application core, and `SQLite` store. Each test
//! plays the workspace page: it holds the account token and the view's
//! initialization secret, and calls `data/set`, `data/update` and
//! `data/delete` as a frame does. The owner's routes seed the space and read
//! it back, and the owner's change feed shows what was announced.

use std::future::poll_fn;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use artifree_contracts::api::MAX_RUNTIME_DATA_WRITE_BYTES;
use artifree_domain::data::MAX_VALUE_BYTES;
use artifree_providers::storage::SqliteStore;
use artifree_providers::tokens::SqliteTokenStore;
use artifree_server::auth::AccessToken;
use artifree_server::changes;
use artifree_server::http::{self, AppState, Settings};
use artifree_server::limits::RequestLimits;
use artifree_server::preview;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use http_body::Body as _;
use serde_json::{Value, json};
use tower::ServiceExt;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const CONTENT_ORIGIN: &str = "http://127.0.0.1:7411";
const ENTRY: &str =
    "<!doctype html><html lang=\"en\"><head><!-- artifree-runtime --></head><body>x</body></html>";
/// The read capability, which a manifest may declare beside the write.
const DATA_READ: &str = "data.read";
/// The write capability this file exercises.
const DATA_WRITE: &str = "data.write";
/// The manifest of every artifact here: both data capabilities. Each test
/// grants the ones it needs.
const DECLARED: [&str; 2] = [DATA_READ, DATA_WRITE];
/// The longest value text decision 0031 allows: 256 KiB, in bytes. The boundary
/// tests state the number, so they do not follow the contract's constant.
const VALUE_TEXT_LIMIT: usize = 262_144;
/// Distinct identities, so that tests can share a process.
static NEXT_KEY: AtomicU32 = AtomicU32::new(1);

fn plentiful() -> RequestLimits {
    RequestLimits {
        burst: 100_000,
        per_minute: 6_000_000,
        concurrent: 64,
    }
}

struct Harness {
    directory: PathBuf,
    api: Router,
    content: Router,
}

impl Harness {
    fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!(
            "artifree-runtime-data-writes-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory)
            .unwrap_or_else(|error| panic!("cannot create test directory: {error}"));
        let database = directory.join("db.sqlite3");
        let state = AppState::new(
            SqliteStore::open(&database)
                .unwrap_or_else(|error| panic!("cannot open store: {error}")),
            SqliteTokenStore::open(&database)
                .unwrap_or_else(|error| panic!("cannot open token store: {error}")),
            AccessToken::parse(TOKEN).unwrap_or_else(|| panic!("fixture token is invalid")),
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
            api: http::router(state.clone()),
            content: preview::router(state),
        }
    }

    async fn send(&self, request: Request<Body>) -> Result<Reply, Box<dyn std::error::Error>> {
        Reply::collect(self.api.clone().oneshot(request).await?).await
    }

    /// Serves a view's document, which the page does before a frame may call.
    async fn serve(&self, url: &str) -> Result<Reply, Box<dyn std::error::Error>> {
        let path = url
            .strip_prefix(CONTENT_ORIGIN)
            .ok_or("preview URL uses another origin")?;
        let request = Request::get(path).body(Body::empty())?;
        Reply::collect(self.content.clone().oneshot(request).await?).await
    }

    /// Opens the owner's change feed, which starts at the head.
    async fn follow(&self) -> Result<Events, Box<dyn std::error::Error>> {
        let request = Request::get("/v1/changes")
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .header(header::ACCEPT, "text/event-stream")
            .body(Body::empty())?;
        let response = self.api.clone().oneshot(request).await?;
        assert_eq!(response.status(), StatusCode::OK);
        Ok(Events {
            body: response.into_body(),
            buffer: String::new(),
        })
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

struct Reply {
    status: StatusCode,
    body: Vec<u8>,
}

impl Reply {
    async fn collect(
        response: axum::response::Response,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await?.to_vec();
        Ok(Self { status, body })
    }

    fn json(&self) -> Result<Value, serde_json::Error> {
        serde_json::from_slice(&self.body)
    }

    fn code(&self) -> String {
        self.json()
            .ok()
            .and_then(|body| body["error"]["code"].as_str().map(str::to_owned))
            .unwrap_or_else(|| {
                format!("<no JSON error: {:?}>", String::from_utf8_lossy(&self.body))
            })
    }
}

/// One server-sent event of the change feed.
struct Event {
    name: String,
    data: Value,
}

/// The events of one stream, read as they arrive.
struct Events {
    body: Body,
    buffer: String,
}

impl Events {
    /// The next event, skipping comment lines; `None` when the stream ended.
    async fn next(&mut self) -> Result<Option<Event>, Box<dyn std::error::Error>> {
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

    /// The next event, which must be called `name`.
    async fn expect(&mut self, name: &str) -> Result<Event, Box<dyn std::error::Error>> {
        let event = self.next().await?.ok_or("the stream ended")?;
        assert_eq!(event.name, name, "{}", event.data);
        Ok(event)
    }

    /// Whether no event arrives for a moment.
    async fn quiet(&mut self) -> bool {
        tokio::time::timeout(Duration::from_millis(300), self.next())
            .await
            .is_err()
    }
}

fn bearer(token: &str, builder: axum::http::request::Builder) -> axum::http::request::Builder {
    builder.header(header::AUTHORIZATION, format!("Bearer {token}"))
}

/// A JSON request with `token` as its bearer token; `key`, when given, is its
/// idempotency key.
fn json_request(
    token: &str,
    builder: axum::http::request::Builder,
    key: Option<&str>,
    body: &Value,
) -> Result<Request<Body>, axum::http::Error> {
    let mut builder = bearer(token, builder).header(header::CONTENT_TYPE, "application/json");
    if let Some(key) = key {
        builder = builder.header("idempotency-key", key);
    }
    builder.body(Body::from(body.to_string()))
}

fn post(path: &str, body: &Value) -> Result<Request<Body>, axum::http::Error> {
    json_request(TOKEN, Request::post(path), None, body)
}

fn post_keyed(
    token: &str,
    path: &str,
    key: &str,
    body: &Value,
) -> Result<Request<Body>, axum::http::Error> {
    json_request(token, Request::post(path), Some(key), body)
}

fn get(token: &str, path: &str) -> Result<Request<Body>, axum::http::Error> {
    bearer(token, Request::get(path)).body(Body::empty())
}

/// A retry identity or owner key no other call of this process uses. It has
/// at least eight characters, as both grammars require.
fn fresh(prefix: &str) -> String {
    format!("{prefix}-{:08}", NEXT_KEY.fetch_add(1, Ordering::Relaxed))
}

/// A view id no other view of this process has: 32 lowercase hex digits, as
/// `validate_view` requires.
fn next_view() -> String {
    format!("{:032x}", NEXT_KEY.fetch_add(1, Ordering::Relaxed))
}

fn manifest(declared: &[&str]) -> String {
    json!({"sdk": "artifree.app.v1", "capabilities": declared}).to_string()
}

/// An artifact that declares `declared`, created by `token`.
async fn create_artifact(
    harness: &Harness,
    token: &str,
    declared: &[&str],
) -> Result<String, Box<dyn std::error::Error>> {
    let body = json!({
        "title": "Data writes",
        "files": [
            {"path": "index.html", "encoding": "utf8", "content": ENTRY},
            {"path": "artifree-runtime.json", "encoding": "utf8", "content": manifest(declared)}
        ]
    });
    let reply = harness
        .send(post_keyed(
            token,
            "/v1/artifacts",
            &fresh("create-data"),
            &body,
        )?)
        .await?;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.code());
    Ok(reply.json()?["artifact"]["id"]
        .as_str()
        .ok_or("missing artifact id")?
        .to_owned())
}

async fn open_session(harness: &Harness) -> Result<String, Box<dyn std::error::Error>> {
    let created = harness
        .send(post("/v1/runtime-sessions", &json!({}))?)
        .await?;
    assert_eq!(created.status, StatusCode::OK, "{}", created.code());
    Ok(created.json()?["session_id"]
        .as_str()
        .ok_or("missing session id")?
        .to_owned())
}

/// Replaces the connection's grant on revision 1 of `artifact` with
/// `capabilities`, the grant at `expected_version` being the one replaced.
fn grant_request(
    session: &str,
    artifact: &str,
    expected_version: u64,
    capabilities: &[&str],
) -> Result<Request<Body>, axum::http::Error> {
    json_request(
        TOKEN,
        Request::put(format!(
            "/v1/runtime-sessions/{session}/artifacts/{artifact}/revisions/1/grant"
        )),
        None,
        &json!({
            "head_revision": 1,
            "expected_version": expected_version,
            "capabilities": capabilities
        }),
    )
}

/// What the page holds for one view of revision 1 of an artifact.
struct Frame {
    id: String,
    session: String,
    view: String,
    initialization: String,
    generation: u64,
}

impl Frame {
    fn route(&self, tail: &str) -> String {
        format!(
            "/v1/artifacts/{}/revisions/1/runtime-views/{}/{}/{tail}",
            self.id, self.view, self.generation
        )
    }

    fn body(&self, extra: Value) -> Value {
        let mut fields = serde_json::Map::new();
        fields.insert("initialization".into(), json!(self.initialization));
        if let Value::Object(extra) = extra {
            fields.extend(extra);
        }
        Value::Object(fields)
    }
}

/// An artifact that declares `declared`, with a session of the owner's and no
/// grant yet.
async fn connect(
    harness: &Harness,
    declared: &[&str],
) -> Result<Frame, Box<dyn std::error::Error>> {
    Ok(Frame {
        id: create_artifact(harness, TOKEN, declared).await?,
        session: open_session(harness).await?,
        view: next_view(),
        initialization: String::new(),
        generation: 1,
    })
}

/// A view of generation 1 under the connection's grant `granted`.
async fn granted_frame(
    harness: &Harness,
    declared: &[&str],
    granted: &[&str],
) -> Result<Frame, Box<dyn std::error::Error>> {
    let mut frame = connect(harness, declared).await?;
    let set = harness
        .send(grant_request(&frame.session, &frame.id, 0, granted)?)
        .await?;
    assert_eq!(set.status, StatusCode::OK, "{}", set.code());
    let view = issue(harness, &frame, 1).await?;
    assert_eq!(
        view["granted"].as_array().map(Vec::len),
        Some(granted.len())
    );
    frame.initialization = secret_of(&view)?;
    Ok(frame)
}

/// Creates view `generation` and serves its document.
async fn issue(
    harness: &Harness,
    frame: &Frame,
    generation: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    let created = harness
        .send(post(
            &format!("/v1/artifacts/{}/revisions/1/runtime-views", frame.id),
            &json!({"session_id": frame.session, "view_id": frame.view, "generation": generation}),
        )?)
        .await?;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.code());
    let view = created.json()?;
    let served = harness
        .serve(view["url"].as_str().ok_or("missing url")?)
        .await?;
    assert_eq!(
        served.status,
        StatusCode::OK,
        "the view's document is served"
    );
    Ok(view)
}

fn secret_of(view: &Value) -> Result<String, Box<dyn std::error::Error>> {
    Ok(view["initialization"]
        .as_str()
        .ok_or("initialization")?
        .to_owned())
}

/// A frame call to `tail` as `token`, with `fields` beside the view's
/// initialization.
fn frame_call(
    token: &str,
    frame: &Frame,
    tail: &str,
    fields: Value,
) -> Result<Request<Body>, axum::http::Error> {
    json_request(
        token,
        Request::post(frame.route(tail)),
        None,
        &frame.body(fields),
    )
}

/// The three frame writes. They share one route shape and differ in the field
/// that carries the change (decision 0053, rule 4).
#[derive(Clone, Copy, Debug)]
enum Kind {
    Set,
    Update,
    Remove,
}

/// A path and the JSON text sent with it: the value of a `set`, the patch of
/// an `update`, and nothing for a `remove`.
#[derive(Clone, Copy)]
struct Write<'a> {
    path: &'a str,
    text: &'a str,
}

impl Kind {
    /// Every frame write, in the order of their routes.
    const ALL: [Self; 3] = [Self::Set, Self::Update, Self::Remove];

    /// The route below a view.
    fn tail(self) -> &'static str {
        match self {
            Self::Set => "data/set",
            Self::Update => "data/update",
            Self::Remove => "data/delete",
        }
    }

    /// The body field that carries JSON text, if the kind has one.
    fn text_field(self) -> Option<&'static str> {
        match self {
            Self::Set => Some("value"),
            Self::Update => Some("patch"),
            Self::Remove => None,
        }
    }

    /// A write that changes `{"notes": {"a": 1}}`.
    fn changing(self) -> Write<'static> {
        match self {
            Self::Set => Write {
                path: "notes/a",
                text: "2",
            },
            Self::Update => Write {
                path: "notes",
                text: r#"{"a":2}"#,
            },
            Self::Remove => Write {
                path: "notes/a",
                text: "",
            },
        }
    }

    /// A different change of the same kind, from `{"notes": {"a": 1}}`. Under
    /// the retry identity of an earlier write it is refused as reuse.
    fn different(self) -> Write<'static> {
        match self {
            Self::Set => Write {
                path: "notes/a",
                text: "3",
            },
            Self::Update => Write {
                path: "notes",
                text: r#"{"a":3}"#,
            },
            Self::Remove => Write {
                path: "notes/b",
                text: "",
            },
        }
    }

    /// A write of the same kind that changes nothing in `{"notes": {"a": 1}}`.
    fn no_op(self) -> Write<'static> {
        match self {
            Self::Set => Write {
                path: "notes/a",
                text: "1",
            },
            Self::Update => Write {
                path: "notes",
                text: r#"{"a":1}"#,
            },
            Self::Remove => Write {
                path: "nothing/here",
                text: "",
            },
        }
    }
}

/// A frame call of `kind` under the retry identity `retry`, at base version
/// `base`, writing `write`.
fn write_call(
    frame: &Frame,
    kind: Kind,
    retry: &str,
    base: u64,
    write: Write<'_>,
) -> Result<Request<Body>, axum::http::Error> {
    let mut fields = json!({"retry_id": retry, "base_data_version": base, "path": write.path});
    if let Some(field) = kind.text_field() {
        fields[field] = json!(write.text);
    }
    frame_call(TOKEN, frame, kind.tail(), fields)
}

/// A frame that holds `data.write` on an artifact whose space is still empty.
async fn write_frame(harness: &Harness) -> Result<Frame, Box<dyn std::error::Error>> {
    granted_frame(harness, &DECLARED, &[DATA_WRITE]).await
}

/// A frame that holds `data.write` on an artifact whose space is at version 1
/// and holds `{"notes": {"a": 1}}`, written by the owner.
async fn notes_frame(harness: &Harness) -> Result<Frame, Box<dyn std::error::Error>> {
    let frame = write_frame(harness).await?;
    let version = owner_set(harness, &frame.id, 0, "notes", &json!({"a": 1})).await?;
    assert_eq!(
        version, 1,
        "one changed write leaves the space at version 1"
    );
    Ok(frame)
}

/// The owner's write of `value` at `path`, under the idempotency key `key`.
async fn owner_write(
    harness: &Harness,
    artifact: &str,
    key: &str,
    base: u64,
    path: &str,
    value: &Value,
) -> Result<Reply, Box<dyn std::error::Error>> {
    harness
        .send(json_request(
            TOKEN,
            Request::put(format!("/v1/artifacts/{artifact}/data")),
            Some(key),
            &json!({"path": path, "base_data_version": base, "value": value}),
        )?)
        .await
}

/// Writes `value` at `path` of the owner's data space of `artifact`, which is
/// at version `base`, and returns the version the space is at after the write.
async fn owner_set(
    harness: &Harness,
    artifact: &str,
    base: u64,
    path: &str,
    value: &Value,
) -> Result<u64, Box<dyn std::error::Error>> {
    let reply = owner_write(harness, artifact, &fresh("owner"), base, path, value).await?;
    assert_eq!(reply.status, StatusCode::OK, "set {path}: {}", reply.code());
    Ok(reply.json()?["data_version"]
        .as_u64()
        .ok_or("data_version is not a number")?)
}

/// The owner's own read of `at` in the data space of `artifact`, or of the
/// root when `at` is `None`.
async fn owner_read(
    harness: &Harness,
    artifact: &str,
    at: Option<&str>,
) -> Result<Value, Box<dyn std::error::Error>> {
    let route = match at {
        Some(at) => format!("/v1/artifacts/{artifact}/data?path={at}"),
        None => format!("/v1/artifacts/{artifact}/data"),
    };
    let reply = harness.send(get(TOKEN, &route)?).await?;
    assert_eq!(reply.status, StatusCode::OK, "owner read: {}", reply.code());
    Ok(reply.json()?)
}

/// Saves revision 2 of `artifact`, which moves its head past the revision a
/// view of revision 1 is pinned to.
async fn save_revision(
    harness: &Harness,
    artifact: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let reply = harness
        .send(post_keyed(
            TOKEN,
            &format!("/v1/artifacts/{artifact}/revisions"),
            &fresh("owner"),
            &json!({
                "base_revision": 1,
                "changes": [{"op": "put", "path": "notes.txt", "encoding": "utf8", "content": "new"}]
            }),
        )?)
        .await?;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.code());
    Ok(())
}

/// A JSON string whose text is `len` bytes long, quotes included.
fn quoted(len: usize) -> String {
    format!("\"{}\"", "x".repeat(len - 2))
}

/// The text of `levels` nested arrays: a value of nesting `levels`.
fn arrays(levels: usize) -> String {
    format!("{}{}", "[".repeat(levels), "]".repeat(levels))
}

#[tokio::test]
async fn a_frame_set_stores_null_in_place_of_a_value() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &DECLARED, &[DATA_READ, DATA_WRITE]).await?;
    let filled = harness
        .send(write_call(
            &frame,
            Kind::Set,
            &fresh("frame"),
            0,
            Write {
                path: "gone",
                text: "5",
            },
        )?)
        .await?;
    assert_eq!(filled.status, StatusCode::OK, "{}", filled.code());
    let stored = harness
        .send(write_call(
            &frame,
            Kind::Set,
            &fresh("frame"),
            1,
            Write {
                path: "gone",
                text: "null",
            },
        )?)
        .await?;
    assert_eq!(stored.status, StatusCode::OK, "{}", stored.code());
    let body = stored.json()?;
    assert_eq!(body["data_version"], 2, "{body}");
    assert_eq!(body["changed"], true, "null replaces 5: {body}");

    let owner = owner_read(&harness, &frame.id, Some("gone")).await?;
    assert_eq!(owner["exists"], true, "{owner}");
    assert_eq!(
        owner.get("value"),
        Some(&Value::Null),
        "the owner reads a stored null, present as null: {owner}"
    );
    let read = harness
        .send(frame_call(
            TOKEN,
            &frame,
            "data/read",
            json!({"path": "gone"}),
        )?)
        .await?;
    assert_eq!(read.status, StatusCode::OK, "data/read: {}", read.code());
    let body = read.json()?;
    assert_eq!(body["exists"], true, "{body}");
    assert_eq!(
        body["value"], "null",
        "the frame reads the text null: {body}"
    );
    Ok(())
}

#[tokio::test]
async fn a_frame_update_is_a_json_merge_patch_at_its_path() -> TestResult {
    let harness = Harness::new();
    let frame = write_frame(&harness).await?;
    owner_set(
        &harness,
        &frame.id,
        0,
        "notes",
        &json!({"a": 1, "b": {"c": 2}, "gone": true}),
    )
    .await?;
    let patched = harness
        .send(write_call(
            &frame,
            Kind::Update,
            &fresh("frame"),
            1,
            Write {
                path: "notes",
                text: r#"{"a":null,"b":{"d":3},"e":[1,2]}"#,
            },
        )?)
        .await?;
    assert_eq!(patched.status, StatusCode::OK, "{}", patched.code());
    let body = patched.json()?;
    assert_eq!(body["data_version"], 2, "{body}");
    assert_eq!(body["changed"], true, "{body}");
    let owner = owner_read(&harness, &frame.id, Some("notes")).await?;
    assert_eq!(
        owner["value"],
        json!({"b": {"c": 2, "d": 3}, "e": [1, 2], "gone": true}),
        "a null removes its member, objects merge, and the rest stays: {owner}"
    );
    Ok(())
}

#[tokio::test]
async fn a_frame_remove_deletes_the_value_at_its_path() -> TestResult {
    let harness = Harness::new();
    let frame = notes_frame(&harness).await?;
    let removed = harness
        .send(write_call(
            &frame,
            Kind::Remove,
            &fresh("frame"),
            1,
            Kind::Remove.changing(),
        )?)
        .await?;
    assert_eq!(removed.status, StatusCode::OK, "{}", removed.code());
    let body = removed.json()?;
    assert_eq!(body["data_version"], 2, "{body}");
    assert_eq!(body["changed"], true, "{body}");
    let gone = owner_read(&harness, &frame.id, Some("notes/a")).await?;
    assert_eq!(gone["exists"], false, "{gone}");
    assert!(
        gone.get("value").is_none(),
        "an absent value has no member: {gone}"
    );
    let notes = owner_read(&harness, &frame.id, Some("notes")).await?;
    assert_eq!(notes["value"], json!({}), "{notes}");
    Ok(())
}

#[tokio::test]
async fn a_stale_base_is_data_conflict_with_the_current_version() -> TestResult {
    let harness = Harness::new();
    for kind in Kind::ALL {
        let frame = notes_frame(&harness).await?;
        let retry = fresh("frame");
        let refused = harness
            .send(write_call(&frame, kind, &retry, 0, kind.changing())?)
            .await?;
        assert_eq!(
            refused.status,
            StatusCode::CONFLICT,
            "{kind:?}: {}",
            refused.code()
        );
        assert_eq!(refused.code(), "data_conflict", "{kind:?}");
        assert_eq!(
            refused.json()?["error"]["current_data_version"],
            1,
            "{kind:?}: the space is at version 1"
        );
        // The refusal recorded no receipt, so the same identity at the current
        // base is a first write.
        let written = harness
            .send(write_call(&frame, kind, &retry, 1, kind.changing())?)
            .await?;
        assert_eq!(
            written.status,
            StatusCode::OK,
            "{kind:?}: {}",
            written.code()
        );
        let body = written.json()?;
        assert_eq!(body["data_version"], 2, "{kind:?}: {body}");
        assert_eq!(body["replayed"], false, "{kind:?}: {body}");
    }
    Ok(())
}

#[tokio::test]
async fn a_retry_answers_with_its_first_receipt_and_another_change_under_it_is_refused()
-> TestResult {
    let harness = Harness::new();
    for kind in Kind::ALL {
        let frame = notes_frame(&harness).await?;
        let retry = fresh("frame");
        let first = harness
            .send(write_call(&frame, kind, &retry, 1, kind.changing())?)
            .await?;
        assert_eq!(first.status, StatusCode::OK, "{kind:?}: {}", first.code());
        assert_eq!(first.json()?["data_version"], 2, "{kind:?}");
        // The owner moves the space on, so the replay cannot answer with the
        // version the space is at now.
        let moved = owner_set(&harness, &frame.id, 2, "other", &json!(1)).await?;
        assert_eq!(moved, 3, "{kind:?}");
        let replayed = harness
            .send(write_call(&frame, kind, &retry, 1, kind.changing())?)
            .await?;
        assert_eq!(
            replayed.status,
            StatusCode::OK,
            "{kind:?}: {}",
            replayed.code()
        );
        let body = replayed.json()?;
        assert_eq!(body["replayed"], true, "{kind:?}: {body}");
        assert_eq!(
            body["data_version"], 2,
            "{kind:?}: the first receipt's version, not the current {moved}: {body}"
        );
        assert_eq!(body["changed"], true, "{kind:?}: {body}");
        let other = harness
            .send(write_call(&frame, kind, &retry, 1, kind.different())?)
            .await?;
        assert_eq!(
            other.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{kind:?}: {}",
            other.code()
        );
        assert_eq!(other.code(), "request_key_reused", "{kind:?}");
    }
    Ok(())
}

#[tokio::test]
async fn an_owner_key_in_the_form_of_a_frame_key_is_refused_by_the_grammar() -> TestResult {
    let harness = Harness::new();
    let artifact = create_artifact(&harness, TOKEN, &DECLARED).await?;
    // A frame's key starts with `rtd~`, which no owner or agent key can
    // contain (decision 0053, rule 9).
    let frame_form = format!("rtd~{artifact}:{}:1:frame-00000001", next_view());
    let refused = owner_write(&harness, &artifact, &frame_form, 0, "owned", &json!(1)).await?;
    assert_eq!(
        refused.status,
        StatusCode::BAD_REQUEST,
        "{}",
        refused.code()
    );
    assert_eq!(refused.code(), "invalid_request");
    let owner = owner_read(&harness, &artifact, None).await?;
    assert_eq!(
        owner["data_version"], 0,
        "a refused key writes nothing: {owner}"
    );
    Ok(())
}

#[tokio::test]
async fn an_owner_key_equal_to_a_frame_retry_identity_neither_replays_nor_blocks_it() -> TestResult
{
    let harness = Harness::new();
    let frame = write_frame(&harness).await?;
    // Valid both as an owner's key and as a frame's retry identity.
    let shared = "shared-key-0001";
    let owned = owner_write(&harness, &frame.id, shared, 0, "owned", &json!(1)).await?;
    assert_eq!(owned.status, StatusCode::OK, "{}", owned.code());
    assert_eq!(owned.json()?["data_version"], 1);
    let framed = harness
        .send(write_call(
            &frame,
            Kind::Set,
            shared,
            1,
            Write {
                path: "framed",
                text: "2",
            },
        )?)
        .await?;
    assert_eq!(framed.status, StatusCode::OK, "{}", framed.code());
    let body = framed.json()?;
    assert_eq!(body["data_version"], 2, "{body}");
    assert_eq!(
        body["replayed"], false,
        "the frame's write is not a replay: {body}"
    );
    // Each one replays its own receipt, and neither takes the other's.
    let owned_again = owner_write(&harness, &frame.id, shared, 0, "owned", &json!(1)).await?;
    assert_eq!(owned_again.status, StatusCode::OK, "{}", owned_again.code());
    let body = owned_again.json()?;
    assert_eq!(body["replayed"], true, "{body}");
    assert_eq!(body["data_version"], 1, "{body}");
    let framed_again = harness
        .send(write_call(
            &frame,
            Kind::Set,
            shared,
            1,
            Write {
                path: "framed",
                text: "2",
            },
        )?)
        .await?;
    assert_eq!(
        framed_again.status,
        StatusCode::OK,
        "{}",
        framed_again.code()
    );
    let body = framed_again.json()?;
    assert_eq!(body["replayed"], true, "{body}");
    assert_eq!(body["data_version"], 2, "{body}");
    Ok(())
}

#[tokio::test]
async fn a_write_that_changes_nothing_keeps_the_version_and_records_no_change() -> TestResult {
    let harness = Harness::new();
    let mut frames = Vec::new();
    for _ in Kind::ALL {
        frames.push(notes_frame(&harness).await?);
    }
    // The stream starts at the head, after the seeding writes.
    let mut events = harness.follow().await?;
    events.expect("ready").await?;
    for (kind, frame) in Kind::ALL.into_iter().zip(&frames) {
        let reply = harness
            .send(write_call(frame, kind, &fresh("frame"), 1, kind.no_op())?)
            .await?;
        assert_eq!(reply.status, StatusCode::OK, "{kind:?}: {}", reply.code());
        let body = reply.json()?;
        assert_eq!(body["changed"], false, "{kind:?}: {body}");
        assert_eq!(
            body["data_version"], 1,
            "{kind:?}: a write that changed nothing keeps the version: {body}"
        );
        let owner = owner_read(&harness, &frame.id, None).await?;
        assert_eq!(owner["data_version"], 1, "{kind:?}: {owner}");
        assert!(
            events.quiet().await,
            "{kind:?}: a write that changed nothing was announced"
        );
    }
    Ok(())
}

#[tokio::test]
async fn each_changing_write_is_announced_once_with_its_new_version() -> TestResult {
    let harness = Harness::new();
    let mut frames = Vec::new();
    for _ in Kind::ALL {
        frames.push(notes_frame(&harness).await?);
    }
    let mut events = harness.follow().await?;
    events.expect("ready").await?;
    for (kind, frame) in Kind::ALL.into_iter().zip(&frames) {
        let reply = harness
            .send(write_call(
                frame,
                kind,
                &fresh("frame"),
                1,
                kind.changing(),
            )?)
            .await?;
        assert_eq!(reply.status, StatusCode::OK, "{kind:?}: {}", reply.code());
        let event = events.expect("change").await?;
        assert_eq!(
            event.data["change"], "data_changed",
            "{kind:?}: {}",
            event.data
        );
        assert_eq!(
            event.data["artifact_id"],
            frame.id.as_str(),
            "{kind:?}: {}",
            event.data
        );
        assert_eq!(event.data["data_version"], 2, "{kind:?}: {}", event.data);
        assert!(
            !event.data.to_string().contains("notes"),
            "{kind:?}: an event carries no path: {}",
            event.data
        );
    }
    assert!(
        events.quiet().await,
        "every announced change was one commit"
    );
    Ok(())
}

#[tokio::test]
async fn a_value_text_of_the_limit_is_stored_and_one_byte_more_is_refused() -> TestResult {
    let harness = Harness::new();
    for kind in [Kind::Set, Kind::Update] {
        let frame = write_frame(&harness).await?;
        let at_limit = quoted(VALUE_TEXT_LIMIT);
        let stored = harness
            .send(write_call(
                &frame,
                kind,
                &fresh("frame"),
                0,
                Write {
                    path: "edge",
                    text: &at_limit,
                },
            )?)
            .await?;
        assert_eq!(
            stored.status,
            StatusCode::OK,
            "{kind:?} at the limit: {}",
            stored.code()
        );
        assert_eq!(stored.json()?["data_version"], 1, "{kind:?}");
        let over = quoted(VALUE_TEXT_LIMIT + 1);
        let refused = harness
            .send(write_call(
                &frame,
                kind,
                &fresh("frame"),
                1,
                Write {
                    path: "edge",
                    text: &over,
                },
            )?)
            .await?;
        assert_eq!(
            refused.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{kind:?} one byte over: {}",
            refused.code()
        );
        assert_eq!(refused.code(), "data_too_large", "{kind:?}");
        let owner = owner_read(&harness, &frame.id, Some("edge")).await?;
        assert_eq!(
            owner["data_version"], 1,
            "{kind:?}: the refused write left the space at version 1"
        );
        assert_eq!(
            owner["value"].as_str().map(str::len),
            Some(VALUE_TEXT_LIMIT - 2),
            "{kind:?}: the stored value is the one at the limit"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_value_nested_32_levels_is_stored_and_33_levels_are_refused() -> TestResult {
    let harness = Harness::new();
    let frame = write_frame(&harness).await?;
    // The root is an object, so the object and 31 arrays make 32 levels.
    let fits = format!("{{\"a\":{}}}", arrays(31));
    let stored = harness
        .send(write_call(
            &frame,
            Kind::Set,
            &fresh("frame"),
            0,
            Write {
                path: "",
                text: &fits,
            },
        )?)
        .await?;
    assert_eq!(
        stored.status,
        StatusCode::OK,
        "32 levels: {}",
        stored.code()
    );
    assert_eq!(stored.json()?["data_version"], 1);
    let too_deep = format!("{{\"a\":{}}}", arrays(32));
    let refused = harness
        .send(write_call(
            &frame,
            Kind::Set,
            &fresh("frame"),
            1,
            Write {
                path: "",
                text: &too_deep,
            },
        )?)
        .await?;
    assert_eq!(
        refused.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "33 levels: {}",
        refused.code()
    );
    assert_eq!(refused.code(), "data_too_large");
    let owner = owner_read(&harness, &frame.id, None).await?;
    assert_eq!(owner["data_version"], 1, "{owner}");
    Ok(())
}

#[tokio::test]
async fn a_write_that_nests_the_space_past_32_levels_is_refused_though_its_value_fits() -> TestResult
{
    let harness = Harness::new();
    let frame = write_frame(&harness).await?;
    // Alone, 32 nested arrays are a valid value. At "deeper" they sit one level
    // below the root, which would make the space 33 levels deep.
    let shallow = arrays(31);
    let stored = harness
        .send(write_call(
            &frame,
            Kind::Set,
            &fresh("frame"),
            0,
            Write {
                path: "deep",
                text: &shallow,
            },
        )?)
        .await?;
    assert_eq!(stored.status, StatusCode::OK, "{}", stored.code());
    let deep = arrays(32);
    let refused = harness
        .send(write_call(
            &frame,
            Kind::Set,
            &fresh("frame"),
            1,
            Write {
                path: "deeper",
                text: &deep,
            },
        )?)
        .await?;
    assert_eq!(
        refused.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        refused.code()
    );
    assert_eq!(refused.code(), "data_too_large");
    let owner = owner_read(&harness, &frame.id, Some("deeper")).await?;
    assert_eq!(owner["exists"], false, "{owner}");
    Ok(())
}

#[tokio::test]
async fn a_write_past_one_mebibyte_of_space_is_refused_and_leaves_the_space() -> TestResult {
    let harness = Harness::new();
    let frame = write_frame(&harness).await?;
    let member = quoted(250_000);
    let mut version = 0;
    for name in ["p1", "p2", "p3", "p4"] {
        let reply = harness
            .send(write_call(
                &frame,
                Kind::Set,
                &fresh("frame"),
                version,
                Write {
                    path: name,
                    text: &member,
                },
            )?)
            .await?;
        assert_eq!(reply.status, StatusCode::OK, "{name}: {}", reply.code());
        version = reply.json()?["data_version"]
            .as_u64()
            .ok_or("data_version is not a number")?;
    }
    assert_eq!(
        version, 4,
        "four changed writes leave the space at version 4"
    );
    // Four members of 250,000 characters make about 1,000,000 bytes, and a
    // fifth one takes the space past 1 MiB.
    let refused = harness
        .send(write_call(
            &frame,
            Kind::Set,
            &fresh("frame"),
            4,
            Write {
                path: "p5",
                text: &member,
            },
        )?)
        .await?;
    assert_eq!(
        refused.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        refused.code()
    );
    assert_eq!(refused.code(), "data_too_large");
    let owner = owner_read(&harness, &frame.id, None).await?;
    assert_eq!(owner["data_version"], 4, "{owner}");
    let fifth = owner_read(&harness, &frame.id, Some("p5")).await?;
    assert_eq!(fifth["exists"], false, "{fifth}");
    Ok(())
}

#[tokio::test]
async fn a_write_past_ten_thousand_values_is_refused_and_leaves_the_space() -> TestResult {
    let harness = Harness::new();
    let frame = write_frame(&harness).await?;
    // The array and its 9,999 numbers are 10,000 values below the root.
    let many = format!("[{}]", vec!["0"; 9_999].join(","));
    let stored = harness
        .send(write_call(
            &frame,
            Kind::Set,
            &fresh("frame"),
            0,
            Write {
                path: "many",
                text: &many,
            },
        )?)
        .await?;
    assert_eq!(stored.status, StatusCode::OK, "{}", stored.code());
    let refused = harness
        .send(write_call(
            &frame,
            Kind::Set,
            &fresh("frame"),
            1,
            Write {
                path: "one_more",
                text: "1",
            },
        )?)
        .await?;
    assert_eq!(
        refused.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        refused.code()
    );
    assert_eq!(refused.code(), "data_too_large");
    let owner = owner_read(&harness, &frame.id, Some("one_more")).await?;
    assert_eq!(owner["exists"], false, "{owner}");
    Ok(())
}

#[tokio::test]
async fn text_that_is_not_json_is_refused_as_unprocessable() -> TestResult {
    let harness = Harness::new();
    for kind in [Kind::Set, Kind::Update] {
        let frame = write_frame(&harness).await?;
        let refused = harness
            .send(write_call(
                &frame,
                kind,
                &fresh("frame"),
                0,
                Write {
                    path: "x",
                    text: "{not json",
                },
            )?)
            .await?;
        assert_eq!(
            refused.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{kind:?}: {}",
            refused.code()
        );
        assert_eq!(refused.code(), "invalid_request", "{kind:?}");
        let owner = owner_read(&harness, &frame.id, None).await?;
        assert_eq!(owner["data_version"], 0, "{kind:?}: {owner}");
    }
    Ok(())
}

#[test]
fn the_write_bound_of_the_contract_is_the_value_limit_of_the_domain() -> TestResult {
    let domain = u64::try_from(MAX_VALUE_BYTES)?;
    assert_eq!(
        MAX_RUNTIME_DATA_WRITE_BYTES, domain,
        "the frame's write bound must follow decision 0031's value limit"
    );
    Ok(())
}

#[tokio::test]
async fn a_retry_after_the_head_moved_is_refused_not_replayed() -> TestResult {
    let harness = Harness::new();
    for kind in Kind::ALL {
        let frame = notes_frame(&harness).await?;
        let retry = fresh("frame");
        let first = harness
            .send(write_call(&frame, kind, &retry, 1, kind.changing())?)
            .await?;
        assert_eq!(first.status, StatusCode::OK, "{kind:?}: {}", first.code());
        assert_eq!(first.json()?["replayed"], false, "{kind:?}");
        // Revision 2 is saved, so the head moves past the revision the view is
        // pinned to.
        save_revision(&harness, &frame.id).await?;
        let retried = harness
            .send(write_call(&frame, kind, &retry, 1, kind.changing())?)
            .await?;
        assert_eq!(
            retried.status,
            StatusCode::CONFLICT,
            "{kind:?}: {}",
            retried.code()
        );
        assert_eq!(retried.code(), "revision_conflict", "{kind:?}");
        assert_eq!(
            retried.json()?["error"]["current_revision"],
            2,
            "{kind:?}: the head is at revision 2"
        );
        // A new write from the same stale view is refused for the same reason.
        let new_write = harness
            .send(write_call(
                &frame,
                kind,
                &fresh("frame"),
                2,
                kind.changing(),
            )?)
            .await?;
        assert_eq!(
            new_write.status,
            StatusCode::CONFLICT,
            "{kind:?}: {}",
            new_write.code()
        );
        assert_eq!(new_write.code(), "revision_conflict", "{kind:?}");
    }
    Ok(())
}
