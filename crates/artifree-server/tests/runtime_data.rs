//! Runtime `data.get` over HTTP (decision 0053, R14a), exercised in process
//! through the real routers, application core, and `SQLite` store. The
//! workspace page proxies this call for a frame, so each test plays the page:
//! it holds the account token and the view's initialization secret.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

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
use serde_json::{Value, json};
use tower::ServiceExt;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const CONTENT_ORIGIN: &str = "http://127.0.0.1:7411";
const ENTRY: &str =
    "<!doctype html><html lang=\"en\"><head><!-- artifree-runtime --></head><body>x</body></html>";
/// The capability this file grants and withholds.
const DATA_READ: &str = "data.read";
/// Every capability name, in name order.
const ALL_CAPABILITIES: [&str; 9] = [
    "assets.download",
    "assets.read",
    "assets.write",
    "data.read",
    "data.write",
    "metadata.read",
    "source.download",
    "source.read",
    "source.write",
];
/// Distinct idempotency keys, so that tests can share a process.
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
            "artifree-runtime-data-{}-{}",
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

fn key(prefix: &str) -> String {
    format!("{prefix}-{}", NEXT_KEY.fetch_add(1, Ordering::Relaxed))
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
        "title": "Data probe",
        "files": [
            {"path": "index.html", "encoding": "utf8", "content": ENTRY},
            {"path": "artifree-runtime.json", "encoding": "utf8", "content": manifest(declared)}
        ]
    });
    let reply = harness
        .send(post_keyed(
            token,
            "/v1/artifacts",
            &key("create-data"),
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
#[derive(Clone)]
struct Frame {
    id: String,
    session: String,
    view: String,
    initialization: String,
    generation: u64,
    grant_version: u64,
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
        grant_version: 0,
    })
}

/// A view of generation 1 under the connection's grant `granted`, which may be
/// empty.
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
    frame.grant_version = set.json()?["version"].as_u64().ok_or("version")?;
    let view = issue(harness, &frame, 1).await?;
    assert_eq!(
        view["granted"].as_array().map(Vec::len),
        Some(granted.len())
    );
    frame.initialization = secret_of(&view)?;
    Ok(frame)
}

/// A view of generation 1 issued before the connection granted anything.
async fn ungranted_frame(
    harness: &Harness,
    declared: &[&str],
) -> Result<Frame, Box<dyn std::error::Error>> {
    let mut frame = connect(harness, declared).await?;
    let view = issue(harness, &frame, 1).await?;
    frame.initialization = secret_of(&view)?;
    Ok(frame)
}

fn create_view(frame: &Frame, generation: u64) -> Result<Request<Body>, axum::http::Error> {
    post(
        &format!("/v1/artifacts/{}/revisions/1/runtime-views", frame.id),
        &json!({"session_id": frame.session, "view_id": frame.view, "generation": generation}),
    )
}

/// Creates view `generation` and serves its document.
async fn issue(
    harness: &Harness,
    frame: &Frame,
    generation: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    let created = harness.send(create_view(frame, generation)?).await?;
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

/// A frame call to `tail` as the owner, whose body is `text` exactly.
fn raw_frame_call(
    frame: &Frame,
    tail: &str,
    text: String,
) -> Result<Request<Body>, axum::http::Error> {
    bearer(TOKEN, Request::post(frame.route(tail)))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(text))
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
    let retry = key("data-set");
    let reply = harness
        .send(json_request(
            TOKEN,
            Request::put(format!("/v1/artifacts/{artifact}/data")),
            Some(retry.as_str()),
            &json!({"path": path, "base_data_version": base, "value": value}),
        )?)
        .await?;
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
async fn save_revision(harness: &Harness, artifact: &str) -> TestResult {
    let retry = key("data-save");
    let reply = harness
        .send(post_keyed(
            TOKEN,
            &format!("/v1/artifacts/{artifact}/revisions"),
            &retry,
            &json!({
                "base_revision": 1,
                "changes": [{"op": "put", "path": "notes.txt", "encoding": "utf8", "content": "new"}]
            }),
        )?)
        .await?;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.code());
    Ok(())
}

/// A second account with a token of its own, which the owner registers.
async fn second_account(harness: &Harness) -> Result<String, Box<dyn std::error::Error>> {
    let user = harness
        .send(post("/v1/users", &json!({"label": "bob"}))?)
        .await?;
    assert_eq!(user.status, StatusCode::CREATED, "{}", user.code());
    let user_id = user.json()?["id"].clone();
    let issued = harness
        .send(post(
            "/v1/tokens",
            &json!({"name": "bob", "user_id": user_id}),
        )?)
        .await?;
    assert_eq!(issued.status, StatusCode::CREATED, "{}", issued.code());
    Ok(issued.json()?["secret"]
        .as_str()
        .ok_or("secret")?
        .to_owned())
}

