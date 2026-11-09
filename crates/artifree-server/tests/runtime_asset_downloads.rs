//! Runtime `assets.download` over HTTP (decision 0045, rule 12; R09d3),
//! exercised in process through the real routers, application core, and
//! `SQLite` store. The workspace page proxies these calls for a frame, so each
//! test plays the page: it holds the account token and the view's secrets. The
//! owner's upload and revision routes set up what a frame may download.

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
use axum::http::{HeaderMap, Request, StatusCode, header};
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
/// What the test artifact declares, so that each of these may be granted.
const DECLARED: &[&str] = &[
    "assets.download",
    "assets.read",
    "assets.write",
    "source.download",
];
/// Text with multi-byte characters and CRLF line ends, so that a re-encoding or
/// a newline conversion would change its bytes.
const NOTES: &str = "naïve café — 日本語 🙂\r\nsecond line\n";
/// The SHA-256 of the bytes of `NOTES`, as `sha256sum` prints it.
const NOTES_SHA256: &str = "325303f9f5ab0c089175ea242e61b200ab6a706f7220696dd9e59d4cb3cabeeb";
/// A CSV file with a byte-order mark and CRLF line ends.
const CSV: &[u8] = b"\xef\xbb\xbfa,b\r\n1,2\r\n";
/// The SHA-256 of `CSV`, as `sha256sum` prints it.
const CSV_SHA256: &str = "12d55567496e2714a23469371b5888bb9d0fb803d52b770c7c08ced31826d2d9";
/// A one-pixel PNG with a text chunk, in base64 (the fixture of the other runtime tests).
const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAACnRFWHRDb21tZW50AGhpoqJYZgAAAAJwclZ0enrhbIZhAAAADUlEQVR4nGNgaPjPAAADgwGAJwGhsAAAAABJRU5ErkJggg==";
/// The SHA-256 of the decoded `PNG`, as `sha256sum` prints it.
const PNG_SHA256: &str = "3e2841980d3e451678e92cae6fdb667d3d4f3cdec9795fccf19005a3f688fbf7";
/// A baseline JPEG, in base64 (the fixture of the other asset tests).
const JPEG: &str = "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAgGBgcGBQgHBwcJCQgKDBQNDAsLDBkSEw8UHRofHh0aHBwgJC4nICIsIxwcKDcpLDAxNDQ0Hyc5PTgyPC4zNDL/wAALCAAIAAgBAREA/8QAHwAAAQUBAQEBAQEAAAAAAAAAAAECAwQFBgcICQoL/8QAtRAAAgEDAwIEAwUFBAQAAAF9AQIDAAQRBRIhMUEGE1FhByJxFDKBkaEII0KxwRVS0fAkM2JyggkKFhcYGRolJicoKSo0NTY3ODk6Q0RFRkdISUpTVFVWV1hZWmNkZWZnaGlqc3R1dnd4eXqDhIWGh4iJipKTlJWWl5iZmqKjpKWmp6ipqrKztLW2t7i5usLDxMXGx8jJytLT1NXW19jZ2uHi4+Tl5ufo6erx8vP09fb3+Pn6/9oACAEBAAA/ADQVt/A3hJtWli3XDfubOMxlhJOVJQNgjC8EnkcA45wD/9k=";
/// The SHA-256 of the decoded `JPEG`, as `sha256sum` prints it.
const JPEG_SHA256: &str = "6de849fcb1f928693b9890581ee1bc1a5ba7c68f6bd49f602e086b3228a25357";
/// Five mebibytes, the largest asset the store admits (decision 0045).
const FIVE_MIB: u64 = 5 * 1024 * 1024;
/// The SHA-256 of `five_mib()`, as `sha256sum` prints it.
const FIVE_MIB_SHA256: &str = "f75dcb28baeb658cff66ae25717d5a3ab7ee68c251115c1a3d27271f71be3839";
/// Distinct idempotency keys and view identifiers, so that tests can share a process.
static NEXT_KEY: AtomicU32 = AtomicU32::new(1);

fn plentiful() -> RequestLimits {
    RequestLimits {
        burst: 100_000,
        per_minute: 6_000_000,
        concurrent: 64,
    }
}

/// One server process over a fresh directory.
struct Harness {
    directory: PathBuf,
    api: Router,
    content: Router,
}

