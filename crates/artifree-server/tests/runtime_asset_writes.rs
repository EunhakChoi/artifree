//! Runtime asset uploads and deletes over HTTP (decision 0045, rule 12),
//! exercised in process through the real routers, application core, and
//! `SQLite` store. The workspace page proxies these calls for a frame, so each
//! test plays the page: it holds the account token and the view's secrets. The
//! owner's own asset and revision routes check what a frame's writes stored.

use std::collections::BTreeMap;
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

type Failure = Box<dyn std::error::Error>;
type TestResult = Result<(), Failure>;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const CONTENT_ORIGIN: &str = "http://127.0.0.1:7411";
const ENTRY: &str =
    "<!doctype html><html lang=\"en\"><head><!-- artifree-runtime --></head><body>x</body></html>";
/// Text with multi-byte characters and CRLF line ends, so that a re-encoding or
/// a newline conversion would change its bytes.
const NOTES: &str = "naïve café — 日本語 🙂\r\nsecond line\n";
/// The byte count of `NOTES`, as `wc -c` prints it.
const NOTES_BYTES: u64 = 45;
/// The SHA-256 of the bytes of `NOTES`, as `sha256sum` prints it.
const NOTES_SHA256: &str = "325303f9f5ab0c089175ea242e61b200ab6a706f7220696dd9e59d4cb3cabeeb";
/// Another text, of 13 bytes.
const OTHER: &str = "another note\n";
/// The largest part a read may ask for (decision 0045).
const MAX_PART: u64 = 262_144;
/// Distinct keys and view identifiers, so that tests can share a process.
static NEXT_KEY: AtomicU32 = AtomicU32::new(1);

fn plentiful() -> RequestLimits {
    RequestLimits {
        burst: 100_000,
        per_minute: 6_000_000,
        concurrent: 64,
    }
}

/// One server process over a directory. A second one over the same directory
/// is a restart: only what the store kept survives it.
struct Harness {
    directory: PathBuf,
    owns_directory: bool,
    api: Router,
    content: Router,
}

impl Harness {
    fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!(
            "artifree-runtime-asset-writes-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory)
            .unwrap_or_else(|error| panic!("cannot create test directory: {error}"));
        Self::at(directory, true)
    }

    fn at(directory: PathBuf, owns_directory: bool) -> Self {
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
            owns_directory,
            api: http::router(state.clone()),
            content: preview::router(state),
        }
    }

    async fn send(&self, request: Request<Body>) -> Result<Reply, Failure> {
        Reply::collect(self.api.clone().oneshot(request).await?).await
    }

    /// Serves a view's document, which the page does before a frame may call.
    async fn serve(&self, url: &str) -> Result<Reply, Failure> {
        let path = url
            .strip_prefix(CONTENT_ORIGIN)
            .ok_or("preview URL uses another origin")?;
        let request = Request::get(path).body(Body::empty())?;
        Reply::collect(self.content.clone().oneshot(request).await?).await
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
    body: Vec<u8>,
}

impl Reply {
    async fn collect(response: axum::response::Response) -> Result<Self, Failure> {
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await?.to_vec();
        Ok(Self { status, body })
    }

    fn json(&self) -> Result<Value, serde_json::Error> {
        serde_json::from_slice(&self.body)
    }

    /// The error code of a refused call, or the body when it has none.
    fn code(&self) -> String {
        self.json()
            .ok()
            .and_then(|body| body["error"]["code"].as_str().map(str::to_owned))
            .unwrap_or_else(|| {
                format!("<no JSON error: {:?}>", String::from_utf8_lossy(&self.body))
            })
    }
}

/// A JSON request with the account token; `key`, when given, is its
/// idempotency key.
fn json_request(
    builder: axum::http::request::Builder,
    key: Option<&str>,
    body: &Value,
) -> Result<Request<Body>, axum::http::Error> {
    let mut builder = builder
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(key) = key {
        builder = builder.header("idempotency-key", key);
    }
    builder.body(Body::from(body.to_string()))
}

