//! Rule 8 of decision 0053 for the runtime data write routes (`data/set`,
//! `data/update` and `data/delete`, R14b), exercised in process through the real
//! routers, application core and `SQLite` store. The workspace page proxies a
//! frame's write with the account token and the view's initialization secret,
//! so each test plays the page. A call is judged in this order, and a later
//! check never runs for a caller an earlier one refuses: the binding, the grant
//! of the one capability, the artifact's head, then the arguments. The clock is
//! fixed. The view identities and secrets are the server's, read back from the
//! replies, so no randomness is injected.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use artifree_core::jobs::Clock;
use artifree_domain::identity::UnixMillis;
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
const DATA_READ: &str = "data.read";
const DATA_WRITE: &str = "data.write";
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
/// What a manifest declares in the write tests: the write, and the read that a
/// grant of the write must not stand in for.
const WRITE_DECLARED: [&str; 2] = [DATA_READ, DATA_WRITE];
/// 2^53 - 1, the largest base version a frame may name.
const LARGEST_BASE: u64 = 9_007_199_254_740_991;
/// Distinct idempotency keys and retry identities, so that tests can share a
/// process.
static NEXT_KEY: AtomicU32 = AtomicU32::new(1);

fn plentiful() -> RequestLimits {
    RequestLimits {
        burst: 100_000,
        per_minute: 6_000_000,
        concurrent: 64,
    }
}