impl Harness {
    fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!(
            "artifree-runtime-asset-downloads-{}-{}",
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
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Reply {
    async fn collect(response: axum::response::Response) -> Result<Self, Failure> {
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

    /// The error code of a refused call, or the body when it has none.
    fn code(&self) -> String {
        self.json()
            .ok()
            .and_then(|body| body["error"]["code"].as_str().map(str::to_owned))
            .unwrap_or_else(|| {
                format!("<no JSON error: {:?}>", String::from_utf8_lossy(&self.body))
            })
    }

    /// The value of the header `name`, or `""` when it is absent or not text.
    fn header(&self, name: header::HeaderName) -> &str {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
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

/// A frame call whose body is exactly the text `body`, sent with the account token.
fn raw_post(path: &str, body: &str) -> Result<Request<Body>, axum::http::Error> {
    Request::post(path)
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_owned()))
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
        "title": "Asset download probe",
        "files": [
            {"path": "index.html", "encoding": "utf8", "content": ENTRY},
            {"path": "artifree-runtime.json", "encoding": "utf8", "content": manifest(declared)}
        ]
    });
    let reply = harness
        .send(post_keyed(
            "/v1/artifacts",
            &key("create-asset-download"),
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

fn create_view(frame: &Frame, generation: u64) -> Result<Request<Body>, axum::http::Error> {
    post(
        &format!("/v1/artifacts/{}/revisions/1/runtime-views", frame.id),
        &json!({"session_id": frame.session, "view_id": frame.view, "generation": generation}),
    )
}

/// Creates view `generation` of `frame` and serves its document, which a frame
/// may call only after.
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

/// A view of generation 1 on a new connection of `artifact`'s owner, whose
/// grant is `granted` (which may be empty).
async fn frame_on(harness: &Harness, artifact: &str, granted: &[&str]) -> Result<Frame, Failure> {
    let session = open_session(harness).await?;
    let set = harness
        .send(grant_request(&session, artifact, 0, granted)?)
        .await?;
    assert_eq!(set.status, StatusCode::OK, "{}", set.code());
    let mut frame = Frame {
        id: artifact.to_owned(),
        session,
        view: next_view(),
        initialization: String::new(),
        generation: 1,
        grant_version: set.json()?["version"].as_u64().ok_or("grant version")?,
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

/// The offer or the download (`tail`, `assets/offer` or `assets/download`) of
/// `asset` by `frame`.
fn asset_call(tail: &str, frame: &Frame, asset: &str) -> Result<Request<Body>, axum::http::Error> {
    post(&frame.route(tail), &frame.body(json!({"asset_id": asset})))
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

/// The frame's deletion of `asset`, under `retry` at the asset space's version `base`.
fn delete_call(
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

/// The owner's upload of `bytes` as `media`, at the asset space's version
/// `base`, which must be created.
async fn upload(
    harness: &Harness,
    artifact: &str,
    base: u64,
    media: &str,
    bytes: &[u8],
) -> Result<Value, Failure> {
    let reply = harness
        .send(post_keyed(
            &format!("/v1/artifacts/{artifact}/assets"),
            &key("asset-download-upload"),
            &json!({
                "base_asset_version": base,
                "content_type": media,
                "content_base64": BASE64.encode(bytes)
            }),
        )?)
        .await?;
    assert_eq!(
        reply.status,
        StatusCode::CREATED,
        "{media}: {}",
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

/// Saves revision 2 of `artifact` through the owner's route, which moves its
/// head past the revision a view of revision 1 is pinned to.
async fn save_revision_two(harness: &Harness, artifact: &str) -> TestResult {
    let reply = harness
        .send(post_keyed(
            &format!("/v1/artifacts/{artifact}/revisions"),
            &key("asset-download-save"),
            &json!({
                "base_revision": 1,
                "changes": [{"op": "put", "path": "notes.txt", "encoding": "utf8", "content": "new"}]
            }),
        )?)
        .await?;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.code());
    Ok(())
}

/// Five mebibytes of the line `abcdefghij`, repeated: text the validator admits.
fn five_mib() -> Result<Vec<u8>, Failure> {
    Ok(b"abcdefghij\n"
        .iter()
        .copied()
        .cycle()
        .take(usize::try_from(FIVE_MIB)?)
        .collect())
}

/// One refusal to check: its label, the view making the call, the asset it
/// names, and the status and code it must answer on both routes.
type Refusal<'a> = (&'a str, &'a Frame, &'a str, StatusCode, &'a str);

/// Each row's offer and download answer its status and code.
async fn assert_refusals(harness: &Harness, rows: &[Refusal<'_>]) -> TestResult {
    for &(label, frame, asset, status, code) in rows {
        for tail in ["assets/offer", "assets/download"] {
            let reply = harness.send(asset_call(tail, frame, asset)?).await?;
            assert_eq!(reply.status, status, "{label}, {tail}: {}", reply.code());
            assert_eq!(reply.code(), code, "{label}, {tail}");
        }
    }
    Ok(())
}

#[tokio::test]
async fn each_stored_type_offers_then_downloads_its_exact_bytes_under_the_hardened_headers()
-> TestResult {
    let harness = Harness::new();
    let artifact = create_artifact(&harness, DECLARED).await?;
    let frame = frame_on(&harness, &artifact, &["assets.download"]).await?;
    let stored: [(Vec<u8>, &str, &str, &str); 4] = [
        (NOTES.as_bytes().to_vec(), "text/plain", "txt", NOTES_SHA256),
        (CSV.to_vec(), "text/csv", "csv", CSV_SHA256),
        (BASE64.decode(PNG)?, "image/png", "png", PNG_SHA256),
        (BASE64.decode(JPEG)?, "image/jpeg", "jpg", JPEG_SHA256),
    ];
    for (index, (bytes, media, extension, digest)) in stored.iter().enumerate() {
        // Each upload steps the asset space by one, so upload `index` is at base `index`.
        let base = u64::try_from(index)?;
        let id = uploaded_id(&upload(&harness, &artifact, base, media, bytes).await?)?;
        let name = format!("asset-{id}.{extension}");

        let offered = harness
            .send(asset_call("assets/offer", &frame, &id)?)
            .await?;
        assert_eq!(
            offered.status,
            StatusCode::OK,
            "{media}: {}",
            offered.code()
        );
        assert_eq!(
            offered.json()?,
            json!({
                "asset_id": id,
                "file_name": name,
                "size": bytes.len(),
                "sha256": digest,
            }),
            "{media}: the offer names the asset, its size and its digest"
        );

        let downloaded = harness
            .send(asset_call("assets/download", &frame, &id)?)
            .await?;
        assert_eq!(
            downloaded.status,
            StatusCode::OK,
            "{media}: {}",
            downloaded.code()
        );
        assert_eq!(downloaded.body, *bytes, "{media}: the stored bytes");
        assert_eq!(
            downloaded.header(header::CONTENT_TYPE),
            "application/octet-stream",
            "{media}: a closed content type whatever the stored one"
        );
        assert_eq!(
            downloaded.header(header::CONTENT_DISPOSITION),
            format!("attachment; filename=\"{name}\""),
            "{media}"
        );
        assert_eq!(downloaded.header(header::X_CONTENT_TYPE_OPTIONS), "nosniff");
        assert_eq!(
            downloaded.header(header::CONTENT_SECURITY_POLICY),
            "sandbox; default-src 'none'"
        );
        assert_eq!(downloaded.header(header::CACHE_CONTROL), "no-store");
    }
    Ok(())
}

#[tokio::test]
async fn a_five_mebibyte_asset_offers_and_downloads_intact() -> TestResult {
    let harness = Harness::new();
    let artifact = create_artifact(&harness, DECLARED).await?;
    let frame = frame_on(&harness, &artifact, &["assets.download"]).await?;
    let bytes = five_mib()?;
    let id = uploaded_id(&upload(&harness, &artifact, 0, "text/plain", &bytes).await?)?;

    let offered = harness
        .send(asset_call("assets/offer", &frame, &id)?)
        .await?;
    assert_eq!(offered.status, StatusCode::OK, "{}", offered.code());
    let offer = offered.json()?;
    assert_eq!(offer["size"], FIVE_MIB, "the offer states the whole size");
    assert_eq!(
        offer["sha256"], FIVE_MIB_SHA256,
        "the offer states the whole digest"
    );

    let downloaded = harness
        .send(asset_call("assets/download", &frame, &id)?)
        .await?;
    assert_eq!(downloaded.status, StatusCode::OK, "{}", downloaded.code());
    assert_eq!(downloaded.body.len(), bytes.len(), "every byte is returned");
    assert!(
        downloaded.body == bytes,
        "the five mebibytes are returned exactly"
    );
    assert_eq!(
        downloaded.header(header::CONTENT_DISPOSITION),
        format!("attachment; filename=\"asset-{id}.txt\"")
    );
    Ok(())
}

#[tokio::test]
async fn a_frame_is_judged_binding_then_grant_then_head_then_asset_id() -> TestResult {
    let harness = Harness::new();
    let artifact = create_artifact(&harness, DECLARED).await?;
    let asset =
        uploaded_id(&upload(&harness, &artifact, 0, "text/plain", NOTES.as_bytes()).await?)?;
    let granted = frame_on(&harness, &artifact, &["assets.download"]).await?;
    let ungranted = frame_on(&harness, &artifact, &["assets.read"]).await?;
    let forged = Frame {
        initialization: "0".repeat(64),
        ..granted.clone()
    };
    let forged_ungranted = Frame {
        initialization: "0".repeat(64),
        ..ungranted.clone()
    };
    let bad = "not-an-asset-id";
    let good = asset.as_str();
    let (not_found, forbidden) = (StatusCode::NOT_FOUND, StatusCode::FORBIDDEN);
    let conflict = StatusCode::CONFLICT;

    // While the head is revision 1: the binding, then the grant, then the id.
    assert_refusals(
        &harness,
        &[
            (
                "a forged view, no download grant, bad id",
                &forged_ungranted,
                bad,
                not_found,
                "not_found",
            ),
            (
                "a forged view, grant, good id",
                &forged,
                good,
                not_found,
                "not_found",
            ),
            (
                "no download grant, good id",
                &ungranted,
                good,
                forbidden,
                "forbidden",
            ),
            (
                "no download grant, bad id",
                &ungranted,
                bad,
                forbidden,
                "forbidden",
            ),
            ("grant, bad id", &granted, bad, not_found, "not_found"),
        ],
    )
    .await?;

    // Once the head has moved: the binding and the grant still come first, and
    // the head comes before the identifier.
    save_revision_two(&harness, &artifact).await?;
    assert_refusals(
        &harness,
        &[
            (
                "a forged view, no download grant, good id",
                &forged_ungranted,
                good,
                not_found,
                "not_found",
            ),
            (
                "a forged view, grant, good id",
                &forged,
                good,
                not_found,
                "not_found",
            ),
            (
                "no download grant, good id",
                &ungranted,
                good,
                forbidden,
                "forbidden",
            ),
            (
                "grant, bad id",
                &granted,
                bad,
                conflict,
                "revision_conflict",
            ),
            (
                "grant, good id",
                &granted,
                good,
                conflict,
                "revision_conflict",
            ),
        ],
    )
    .await
}

#[tokio::test]
async fn a_grant_of_another_capability_is_forbidden_on_both_routes() -> TestResult {
    let harness = Harness::new();
    let artifact = create_artifact(&harness, DECLARED).await?;
    let asset =
        uploaded_id(&upload(&harness, &artifact, 0, "text/plain", NOTES.as_bytes()).await?)?;
    let grants: [&[&str]; 4] = [
        &[],
        &["assets.read"],
        &["assets.write"],
        &["source.download"],
    ];
    for granted in grants {
        let frame = frame_on(&harness, &artifact, granted).await?;
        for tail in ["assets/offer", "assets/download"] {
            let reply = harness.send(asset_call(tail, &frame, &asset)?).await?;
            assert_eq!(
                reply.status,
                StatusCode::FORBIDDEN,
                "{granted:?} on {tail}: {}",
                reply.code()
            );
            assert_eq!(reply.code(), "forbidden", "{granted:?} on {tail}");
        }
    }
    Ok(())
}

#[tokio::test]
async fn an_assets_download_grant_does_not_admit_listing_or_reading() -> TestResult {
    let harness = Harness::new();
    let artifact = create_artifact(&harness, DECLARED).await?;
    let asset =
        uploaded_id(&upload(&harness, &artifact, 0, "text/plain", NOTES.as_bytes()).await?)?;
    let frame = frame_on(&harness, &artifact, &["assets.download"]).await?;
    for (label, request) in [
        ("list", assets_list(&frame)?),
        ("read", assets_read(&frame, &asset, 0, 16)?),
    ] {
        let reply = harness.send(request).await?;
        assert_eq!(
            reply.status,
            StatusCode::FORBIDDEN,
            "{label}: {}",
            reply.code()
        );
        assert_eq!(reply.code(), "forbidden", "{label}");
    }
    // The same view still downloads: the grant is its own consent.
    let offered = harness
        .send(asset_call("assets/offer", &frame, &asset)?)
        .await?;
    assert_eq!(offered.status, StatusCode::OK, "{}", offered.code());
    Ok(())
}

#[tokio::test]
async fn a_deleted_asset_stops_offering_and_downloading() -> TestResult {
    let harness = Harness::new();
    let artifact = create_artifact(&harness, DECLARED).await?;
    let asset =
        uploaded_id(&upload(&harness, &artifact, 0, "text/plain", NOTES.as_bytes()).await?)?;
    let frame = frame_on(&harness, &artifact, &["assets.download", "assets.write"]).await?;
    let offered = harness
        .send(asset_call("assets/offer", &frame, &asset)?)
        .await?;
    assert_eq!(offered.status, StatusCode::OK, "{}", offered.code());
    let downloaded = harness
        .send(asset_call("assets/download", &frame, &asset)?)
        .await?;
    assert_eq!(downloaded.status, StatusCode::OK, "{}", downloaded.code());

    let deleted = harness
        .send(delete_call(&frame, "retry-0001", 1, &asset)?)
        .await?;
    assert_eq!(deleted.status, StatusCode::OK, "{}", deleted.code());
    assert_eq!(deleted.json()?["deleted"], true, "the asset was deleted");

    // Once deleted, neither route releases the asset.
    assert_refusals(
        &harness,
        &[(
            "a deleted asset",
            &frame,
            asset.as_str(),
            StatusCode::NOT_FOUND,
            "not_found",
        )],
    )
    .await
}

#[tokio::test]
async fn a_revoked_download_grant_is_forbidden_on_both_routes() -> TestResult {
    let harness = Harness::new();
    let artifact = create_artifact(&harness, DECLARED).await?;
    let asset =
        uploaded_id(&upload(&harness, &artifact, 0, "text/plain", NOTES.as_bytes()).await?)?;
    let frame = frame_on(&harness, &artifact, &["assets.download"]).await?;
    let before = harness
        .send(asset_call("assets/offer", &frame, &asset)?)
        .await?;
    assert_eq!(before.status, StatusCode::OK, "{}", before.code());

    // The user revokes every capability. The view is still bound, but to the
    // grant version it was issued under.
    let revoked = harness
        .send(grant_request(
            &frame.session,
            &frame.id,
            frame.grant_version,
            &[],
        )?)
        .await?;
    assert_eq!(revoked.status, StatusCode::OK, "{}", revoked.code());
    assert_refusals(
        &harness,
        &[(
            "a revoked grant",
            &frame,
            asset.as_str(),
            StatusCode::FORBIDDEN,
            "forbidden",
        )],
    )
    .await
}

#[tokio::test]
async fn a_moved_source_head_conflicts_on_both_routes() -> TestResult {
    let harness = Harness::new();
    let artifact = create_artifact(&harness, DECLARED).await?;
    let asset =
        uploaded_id(&upload(&harness, &artifact, 0, "text/plain", NOTES.as_bytes()).await?)?;
    let frame = frame_on(&harness, &artifact, &["assets.download"]).await?;
    save_revision_two(&harness, &artifact).await?;
    for tail in ["assets/offer", "assets/download"] {
        let reply = harness.send(asset_call(tail, &frame, &asset)?).await?;
        assert_eq!(
            reply.status,
            StatusCode::CONFLICT,
            "{tail}: {}",
            reply.code()
        );
        assert_eq!(reply.code(), "revision_conflict", "{tail}");
        assert_eq!(
            reply.json()?["error"]["current_revision"],
            2,
            "{tail}: the conflict names the current revision"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_get_on_either_route_is_method_not_allowed() -> TestResult {
    let harness = Harness::new();
    let artifact = create_artifact(&harness, DECLARED).await?;
    let frame = frame_on(&harness, &artifact, &["assets.download"]).await?;
    for tail in ["assets/offer", "assets/download"] {
        let reply = harness.send(get(&frame.route(tail))?).await?;
        assert_eq!(
            reply.status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{tail}: {}",
            reply.code()
        );
        assert_eq!(reply.code(), "invalid_request", "{tail}");
    }
    Ok(())
}

#[tokio::test]
async fn a_malformed_or_extended_body_gets_the_fixed_client_error() -> TestResult {
    let harness = Harness::new();
    let artifact = create_artifact(&harness, DECLARED).await?;
    let asset =
        uploaded_id(&upload(&harness, &artifact, 0, "text/plain", NOTES.as_bytes()).await?)?;
    let frame = frame_on(&harness, &artifact, &["assets.download"]).await?;
    let init = frame.initialization.as_str();
    let bodies = [
        String::new(),
        "{".to_owned(),
        "null".to_owned(),
        "[]".to_owned(),
        format!("{{\"initialization\":\"{init}\",\"asset_id\":\"{asset}\",\"extra\":1}}"),
        format!("{{\"initialization\":\"{init}\"}}"),
        format!("{{\"initialization\":\"{init}\",\"asset_id\":1}}"),
    ];
    for tail in ["assets/offer", "assets/download"] {
        for body in &bodies {
            let reply = harness.send(raw_post(&frame.route(tail), body)?).await?;
            assert_eq!(
                reply.status,
                StatusCode::BAD_REQUEST,
                "{tail} with {body:?}: {}",
                reply.code()
            );
            assert_eq!(reply.code(), "invalid_request", "{tail} with {body:?}");
            assert_eq!(
                reply.json()?["error"]["message"],
                "the frame call is not a valid request",
                "{tail} with {body:?}: the message says nothing of the input"
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn a_hostile_asset_id_is_not_found_on_either_route() -> TestResult {
    let harness = Harness::new();
    let artifact = create_artifact(&harness, DECLARED).await?;
    let asset =
        uploaded_id(&upload(&harness, &artifact, 0, "text/plain", NOTES.as_bytes()).await?)?;
    let frame = frame_on(&harness, &artifact, &["assets.download"]).await?;
    let upper = "A".repeat(64);
    let short = "a".repeat(63);
    let long = "a".repeat(65);
    let not_hex = "g".repeat(64);
    let absent = "b".repeat(64);
    assert_refusals(
        &harness,
        &[
            (
                "an empty id",
                &frame,
                "",
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                "upper-case hex",
                &frame,
                upper.as_str(),
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                "63 digits",
                &frame,
                short.as_str(),
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                "65 digits",
                &frame,
                long.as_str(),
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                "not hex",
                &frame,
                not_hex.as_str(),
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                "a path",
                &frame,
                "../../etc/passwd",
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                "an absent id",
                &frame,
                absent.as_str(),
                StatusCode::NOT_FOUND,
                "not_found",
            ),
        ],
    )
    .await?;
    // The real asset is still there, so the refusals were about the identifiers.
    let offered = harness
        .send(asset_call("assets/offer", &frame, &asset)?)
        .await?;
    assert_eq!(offered.status, StatusCode::OK, "{}", offered.code());
    Ok(())
}

#[tokio::test]
async fn a_download_does_not_spend_the_grant() -> TestResult {
    let harness = Harness::new();
    let artifact = create_artifact(&harness, DECLARED).await?;
    let asset =
        uploaded_id(&upload(&harness, &artifact, 0, "text/plain", NOTES.as_bytes()).await?)?;
    let frame = frame_on(&harness, &artifact, &["assets.download"]).await?;
    for round in 1..=2 {
        let reply = harness
            .send(asset_call("assets/download", &frame, &asset)?)
            .await?;
        assert_eq!(
            reply.status,
            StatusCode::OK,
            "download {round}: {}",
            reply.code()
        );
        assert_eq!(reply.body, NOTES.as_bytes(), "download {round}");
    }
    let offered = harness
        .send(asset_call("assets/offer", &frame, &asset)?)
        .await?;
    assert_eq!(offered.status, StatusCode::OK, "{}", offered.code());
    Ok(())
}