fn post(path: &str, body: &Value) -> Result<Request<Body>, axum::http::Error> {
    json_request(Request::post(path), None, body)
}

fn post_keyed(path: &str, key: &str, body: &Value) -> Result<Request<Body>, axum::http::Error> {
    json_request(Request::post(path), Some(key), body)
}

fn get(path: &str) -> Result<Request<Body>, axum::http::Error> {
    Request::get(path)
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::empty())
}

/// An owner idempotency key that no other request of this process uses.
fn key(prefix: &str) -> String {
    format!("{prefix}-{}", NEXT_KEY.fetch_add(1, Ordering::Relaxed))
}

/// A view id no other view of this process has: 32 lowercase hex digits, as
/// the view routes require.
fn next_view() -> String {
    format!("{:032x}", NEXT_KEY.fetch_add(1, Ordering::Relaxed))
}

fn manifest(declared: &[&str]) -> String {
    json!({"sdk": "artifree.app.v1", "capabilities": declared}).to_string()
}

/// An artifact that declares `declared`, created by the owner.
async fn create_artifact(harness: &Harness, declared: &[&str]) -> Result<String, Failure> {
    let body = json!({
        "title": "Asset write probe",
        "files": [
            {"path": "index.html", "encoding": "utf8", "content": ENTRY},
            {"path": "artifree-runtime.json", "encoding": "utf8", "content": manifest(declared)}
        ]
    });
    let reply = harness
        .send(post_keyed(
            "/v1/artifacts",
            &key("create-asset-write"),
            &body,
        )?)
        .await?;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.code());
    Ok(reply.json()?["artifact"]["id"]
        .as_str()
        .ok_or("missing artifact id")?
        .to_owned())
}

async fn open_session(harness: &Harness) -> Result<String, Failure> {
    let created = harness
        .send(post("/v1/runtime-sessions", &json!({}))?)
        .await?;
    assert_eq!(created.status, StatusCode::OK, "{}", created.code());
    Ok(created.json()?["session_id"]
        .as_str()
        .ok_or("missing session id")?
        .to_owned())
}