/// The clock of every test, so that a write records the same time in every run.
#[derive(Debug)]
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> UnixMillis {
        UnixMillis(1_000)
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
                clock: Arc::new(FixedClock),
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

/// A JSON request with `token` as its bearer token; `idempotency`, when given,
/// is its idempotency key.
fn json_request(
    token: &str,
    builder: axum::http::request::Builder,
    idempotency: Option<&str>,
    body: &Value,
) -> Result<Request<Body>, axum::http::Error> {
    let mut builder = bearer(token, builder).header(header::CONTENT_TYPE, "application/json");
    if let Some(idempotency) = idempotency {
        builder = builder.header("idempotency-key", idempotency);
    }
    builder.body(Body::from(body.to_string()))
}

fn post(path: &str, body: &Value) -> Result<Request<Body>, axum::http::Error> {
    json_request(TOKEN, Request::post(path), None, body)
}

fn post_keyed(
    token: &str,
    path: &str,
    idempotency: &str,
    body: &Value,
) -> Result<Request<Body>, axum::http::Error> {
    json_request(token, Request::post(path), Some(idempotency), body)
}

fn get(token: &str, path: &str) -> Result<Request<Body>, axum::http::Error> {
    bearer(token, Request::get(path)).body(Body::empty())
}

/// A fresh key with `prefix`, unique within the process.
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
        "title": "Data write probe",
        "files": [
            {"path": "index.html", "encoding": "utf8", "content": ENTRY},
            {"path": "artifree-runtime.json", "encoding": "utf8", "content": manifest(declared)}
        ]
    });
    let reply = harness
        .send(post_keyed(
            token,
            "/v1/artifacts",
            &key("create-write"),
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
#[derive(Debug, Clone)]
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

    /// The body of a call: the view's initialization beside `extra`.
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
/// empty, with the reply that issued it.
async fn granted_view(
    harness: &Harness,
    declared: &[&str],
    granted: &[&str],
) -> Result<(Frame, Value), Box<dyn std::error::Error>> {
    let mut frame = connect(harness, declared).await?;
    let set = harness
        .send(grant_request(&frame.session, &frame.id, 0, granted)?)
        .await?;
    assert_eq!(set.status, StatusCode::OK, "{}", set.code());
    frame.grant_version = set.json()?["version"]
        .as_u64()
        .ok_or("the grant's version")?;
    let view = issue(harness, &frame, 1).await?;
    frame.initialization = secret_of(&view)?;
    Ok((frame, view))
}

/// A view of generation 1 under the connection's grant `granted`, which may be
/// empty.
async fn granted_frame(
    harness: &Harness,
    declared: &[&str],
    granted: &[&str],
) -> Result<Frame, Box<dyn std::error::Error>> {
    Ok(granted_view(harness, declared, granted).await?.0)
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

/// One call of `tail` on `frame` as `token`, with `fields` beside the view's
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

/// One write call of `frame` as `token`.
async fn write_call(
    harness: &Harness,
    token: &str,
    frame: &Frame,
    route: Write,
    fields: Value,
) -> Result<Reply, Box<dyn std::error::Error>> {
    harness
        .send(frame_call(token, frame, route.tail(), fields)?)
        .await
}

/// The receipt that a write of `frame` to `path` answers: the version it left
/// the space at, whether it changed the space, and that it is not a replay.
fn receipt(frame: &Frame, path: &str, version: u64, changed: bool) -> Value {
    json!({
        "artifact_id": frame.id,
        "path": path,
        "data_version": version,
        "changed": changed,
        "replayed": false
    })
}

/// The owner's own read of `at` in the data space of `artifact`, or of the root
/// when `at` is `None`.
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

/// The owner's read of the data space of `artifact` reports `version`, so that
/// nothing the calls of a test carried was stored.
async fn assert_space_version(
    harness: &Harness,
    artifact: &str,
    version: u64,
    context: &str,
) -> TestResult {
    let owner = owner_read(harness, artifact, None).await?;
    assert_eq!(
        owner["data_version"], version,
        "{context}: the space must stay at version {version}"
    );
    Ok(())
}

/// Saves revision 2 of `artifact`, which moves its head past the revision a
/// view of revision 1 is pinned to.
async fn save_revision(harness: &Harness, artifact: &str) -> TestResult {
    let reply = harness
        .send(post_keyed(
            TOKEN,
            &format!("/v1/artifacts/{artifact}/revisions"),
            &key("write-save"),
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

/// The three write routes of a frame (decision 0053, rule 4).
#[derive(Debug, Clone, Copy)]
enum Write {
    Set,
    Update,
    Delete,
}

/// A call with one argument refused, and the status that argument earns from a
/// caller that the earlier checks admit (rule 8).
struct Refusal {
    label: &'static str,
    fields: Value,
    status: StatusCode,
}

impl Write {
    /// Every write route, in the order of rule 4.
    const ALL: [Self; 3] = [Self::Set, Self::Update, Self::Delete];

    /// The route's name below a view.
    fn tail(self) -> &'static str {
        match self {
            Self::Set => "data/set",
            Self::Update => "data/update",
            Self::Delete => "data/delete",
        }
    }

    /// The field that carries JSON text: `value` for `set`, `patch` for
    /// `update`. `delete` carries none.
    fn text_field(self) -> Option<&'static str> {
        match self {
            Self::Set => Some("value"),
            Self::Update => Some("patch"),
            Self::Delete => None,
        }
    }

    /// JSON text that the route accepts, or `None` for a route without text.
    fn valid_text(self) -> Option<&'static str> {
        match self {
            Self::Set => Some("{\"b\":1}"),
            Self::Update => Some("{\"c\":2}"),
            Self::Delete => None,
        }
    }

    /// The fields of a call of this route on `path` at base `base`, with `text`
    /// in its text field when the route has one.
    fn fields(self, path: &str, retry: &str, base: u64, text: Option<&str>) -> Value {
        let mut fields = json!({"retry_id": retry, "base_data_version": base, "path": path});
        if let (Some(name), Some(text)) = (self.text_field(), text) {
            fields[name] = json!(text);
        }
        fields
    }

    /// The calls of this route that refuse one argument each, with the status
    /// each argument earns from an admitted caller. The path comes first, so a
    /// check misplaced ahead of the binding shows on the first row.
    fn refusals(self) -> Vec<Refusal> {
        let mut refusals = vec![
            Refusal {
                label: "a path that breaks the grammar",
                fields: self.fields("a//b", &key("frame-write"), 0, self.valid_text()),
                status: StatusCode::UNPROCESSABLE_ENTITY,
            },
            Refusal {
                label: "a retry identity of one character",
                fields: self.fields("notes", "x", 0, self.valid_text()),
                status: StatusCode::BAD_REQUEST,
            },
            Refusal {
                label: "a base past 2^53 - 1",
                fields: self.fields(
                    "notes",
                    &key("frame-write"),
                    LARGEST_BASE + 1,
                    self.valid_text(),
                ),
                status: StatusCode::BAD_REQUEST,
            },
        ];
        if self.text_field().is_some() {
            refusals.push(Refusal {
                label: "text that is not JSON",
                fields: self.fields("notes", &key("frame-write"), 0, Some("{not json")),
                status: StatusCode::UNPROCESSABLE_ENTITY,
            });
        }
        refusals
    }
}

/// Every write route answers `status` with `code` to each of its refused calls
/// and to a valid call, as `token` on `frame`.
async fn assert_every_write_answers(
    harness: &Harness,
    token: &str,
    frame: &Frame,
    status: StatusCode,
    code: &str,
    context: &str,
) -> TestResult {
    for route in Write::ALL {
        let mut calls: Vec<(&'static str, Value)> = route
            .refusals()
            .into_iter()
            .map(|refusal| (refusal.label, refusal.fields))
            .collect();
        calls.push((
            "a valid call",
            route.fields("notes", &key("frame-write"), 0, route.valid_text()),
        ));
        for (label, fields) in calls {
            let reply = write_call(harness, token, frame, route, fields).await?;
            assert_eq!(
                reply.status,
                status,
                "{context}, {}, {label}: {}",
                route.tail(),
                reply.code()
            );
            assert_eq!(reply.code(), code, "{context}, {}, {label}", route.tail());
        }
    }
    Ok(())
}

#[tokio::test]
async fn a_live_view_answers_each_refused_argument_with_its_own_status() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &WRITE_DECLARED, &[DATA_WRITE]).await?;
    // Admitted, each refused argument is judged on its own: these answers are
    // what the refusals below must not reach.
    for route in Write::ALL {
        for refusal in route.refusals() {
            let reply = write_call(&harness, TOKEN, &frame, route, refusal.fields).await?;
            assert_eq!(
                reply.status,
                refusal.status,
                "{}, {}: {}",
                route.tail(),
                refusal.label,
                reply.code()
            );
            assert_eq!(
                reply.code(),
                "invalid_request",
                "{}, {}",
                route.tail(),
                refusal.label
            );
        }
    }
    assert_space_version(&harness, &frame.id, 0, "after the refused arguments").await
}

#[tokio::test]
async fn a_live_view_with_the_grant_stores_each_write_once() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &WRITE_DECLARED, &[DATA_WRITE]).await?;
    let set = write_call(
        &harness,
        TOKEN,
        &frame,
        Write::Set,
        Write::Set.fields("notes", &key("frame-write"), 0, Write::Set.valid_text()),
    )
    .await?;
    assert_eq!(set.status, StatusCode::OK, "set: {}", set.code());
    assert_eq!(set.json()?, receipt(&frame, "notes", 1, true), "set");
    let update = write_call(
        &harness,
        TOKEN,
        &frame,
        Write::Update,
        Write::Update.fields("notes", &key("frame-write"), 1, Write::Update.valid_text()),
    )
    .await?;
    assert_eq!(update.status, StatusCode::OK, "update: {}", update.code());
    assert_eq!(update.json()?, receipt(&frame, "notes", 2, true), "update");
    let merged = owner_read(&harness, &frame.id, Some("notes")).await?;
    assert_eq!(
        merged["value"],
        json!({"b": 1, "c": 2}),
        "a merge keeps the stored member and adds the patch's"
    );
    let delete = write_call(
        &harness,
        TOKEN,
        &frame,
        Write::Delete,
        Write::Delete.fields("notes", &key("frame-write"), 2, None),
    )
    .await?;
    assert_eq!(delete.status, StatusCode::OK, "delete: {}", delete.code());
    assert_eq!(delete.json()?, receipt(&frame, "notes", 3, true), "delete");
    let removed = owner_read(&harness, &frame.id, Some("notes")).await?;
    assert_eq!(removed["exists"], false, "the deleted path holds nothing");
    Ok(())
}

