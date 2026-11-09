//! Runtime `assets.list` and `assets.read` over HTTP (decision 0045, R09d1),
//! exercised in process through the real routers, application core, and
//! `SQLite` store. The workspace page proxies these calls for a frame, so each
//! test plays the page: it holds the account token and the view's secrets.

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
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::{Value, json};
use tower::ServiceExt;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const CONTENT_ORIGIN: &str = "http://127.0.0.1:7411";
const ENTRY: &str =
    "<!doctype html><html lang=\"en\"><head><!-- artifree-runtime --></head><body>x</body></html>";
/// The most bytes one asset part may carry (decision 0045, rule 12).
const MAX_PART: u64 = 262_144;
/// 2^53 - 1, the largest offset a frame may name.
const LARGEST_OFFSET: u64 = 9_007_199_254_740_991;
/// 2^53, the first offset a frame may not name.
const REFUSED_OFFSET: u64 = 9_007_199_254_740_992;
/// 600 KiB: 204,800 characters of three bytes each.
const LARGE_SIZE: u64 = 614_400;
/// SHA-256 of those bytes, computed outside this code with `sha256sum`.
const LARGE_SHA256: &str = "50e79cf8d300f5c641fb7620c304f9b4d12b1582f112ce48a95ac6550f93eaea";
/// SHA-256 of the one-byte asset `a`, computed outside this code.
const SMALL_SHA256: &str = "ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb";
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
            "artifree-runtime-assets-{}-{}",
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
        "title": "Asset probe",
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

/// The frame of view `generation`, which replaces every older generation.
async fn next_generation(
    harness: &Harness,
    frame: &Frame,
    generation: u64,
) -> Result<Frame, Box<dyn std::error::Error>> {
    let view = issue(harness, frame, generation).await?;
    Ok(Frame {
        generation,
        initialization: secret_of(&view)?,
        ..frame.clone()
    })
}

fn secret_of(view: &Value) -> Result<String, Box<dyn std::error::Error>> {
    Ok(view["initialization"]
        .as_str()
        .ok_or("initialization")?
        .to_owned())
}

fn assets_list(frame: &Frame) -> Result<Request<Body>, axum::http::Error> {
    post(&frame.route("assets/list"), &frame.body(json!({})))
}

fn assets_read(
    frame: &Frame,
    asset: &str,
    offset: u64,
    length: u64,
) -> Result<Request<Body>, axum::http::Error> {
    post(
        &frame.route("assets/read"),
        &frame.body(json!({"asset_id": asset, "offset": offset, "length": length})),
    )
}

fn source_list(frame: &Frame) -> Result<Request<Body>, axum::http::Error> {
    post(&frame.route("source/list"), &frame.body(json!({})))
}

fn source_read(frame: &Frame, path: &str) -> Result<Request<Body>, axum::http::Error> {
    post(
        &frame.route("source/read"),
        &frame.body(json!({"path": path, "offset": 0, "length": 1})),
    )
}

async fn read_part(
    harness: &Harness,
    frame: &Frame,
    asset: &str,
    offset: u64,
    length: u64,
) -> Result<Reply, Box<dyn std::error::Error>> {
    harness
        .send(assets_read(frame, asset, offset, length)?)
        .await
}

