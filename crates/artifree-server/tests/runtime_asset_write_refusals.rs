//! Refusals of runtime `assets.write` uploads and deletions over HTTP
//! (decision 0045, R09d2), exercised in process through the real routers,
//! application core and `SQLite` store. The workspace page proxies a frame's
//! write with the account token and the view's secret, so each test plays the
//! page. The tests cover what answers before an argument is judged, the
//! content, quota and body limits, and that a refused write leaves no receipt,
//! no association and no version step behind.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use artifree_domain::assets::MAX_SPACE_ASSETS;
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
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::{Value, json};
use tower::ServiceExt;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const CONTENT_ORIGIN: &str = "http://127.0.0.1:7411";
const ENTRY: &str =
    "<!doctype html><html lang=\"en\"><head><!-- artifree-runtime --></head><body>x</body></html>";
/// The base64 text of one mebibyte, padding included: the longest content one
/// runtime upload may carry (`MAX_RUNTIME_ASSET_UPLOAD_BASE64`).
const MEBIBYTE_BASE64: usize = 1_398_104;
/// The body limit of the upload route: 1.5 MiB.
const UPLOAD_BODY_LIMIT: usize = 1_572_864;
/// The body limit of every other frame route, the asset deletion included.
const FRAME_BODY_LIMIT: usize = 16_384;
/// 2^53 - 1, the largest base version a frame may name.
const LARGEST_BASE: u64 = 9_007_199_254_740_991;
/// SHA-256 of one mebibyte of text (349,525 three-byte characters and one
/// `a`), computed outside this code with Python's `hashlib`.
const MEBIBYTE_SHA256: &str = "93d085d13f703ad4c6ee0b38326cbe1c38fc080fb2f80ca0867f259571d0f499";
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
            "artifree-runtime-asset-refusals-{}-{}",
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
        "title": "Asset refusals probe",
        "files": [
            {"path": "index.html", "encoding": "utf8", "content": ENTRY},
            {"path": "artifree-runtime.json", "encoding": "utf8", "content": manifest(declared)}
        ]
    });
    let reply = harness
        .send(post_keyed(
            token,
            "/v1/artifacts",
            &key("create-asset"),
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
    let view = issue(harness, &frame, 1).await?;
    assert_eq!(
        view["granted"].as_array().map(Vec::len),
        Some(granted.len())
    );
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

/// Uploads one asset to `artifact` as `token`, at the space's version `base`.
async fn upload(
    harness: &Harness,
    token: &str,
    artifact: &str,
    base: u64,
    content_type: &str,
    bytes: &[u8],
) -> Result<Value, Box<dyn std::error::Error>> {
    let reply = harness
        .send(post_keyed(
            token,
            &format!("/v1/artifacts/{artifact}/assets"),
            &key("asset-upload"),
            &json!({
                "base_asset_version": base,
                "content_type": content_type,
                "content_base64": BASE64.encode(bytes)
            }),
        )?)
        .await?;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.code());
    Ok(reply.json()?)
}

fn uploaded_id(receipt: &Value) -> Result<String, Box<dyn std::error::Error>> {
    Ok(receipt["asset"]["id"]
        .as_str()
        .ok_or("asset id")?
        .to_owned())
}

async fn http_listing(
    harness: &Harness,
    artifact: &str,
) -> Result<Value, Box<dyn std::error::Error>> {
    let reply = harness
        .send(get(TOKEN, &format!("/v1/artifacts/{artifact}/assets"))?)
        .await?;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.code());
    Ok(reply.json()?)
}

/// Moves the artifact to the trash through the owner's HTTP route.
async fn trash(harness: &Harness, artifact: &str) -> TestResult {
    let retry = key("asset-trash");
    let reply = harness
        .send(json_request(
            TOKEN,
            Request::patch(format!("/v1/artifacts/{artifact}")),
            Some(retry.as_str()),
            &json!({"base_metadata_revision": 1, "deleted": true}),
        )?)
        .await?;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.code());
    Ok(())
}