/// Sets the connection's grant on revision 1 of `artifact` to `capabilities`,
/// replacing the grant at `expected_version`.
fn grant_request(
    session: &str,
    artifact: &str,
    expected_version: u64,
    capabilities: &[&str],
) -> Result<Request<Body>, axum::http::Error> {
    json_request(
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

fn create_view(frame: &Frame, generation: u64) -> Result<Request<Body>, axum::http::Error> {
    post(
        &format!("/v1/artifacts/{}/revisions/1/runtime-views", frame.id),
        &json!({"session_id": frame.session, "view_id": frame.view, "generation": generation}),
    )
}

/// Creates view `generation` and serves its document.
async fn issue(harness: &Harness, frame: &Frame, generation: u64) -> Result<Value, Failure> {
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

fn secret_of(view: &Value) -> Result<String, Failure> {
    Ok(view["initialization"]
        .as_str()
        .ok_or("initialization")?
        .to_owned())
}

/// An artifact that declares `declared`, and a view of generation 1 on a
/// connection whose grant is `granted`.
async fn granted_frame(
    harness: &Harness,
    declared: &[&str],
    granted: &[&str],
) -> Result<Frame, Failure> {
    let id = create_artifact(harness, declared).await?;
    let session = open_session(harness).await?;
    let set = harness
        .send(grant_request(&session, &id, 0, granted)?)
        .await?;
    assert_eq!(set.status, StatusCode::OK, "{}", set.code());
    let mut frame = Frame {
        id,
        session,
        view: next_view(),
        initialization: String::new(),
        generation: 1,
    };
    let view = issue(harness, &frame, 1).await?;
    assert_eq!(
        view["granted"].as_array().map(Vec::len),
        Some(granted.len()),
        "the view holds the connection's grant"
    );
    frame.initialization = secret_of(&view)?;
    Ok(frame)
}

/// The frame of view `generation`, which replaces every older generation of
/// the same view.
async fn next_generation(
    harness: &Harness,
    frame: &Frame,
    generation: u64,
) -> Result<Frame, Failure> {
    let view = issue(harness, frame, generation).await?;
    Ok(Frame {
        generation,
        initialization: secret_of(&view)?,
        ..frame.clone()
    })
}

/// Another view of the same connection and artifact, at generation 1.
async fn sibling_view(harness: &Harness, frame: &Frame) -> Result<Frame, Failure> {
    let candidate = Frame {
        view: next_view(),
        ..frame.clone()
    };
    let view = issue(harness, &candidate, 1).await?;
    Ok(Frame {
        initialization: secret_of(&view)?,
        ..candidate
    })
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

/// A frame's upload of `bytes` as plain text, under `retry` at base `base`.
fn upload_request(
    frame: &Frame,
    retry: &str,
    base: u64,
    bytes: &[u8],
) -> Result<Request<Body>, axum::http::Error> {
    post(
        &frame.route("assets/upload"),
        &frame.body(json!({
            "retry_id": retry,
            "base_asset_version": base,
            "content_type": "text/plain",
            "content_base64": BASE64.encode(bytes),
        })),
    )
}

/// A frame's deletion of `asset`, under `retry` at base `base`.
fn delete_request(
    frame: &Frame,
    retry: &str,
    base: u64,
    asset: &str,
) -> Result<Request<Body>, axum::http::Error> {
    post(
        &frame.route("assets/delete"),
        &frame.body(json!({
            "retry_id": retry,
            "base_asset_version": base,
            "asset_id": asset,
        })),
    )
}

/// A frame's save of `bytes` to `path`, under `retry`.
fn source_write_request(
    frame: &Frame,
    retry: &str,
    path: &str,
    bytes: &[u8],
) -> Result<Request<Body>, axum::http::Error> {
    post(
        &frame.route("source/write"),
        &frame.body(json!({
            "retry_id": retry,
            "path": path,
            "content": BASE64.encode(bytes),
        })),
    )
}

/// Uploads `bytes` as plain text through the frame, which must be answered
/// with its receipt.
async fn upload_ok(
    harness: &Harness,
    frame: &Frame,
    retry: &str,
    base: u64,
    bytes: &[u8],
) -> Result<Value, Failure> {
    let reply = harness
        .send(upload_request(frame, retry, base, bytes)?)
        .await?;
    assert_eq!(
        reply.status,
        StatusCode::OK,
        "upload {retry}: {}",
        reply.code()
    );
    Ok(reply.json()?)
}

fn uploaded_id(receipt: &Value) -> Result<String, Failure> {
    Ok(receipt["asset"]["id"]
        .as_str()
        .ok_or("asset id")?
        .to_owned())
}

fn decode(part: &Value) -> Result<Vec<u8>, Failure> {
    Ok(BASE64.decode(part["content"].as_str().ok_or("content")?)?)
}

/// The owner's listing of the artifact's asset space.
async fn owner_listing(harness: &Harness, artifact: &str) -> Result<Value, Failure> {
    let reply = harness
        .send(get(&format!("/v1/artifacts/{artifact}/assets"))?)
        .await?;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.code());
    Ok(reply.json()?)
}

/// The owner's content route for one asset.
async fn owner_content(harness: &Harness, artifact: &str, asset: &str) -> Result<Reply, Failure> {
    harness
        .send(get(&format!(
            "/v1/artifacts/{artifact}/assets/{asset}/content"
        ))?)
        .await
}

/// An upload through the owner's route, under the owner's own `owner_key`.
async fn owner_upload(
    harness: &Harness,
    artifact: &str,
    owner_key: &str,
    base: u64,
    bytes: &[u8],
) -> Result<Reply, Failure> {
    harness
        .send(post_keyed(
            &format!("/v1/artifacts/{artifact}/assets"),
            owner_key,
            &json!({
                "base_asset_version": base,
                "content_type": "text/plain",
                "content_base64": BASE64.encode(bytes)
            }),
        )?)
        .await
}

/// How many revisions the owner sees for the artifact.
async fn revision_count(harness: &Harness, artifact: &str) -> Result<usize, Failure> {
    let listed = harness
        .send(get(&format!("/v1/artifacts/{artifact}/revisions"))?)
        .await?;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.code());
    let body = listed.json()?;
    Ok(body["revisions"].as_array().ok_or("revisions")?.len())
}

/// The owner's reading of `path` in `revision`.
async fn stored(
    harness: &Harness,
    artifact: &str,
    revision: u64,
    path: &str,
) -> Result<Vec<u8>, Failure> {
    let reply = harness
        .send(get(&format!(
            "/v1/artifacts/{artifact}/revisions/{revision}/files/{path}"
        ))?)
        .await?;
    assert_eq!(reply.status, StatusCode::OK, "{path}: {}", reply.code());
    Ok(reply.body)
}

/// The owner's listing of `revision`: path to SHA-256.
async fn digests(
    harness: &Harness,
    artifact: &str,
    revision: u64,
) -> Result<BTreeMap<String, String>, Failure> {
    let reply = harness
        .send(get(&format!(
            "/v1/artifacts/{artifact}/revisions/{revision}"
        ))?)
        .await?;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.code());
    let body = reply.json()?;
    let mut files = BTreeMap::new();
    for file in body["files"].as_array().ok_or("files")? {
        files.insert(
            file["path"].as_str().ok_or("path")?.to_owned(),
            file["sha256"].as_str().ok_or("sha256")?.to_owned(),
        );
    }
    Ok(files)
}

#[tokio::test]
async fn an_upload_reads_back_exactly_and_moves_only_the_asset_version() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(
        &harness,
        &["assets.read", "assets.write"],
        &["assets.read", "assets.write"],
    )
    .await?;
    let source_before = digests(&harness, &frame.id, 1).await?;

    let receipt = upload_ok(&harness, &frame, "retry-0001", 0, NOTES.as_bytes()).await?;
    assert_eq!(receipt["asset_version"], 1, "base 0 plus one");
    assert_eq!(receipt["replayed"], false, "a first upload is not a replay");
    assert_eq!(receipt["asset"]["content_type"], "text/plain");
    assert_eq!(receipt["asset"]["size_bytes"], NOTES_BYTES);
    assert_eq!(receipt["asset"]["sha256"], NOTES_SHA256);
    let asset = uploaded_id(&receipt)?;

    // The owner's content route returns the stored bytes exactly.
    let content = owner_content(&harness, &frame.id, &asset).await?;
    assert_eq!(content.status, StatusCode::OK, "{}", content.code());
    assert_eq!(
        content.body,
        NOTES.as_bytes(),
        "the owner reads the exact bytes"
    );

    // So does the frame's own read, in one part that ends the asset.
    let read = harness
        .send(assets_read(&frame, &asset, 0, MAX_PART)?)
        .await?;
    assert_eq!(read.status, StatusCode::OK, "{}", read.code());
    let part = read.json()?;
    assert_eq!(
        decode(&part)?,
        NOTES.as_bytes(),
        "the frame reads the exact bytes"
    );
    assert_eq!(part["size"], NOTES_BYTES, "the whole asset's size");
    assert_eq!(part["sha256"], NOTES_SHA256);
    assert_eq!(part["next_offset"], Value::Null);

    let listing = owner_listing(&harness, &frame.id).await?;
    assert_eq!(listing["asset_version"], 1);
    assert_eq!(listing["bytes"], NOTES_BYTES);
    assert_eq!(listing["assets"].as_array().map(Vec::len), Some(1));
    // The source is what it was: the same files, and no new revision.
    assert_eq!(
        digests(&harness, &frame.id, 1).await?,
        source_before,
        "the source files are unchanged"
    );
    assert_eq!(
        revision_count(&harness, &frame.id).await?,
        1,
        "the upload made no revision"
    );
    Ok(())
}