#[tokio::test]
async fn a_wrong_view_generation_or_initialization_is_not_found_before_any_argument_is_judged()
-> TestResult {
    let harness = Harness::new();
    let granted = granted_frame(&harness, &WRITE_DECLARED, &[DATA_WRITE]).await?;
    let bindings = [
        (
            "a forged initialization",
            Frame {
                initialization: "0".repeat(64),
                ..granted.clone()
            },
        ),
        (
            "a wrong generation",
            Frame {
                generation: granted.generation + 1,
                ..granted.clone()
            },
        ),
        (
            "a wrong view",
            Frame {
                view: next_view(),
                ..granted.clone()
            },
        ),
    ];
    for (label, frame) in &bindings {
        assert_every_write_answers(
            &harness,
            TOKEN,
            frame,
            StatusCode::NOT_FOUND,
            "not_found",
            label,
        )
        .await?;
    }
    assert_space_version(&harness, &granted.id, 0, "after the refused writes").await
}

#[tokio::test]
async fn a_write_without_its_grant_is_forbidden_before_any_argument_is_judged() -> TestResult {
    let harness = Harness::new();
    let ungranted = ungranted_frame(&harness, &WRITE_DECLARED).await?;
    let read_only = granted_frame(&harness, &WRITE_DECLARED, &[DATA_READ]).await?;
    let (cleared, _) = granted_view(&harness, &WRITE_DECLARED, &[DATA_WRITE]).await?;
    let revoked = harness
        .send(grant_request(
            &cleared.session,
            &cleared.id,
            cleared.grant_version,
            &[],
        )?)
        .await?;
    assert_eq!(revoked.status, StatusCode::OK, "{}", revoked.code());
    let cases = [
        ("no grant", ungranted),
        ("a read-only grant", read_only),
        ("a cleared grant", cleared),
    ];
    for (label, frame) in &cases {
        assert_every_write_answers(
            &harness,
            TOKEN,
            frame,
            StatusCode::FORBIDDEN,
            "forbidden",
            label,
        )
        .await?;
        assert_space_version(&harness, &frame.id, 0, label).await?;
    }
    Ok(())
}

