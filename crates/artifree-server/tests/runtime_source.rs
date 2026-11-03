//! Runtime source reads and downloads over HTTP (decision 0032), exercised
//! in process through the real routers, application core, and `SQLite`
//! store. The workspace page proxies these calls for a frame, so each test
//! plays the page: it holds the account token and the view's secrets.
use std::path::{Path, PathBuf};
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
const VIEW: &str = "0123456789abcdef0123456789abcdef";
const MAX_PART: u64 = 1_048_576;

fn plentiful() -> RequestLimits {
    RequestLimits {
        burst: 100_000,
        per_minute: 6_000_000,
        concurrent: 64,
    }
}

struct Harness {
    directory: PathBuf,
    owns_directory: bool,
    api: Router,
    content: Router,
}

impl Harness {
    /// Frame limits far above any test's traffic; the budget tests choose
    /// their own.
    fn new() -> Self {
        Self::with_limits(RequestLimits::default(), plentiful())
    }

    fn with_limits(limits: RequestLimits, frame_limits: RequestLimits) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!(
            "artifree-runtime-source-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory)
            .unwrap_or_else(|error| panic!("cannot create test directory: {error}"));
        Self::at(directory, true, limits, frame_limits)
    }

    /// A server process over `directory`: a second one is a restart.
    fn at(
        directory: PathBuf,
        owns_directory: bool,
        limits: RequestLimits,
        frame_limits: RequestLimits,
    ) -> Self {
        let store = SqliteStore::open(&directory.join("db.sqlite3"))
            .unwrap_or_else(|error| panic!("cannot open store: {error}"));
        let token_store = SqliteTokenStore::open(&directory.join("db.sqlite3"))
            .unwrap_or_else(|error| panic!("cannot open token store: {error}"));
        let token = AccessToken::parse(TOKEN).unwrap_or_else(|| panic!("fixture token is invalid"));
        let state = AppState::new(
            store,
            token_store,
            token,
            Settings {
                content_origin: CONTENT_ORIGIN.into(),
                preview_ttl: Duration::from_secs(60),
                frame_ancestors: "http://127.0.0.1:*".into(),
                limits,
                frame_limits,
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

    async fn send(&self, request: Request<Body>) -> Result<Reply, Box<dyn std::error::Error>> {
        Reply::from(self.api.clone().oneshot(request).await?).await
    }

    async fn content(&self, url: &str) -> Result<Reply, Box<dyn std::error::Error>> {
        let path = url
            .strip_prefix(CONTENT_ORIGIN)
            .ok_or("preview URL uses another origin")?;
        let request = Request::get(path).body(Body::empty())?;
        Reply::from(self.content.clone().oneshot(request).await?).await
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
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}

impl Reply {
    async fn from(response: axum::response::Response) -> Result<Self, Box<dyn std::error::Error>> {
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), usize::MAX).await?.to_vec();
        Ok(Self {
            status,
            headers,
            body,
        })
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

    fn header(&self, name: header::HeaderName) -> &str {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
    }
}

fn authorized(builder: axum::http::request::Builder) -> axum::http::request::Builder {
    builder.header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
}

fn post_raw(path: &str, body: String) -> Result<Request<Body>, axum::http::Error> {
    authorized(Request::post(path))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
}

fn post(path: &str, body: &Value) -> Result<Request<Body>, axum::http::Error> {
    post_raw(path, body.to_string())
}

fn post_keyed(path: &str, key: &str, body: &Value) -> Result<Request<Body>, axum::http::Error> {
    authorized(Request::post(path))
        .header(header::CONTENT_TYPE, "application/json")
        .header("idempotency-key", key)
        .body(Body::from(body.to_string()))
}

fn get(path: &str) -> Result<Request<Body>, axum::http::Error> {
    authorized(Request::get(path)).body(Body::empty())
}

/// Text with multi-byte characters, so a byte cut lands inside one.
fn notes() -> String {
    "naïve café — 日本語 🙂\n".repeat(40)
}

/// Every byte value several times: not UTF-8.
fn binary() -> Vec<u8> {
    (0..=255_u8).cycle().take(2_600).collect()
}

const ENTRY: &str =
    "<!doctype html><html lang=\"en\"><head><!-- artifree-runtime --></head><body>x</body></html>";

fn manifest(capabilities: &[&str]) -> String {
    json!({"sdk": "artifree.app.v1", "capabilities": capabilities}).to_string()
}

async fn create_artifact(
    harness: &Harness,
    capabilities: &[&str],
) -> Result<String, Box<dyn std::error::Error>> {
    let body = json!({
        "title": "Source probe",
        "files": [
            {"path": "index.html", "encoding": "utf8", "content": ENTRY},
            {"path": "artifree-runtime.json", "encoding": "utf8", "content": manifest(capabilities)},
            {"path": "notes.txt", "encoding": "utf8", "content": notes()},
            {"path": "assets/blob.png", "encoding": "base64", "content": BASE64.encode(binary())},
            {"path": "empty.txt", "encoding": "utf8", "content": ""}
        ]
    });
    let reply = harness
        .send(post_keyed("/v1/artifacts", "create-source-1", &body)?)
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

fn grant_request(
    session: &str,
    artifact: &str,
    head: u64,
    expected_version: u64,
    capabilities: &[&str],
) -> Result<Request<Body>, axum::http::Error> {
    authorized(Request::put(format!(
        "/v1/runtime-sessions/{session}/artifacts/{artifact}/revisions/1/grant"
    )))
    .header(header::CONTENT_TYPE, "application/json")
    .body(Body::from(
        json!({"head_revision": head, "expected_version": expected_version, "capabilities": capabilities})
            .to_string(),
    ))
}

/// A view of revision 1 whose document the page already served, the state
/// in which a frame's calls are accepted.
struct Frame {
    id: String,
    session: String,
    initialization: String,
    generation: u64,
    grant_version: u64,
}

impl Frame {
    fn route(&self, tail: &str) -> String {
        format!(
            "/v1/artifacts/{}/revisions/1/runtime-views/{VIEW}/{}/{tail}",
            self.id, self.generation
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

async fn frame(
    harness: &Harness,
    declared: &[&str],
    granted: &[&str],
) -> Result<Frame, Box<dyn std::error::Error>> {
    let id = create_artifact(harness, declared).await?;
    let session = open_session(harness).await?;
    let mut frame = Frame {
        id,
        session,
        initialization: String::new(),
        generation: 1,
        grant_version: 0,
    };
    let reply = harness
        .send(grant_request(&frame.session, &frame.id, 1, 0, granted)?)
        .await?;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.code());
    frame.grant_version = reply.json()?["version"].as_u64().ok_or("version")?;
    let view = issue(harness, &frame, 1).await?;
    assert_eq!(
        view["granted"].as_array().map(Vec::len),
        Some(granted.len())
    );
    view["initialization"]
        .as_str()
        .ok_or("init")?
        .clone_into(&mut frame.initialization);
    Ok(frame)
}

async fn issue(
    harness: &Harness,
    frame: &Frame,
    generation: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    let created = harness
        .send(post(
            &format!("/v1/artifacts/{}/revisions/1/runtime-views", frame.id),
            &json!({"session_id": frame.session, "view_id": VIEW, "generation": generation}),
        )?)
        .await?;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.code());
    let view = created.json()?;
    let served = harness
        .content(view["url"].as_str().ok_or("missing url")?)
        .await?;
    assert_eq!(served.status, StatusCode::OK);
    Ok(view)
}

async fn read_part(
    harness: &Harness,
    frame: &Frame,
    path: &str,
    offset: u64,
    length: u64,
) -> Result<Reply, Box<dyn std::error::Error>> {
    harness
        .send(post(
            &frame.route("source/read"),
            &frame.body(json!({"path": path, "offset": offset, "length": length})),
        )?)
        .await
}

fn decode(part: &Value) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    Ok(BASE64.decode(part["content"].as_str().ok_or("content")?)?)
}

/// Reads a whole file in parts of `length` bytes, checking each part the way
/// a client must: its bytes number what its offsets say.
async fn read_all(
    harness: &Harness,
    frame: &Frame,
    path: &str,
    length: u64,
) -> Result<(Vec<u8>, Value), Box<dyn std::error::Error>> {
    let mut joined = Vec::new();
    let mut offset = 0_u64;
    loop {
        let reply = read_part(harness, frame, path, offset, length).await?;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.code());
        let part = reply.json()?;
        let bytes = decode(&part)?;
        assert_eq!(part["path"], path);
        assert_eq!(part["offset"], offset);
        let size = part["size"].as_u64().ok_or("size")?;
        joined.extend_from_slice(&bytes);
        if let Some(next) = part["next_offset"].as_u64() {
            assert!(next > offset && next < size, "{part}");
            assert_eq!(bytes.len() as u64, next - offset);
            offset = next;
        } else {
            assert!(part["next_offset"].is_null());
            assert_eq!(bytes.len() as u64, size - offset);
            return Ok((joined, part));
        }
    }
}

#[tokio::test]
async fn a_granted_view_lists_and_reads_the_bytes_of_its_pinned_revision() -> TestResult {
    let harness = Harness::new();
    let frame = frame(
        &harness,
        &["source.read", "source.download"],
        &["source.read"],
    )
    .await?;
    let listing = harness
        .send(post(&frame.route("source/list"), &frame.body(json!({})))?)
        .await?;
    assert_eq!(listing.status, StatusCode::OK, "{}", listing.code());
    let listing = listing.json()?;
    assert_eq!(listing["revision"], 1);
    let files = listing["files"].as_array().ok_or("files")?;
    let paths: Vec<&str> = files.iter().filter_map(|f| f["path"].as_str()).collect();
    assert_eq!(
        paths,
        [
            "artifree-runtime.json",
            "assets/blob.png",
            "empty.txt",
            "index.html",
            "notes.txt"
        ]
    );
    let entry = |path: &str| files.iter().find(|f| f["path"] == path).cloned();
    let blob = entry("assets/blob.png").ok_or("blob")?;
    assert_eq!(blob["size"], binary().len());

    // Text is cut inside characters at a length of 7; the parts still join.
    let (text, last) = read_all(&harness, &frame, "notes.txt", 7).await?;
    assert_eq!(text, notes().into_bytes());
    assert_eq!(last["sha256"], entry("notes.txt").ok_or("notes")?["sha256"]);
    let (bytes, last) = read_all(&harness, &frame, "assets/blob.png", 500).await?;
    assert_eq!(bytes, binary());
    assert_eq!(last["sha256"], blob["sha256"]);
    // One part for the whole file, and the largest part allowed.
    let (whole, _) = read_all(&harness, &frame, "notes.txt", MAX_PART).await?;
    assert_eq!(whole, notes().into_bytes());

    // An empty file has one empty final part; so does the offset at the end.
    let empty = read_part(&harness, &frame, "empty.txt", 0, 10).await?;
    assert_eq!(empty.status, StatusCode::OK, "{}", empty.code());
    let empty = empty.json()?;
    assert_eq!(
        (
            empty["size"].clone(),
            empty["content"].clone(),
            empty["next_offset"].clone()
        ),
        (json!(0), json!(""), Value::Null)
    );
    let size = notes().len() as u64;
    let at_end = read_part(&harness, &frame, "notes.txt", size, 10)
        .await?
        .json()?;
    assert_eq!(at_end["content"], "");
    assert!(at_end["next_offset"].is_null());
    Ok(())
}

#[tokio::test]
async fn a_read_refuses_what_it_cannot_serve_with_fixed_answers() -> TestResult {
    let harness = Harness::new();
    let frame = frame(&harness, &["source.read"], &["source.read"]).await?;
    let size = notes().len() as u64;
    let past = read_part(&harness, &frame, "notes.txt", size + 1, 10).await?;
    assert_eq!(past.status, StatusCode::RANGE_NOT_SATISFIABLE);
    for (path, offset, length, status) in [
        ("notes.txt", 0, 0, StatusCode::BAD_REQUEST),
        ("notes.txt", 0, MAX_PART + 1, StatusCode::BAD_REQUEST),
        (
            "notes.txt",
            9_007_199_254_740_992,
            10,
            StatusCode::BAD_REQUEST,
        ),
        ("../notes.txt", 0, 10, StatusCode::UNPROCESSABLE_ENTITY),
        ("Notes.txt", 0, 10, StatusCode::NOT_FOUND),
        ("absent.txt", 0, 10, StatusCode::NOT_FOUND),
    ] {
        let reply = read_part(&harness, &frame, path, offset, length).await?;
        assert_eq!(reply.status, status, "{path} {offset} {length}");
        let message = reply.json()?["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        assert!(
            !message.contains("notes") && !message.contains("absent"),
            "an error never echoes what the frame supplied: {message}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn each_capability_and_the_binding_are_checked_on_every_call() -> TestResult {
    let harness = Harness::new();
    let frame = granted_download_and_metadata(&harness).await?;
    // Downloading and metadata were granted, reading was not.
    let no_read = harness.send(list(&frame)?).await?;
    assert_eq!(no_read.status, StatusCode::FORBIDDEN);
    assert_eq!(no_read.code(), "forbidden");
    assert_eq!(
        read_part(&harness, &frame, "notes.txt", 0, 1).await?.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(harness.send(offer(&frame)?).await?.status, StatusCode::OK);

    // A forged secret, an old generation and a wrong view are bindings that
    // do not exist: 404, however the grant stands.
    let forged = Frame {
        initialization: "0".repeat(64),
        ..clone(&frame)
    };
    assert_eq!(
        harness.send(list(&forged)?).await?.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        harness.send(offer(&forged)?).await?.status,
        StatusCode::NOT_FOUND
    );
    let other_generation = Frame {
        generation: 2,
        ..clone(&frame)
    };
    assert_eq!(
        harness.send(list(&other_generation)?).await?.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        harness
            .send(post(
                &frame
                    .route("source/list")
                    .replace(VIEW, "fedcba9876543210fedcba9876543210"),
                &frame.body(json!({})),
            )?)
            .await?
            .status,
        StatusCode::NOT_FOUND
    );

    Ok(())
}

async fn granted_download_and_metadata(
    harness: &Harness,
) -> Result<Frame, Box<dyn std::error::Error>> {
    let all = ["metadata.read", "source.download", "source.read"];
    frame(harness, &all, &["metadata.read", "source.download"]).await
}

fn list(frame: &Frame) -> Result<Request<Body>, axum::http::Error> {
    post(&frame.route("source/list"), &frame.body(json!({})))
}

fn offer(frame: &Frame) -> Result<Request<Body>, axum::http::Error> {
    post(
        &frame.route("source/offer"),
        &frame.body(json!({"kind": "archive", "path": null})),
    )
}

#[tokio::test]
async fn a_replaced_grant_and_a_closed_connection_end_the_views_they_covered() -> TestResult {
    let harness = Harness::new();
    let frame = granted_download_and_metadata(&harness).await?;
    // The owner replaces the grant: reading only. The old view is bound to
    // the old grant version, so nothing it could do before works now.
    let regrant = harness
        .send(grant_request(
            &frame.session,
            &frame.id,
            1,
            frame.grant_version,
            &["source.read"],
        )?)
        .await?;
    assert_eq!(regrant.status, StatusCode::OK, "{}", regrant.code());
    assert_eq!(
        harness.send(list(&frame)?).await?.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        harness.send(offer(&frame)?).await?.status,
        StatusCode::FORBIDDEN
    );

    // A second generation sees the new grant: reading works, downloading
    // does not, and the first generation stays dead.
    let second = Frame {
        generation: 2,
        grant_version: regrant.json()?["version"].as_u64().ok_or("version")?,
        ..clone(&frame)
    };
    let view = issue(&harness, &second, 2).await?;
    let second = Frame {
        initialization: view["initialization"].as_str().ok_or("init")?.to_owned(),
        ..second
    };
    assert_eq!(harness.send(list(&second)?).await?.status, StatusCode::OK);
    assert_eq!(
        harness.send(offer(&second)?).await?.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        harness.send(list(&frame)?).await?.status,
        StatusCode::NOT_FOUND
    );

    // Revoking everything makes the matching binding a missing capability.
    let cleared = harness
        .send(grant_request(
            &second.session,
            &second.id,
            1,
            second.grant_version,
            &[],
        )?)
        .await?;
    assert_eq!(cleared.status, StatusCode::OK);
    assert_eq!(
        harness.send(list(&second)?).await?.status,
        StatusCode::FORBIDDEN
    );
    // And closing the connection ends the views with it.
    let closed = harness
        .send(
            authorized(Request::delete(format!(
                "/v1/runtime-sessions/{}",
                second.session
            )))
            .body(Body::empty())?,
        )
        .await?;
    assert_eq!(closed.status, StatusCode::NO_CONTENT);
    assert_eq!(
        harness.send(list(&second)?).await?.status,
        StatusCode::NOT_FOUND
    );
    Ok(())
}

fn clone(frame: &Frame) -> Frame {
    Frame {
        id: frame.id.clone(),
        session: frame.session.clone(),
        initialization: frame.initialization.clone(),
        generation: frame.generation,
        grant_version: frame.grant_version,
    }
}

#[tokio::test]
async fn an_undeclared_capability_cannot_be_granted() -> TestResult {
    let harness = Harness::new();
    let id = create_artifact(&harness, &["source.read"]).await?;
    let session = open_session(&harness).await?;
    for capabilities in [
        vec!["source.download"],
        vec!["source.read", "source.download"],
        vec!["source.read", "source.read"],
        vec!["source.write"],
    ] {
        let reply = harness
            .send(grant_request(&session, &id, 1, 0, &capabilities)?)
            .await?;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{capabilities:?}"
        );
    }
    let ok = harness
        .send(grant_request(&session, &id, 1, 0, &["source.read"])?)
        .await?;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.code());
    Ok(())
}

async fn all_calls(
    harness: &Harness,
    frame: &Frame,
) -> Result<Vec<Reply>, Box<dyn std::error::Error>> {
    let file = json!({"kind": "file", "path": "notes.txt"});
    Ok(vec![
        harness
            .send(post(&frame.route("source/list"), &frame.body(json!({})))?)
            .await?,
        read_part(harness, frame, "notes.txt", 0, 10).await?,
        harness
            .send(post(
                &frame.route("source/offer"),
                &frame.body(file.clone()),
            )?)
            .await?,
        harness
            .send(post(&frame.route("source/download"), &frame.body(file))?)
            .await?,
    ])
}

#[tokio::test]
async fn a_view_is_stale_once_the_head_moves_past_its_pinned_revision() -> TestResult {
    let harness = Harness::new();
    let frame = frame(
        &harness,
        &["source.read", "source.download"],
        &["source.read", "source.download"],
    )
    .await?;
    for reply in all_calls(&harness, &frame).await? {
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.code());
    }
    let saved = harness
        .send(post_keyed(
            &format!("/v1/artifacts/{}/revisions", frame.id),
            "source-save-1",
            &json!({
                "base_revision": 1,
                "changes": [{"op": "put", "path": "notes.txt", "encoding": "utf8", "content": "new"}]
            }),
        )?)
        .await?;
    assert_eq!(saved.status, StatusCode::CREATED, "{}", saved.code());
    for reply in all_calls(&harness, &frame).await? {
        assert_eq!(reply.status, StatusCode::CONFLICT);
        assert_eq!(reply.code(), "revision_conflict");
        assert_eq!(reply.json()?["error"]["current_revision"], 2);
    }
    Ok(())
}

#[tokio::test]
async fn a_trashed_artifact_answers_as_an_unknown_one() -> TestResult {
    let harness = Harness::new();
    let frame = frame(
        &harness,
        &["source.read", "source.download"],
        &["source.read", "source.download"],
    )
    .await?;
    let trashed = harness
        .send(
            authorized(Request::patch(format!("/v1/artifacts/{}", frame.id)))
                .header(header::CONTENT_TYPE, "application/json")
                .header("idempotency-key", "source-trash-1")
                .body(Body::from(
                    json!({"base_metadata_revision": 1, "deleted": true}).to_string(),
                ))?,
        )
        .await?;
    assert_eq!(trashed.status, StatusCode::OK, "{}", trashed.code());
    for reply in all_calls(&harness, &frame).await? {
        assert_eq!(reply.status, StatusCode::NOT_FOUND);
    }
    Ok(())
}

#[tokio::test]
async fn a_download_is_the_stored_file_or_the_export_of_the_pinned_revision() -> TestResult {
    let harness = Harness::new();
    let frame = frame(
        &harness,
        &["source.read", "source.download"],
        &["source.download"],
    )
    .await?;
    let offer = |body: Value| post(&frame.route("source/offer"), &frame.body(body));
    let download = |body: Value| post(&frame.route("source/download"), &frame.body(body));

    let file = json!({"kind": "file", "path": "assets/blob.png"});
    let offered = harness.send(offer(file.clone())?).await?;
    assert_eq!(offered.status, StatusCode::OK, "{}", offered.code());
    let offered = offered.json()?;
    assert_eq!(offered["file_name"], "blob.png");
    assert_eq!(offered["size"], binary().len());
    assert_eq!(offered["path"], "assets/blob.png");
    assert_eq!(offered["sha256"].as_str().map(str::len), Some(64));

    let stored = harness.send(download(file)?).await?;
    assert_eq!(stored.status, StatusCode::OK, "{}", stored.code());
    assert_eq!(stored.body, binary());
    // A closed content type whatever the file's own, and no way to render it.
    assert_eq!(
        stored.header(header::CONTENT_TYPE),
        "application/octet-stream"
    );
    assert_eq!(
        stored.header(header::CONTENT_DISPOSITION),
        "attachment; filename=\"blob.png\""
    );
    assert_eq!(stored.header(header::X_CONTENT_TYPE_OPTIONS), "nosniff");
    assert_eq!(
        stored.header(header::CONTENT_SECURITY_POLICY),
        "sandbox; default-src 'none'"
    );
    assert_eq!(stored.header(header::CACHE_CONTROL), "no-store");

    let archive_request = json!({"kind": "archive", "path": null});
    let offered = harness
        .send(offer(archive_request.clone())?)
        .await?
        .json()?;
    assert_eq!(offered["kind"], "archive");
    assert_eq!(offered["file_name"], format!("{}-r1.zip", frame.id));
    assert!(offered["size"].is_null() && offered["path"].is_null() && offered["sha256"].is_null());
    let archive = harness.send(download(archive_request)?).await?;
    assert_eq!(archive.status, StatusCode::OK, "{}", archive.code());
    assert_eq!(archive.header(header::CONTENT_TYPE), "application/zip");
    assert_eq!(
        archive.header(header::CONTENT_DISPOSITION),
        format!("attachment; filename=\"{}-r1.zip\"", frame.id)
    );
    assert_eq!(archive.header(header::X_CONTENT_TYPE_OPTIONS), "nosniff");
    let exported = harness
        .send(get(&format!(
            "/v1/artifacts/{}/revisions/1/export",
            frame.id
        ))?)
        .await?;
    assert_eq!(archive.body, exported.body, "the archive is the A07 export");

    // A target must be exactly one thing, and it must exist.
    for (body, status) in [
        (
            json!({"kind": "file", "path": null}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"kind": "archive", "path": "notes.txt"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"kind": "folder", "path": null}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"kind": "file", "path": "absent.txt"}),
            StatusCode::NOT_FOUND,
        ),
    ] {
        for reply in [
            harness.send(offer(body.clone())?).await?,
            harness.send(download(body.clone())?).await?,
        ] {
            assert_eq!(reply.status, status, "{body}");
        }
    }
    // Only the explicit POST reaches the bytes: no link can.
    let navigation = harness.send(get(&frame.route("source/download"))?).await?;
    assert_eq!(navigation.status, StatusCode::METHOD_NOT_ALLOWED);
    Ok(())
}

#[tokio::test]
async fn downloading_needs_its_own_consent_not_the_one_for_reading() -> TestResult {
    let harness = Harness::new();
    let frame = frame(
        &harness,
        &["source.read", "source.download"],
        &["source.read"],
    )
    .await?;
    for tail in ["source/offer", "source/download"] {
        let reply = harness
            .send(post(
                &frame.route(tail),
                &frame.body(json!({"kind": "archive", "path": null})),
            )?)
            .await?;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{tail}");
    }
    Ok(())
}

#[tokio::test]
async fn hostile_frame_bodies_get_fixed_client_errors_never_a_server_error() -> TestResult {
    let harness = Harness::new();
    let frame = frame(
        &harness,
        &["metadata.read", "source.read", "source.download"],
        &["metadata.read", "source.read", "source.download"],
    )
    .await?;
    let init = &frame.initialization;
    let deep = format!("{}1{}", "[".repeat(5_000), "]".repeat(5_000));
    let bodies = [
        String::new(),
        "null".to_owned(),
        "[]".to_owned(),
        "\"x\"".to_owned(),
        "{".to_owned(),
        format!("{{\"initialization\":\"{init}\",\"initialization\":\"{init}\"}}"),
        format!("{{\"initialization\":\"{init}\",\"extra\":1}}"),
        format!("{{\"initialization\":{deep}}}"),
        "{\"initialization\":1}".to_owned(),
        format!(
            "{{\"initialization\":\"{init}\",\"path\":[\"a\"],\"offset\":-1,\"length\":1.5,\"kind\":7}}"
        ),
        format!(
            "{{\"initialization\":\"{init}\",\"path\":\"notes.txt\",\"offset\":0,\"length\":1,\"offset\":2}}"
        ),
        format!(
            "{{\"initialization\":\"{init}\",\"path\":\"{}\",\"offset\":0,\"length\":1}}",
            "a".repeat(40_000)
        ),
        format!("{{\"initialization\":\"{}\"}}", "0".repeat(40_000)),
    ];
    for tail in [
        "metadata",
        "source/list",
        "source/read",
        "source/offer",
        "source/download",
    ] {
        for body in &bodies {
            let reply = harness
                .send(post_raw(&frame.route(tail), body.clone())?)
                .await?;
            assert!(
                reply.status.is_client_error(),
                "{tail}: {} for {}",
                reply.status,
                &body[..body.len().min(60)]
            );
            // Closed vocabulary: a contract error code, no frame text.
            let code = reply.code();
            assert!(
                ["invalid_request", "payload_too_large"].contains(&code.as_str()),
                "{tail}: {code}"
            );
            let message = reply.json()?["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            assert!(
                [
                    "the frame call is not a valid request",
                    "the frame call is too large"
                ]
                .contains(&message.as_str()),
                "{tail}: a fixed message, never the frame's text: {message}"
            );
        }
    }
    // The same bodies did not disturb the view.
    let still = harness
        .send(post(&frame.route("source/list"), &frame.body(json!({})))?)
        .await?;
    assert_eq!(still.status, StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn a_frame_that_floods_cannot_use_up_the_owners_budget() -> TestResult {
    let harness = Harness::with_limits(
        RequestLimits::default(),
        RequestLimits {
            burst: 3,
            per_minute: 60,
            concurrent: 4,
        },
    );
    let frame = frame(&harness, &["source.read"], &["source.read"]).await?;
    let mut limited = 0;
    for _ in 0..20 {
        let reply = harness
            .send(post(&frame.route("source/list"), &frame.body(json!({})))?)
            .await?;
        if reply.status == StatusCode::TOO_MANY_REQUESTS {
            limited += 1;
            assert_eq!(reply.code(), "rate_limited");
            assert!(!reply.header(header::RETRY_AFTER).is_empty());
        }
    }
    assert!(limited >= 10, "the frame budget must bind: {limited}");
    // The owner's own requests, and the revoke of the flooding frame's grant,
    // are untouched.
    let shown = harness
        .send(get(&format!("/v1/artifacts/{}", frame.id))?)
        .await?;
    assert_eq!(shown.status, StatusCode::OK);
    let revoked = harness
        .send(grant_request(
            &frame.session,
            &frame.id,
            1,
            frame.grant_version,
            &[],
        )?)
        .await?;
    assert_eq!(revoked.status, StatusCode::OK, "{}", revoked.code());
    // Metadata is frame traffic as well.
    let metadata = harness
        .send(post(&frame.route("metadata"), &frame.body(json!({})))?)
        .await?;
    assert_eq!(metadata.status, StatusCode::TOO_MANY_REQUESTS);
    Ok(())
}

#[tokio::test]
async fn the_owners_budget_does_not_spend_the_frames() -> TestResult {
    let harness = Harness::with_limits(
        RequestLimits {
            burst: 40,
            per_minute: 60,
            concurrent: 16,
        },
        plentiful(),
    );
    let frame = frame(&harness, &["source.read"], &["source.read"]).await?;
    let mut refused = 0;
    for _ in 0..80 {
        let reply = harness
            .send(get(&format!("/v1/artifacts/{}", frame.id))?)
            .await?;
        if reply.status == StatusCode::TOO_MANY_REQUESTS {
            refused += 1;
        }
    }
    assert!(refused > 0, "the owner budget must be spent for this test");
    let listing = harness
        .send(post(&frame.route("source/list"), &frame.body(json!({})))?)
        .await?;
    assert_eq!(listing.status, StatusCode::OK, "{}", listing.code());
    Ok(())
}

#[tokio::test]
async fn a_restart_ends_every_connection_grant_and_view() -> TestResult {
    let harness = Harness::new();
    let frame = frame(&harness, &["source.read"], &["source.read"]).await?;
    let list = |frame: &Frame| post(&frame.route("source/list"), &frame.body(json!({})));
    assert_eq!(harness.send(list(&frame)?).await?.status, StatusCode::OK);
    let restarted = Harness::at(
        harness.directory.clone(),
        false,
        RequestLimits::default(),
        plentiful(),
    );
    // The stored artifact survived; the in-memory view and grant did not.
    let shown = restarted
        .send(get(&format!("/v1/artifacts/{}", frame.id))?)
        .await?;
    assert_eq!(shown.status, StatusCode::OK);
    assert_eq!(
        restarted.send(list(&frame)?).await?.status,
        StatusCode::NOT_FOUND
    );
    let grant = restarted
        .send(grant_request(
            &frame.session,
            &frame.id,
            1,
            frame.grant_version,
            &["source.read"],
        )?)
        .await?;
    assert_eq!(grant.status, StatusCode::NOT_FOUND);
    assert_eq!(grant.code(), "runtime_session_expired");
    // A fresh connection starts with nothing granted.
    let session = open_session(&restarted).await?;
    let created = restarted
        .send(post(
            &format!("/v1/artifacts/{}/revisions/1/runtime-views", frame.id),
            &json!({"session_id": session, "view_id": VIEW, "generation": 1}),
        )?)
        .await?;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.code());
    assert_eq!(created.json()?["granted"], json!([]));
    let _directory: &Path = &restarted.directory;
    Ok(())
}