#[tokio::test]
async fn a_granted_frame_reads_a_value_as_its_canonical_json_text() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &[DATA_READ], &[DATA_READ]).await?;
    let version = owner_set(
        &harness,
        &frame.id,
        0,
        "notes",
        &json!({"b": 1, "a": [true, null]}),
    )
    .await?;
    assert_eq!(
        version, 1,
        "one changed write leaves the space at version 1"
    );
    let owner = owner_read(&harness, &frame.id, Some("notes")).await?;
    let reply = harness
        .send(frame_call(
            TOKEN,
            &frame,
            "data/read",
            json!({"path": "notes"}),
        )?)
        .await?;
    assert_eq!(reply.status, StatusCode::OK, "data/read: {}", reply.code());
    let body = reply.json()?;
    let mut members: Vec<&str> = body
        .as_object()
        .ok_or("the reply is not an object")?
        .keys()
        .map(String::as_str)
        .collect();
    members.sort_unstable();
    assert_eq!(
        members,
        ["artifact_id", "data_version", "exists", "path", "value"],
        "the reply has exactly the members of the contract"
    );
    assert_eq!(body["artifact_id"], frame.id.as_str());
    assert_eq!(body["path"], "notes");
    assert_eq!(
        body["data_version"], owner["data_version"],
        "the frame reads the version the owner reads"
    );
    assert_eq!(body["exists"], true);
    let text = body["value"]
        .as_str()
        .ok_or("the value is not a JSON string")?;
    assert_eq!(
        text, r#"{"a":[true,null],"b":1}"#,
        "the value leaves as the canonical text the store keeps"
    );
    assert_eq!(
        serde_json::from_str::<Value>(text)?,
        owner["value"],
        "the text parses to the value the owner wrote"
    );
    Ok(())
}

#[tokio::test]
async fn a_stored_null_is_present_as_the_text_null() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &[DATA_READ], &[DATA_READ]).await?;
    owner_set(&harness, &frame.id, 0, "gone", &Value::Null).await?;
    let reply = harness
        .send(frame_call(
            TOKEN,
            &frame,
            "data/read",
            json!({"path": "gone"}),
        )?)
        .await?;
    assert_eq!(reply.status, StatusCode::OK, "data/read: {}", reply.code());
    let body = reply.json()?;
    assert_eq!(body["exists"], true, "a stored null exists");
    assert_eq!(
        body["value"], "null",
        "a stored null is the text null, not an absent value"
    );
    Ok(())
}

#[tokio::test]
async fn a_missing_path_has_no_value_member_and_the_root_is_an_object() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &[DATA_READ], &[DATA_READ]).await?;
    owner_set(&harness, &frame.id, 0, "notes", &json!({"b": 1})).await?;
    let missing = harness
        .send(frame_call(
            TOKEN,
            &frame,
            "data/read",
            json!({"path": "nothing/here"}),
        )?)
        .await?;
    assert_eq!(missing.status, StatusCode::OK, "{}", missing.code());
    let body = missing.json()?;
    assert_eq!(body["exists"], false, "nothing is at nothing/here");
    assert!(
        body.get("value").is_none(),
        "an absent value has no member, got {body}"
    );
    let root = harness
        .send(frame_call(TOKEN, &frame, "data/read", json!({"path": ""}))?)
        .await?;
    assert_eq!(root.status, StatusCode::OK, "the root: {}", root.code());
    let body = root.json()?;
    assert_eq!(body["exists"], true, "the root always exists");
    let text = body["value"]
        .as_str()
        .ok_or("the root's value is not a JSON string")?;
    assert_eq!(
        serde_json::from_str::<Value>(text)?,
        json!({"notes": {"b": 1}}),
        "the root is the whole object"
    );
    Ok(())
}