/// Erases the trashed artifact through the owner's HTTP route. The trash
/// moved its metadata to revision 2.
async fn purge(harness: &Harness, artifact: &str) -> TestResult {
    let retry = key("asset-purge");
    let reply = harness
        .send(json_request(
            TOKEN,
            Request::post(format!("/v1/artifacts/{artifact}/purge")),
            Some(retry.as_str()),
            &json!({"base_metadata_revision": 2}),
        )?)
        .await?;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.code());
    Ok(())
}

/// Saves revision 2 of `artifact` through the owner's route, which moves its
/// head past the revision a view of revision 1 is pinned to.
async fn save_revision_two(harness: &Harness, artifact: &str) -> TestResult {
    let reply = harness
        .send(post_keyed(
            TOKEN,
            &format!("/v1/artifacts/{artifact}/revisions"),
            &key("asset-save"),
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

/// The rows of the asset receipts and of the asset associations in the store
/// file the harness uses.
fn asset_rows(harness: &Harness) -> Result<(i64, i64), Box<dyn std::error::Error>> {
    let connection = rusqlite::Connection::open(harness.directory.join("db.sqlite3"))?;
    let receipts = connection.query_row("SELECT count(*) FROM asset_requests", [], |row| {
        row.get::<_, i64>(0)
    })?;
    let associations = connection.query_row("SELECT count(*) FROM assets", [], |row| {
        row.get::<_, i64>(0)
    })?;
    Ok((receipts, associations))
}

/// 349,525 three-byte characters followed by `tail`: 1,048,575 bytes plus the
/// tail, so the one-byte tail `a` makes exactly one mebibyte.
fn text_of(tail: &[u8]) -> Vec<u8> {
    let mut bytes = "日".repeat(349_525).into_bytes();
    bytes.extend_from_slice(tail);
    bytes
}

/// Fields of an upload call: the retry identity, the base version, the
/// declared type and the content as base64 text.
fn upload_fields(retry: &str, base: u64, content_type: &str, content_base64: &str) -> Value {
    json!({
        "retry_id": retry,
        "base_asset_version": base,
        "content_type": content_type,
        "content_base64": content_base64
    })
}

/// Fields of an upload of `bytes` as `text/plain`.
fn text_upload(retry: &str, base: u64, bytes: &[u8]) -> Value {
    upload_fields(retry, base, "text/plain", &BASE64.encode(bytes))
}

/// Fields of a deletion call.
fn delete_fields(retry: &str, base: u64, asset: &str) -> Value {
    json!({"retry_id": retry, "base_asset_version": base, "asset_id": asset})
}

/// Upload fields that a live view refuses, each for its own reason. The retry
/// identity, the base, the media type's syntax and the base64 text are judged
/// before the store is asked; the admitted types and the empty content are
/// judged by the content rules. None of them may be judged before the binding,
/// the grant, the head or the artifact.
fn refused_uploads() -> Vec<(&'static str, Value)> {
    vec![
        (
            "a retry identity of one character",
            upload_fields("x", 0, "text/plain", "YQ=="),
        ),
        (
            "a base past 2^53 - 1",
            upload_fields("argument-01", LARGEST_BASE + 1, "text/plain", "YQ=="),
        ),
        (
            "a media type that is not a type",
            upload_fields("argument-01", 0, "not a type", "YQ=="),
        ),
        (
            "content that is not base64",
            upload_fields("argument-01", 0, "text/plain", "!!!!"),
        ),
        (
            "content past the base64 limit",
            upload_fields(
                "argument-01",
                0,
                "text/plain",
                &"A".repeat(MEBIBYTE_BASE64 + 1),
            ),
        ),
        (
            "a type outside the admitted four",
            upload_fields("argument-01", 0, "text/html", &BASE64.encode(b"<p>a</p>")),
        ),
        (
            "empty content",
            upload_fields("argument-01", 0, "text/plain", ""),
        ),
    ]
}

/// Deletion fields that a live view refuses, each for its own reason.
fn refused_deletes(asset: &str) -> Vec<(&'static str, Value)> {
    vec![
        (
            "a retry identity of one character",
            delete_fields("x", 0, asset),
        ),
        (
            "a base past 2^53 - 1",
            delete_fields("argument-01", LARGEST_BASE + 1, asset),
        ),
        (
            "an asset identifier that is not one",
            delete_fields("argument-01", 0, "not-an-asset"),
        ),
    ]
}

/// One write call of a frame, as the page sends it: `token` as the bearer and
/// the view's secret in the body.
async fn send_frame(
    harness: &Harness,
    token: &str,
    frame: &Frame,
    tail: &str,
    fields: Value,
) -> Result<Reply, Box<dyn std::error::Error>> {
    harness
        .send(json_request(
            token,
            Request::post(frame.route(tail)),
            None,
            &frame.body(fields),
        )?)
        .await
}

/// One write call of a frame whose body is exactly `size` bytes: its fields
/// and a `padding` string that no write route accepts. A body that the limit
/// admits is refused as a bad request, and one it does not admit as too large.
async fn send_frame_sized(
    harness: &Harness,
    token: &str,
    frame: &Frame,
    tail: &str,
    fields: Value,
    size: usize,
) -> Result<Reply, Box<dyn std::error::Error>> {
    let mut body = frame.body(fields);
    body["padding"] = json!("");
    let filler = size
        .checked_sub(body.to_string().len())
        .ok_or("the fields are larger than the body")?;
    body["padding"] = json!("x".repeat(filler));
    assert_eq!(body.to_string().len(), size, "the body's size");
    harness
        .send(json_request(
            token,
            Request::post(frame.route(tail)),
            None,
            &body,
        )?)
        .await
}

/// Every refused upload and deletion of `asset`, whatever its arguments,
/// answers `status` with `code`.
async fn assert_every_write_answers(
    harness: &Harness,
    token: &str,
    frame: &Frame,
    asset: &str,
    status: StatusCode,
    code: &str,
    context: &str,
) -> TestResult {
    for (label, fields) in refused_uploads() {
        let reply = send_frame(harness, token, frame, "assets/upload", fields).await?;
        assert_eq!(
            reply.status,
            status,
            "{context}, upload, {label}: {}",
            reply.code()
        );
        assert_eq!(reply.code(), code, "{context}, upload, {label}");
    }
    for (label, fields) in refused_deletes(asset) {
        let reply = send_frame(harness, token, frame, "assets/delete", fields).await?;
        assert_eq!(
            reply.status,
            status,
            "{context}, delete, {label}: {}",
            reply.code()
        );
        assert_eq!(reply.code(), code, "{context}, delete, {label}");
    }
    Ok(())
}

/// Every write of the view is forbidden and changes nothing: the refused
/// arguments, and a valid upload and deletion too.
async fn assert_writes_forbidden(
    harness: &Harness,
    frame: &Frame,
    asset: &str,
    context: &str,
) -> TestResult {
    let before = http_listing(harness, &frame.id).await?;
    assert_every_write_answers(
        harness,
        TOKEN,
        frame,
        asset,
        StatusCode::FORBIDDEN,
        "forbidden",
        context,
    )
    .await?;
    let uploaded = send_frame(
        harness,
        TOKEN,
        frame,
        "assets/upload",
        text_upload("granted-refusal-01", 1, b"b"),
    )
    .await?;
    assert_eq!(
        uploaded.status,
        StatusCode::FORBIDDEN,
        "{context}, a valid upload: {}",
        uploaded.code()
    );
    let deleted = send_frame(
        harness,
        TOKEN,
        frame,
        "assets/delete",
        delete_fields("granted-refusal-02", 1, asset),
    )
    .await?;
    assert_eq!(
        deleted.status,
        StatusCode::FORBIDDEN,
        "{context}, a valid deletion: {}",
        deleted.code()
    );
    assert_eq!(
        http_listing(harness, &frame.id).await?,
        before,
        "{context}: the refused writes changed nothing"
    );
    Ok(())
}

#[tokio::test]
async fn a_forged_view_is_not_found_before_any_write_argument_is_judged() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.write"], &["assets.write"]).await?;
    let asset = uploaded_id(&upload(&harness, TOKEN, &frame.id, 0, "text/plain", b"a").await?)?;
    let forged = Frame {
        initialization: "0".repeat(64),
        ..frame.clone()
    };
    assert_every_write_answers(
        &harness,
        TOKEN,
        &forged,
        &asset,
        StatusCode::NOT_FOUND,
        "not_found",
        "a forged view",
    )
    .await
}

#[tokio::test]
async fn a_read_only_grant_is_forbidden_for_both_writes_before_any_argument_is_judged() -> TestResult
{
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.read", "assets.write"], &["assets.read"]).await?;
    let asset = uploaded_id(&upload(&harness, TOKEN, &frame.id, 0, "text/plain", b"a").await?)?;
    assert_writes_forbidden(&harness, &frame, &asset, "a read-only grant").await
}

#[tokio::test]
async fn a_source_write_grant_is_forbidden_for_both_writes() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(
        &harness,
        &["assets.write", "source.write"],
        &["source.write"],
    )
    .await?;
    let asset = uploaded_id(&upload(&harness, TOKEN, &frame.id, 0, "text/plain", b"a").await?)?;
    assert_writes_forbidden(&harness, &frame, &asset, "a source.write grant").await
}

#[tokio::test]
async fn a_moved_source_head_conflicts_for_both_writes_before_any_argument_is_judged() -> TestResult
{
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.write"], &["assets.write"]).await?;
    let asset = uploaded_id(&upload(&harness, TOKEN, &frame.id, 0, "text/plain", b"a").await?)?;
    save_revision_two(&harness, &frame.id).await?;
    assert_every_write_answers(
        &harness,
        TOKEN,
        &frame,
        &asset,
        StatusCode::CONFLICT,
        "revision_conflict",
        "a moved head",
    )
    .await?;
    let reply = send_frame(
        &harness,
        TOKEN,
        &frame,
        "assets/delete",
        delete_fields("moved-head-01", 1, &asset),
    )
    .await?;
    assert_eq!(
        reply.json()?["error"]["current_revision"],
        2,
        "{}",
        reply.code()
    );
    Ok(())
}

#[tokio::test]
async fn a_trashed_or_purged_artifact_is_not_found_for_both_writes() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.write"], &["assets.write"]).await?;
    let asset = uploaded_id(&upload(&harness, TOKEN, &frame.id, 0, "text/plain", b"a").await?)?;
    trash(&harness, &frame.id).await?;
    assert_every_write_answers(
        &harness,
        TOKEN,
        &frame,
        &asset,
        StatusCode::NOT_FOUND,
        "not_found",
        "a trashed artifact",
    )
    .await?;
    purge(&harness, &frame.id).await?;
    assert_every_write_answers(
        &harness,
        TOKEN,
        &frame,
        &asset,
        StatusCode::NOT_FOUND,
        "not_found",
        "a purged artifact",
    )
    .await
}