#[tokio::test]
async fn a_delete_removes_the_asset_and_its_retry_answers_the_first_outcome() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.write"], &["assets.write"]).await?;
    let asset =
        uploaded_id(&upload_ok(&harness, &frame, "retry-0001", 0, NOTES.as_bytes()).await?)?;

    let deleted = harness
        .send(delete_request(&frame, "retry-0002", 1, &asset)?)
        .await?;
    assert_eq!(deleted.status, StatusCode::OK, "{}", deleted.code());
    assert_eq!(
        deleted.json()?,
        json!({"asset_id": asset, "asset_version": 2, "deleted": true, "replayed": false})
    );

    // The same delete, retried, is the first answer again and moves nothing.
    let retried = harness
        .send(delete_request(&frame, "retry-0002", 1, &asset)?)
        .await?;
    assert_eq!(retried.status, StatusCode::OK, "{}", retried.code());
    assert_eq!(
        retried.json()?,
        json!({"asset_id": asset, "asset_version": 2, "deleted": true, "replayed": true})
    );

    let listing = owner_listing(&harness, &frame.id).await?;
    assert_eq!(listing["asset_version"], 2);
    assert_eq!(listing["assets"], json!([]), "the association is gone");
    let gone = owner_content(&harness, &frame.id, &asset).await?;
    assert_eq!(gone.status, StatusCode::NOT_FOUND, "{}", gone.code());
    assert_eq!(revision_count(&harness, &frame.id).await?, 1);
    Ok(())
}