#[tokio::test]
async fn data_read_is_admitted_by_its_own_grant_and_by_no_other() -> TestResult {
    let harness = Harness::new();
    let others: Vec<&str> = ALL_CAPABILITIES
        .into_iter()
        .filter(|name| *name != DATA_READ)
        .collect();
    // Every capability but data.read: the view works, and data/read is refused.
    let without = granted_frame(&harness, &ALL_CAPABILITIES, &others).await?;
    for tail in ["metadata", "source/list", "assets/list"] {
        let reply = harness
            .send(frame_call(TOKEN, &without, tail, json!({}))?)
            .await?;
        assert_eq!(
            reply.status,
            StatusCode::OK,
            "{tail} under every capability but data.read: {}",
            reply.code()
        );
    }
    let refused = harness
        .send(frame_call(
            TOKEN,
            &without,
            "data/read",
            json!({"path": ""}),
        )?)
        .await?;
    assert_eq!(
        refused.status,
        StatusCode::FORBIDDEN,
        "data/read without data.read: {}",
        refused.code()
    );
    assert_eq!(refused.code(), "forbidden");
    // data.read alone: data/read is admitted, and the other routes are refused.
    let only = granted_frame(&harness, &ALL_CAPABILITIES, &[DATA_READ]).await?;
    let admitted = harness
        .send(frame_call(TOKEN, &only, "data/read", json!({"path": ""}))?)
        .await?;
    assert_eq!(
        admitted.status,
        StatusCode::OK,
        "data/read with data.read: {}",
        admitted.code()
    );
    // The metadata route answers 404 for any refusal (decisions 0019 and 0020);
    // the source and asset listings refuse with 403.
    for (tail, status, code) in [
        ("metadata", StatusCode::NOT_FOUND, "not_found"),
        ("source/list", StatusCode::FORBIDDEN, "forbidden"),
        ("assets/list", StatusCode::FORBIDDEN, "forbidden"),
    ] {
        let reply = harness
            .send(frame_call(TOKEN, &only, tail, json!({}))?)
            .await?;
        assert_eq!(
            reply.status,
            status,
            "{tail} with only data.read: {}",
            reply.code()
        );
        assert_eq!(reply.code(), code, "{tail} with only data.read");
    }
    Ok(())
}

#[tokio::test]
async fn the_binding_grant_and_head_are_judged_before_the_path() -> TestResult {
    let harness = Harness::new();
    let granted = granted_frame(&harness, &[DATA_READ], &[DATA_READ]).await?;
    let ungranted = ungranted_frame(&harness, &[DATA_READ]).await?;
    let revoked = granted_frame(&harness, &[DATA_READ], &[DATA_READ]).await?;
    let cleared = harness
        .send(grant_request(
            &revoked.session,
            &revoked.id,
            revoked.grant_version,
            &[],
        )?)
        .await?;
    assert_eq!(cleared.status, StatusCode::OK, "{}", cleared.code());
    let moved = granted_frame(&harness, &[DATA_READ], &[DATA_READ]).await?;
    save_revision(&harness, &moved.id).await?;
    let refusals = [
        (
            "a forged initialization",
            Frame {
                initialization: "0".repeat(64),
                ..granted.clone()
            },
            StatusCode::NOT_FOUND,
            "not_found",
        ),
        (
            "a wrong generation",
            Frame {
                generation: granted.generation + 1,
                ..granted.clone()
            },
            StatusCode::NOT_FOUND,
            "not_found",
        ),
        (
            "a wrong view",
            Frame {
                view: next_view(),
                ..granted.clone()
            },
            StatusCode::NOT_FOUND,
            "not_found",
        ),
        ("no grant", ungranted, StatusCode::FORBIDDEN, "forbidden"),
        (
            "a cleared grant",
            revoked,
            StatusCode::FORBIDDEN,
            "forbidden",
        ),
        (
            "a moved head",
            moved.clone(),
            StatusCode::CONFLICT,
            "revision_conflict",
        ),
        (
            "a moved head and a forged initialization",
            Frame {
                initialization: "0".repeat(64),
                ..moved
            },
            StatusCode::NOT_FOUND,
            "not_found",
        ),
    ];
    for path in ["a//b", ""] {
        for (label, frame, status, code) in &refusals {
            let reply = harness
                .send(frame_call(
                    TOKEN,
                    frame,
                    "data/read",
                    json!({"path": path}),
                )?)
                .await?;
            assert_eq!(
                reply.status,
                *status,
                "{label} with path {path:?}: {}",
                reply.code()
            );
            assert_eq!(reply.code(), *code, "{label} with path {path:?}");
        }
    }
    Ok(())
}