/// 600 KiB of one three-byte character, which is valid UTF-8 text.
fn large() -> Vec<u8> {
    "日".repeat(204_800).into_bytes()
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

fn decode(part: &Value) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    Ok(BASE64.decode(part["content"].as_str().ok_or("content")?)?)
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
async fn a_large_asset_reads_in_parts_that_assemble_its_exact_bytes() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.read"], &["assets.read"]).await?;
    let bytes = large();
    assert_eq!(bytes.len() as u64, LARGE_SIZE, "the fixture is 600 KiB");
    assert!(
        std::str::from_utf8(&bytes[..262_144]).is_err(),
        "the first part must end inside a character, or the parts do not cut at bytes"
    );
    let receipt = upload(&harness, TOKEN, &frame.id, 0, "text/plain", &bytes).await?;
    assert_eq!(
        receipt["asset"]["sha256"], LARGE_SHA256,
        "the upload's digest"
    );
    let asset = uploaded_id(&receipt)?;

    // Parts of 262,144 bytes from offset 0: 262,144, then 262,144, then the
    // rest. Each names the digest of the whole asset, and the chain ends.
    let parts: [(u64, u64, Option<u64>); 3] = [
        (0, 262_144, Some(262_144)),
        (262_144, 262_144, Some(524_288)),
        (524_288, 90_112, None),
    ];
    let mut joined = Vec::new();
    for (offset, length, next_offset) in parts {
        let reply = read_part(&harness, &frame, &asset, offset, MAX_PART).await?;
        assert_eq!(
            reply.status,
            StatusCode::OK,
            "offset {offset}: {}",
            reply.code()
        );
        let part = reply.json()?;
        assert_eq!(part["asset_id"], asset.as_str());
        assert_eq!(part["content_type"], "text/plain");
        assert_eq!(part["offset"], offset);
        assert_eq!(part["size"], LARGE_SIZE);
        assert_eq!(
            part["sha256"], LARGE_SHA256,
            "offset {offset}: the digest of the whole asset"
        );
        assert_eq!(part["next_offset"], json!(next_offset), "offset {offset}");
        let content = decode(&part)?;
        assert_eq!(
            content.len() as u64,
            length,
            "offset {offset}: the part's byte count"
        );
        joined.extend_from_slice(&content);
    }
    assert!(
        joined == bytes,
        "the parts do not assemble the asset's exact bytes"
    );
    Ok(())
}

#[tokio::test]
async fn a_one_byte_asset_reads_as_one_final_part() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.read"], &["assets.read"]).await?;
    let receipt = upload(&harness, TOKEN, &frame.id, 0, "text/plain", b"a").await?;
    let asset = uploaded_id(&receipt)?;
    assert_eq!(receipt["asset"]["size_bytes"], 1);
    for length in [1, MAX_PART] {
        let reply = read_part(&harness, &frame, &asset, 0, length).await?;
        assert_eq!(
            reply.status,
            StatusCode::OK,
            "length {length}: {}",
            reply.code()
        );
        let part = reply.json()?;
        assert_eq!(
            part["content"], "YQ==",
            "length {length}: the single byte a"
        );
        assert_eq!(part["content_type"], "text/plain");
        assert_eq!(part["size"], 1);
        assert_eq!(part["sha256"], SMALL_SHA256);
        assert_eq!(
            part["next_offset"],
            Value::Null,
            "length {length}: the only part is the last"
        );
    }
    Ok(())
}

#[tokio::test]
async fn an_offset_at_the_end_reads_an_empty_part_and_past_the_end_is_416() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.read"], &["assets.read"]).await?;
    let small = uploaded_id(&upload(&harness, TOKEN, &frame.id, 0, "text/plain", b"a").await?)?;
    let large_asset =
        uploaded_id(&upload(&harness, TOKEN, &frame.id, 1, "text/plain", &large()).await?)?;
    for (asset, size) in [(&small, 1_u64), (&large_asset, LARGE_SIZE)] {
        let end = read_part(&harness, &frame, asset, size, MAX_PART).await?;
        assert_eq!(end.status, StatusCode::OK, "size {size}: {}", end.code());
        let final_part = end.json()?;
        assert_eq!(
            final_part["content"], "",
            "size {size}: an offset at the end reads no bytes"
        );
        assert_eq!(final_part["next_offset"], Value::Null, "size {size}");
        assert_eq!(final_part["size"], size);
        let beyond = read_part(&harness, &frame, asset, size + 1, 1).await?;
        assert_eq!(
            beyond.status,
            StatusCode::RANGE_NOT_SATISFIABLE,
            "size {size}: {}",
            beyond.code()
        );
        assert_eq!(beyond.code(), "range_not_satisfiable", "size {size}");
    }
    Ok(())
}