#[tokio::test]
async fn another_users_token_is_not_found_for_both_writes() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.write"], &["assets.write"]).await?;
    let asset = uploaded_id(&upload(&harness, TOKEN, &frame.id, 0, "text/plain", b"a").await?)?;
    let bob = second_account(&harness).await?;
    assert_every_write_answers(
        &harness,
        &bob,
        &frame,
        &asset,
        StatusCode::NOT_FOUND,
        "not_found",
        "another user's token",
    )
    .await?;
    let reply = send_frame(
        &harness,
        &bob,
        &frame,
        "assets/upload",
        text_upload("bob-valid-01", 1, b"b"),
    )
    .await?;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.code());
    assert_eq!(http_listing(&harness, &frame.id).await?["asset_version"], 1);
    Ok(())
}

#[tokio::test]
async fn exactly_one_mebibyte_of_utf8_text_is_accepted() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.write"], &["assets.write"]).await?;
    let reply = send_frame(
        &harness,
        TOKEN,
        &frame,
        "assets/upload",
        text_upload("mebibyte-0001", 0, &text_of(b"a")),
    )
    .await?;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.code());
    let receipt = reply.json()?;
    assert_eq!(receipt["asset_version"], 1);
    assert_eq!(receipt["asset"]["size_bytes"], json!(1_048_576));
    assert_eq!(receipt["asset"]["sha256"], MEBIBYTE_SHA256);
    assert_eq!(receipt["replayed"], false);
    assert_eq!(asset_rows(&harness)?, (1, 1));
    Ok(())
}