#[tokio::test]
async fn deleting_an_absent_asset_says_not_deleted_and_moves_nothing() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.write"], &["assets.write"]).await?;
    let asset =
        uploaded_id(&upload_ok(&harness, &frame, "retry-0001", 0, NOTES.as_bytes()).await?)?;
    let removed = harness
        .send(delete_request(&frame, "retry-0002", 1, &asset)?)
        .await?;
    assert_eq!(removed.status, StatusCode::OK, "{}", removed.code());
    assert_eq!(removed.json()?["deleted"], true);

    // Deleted once, the asset is absent: a new identity says so, and the version stays.
    let absent = harness
        .send(delete_request(&frame, "retry-0003", 2, &asset)?)
        .await?;
    assert_eq!(absent.status, StatusCode::OK, "{}", absent.code());
    assert_eq!(
        absent.json()?,
        json!({"asset_id": asset, "asset_version": 2, "deleted": false, "replayed": false})
    );

    // An identifier the artifact never had gets the same answer.
    let never = "0".repeat(64);
    let unknown = harness
        .send(delete_request(&frame, "retry-0004", 2, &never)?)
        .await?;
    assert_eq!(unknown.status, StatusCode::OK, "{}", unknown.code());
    assert_eq!(
        unknown.json()?,
        json!({"asset_id": never, "asset_version": 2, "deleted": false, "replayed": false})
    );

    let listing = owner_listing(&harness, &frame.id).await?;
    assert_eq!(
        listing["asset_version"], 2,
        "no absent delete moved the version"
    );
    Ok(())
}

#[tokio::test]
async fn a_retry_with_the_same_bytes_replays_one_association_and_one_version_step() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.write"], &["assets.write"]).await?;
    let first = upload_ok(&harness, &frame, "retry-0001", 0, NOTES.as_bytes()).await?;
    assert_eq!(first["replayed"], false);

    // The same call sent again, as after a lost answer, gets the first outcome.
    let retried = upload_ok(&harness, &frame, "retry-0001", 0, NOTES.as_bytes()).await?;
    assert_eq!(retried["replayed"], true, "the retry is a replay");
    assert_eq!(
        retried["asset_version"], 1,
        "the version did not step again"
    );
    assert_eq!(
        retried["asset"], first["asset"],
        "the same association, with the same metadata"
    );

    let listing = owner_listing(&harness, &frame.id).await?;
    assert_eq!(listing["asset_version"], 1);
    assert_eq!(
        listing["assets"].as_array().map(Vec::len),
        Some(1),
        "one association"
    );
    Ok(())
}