#[tokio::test]
async fn no_grant_forbids_both_asset_routes() -> TestResult {
    let harness = Harness::new();
    // A view issued before the connection granted anything, and a view under
    // a grant that was set to nothing.
    let never_granted = ungranted_frame(&harness, &["assets.read"]).await?;
    let emptied = granted_frame(&harness, &["assets.read"], &[]).await?;
    for frame in [&never_granted, &emptied] {
        let asset = uploaded_id(&upload(&harness, TOKEN, &frame.id, 0, "text/plain", b"a").await?)?;
        let listed = harness.send(assets_list(frame)?).await?;
        assert_eq!(
            listed.status,
            StatusCode::FORBIDDEN,
            "assets/list: {}",
            listed.code()
        );
        assert_eq!(listed.code(), "forbidden");
        let read = read_part(&harness, frame, &asset, 0, 1).await?;
        assert_eq!(
            read.status,
            StatusCode::FORBIDDEN,
            "assets/read: {}",
            read.code()
        );
        assert_eq!(read.code(), "forbidden");
    }
    Ok(())
}

#[tokio::test]
async fn a_source_read_grant_does_not_admit_the_asset_routes() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.read", "source.read"], &["source.read"]).await?;
    let asset = uploaded_id(&upload(&harness, TOKEN, &frame.id, 0, "text/plain", b"a").await?)?;
    // The grant does admit the source routes, which shows the view works.
    let source = harness.send(source_list(&frame)?).await?;
    assert_eq!(
        source.status,
        StatusCode::OK,
        "source/list: {}",
        source.code()
    );
    let listed = harness.send(assets_list(&frame)?).await?;
    assert_eq!(
        listed.status,
        StatusCode::FORBIDDEN,
        "assets/list under a source.read grant: {}",
        listed.code()
    );
    let read = read_part(&harness, &frame, &asset, 0, 1).await?;
    assert_eq!(
        read.status,
        StatusCode::FORBIDDEN,
        "assets/read under a source.read grant: {}",
        read.code()
    );
    Ok(())
}

#[tokio::test]
async fn an_assets_read_grant_does_not_admit_the_source_routes() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.read", "source.read"], &["assets.read"]).await?;
    let asset = uploaded_id(&upload(&harness, TOKEN, &frame.id, 0, "text/plain", b"a").await?)?;
    // The grant does admit the asset routes, which shows the view works.
    let listed = harness.send(assets_list(&frame)?).await?;
    assert_eq!(
        listed.status,
        StatusCode::OK,
        "assets/list: {}",
        listed.code()
    );
    let read = read_part(&harness, &frame, &asset, 0, 1).await?;
    assert_eq!(read.status, StatusCode::OK, "assets/read: {}", read.code());
    let source = harness.send(source_list(&frame)?).await?;
    assert_eq!(
        source.status,
        StatusCode::FORBIDDEN,
        "source/list under an assets.read grant: {}",
        source.code()
    );
    let file = harness.send(source_read(&frame, "index.html")?).await?;
    assert_eq!(
        file.status,
        StatusCode::FORBIDDEN,
        "source/read under an assets.read grant: {}",
        file.code()
    );
    Ok(())
}

#[tokio::test]
async fn a_forged_secret_or_a_replaced_generation_is_not_found() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.read"], &["assets.read"]).await?;
    let asset = uploaded_id(&upload(&harness, TOKEN, &frame.id, 0, "text/plain", b"a").await?)?;
    let forged = Frame {
        initialization: "0".repeat(64),
        ..frame.clone()
    };
    let unissued = Frame {
        generation: 7,
        ..frame.clone()
    };
    for (label, hidden) in [
        ("a forged secret", &forged),
        ("an unissued generation", &unissued),
    ] {
        let listed = harness.send(assets_list(hidden)?).await?;
        assert_eq!(
            listed.status,
            StatusCode::NOT_FOUND,
            "{label}, assets/list: {}",
            listed.code()
        );
        let read = read_part(&harness, hidden, &asset, 0, 1).await?;
        assert_eq!(
            read.status,
            StatusCode::NOT_FOUND,
            "{label}, assets/read: {}",
            read.code()
        );
    }
    // Generation 2 replaces generation 1: the old view is gone, the new one reads.
    let second = next_generation(&harness, &frame, 2).await?;
    let old = harness.send(assets_list(&frame)?).await?;
    assert_eq!(
        old.status,
        StatusCode::NOT_FOUND,
        "generation 1, assets/list: {}",
        old.code()
    );
    let old_read = read_part(&harness, &frame, &asset, 0, 1).await?;
    assert_eq!(
        old_read.status,
        StatusCode::NOT_FOUND,
        "generation 1, assets/read"
    );
    let current = harness.send(assets_list(&second)?).await?;
    assert_eq!(
        current.status,
        StatusCode::OK,
        "generation 2, assets/list: {}",
        current.code()
    );
    Ok(())
}