#[tokio::test]
async fn one_byte_more_than_a_mebibyte_is_refused_with_400_and_writes_nothing() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.write"], &["assets.write"]).await?;
    let before = asset_rows(&harness)?;
    let reply = send_frame(
        &harness,
        TOKEN,
        &frame,
        "assets/upload",
        text_upload("mebibyte-0002", 0, &text_of(b"ab")),
    )
    .await?;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.code());
    assert_eq!(reply.code(), "invalid_request");
    // The base64 text fits its limit, so the decoded size is what refuses it.
    assert_eq!(
        reply.json()?["error"]["message"],
        "content is larger than the write limit"
    );
    assert_eq!(asset_rows(&harness)?, before);
    assert_eq!(http_listing(&harness, &frame.id).await?["asset_version"], 0);
    Ok(())
}

#[tokio::test]
async fn over_long_base64_is_refused_by_its_length_before_it_is_decoded() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.write"], &["assets.write"]).await?;
    // One character past the limit, at a length that is not a multiple of
    // four: the text would not decode at all, so the length is what refuses it.
    let text = "A".repeat(MEBIBYTE_BASE64 + 1);
    let reply = send_frame(
        &harness,
        TOKEN,
        &frame,
        "assets/upload",
        upload_fields("over-long-0001", 0, "text/plain", &text),
    )
    .await?;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.code());
    assert_eq!(reply.code(), "invalid_request");
    assert_eq!(
        reply.json()?["error"]["message"],
        "content is larger than the write limit"
    );
    Ok(())
}