#[tokio::test]
async fn the_same_retry_identity_with_other_bytes_is_a_reused_key_that_changes_nothing()
-> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.write"], &["assets.write"]).await?;
    let asset =
        uploaded_id(&upload_ok(&harness, &frame, "retry-0001", 0, NOTES.as_bytes()).await?)?;

    // Other bytes under the same identity are another request, even at the same base.
    let mismatched = harness
        .send(upload_request(&frame, "retry-0001", 0, OTHER.as_bytes())?)
        .await?;
    assert_eq!(
        mismatched.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        mismatched.code()
    );
    assert_eq!(mismatched.code(), "request_key_reused");

    // A delete under that identity is another request too.
    let deleting = harness
        .send(delete_request(&frame, "retry-0001", 1, &asset)?)
        .await?;
    assert_eq!(
        deleting.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        deleting.code()
    );
    assert_eq!(deleting.code(), "request_key_reused");

    let listing = owner_listing(&harness, &frame.id).await?;
    assert_eq!(
        listing["asset_version"], 1,
        "neither refused call moved the version"
    );
    let content = owner_content(&harness, &frame.id, &asset).await?;
    assert_eq!(
        content.body,
        NOTES.as_bytes(),
        "the first bytes are still the stored ones"
    );
    Ok(())
}

#[tokio::test]
async fn the_same_retry_identity_in_another_artifact_view_or_generation_is_another_request()
-> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.write"], &["assets.write"]).await?;
    let elsewhere = granted_frame(&harness, &["assets.write"], &["assets.write"]).await?;
    let first = upload_ok(&harness, &frame, "retry-0001", 0, NOTES.as_bytes()).await?;

    // Another artifact with a view of its own: the same identity and bytes are
    // another request there.
    let other_artifact = upload_ok(&harness, &elsewhere, "retry-0001", 0, NOTES.as_bytes()).await?;
    assert_eq!(
        other_artifact["replayed"], false,
        "another artifact's upload is its own request"
    );
    assert_ne!(other_artifact["asset"]["id"], first["asset"]["id"]);

    // Another view on the same connection: the identity is scoped to the view too.
    let sibling = sibling_view(&harness, &frame).await?;
    let other_view = upload_ok(&harness, &sibling, "retry-0001", 1, NOTES.as_bytes()).await?;
    assert_eq!(
        other_view["replayed"], false,
        "another view's upload is its own request"
    );
    assert_eq!(other_view["asset_version"], 2);

    // The same view one generation later: the identity is scoped to the generation.
    let later = next_generation(&harness, &frame, 2).await?;
    let other_generation = upload_ok(&harness, &later, "retry-0001", 2, NOTES.as_bytes()).await?;
    assert_eq!(
        other_generation["replayed"], false,
        "a new generation's upload is its own request"
    );
    assert_eq!(other_generation["asset_version"], 3);

    let listing = owner_listing(&harness, &frame.id).await?;
    assert_eq!(listing["asset_version"], 3);
    assert_eq!(
        listing["assets"].as_array().map(Vec::len),
        Some(3),
        "one association per request"
    );
    Ok(())
}