#[tokio::test]
async fn closing_a_view_is_final_and_creating_another_does_not_revive_it() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.read"], &["assets.read"]).await?;
    let asset = uploaded_id(&upload(&harness, TOKEN, &frame.id, 0, "text/plain", b"a").await?)?;
    let closed = harness
        .send(
            Request::delete(format!("/v1/runtime-views/{}/1", frame.view))
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(closed.status, StatusCode::NO_CONTENT, "{}", closed.code());
    let listed = harness.send(assets_list(&frame)?).await?;
    assert_eq!(
        listed.status,
        StatusCode::NOT_FOUND,
        "closed view: {}",
        listed.code()
    );
    // Generation 1 cannot be created again: the closed generation still fences it.
    let again = harness.send(create_view(&frame, 1)?).await?;
    assert_eq!(
        again.status,
        StatusCode::CONFLICT,
        "re-creating generation 1: {}",
        again.code()
    );
    let still = harness.send(assets_list(&frame)?).await?;
    assert_eq!(
        still.status,
        StatusCode::NOT_FOUND,
        "after a refused re-create: {}",
        still.code()
    );
    // A newer generation reads, and the closed one stays dead.
    let second = next_generation(&harness, &frame, 2).await?;
    let current = harness.send(assets_list(&second)?).await?;
    assert_eq!(
        current.status,
        StatusCode::OK,
        "generation 2: {}",
        current.code()
    );
    let current_read = read_part(&harness, &second, &asset, 0, 1).await?;
    assert_eq!(
        current_read.status,
        StatusCode::OK,
        "generation 2: {}",
        current_read.code()
    );
    let old_read = read_part(&harness, &frame, &asset, 0, 1).await?;
    assert_eq!(
        old_read.status,
        StatusCode::NOT_FOUND,
        "closed generation 1: {}",
        old_read.code()
    );
    Ok(())
}

#[tokio::test]
async fn clearing_and_regranting_does_not_revive_the_view_bound_to_the_old_grant() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.read"], &["assets.read"]).await?;
    let asset = uploaded_id(&upload(&harness, TOKEN, &frame.id, 0, "text/plain", b"a").await?)?;
    let cleared = harness
        .send(grant_request(
            &frame.session,
            &frame.id,
            frame.grant_version,
            &[],
        )?)
        .await?;
    assert_eq!(cleared.status, StatusCode::OK, "{}", cleared.code());
    let cleared_version = cleared.json()?["version"].as_u64().ok_or("version")?;
    let while_cleared = harness.send(assets_list(&frame)?).await?;
    assert_eq!(
        while_cleared.status,
        StatusCode::FORBIDDEN,
        "{}",
        while_cleared.code()
    );
    let regranted = harness
        .send(grant_request(
            &frame.session,
            &frame.id,
            cleared_version,
            &["assets.read"],
        )?)
        .await?;
    assert_eq!(regranted.status, StatusCode::OK, "{}", regranted.code());
    // The view bound to the first grant stays refused, though its capability is back.
    let old_list = harness.send(assets_list(&frame)?).await?;
    assert_eq!(
        old_list.status,
        StatusCode::FORBIDDEN,
        "the old view after a re-grant: {}",
        old_list.code()
    );
    let old_read = read_part(&harness, &frame, &asset, 0, 1).await?;
    assert_eq!(
        old_read.status,
        StatusCode::FORBIDDEN,
        "the old view's read after a re-grant: {}",
        old_read.code()
    );
    // A view issued under the new grant reads, and it replaces the old one.
    let second = next_generation(&harness, &frame, 2).await?;
    let current = harness.send(assets_list(&second)?).await?;
    assert_eq!(
        current.status,
        StatusCode::OK,
        "new view: {}",
        current.code()
    );
    let replaced = harness.send(assets_list(&frame)?).await?;
    assert_eq!(
        replaced.status,
        StatusCode::NOT_FOUND,
        "{}",
        replaced.code()
    );
    Ok(())
}