#[tokio::test]
async fn refused_content_answers_422_and_writes_no_receipt_or_association() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.write"], &["assets.write"]).await?;
    let before = asset_rows(&harness)?;
    let refusals = [
        (
            "a type outside the admitted four",
            upload_fields(
                "refused-type-01",
                0,
                "text/html",
                &BASE64.encode(b"<p>a</p>"),
            ),
            "asset_type_unsupported",
        ),
        (
            "empty content",
            text_upload("refused-empty-01", 0, b""),
            "invalid_asset",
        ),
    ];
    for (label, fields, code) in refusals {
        let reply = send_frame(&harness, TOKEN, &frame, "assets/upload", fields).await?;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{label}: {}",
            reply.code()
        );
        assert_eq!(reply.code(), code, "{label}");
    }
    assert_eq!(
        asset_rows(&harness)?,
        before,
        "a refused upload writes no receipt and no association"
    );
    assert_eq!(
        http_listing(&harness, &frame.id).await?["asset_version"],
        0,
        "a refused upload moves no version"
    );
    Ok(())
}

#[tokio::test]
async fn an_upload_past_the_artifact_count_limit_is_refused_with_no_row_left_behind() -> TestResult
{
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.write"], &["assets.write"]).await?;
    for base in 0..MAX_SPACE_ASSETS {
        let retry = format!("quota-fill-{base:03}");
        let reply = send_frame(
            &harness,
            TOKEN,
            &frame,
            "assets/upload",
            text_upload(&retry, base, b"a"),
        )
        .await?;
        assert_eq!(
            reply.status,
            StatusCode::OK,
            "upload at {base}: {}",
            reply.code()
        );
    }
    let limit = i64::try_from(MAX_SPACE_ASSETS)?;
    let full = asset_rows(&harness)?;
    assert_eq!(
        full,
        (limit, limit),
        "one receipt and one association per upload"
    );
    let reply = send_frame(
        &harness,
        TOKEN,
        &frame,
        "assets/upload",
        text_upload("quota-over-0001", MAX_SPACE_ASSETS, b"a"),
    )
    .await?;
    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        reply.code()
    );
    assert_eq!(reply.code(), "asset_limit_exceeded");
    assert_eq!(
        asset_rows(&harness)?,
        full,
        "the refused upload left no receipt and no association"
    );
    assert_eq!(
        http_listing(&harness, &frame.id).await?["asset_version"],
        json!(MAX_SPACE_ASSETS),
        "the refused upload moved no version"
    );
    Ok(())
}