#[tokio::test]
async fn a_live_view_admits_the_root_and_refuses_a_bad_path_with_422() -> TestResult {
    let harness = Harness::new();
    let granted = granted_frame(&harness, &[DATA_READ], &[DATA_READ]).await?;
    let live = harness
        .send(frame_call(
            TOKEN,
            &granted,
            "data/read",
            json!({"path": ""}),
        )?)
        .await?;
    assert_eq!(live.status, StatusCode::OK, "the root: {}", live.code());
    let seventeen = ["a"; 17].join("/");
    for path in ["a//b", seventeen.as_str()] {
        let reply = harness
            .send(frame_call(
                TOKEN,
                &granted,
                "data/read",
                json!({"path": path}),
            )?)
            .await?;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "path {path:?} on a live view: {}",
            reply.code()
        );
        assert_eq!(reply.code(), "invalid_request", "path {path:?}");
    }
    Ok(())
}

#[tokio::test]
async fn a_body_that_is_not_the_request_is_refused_before_the_binding_is_judged() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &[DATA_READ], &[DATA_READ]).await?;
    let forged = Frame {
        initialization: "0".repeat(64),
        ..frame.clone()
    };
    for (binding, view) in [
        ("the view's initialization", &frame),
        ("a forged initialization", &forged),
    ] {
        let shapes = [
            ("text that is not JSON", "{not json".to_owned()),
            ("a body without a path", view.body(json!({})).to_string()),
            (
                "a body with an unknown member",
                view.body(json!({"path": "notes", "extra": 1})).to_string(),
            ),
            (
                "a path that is a number",
                view.body(json!({"path": 1})).to_string(),
            ),
        ];
        for (shape, text) in shapes {
            let reply = harness
                .send(raw_frame_call(view, "data/read", text)?)
                .await?;
            assert_eq!(
                reply.status,
                StatusCode::BAD_REQUEST,
                "{shape} with {binding}: {}",
                reply.code()
            );
            assert_eq!(reply.code(), "invalid_request", "{shape} with {binding}");
        }
    }
    let admitted = harness
        .send(frame_call(TOKEN, &frame, "data/read", json!({"path": ""}))?)
        .await?;
    assert_eq!(
        admitted.status,
        StatusCode::OK,
        "a well-formed body on the same view: {}",
        admitted.code()
    );
    Ok(())
}

#[tokio::test]
async fn the_root_of_a_nearly_full_space_reads_in_one_reply() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &[DATA_READ], &[DATA_READ]).await?;
    // Four members of 250,000 characters each. The root's canonical text is
    // then 1,000,029 bytes: under the 1,048,576-byte space bound, over 1,000,000.
    let mut version = 0;
    for name in ["a", "b", "c", "d"] {
        version = owner_set(
            &harness,
            &frame.id,
            version,
            name,
            &json!(name.repeat(250_000)),
        )
        .await?;
    }
    assert_eq!(
        version, 4,
        "four changed writes leave the space at version 4"
    );
    let owner = owner_read(&harness, &frame.id, None).await?;
    let canonical = serde_json::to_string(&owner["value"])?;
    assert!(
        (1_000_001..=1_048_576).contains(&canonical.len()),
        "the space must be nearly full, but holds {} bytes",
        canonical.len()
    );
    let reply = harness
        .send(frame_call(TOKEN, &frame, "data/read", json!({"path": ""}))?)
        .await?;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.code());
    let body = reply.json()?;
    assert_eq!(body["data_version"], 4);
    let text = body["value"]
        .as_str()
        .ok_or("the root's value is not a JSON string")?;
    assert_eq!(
        text.len(),
        canonical.len(),
        "one reply carries the whole space"
    );
    assert_eq!(text, canonical, "the reply is the space's canonical text");
    Ok(())
}

#[tokio::test]
async fn another_user_cannot_use_the_view() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &[DATA_READ], &[DATA_READ]).await?;
    owner_set(&harness, &frame.id, 0, "notes", &json!({"b": 1})).await?;
    let bob = second_account(&harness).await?;
    let refused = harness
        .send(frame_call(
            &bob,
            &frame,
            "data/read",
            json!({"path": "notes"}),
        )?)
        .await?;
    assert_eq!(
        refused.status,
        StatusCode::NOT_FOUND,
        "another user's view: {}",
        refused.code()
    );
    assert_eq!(refused.code(), "not_found");
    // The owner's own view still reads, so the refusal is about the user.
    let own = harness
        .send(frame_call(
            TOKEN,
            &frame,
            "data/read",
            json!({"path": "notes"}),
        )?)
        .await?;
    assert_eq!(
        own.status,
        StatusCode::OK,
        "the owner's view: {}",
        own.code()
    );
    Ok(())
}