#[tokio::test]
async fn a_moved_head_conflicts_before_any_argument_is_judged() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &WRITE_DECLARED, &[DATA_WRITE]).await?;
    let forged = Frame {
        initialization: "0".repeat(64),
        ..frame.clone()
    };
    save_revision(&harness, &frame.id).await?;
    assert_every_write_answers(
        &harness,
        TOKEN,
        &frame,
        StatusCode::CONFLICT,
        "revision_conflict",
        "a moved head",
    )
    .await?;
    let stale = write_call(
        &harness,
        TOKEN,
        &frame,
        Write::Set,
        Write::Set.fields("notes", &key("frame-write"), 0, Write::Set.valid_text()),
    )
    .await?;
    assert_eq!(
        stale.json()?["error"]["current_revision"],
        2,
        "the conflict names the artifact's head: {}",
        stale.code()
    );
    assert_every_write_answers(
        &harness,
        TOKEN,
        &forged,
        StatusCode::NOT_FOUND,
        "not_found",
        "a moved head and a forged initialization",
    )
    .await?;
    assert_space_version(&harness, &frame.id, 0, "after the refused writes").await
}

#[tokio::test]
async fn a_write_is_admitted_by_its_own_grant_and_by_no_other_capability() -> TestResult {
    let harness = Harness::new();
    let others: Vec<&str> = ALL_CAPABILITIES
        .into_iter()
        .filter(|name| *name != DATA_WRITE)
        .collect();
    // Each other capability alone, then every other capability at once. The
    // capability that a mutation would confuse with data.write comes first.
    let mut cases: Vec<(String, Vec<&str>)> = others
        .iter()
        .map(|name| (format!("{name} alone"), vec![*name]))
        .collect();
    cases.push(("every capability but data.write".to_owned(), others.clone()));
    for (label, granted) in &cases {
        let frame = granted_frame(&harness, &ALL_CAPABILITIES, granted).await?;
        for route in Write::ALL {
            let fields = route.fields("notes", &key("frame-write"), 0, route.valid_text());
            let reply = write_call(&harness, TOKEN, &frame, route, fields).await?;
            assert_eq!(
                reply.status,
                StatusCode::FORBIDDEN,
                "{label}, {}: {}",
                route.tail(),
                reply.code()
            );
            assert_eq!(reply.code(), "forbidden", "{label}, {}", route.tail());
        }
        assert_space_version(&harness, &frame.id, 0, label).await?;
    }
    Ok(())
}

#[tokio::test]
async fn a_write_grant_alone_does_not_admit_a_read() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &ALL_CAPABILITIES, &[DATA_WRITE]).await?;
    for path in ["", "a//b"] {
        let reply = harness
            .send(frame_call(
                TOKEN,
                &frame,
                "data/read",
                json!({"path": path}),
            )?)
            .await?;
        assert_eq!(
            reply.status,
            StatusCode::FORBIDDEN,
            "data/read with data.write alone, path {path:?}: {}",
            reply.code()
        );
        assert_eq!(
            reply.code(),
            "forbidden",
            "data/read with data.write alone, path {path:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_view_reports_exactly_the_capabilities_granted_to_it() -> TestResult {
    let harness = Harness::new();
    let others: Vec<&str> = ALL_CAPABILITIES
        .into_iter()
        .filter(|name| *name != DATA_WRITE)
        .collect();
    for granted in [vec![DATA_WRITE], vec![DATA_READ], others] {
        let (_, view) = granted_view(&harness, &ALL_CAPABILITIES, &granted).await?;
        assert_eq!(
            view["granted"],
            json!(granted),
            "a grant of {granted:?} is reported as that grant"
        );
    }
    Ok(())
}

#[tokio::test]
async fn another_users_token_is_not_found_for_every_write() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &WRITE_DECLARED, &[DATA_WRITE]).await?;
    let bob = second_account(&harness).await?;
    assert_every_write_answers(
        &harness,
        &bob,
        &frame,
        StatusCode::NOT_FOUND,
        "not_found",
        "another user's token",
    )
    .await?;
    assert_space_version(&harness, &frame.id, 0, "after another user's writes").await?;
    // The owner's own call on the same view is admitted, so the refusals above
    // are about the user.
    let own = write_call(
        &harness,
        TOKEN,
        &frame,
        Write::Set,
        Write::Set.fields("notes", &key("frame-write"), 0, Write::Set.valid_text()),
    )
    .await?;
    assert_eq!(
        own.status,
        StatusCode::OK,
        "the owner's view: {}",
        own.code()
    );
    Ok(())
}