#[tokio::test]
async fn the_upload_route_has_its_own_1_5_mib_body_limit() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.write"], &["assets.write"]).await?;
    // Larger than the 16 KiB of the other frame routes, and accepted.
    let twenty_kib = vec![b'a'; 20 * 1024];
    let reply = send_frame(
        &harness,
        TOKEN,
        &frame,
        "assets/upload",
        text_upload("body-limit-01", 0, &twenty_kib),
    )
    .await?;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.code());
    // A body of exactly the limit is read as a request: its padding is an
    // unknown field, so it is a bad request and not too large.
    let fields = text_upload("body-limit-02", 1, b"a");
    let at_limit = send_frame_sized(
        &harness,
        TOKEN,
        &frame,
        "assets/upload",
        fields.clone(),
        UPLOAD_BODY_LIMIT,
    )
    .await?;
    assert_eq!(
        at_limit.status,
        StatusCode::BAD_REQUEST,
        "{}",
        at_limit.code()
    );
    assert_eq!(at_limit.code(), "invalid_request");
    // One byte more is too large.
    let over = send_frame_sized(
        &harness,
        TOKEN,
        &frame,
        "assets/upload",
        fields,
        UPLOAD_BODY_LIMIT + 1,
    )
    .await?;
    assert_eq!(
        over.status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "{}",
        over.code()
    );
    assert_eq!(over.code(), "payload_too_large");
    assert_eq!(
        http_listing(&harness, &frame.id).await?["asset_version"],
        1,
        "only the first upload was written"
    );
    Ok(())
}

#[tokio::test]
async fn the_deletion_route_keeps_the_16_kib_body_limit() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.write"], &["assets.write"]).await?;
    let asset = uploaded_id(&upload(&harness, TOKEN, &frame.id, 0, "text/plain", b"a").await?)?;
    let before = http_listing(&harness, &frame.id).await?;
    let fields = delete_fields("body-limit-03", 1, &asset);
    // A body of exactly the limit is read as a request, so it is a bad request.
    let at_limit = send_frame_sized(
        &harness,
        TOKEN,
        &frame,
        "assets/delete",
        fields.clone(),
        FRAME_BODY_LIMIT,
    )
    .await?;
    assert_eq!(
        at_limit.status,
        StatusCode::BAD_REQUEST,
        "{}",
        at_limit.code()
    );
    assert_eq!(at_limit.code(), "invalid_request");
    let over = send_frame_sized(
        &harness,
        TOKEN,
        &frame,
        "assets/delete",
        fields,
        FRAME_BODY_LIMIT + 1,
    )
    .await?;
    assert_eq!(
        over.status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "{}",
        over.code()
    );
    assert_eq!(over.code(), "payload_too_large");
    assert_eq!(
        http_listing(&harness, &frame.id).await?,
        before,
        "neither refused deletion removed the asset"
    );
    Ok(())
}