#[tokio::test]
async fn a_source_write_and_an_asset_upload_with_the_same_retry_identity_both_succeed() -> TestResult
{
    let harness = Harness::new();
    let frame = granted_frame(
        &harness,
        &["assets.write", "source.write"],
        &["assets.write", "source.write"],
    )
    .await?;

    // Both calls come from one view and generation under one identity. The
    // keys differ by operation, so neither is refused as a reused key.
    let uploaded = upload_ok(&harness, &frame, "retry-0001", 0, NOTES.as_bytes()).await?;
    assert_eq!(uploaded["asset_version"], 1);
    let saved = harness
        .send(source_write_request(
            &frame,
            "retry-0001",
            "notes.txt",
            OTHER.as_bytes(),
        )?)
        .await?;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.code());
    assert_eq!(saved.json()?["revision"], 2, "the save made revision 2");

    // The save moved the source head, not the asset space, and each write landed once.
    let listing = owner_listing(&harness, &frame.id).await?;
    assert_eq!(listing["asset_version"], 1);
    assert_eq!(listing["assets"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        stored(&harness, &frame.id, 2, "notes.txt").await?,
        OTHER.as_bytes()
    );
    assert_eq!(revision_count(&harness, &frame.id).await?, 2);
    Ok(())
}

#[tokio::test]
async fn an_owner_key_shaped_like_a_frame_key_is_an_ordinary_key() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.write"], &["assets.write"]).await?;
    let first = upload_ok(&harness, &frame, "retry-0001", 0, NOTES.as_bytes()).await?;

    // The owner's key `rta:abcdefgh` is an ordinary key: it creates, replays and
    // refuses reuse as any owner key does.
    let created = owner_upload(&harness, &frame.id, "rta:abcdefgh", 1, OTHER.as_bytes()).await?;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.code());
    assert_eq!(created.json()?["replayed"], false);
    let replayed = owner_upload(&harness, &frame.id, "rta:abcdefgh", 1, OTHER.as_bytes()).await?;
    assert_eq!(replayed.status, StatusCode::CREATED, "{}", replayed.code());
    assert_eq!(replayed.json()?["replayed"], true);
    let reused = owner_upload(&harness, &frame.id, "rta:abcdefgh", 1, NOTES.as_bytes()).await?;
    assert_eq!(
        reused.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        reused.code()
    );
    assert_eq!(reused.code(), "request_key_reused");

    // A key spelled as the frame's own key would be, with `:` in place of `~`, is an
    // ordinary owner key too: it is not the frame's request.
    let spelled = format!("rta:{}:{}:1:retry-0001", frame.id, frame.view);
    let beside = owner_upload(&harness, &frame.id, &spelled, 2, NOTES.as_bytes()).await?;
    assert_eq!(beside.status, StatusCode::CREATED, "{}", beside.code());
    assert_eq!(beside.json()?["asset_version"], 3);

    // The frame's namespace takes no owner text: the tilde spelling is not a key at all.
    let tilde = format!("rta~{}:{}:1:retry-0001", frame.id, frame.view);
    let rejected = owner_upload(&harness, &frame.id, &tilde, 3, NOTES.as_bytes()).await?;
    assert_eq!(
        rejected.status,
        StatusCode::BAD_REQUEST,
        "{}",
        rejected.code()
    );
    assert_eq!(rejected.code(), "invalid_request");

    // The frame's own receipt is untouched: its retry still replays the first outcome.
    let retried = upload_ok(&harness, &frame, "retry-0001", 0, NOTES.as_bytes()).await?;
    assert_eq!(retried["replayed"], true);
    assert_eq!(retried["asset"], first["asset"]);

    let listing = owner_listing(&harness, &frame.id).await?;
    assert_eq!(
        listing["asset_version"], 3,
        "no refused or replayed call moved the version"
    );
    assert_eq!(listing["assets"].as_array().map(Vec::len), Some(3));
    Ok(())
}