#[tokio::test]
async fn the_length_and_offset_limits_refuse_400_at_their_edges() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.read"], &["assets.read"]).await?;
    let asset = uploaded_id(&upload(&harness, TOKEN, &frame.id, 0, "text/plain", &large()).await?)?;
    for (offset, length, status, code) in [
        (0, 0, StatusCode::BAD_REQUEST, "invalid_request"),
        (0, MAX_PART + 1, StatusCode::BAD_REQUEST, "invalid_request"),
        (
            REFUSED_OFFSET,
            1,
            StatusCode::BAD_REQUEST,
            "invalid_request",
        ),
        (
            LARGEST_OFFSET,
            1,
            StatusCode::RANGE_NOT_SATISFIABLE,
            "range_not_satisfiable",
        ),
    ] {
        let reply = read_part(&harness, &frame, &asset, offset, length).await?;
        assert_eq!(
            reply.status,
            status,
            "offset {offset}, length {length}: {}",
            reply.code()
        );
        assert_eq!(reply.code(), code, "offset {offset}, length {length}");
    }
    // The largest part allowed is accepted, and it ends where the next starts.
    let whole = read_part(&harness, &frame, &asset, 0, MAX_PART).await?;
    assert_eq!(whole.status, StatusCode::OK, "{}", whole.code());
    assert_eq!(whole.json()?["next_offset"], json!(MAX_PART));
    Ok(())
}

#[tokio::test]
async fn the_binding_and_the_grant_are_judged_before_the_arguments() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.read"], &["assets.read"]).await?;
    let asset = uploaded_id(&upload(&harness, TOKEN, &frame.id, 0, "text/plain", b"a").await?)?;
    let source_only =
        granted_frame(&harness, &["assets.read", "source.read"], &["source.read"]).await?;
    let forged = Frame {
        initialization: "0".repeat(64),
        ..frame.clone()
    };
    // Each argument set is refused by a live view under its grant. The
    // answers without a binding or a grant come from the order of the checks.
    let arguments = [
        (
            json!({"asset_id": asset, "offset": 0, "length": 0}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"asset_id": asset, "offset": 0, "length": MAX_PART + 1}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"asset_id": asset, "offset": REFUSED_OFFSET, "length": 1}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"asset_id": "not-an-asset", "offset": 0, "length": 1}),
            StatusCode::NOT_FOUND,
        ),
    ];
    for (body, judged) in arguments {
        let live = harness
            .send(post(
                &frame.route("assets/read"),
                &frame.body(body.clone()),
            )?)
            .await?;
        assert_eq!(
            live.status,
            judged,
            "{body} with a live view: {}",
            live.code()
        );
        let no_binding = harness
            .send(post(
                &forged.route("assets/read"),
                &forged.body(body.clone()),
            )?)
            .await?;
        assert_eq!(
            no_binding.status,
            StatusCode::NOT_FOUND,
            "{body} without a binding: {}",
            no_binding.code()
        );
        let no_grant = harness
            .send(post(
                &source_only.route("assets/read"),
                &source_only.body(body.clone()),
            )?)
            .await?;
        assert_eq!(
            no_grant.status,
            StatusCode::FORBIDDEN,
            "{body} without the assets.read grant: {}",
            no_grant.code()
        );
    }
    Ok(())
}