#[tokio::test]
async fn a_retry_is_found_again_after_the_store_is_reopened() -> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.write"], &["assets.write"]).await?;
    let first = upload_ok(&harness, &frame, "retry-0001", 0, NOTES.as_bytes()).await?;

    // The server restarts between the commit and the retry, so only the stored
    // key can tell the retry from a new upload. The page connects again, grants
    // again and issues the same view again.
    let restarted = Harness::at(harness.directory.clone(), false);
    let session = open_session(&restarted).await?;
    let granted = restarted
        .send(grant_request(&session, &frame.id, 0, &["assets.write"])?)
        .await?;
    assert_eq!(granted.status, StatusCode::OK, "{}", granted.code());
    let connected = Frame {
        session,
        ..frame.clone()
    };
    let view = issue(&restarted, &connected, 1).await?;
    let connected = Frame {
        initialization: secret_of(&view)?,
        ..connected
    };

    let retried = upload_ok(&restarted, &connected, "retry-0001", 0, NOTES.as_bytes()).await?;
    assert_eq!(
        retried["replayed"], true,
        "the stored key answers the retry"
    );
    assert_eq!(retried["asset_version"], 1);
    assert_eq!(retried["asset"], first["asset"], "the first association");
    let listing = owner_listing(&restarted, &frame.id).await?;
    assert_eq!(
        listing["assets"].as_array().map(Vec::len),
        Some(1),
        "no second association"
    );
    Ok(())
}

#[tokio::test]
async fn the_view_keeps_working_after_an_upload_because_the_head_did_not_move() -> TestResult {
    let harness = Harness::new();
    let scope = ["source.read", "assets.read", "assets.write"];
    let frame = granted_frame(&harness, &scope, &scope).await?;
    upload_ok(&harness, &frame, "retry-0001", 0, NOTES.as_bytes()).await?;

    // The upload moved the asset version, not the source head: the view still
    // reads its revision.
    let source = harness.send(source_list(&frame)?).await?;
    assert_eq!(source.status, StatusCode::OK, "{}", source.code());
    assert_eq!(
        source.json()?["revision"],
        1,
        "the view's revision is still the head"
    );
    let listed = harness.send(assets_list(&frame)?).await?;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.code());
    assert_eq!(listed.json()?["asset_version"], 1);

    // The view writes again, at the version the frame now holds.
    let second = upload_ok(&harness, &frame, "retry-0002", 1, OTHER.as_bytes()).await?;
    assert_eq!(second["asset_version"], 2);
    assert_eq!(second["replayed"], false);
    Ok(())
}

#[tokio::test]
async fn a_stale_base_is_an_asset_conflict_with_the_current_version_and_leaves_no_receipt()
-> TestResult {
    let harness = Harness::new();
    let frame = granted_frame(&harness, &["assets.write"], &["assets.write"]).await?;
    let asset =
        uploaded_id(&upload_ok(&harness, &frame, "retry-0001", 0, NOTES.as_bytes()).await?)?;

    // A frame that has not seen that upload still says 0.
    let stale = harness
        .send(upload_request(&frame, "retry-0002", 0, OTHER.as_bytes())?)
        .await?;
    assert_eq!(stale.status, StatusCode::CONFLICT, "{}", stale.code());
    assert_eq!(stale.code(), "asset_conflict");
    assert_eq!(stale.json()?["error"]["current_asset_version"], 1);

    // A stale delete is refused the same way and removes nothing.
    let stale_delete = harness
        .send(delete_request(&frame, "retry-0003", 0, &asset)?)
        .await?;
    assert_eq!(
        stale_delete.status,
        StatusCode::CONFLICT,
        "{}",
        stale_delete.code()
    );
    assert_eq!(stale_delete.code(), "asset_conflict");
    assert_eq!(stale_delete.json()?["error"]["current_asset_version"], 1);

    let listing = owner_listing(&harness, &frame.id).await?;
    assert_eq!(listing["asset_version"], 1);
    assert_eq!(
        listing["assets"].as_array().map(Vec::len),
        Some(1),
        "the refused delete removed nothing"
    );

    // The refused identity left no receipt: the same identity at the current base commits.
    let retried = upload_ok(&harness, &frame, "retry-0002", 1, OTHER.as_bytes()).await?;
    assert_eq!(
        retried["replayed"], false,
        "a refused call is not a receipt"
    );
    assert_eq!(retried["asset_version"], 2);
    Ok(())
}