#[tokio::test]
async fn another_users_asset_a_malformed_id_and_a_trashed_artifact_are_not_found() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.read"], &["assets.read"]).await?;
    let asset = uploaded_id(&upload(&harness, TOKEN, &frame.id, 0, "text/plain", b"a").await?)?;
    // Another user's asset lives in that user's space, not in the owner's.
    let bob = second_account(&harness).await?;
    let bobs_artifact = create_artifact(&harness, &bob, &[]).await?;
    let bobs_asset =
        uploaded_id(&upload(&harness, &bob, &bobs_artifact, 0, "text/plain", b"b").await?)?;
    let read = read_part(&harness, &frame, &bobs_asset, 0, 1).await?;
    assert_eq!(
        read.status,
        StatusCode::NOT_FOUND,
        "another user's asset: {}",
        read.code()
    );
    let reply = harness.send(assets_list(&frame)?).await?;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.code());
    let listed = reply.json()?;
    let ids: Vec<&str> = listed["assets"]
        .as_array()
        .ok_or("assets")?
        .iter()
        .filter_map(|entry| entry["id"].as_str())
        .collect();
    assert_eq!(
        ids,
        [asset.as_str()],
        "the owner's listing holds only the owner's asset"
    );
    // Ids that cannot name an asset, then an artifact id that cannot name one.
    for malformed in [
        asset.to_uppercase(),
        asset[..63].to_owned(),
        "not-an-asset".to_owned(),
    ] {
        let reply = read_part(&harness, &frame, &malformed, 0, 1).await?;
        assert_eq!(
            reply.status,
            StatusCode::NOT_FOUND,
            "{malformed:?}: {}",
            reply.code()
        );
    }
    let unknown = Frame {
        id: "not-an-artifact".to_owned(),
        ..frame.clone()
    };
    let reply = harness.send(assets_list(&unknown)?).await?;
    assert_eq!(
        reply.status,
        StatusCode::NOT_FOUND,
        "a malformed artifact id: {}",
        reply.code()
    );
    // A trashed artifact answers as an unknown one.
    trash(&harness, &frame.id).await?;
    let listed = harness.send(assets_list(&frame)?).await?;
    assert_eq!(
        listed.status,
        StatusCode::NOT_FOUND,
        "trashed, assets/list: {}",
        listed.code()
    );
    let read = read_part(&harness, &frame, &asset, 0, 1).await?;
    assert_eq!(
        read.status,
        StatusCode::NOT_FOUND,
        "trashed, assets/read: {}",
        read.code()
    );
    Ok(())
}

#[tokio::test]
async fn a_moved_source_head_conflicts_before_the_arguments_are_judged() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.read"], &["assets.read"]).await?;
    let asset = uploaded_id(&upload(&harness, TOKEN, &frame.id, 0, "text/plain", b"a").await?)?;
    let before = harness.send(assets_list(&frame)?).await?;
    assert_eq!(before.status, StatusCode::OK, "{}", before.code());
    // Saving revision 2 moves the head past the revision the view is pinned to.
    let saved = harness
        .send(post_keyed(
            TOKEN,
            &format!("/v1/artifacts/{}/revisions", frame.id),
            &key("asset-save"),
            &json!({
                "base_revision": 1,
                "changes": [{"op": "put", "path": "notes.txt", "encoding": "utf8", "content": "new"}]
            }),
        )?)
        .await?;
    assert_eq!(saved.status, StatusCode::CREATED, "{}", saved.code());
    let listed = harness.send(assets_list(&frame)?).await?;
    assert_eq!(
        listed.status,
        StatusCode::CONFLICT,
        "assets/list: {}",
        listed.code()
    );
    assert_eq!(listed.code(), "revision_conflict");
    assert_eq!(listed.json()?["error"]["current_revision"], 2);
    let read = read_part(&harness, &frame, &asset, 0, 1).await?;
    assert_eq!(
        read.status,
        StatusCode::CONFLICT,
        "assets/read: {}",
        read.code()
    );
    assert_eq!(read.json()?["error"]["current_revision"], 2);
    // A refused argument does not come first: the conflict answers.
    let judged_later = read_part(&harness, &frame, &asset, 0, 0).await?;
    assert_eq!(
        judged_later.status,
        StatusCode::CONFLICT,
        "a zero length after a moved head: {}",
        judged_later.code()
    );
    Ok(())
}

#[tokio::test]
async fn the_frame_listing_is_the_http_asset_listing() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.read"], &["assets.read"]).await?;
    upload(&harness, TOKEN, &frame.id, 0, "text/plain", &large()).await?;
    upload(&harness, TOKEN, &frame.id, 1, "text/plain", b"a").await?;
    let http = http_listing(&harness, &frame.id).await?;
    let reply = harness.send(assets_list(&frame)?).await?;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.code());
    let listed = reply.json()?;
    assert_eq!(
        listed, http,
        "the frame listing and the HTTP listing differ"
    );
    assert_eq!(listed["asset_version"], 2);
    assert_eq!(listed["bytes"], LARGE_SIZE + 1);
    assert_eq!(listed["assets"].as_array().map(Vec::len), Some(2));
    Ok(())
}
