//! HTTP API and preview origin behavior, exercised in process through the
//! real routers, application core, and `SQLite` store.
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use artifree_providers::storage::SqliteStore;
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

struct Harness {
    directory: PathBuf,
    api: Router,
    content: Router,
}

impl Harness {
    fn new() -> Self {
        Self::with_ttl(Duration::from_secs(60))
    }

    fn with_ttl(preview_ttl: Duration) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!(
            "artifree-http-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory)
            .unwrap_or_else(|error| panic!("cannot create test directory: {error}"));
        let store = SqliteStore::open(&directory.join("db.sqlite3"))
            .unwrap_or_else(|error| panic!("cannot open store: {error}"));
        let token = AccessToken::parse(TOKEN).unwrap_or_else(|| panic!("fixture token is invalid"));
        let state = AppState::new(
            store,
            token,
            Settings {
                content_origin: CONTENT_ORIGIN.into(),
                preview_ttl,
                frame_ancestors: "http://127.0.0.1:*".into(),
                limits: RequestLimits::default(),
                change_streams: changes::DEFAULT_STREAMS_PER_ACTOR,
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
        Reply::from(self.api.clone().oneshot(request).await?).await
    }

    async fn content(&self, url: &str) -> Result<Reply, Box<dyn std::error::Error>> {
        let path = url
            .strip_prefix(CONTENT_ORIGIN)
            .ok_or("preview URL uses another origin")?;
        let request = Request::get(path).body(Body::empty())?;
        Reply::from(self.content.clone().oneshot(request).await?).await
    }

    async fn create(&self, key: &str, body: &Value) -> Result<Reply, Box<dyn std::error::Error>> {
        self.send(post("/v1/artifacts", Some(key), body)?).await
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
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

fn get(path: &str) -> Result<Request<Body>, axum::http::Error> {
    authorized(Request::get(path)).body(Body::empty())
}

fn post(path: &str, key: Option<&str>, body: &Value) -> Result<Request<Body>, axum::http::Error> {
    let mut builder =
        authorized(Request::post(path)).header(header::CONTENT_TYPE, "application/json");
    if let Some(key) = key {
        builder = builder.header("idempotency-key", key);
    }
    builder.body(Body::from(body.to_string()))
}

fn patch(path: &str, key: Option<&str>, body: &Value) -> Result<Request<Body>, axum::http::Error> {
    let mut builder =
        authorized(Request::patch(path)).header(header::CONTENT_TYPE, "application/json");
    if let Some(key) = key {
        builder = builder.header("idempotency-key", key);
    }
    builder.body(Body::from(body.to_string()))
}

fn page(html: &str) -> Value {
    json!({
        "title": "Demo",
        "files": [
            { "path": "index.html", "encoding": "utf8", "content": html },
            { "path": "assets/logo.png", "encoding": "base64", "content": "iVBORw0KGgo=" }
        ]
    })
}

fn put_index(base: u64, html: &str) -> Value {
    json!({
        "base_revision": base,
        "changes": [{ "op": "put", "path": "index.html", "encoding": "utf8", "content": html }]
    })
}

fn error_code(reply: &Reply) -> String {
    reply
        .json()
        .ok()
        .and_then(|body| body["error"]["code"].as_str().map(str::to_owned))
        .unwrap_or_else(|| {
            format!(
                "<no JSON error: {:?}>",
                String::from_utf8_lossy(&reply.body)
            )
        })
}

async fn created_id(harness: &Harness) -> Result<String, Box<dyn std::error::Error>> {
    let reply = harness.create("create-key-1", &page("<h1>v1</h1>")).await?;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", error_code(&reply));
    Ok(reply.json()?["artifact"]["id"]
        .as_str()
        .ok_or("missing artifact id")?
        .to_owned())
}

#[tokio::test]
async fn every_api_route_requires_the_bearer_token() -> TestResult {
    let harness = Harness::new();
    let id = created_id(&harness).await?;
    for (method, path) in [
        ("GET", "/v1/artifacts".to_owned()),
        ("POST", "/v1/artifacts".to_owned()),
        ("GET", format!("/v1/artifacts/{id}")),
        ("PATCH", format!("/v1/artifacts/{id}")),
        (
            "GET",
            format!("/v1/artifacts/{id}/revisions/1/files/index.html"),
        ),
        ("POST", format!("/v1/artifacts/{id}/revisions/1/previews")),
        ("GET", format!("/v1/artifacts/{id}/revisions/1/export")),
        ("DELETE", "/v1/artifacts".to_owned()),
        ("GET", "/v1/no-such-route".to_owned()),
    ] {
        for token in [
            None,
            Some("Bearer wrong"),
            Some(&format!("Basic {TOKEN}") as &str),
        ] {
            let mut builder = Request::builder().method(method).uri(&path);
            if let Some(token) = token {
                builder = builder.header(header::AUTHORIZATION, token);
            }
            let reply = harness.send(builder.body(Body::empty())?).await?;
            assert_eq!(
                reply.status,
                StatusCode::UNAUTHORIZED,
                "{method} {path} with {token:?}"
            );
            assert_eq!(error_code(&reply), "unauthenticated");
            assert_eq!(reply.header(header::WWW_AUTHENTICATE), "Bearer");
        }
    }
    Ok(())
}

#[tokio::test]
async fn bearer_scheme_is_case_insensitive() -> TestResult {
    let harness = Harness::new();
    for value in [
        format!("bearer {TOKEN}"),
        format!("BEARER {TOKEN}"),
        format!("Bearer   {TOKEN}"),
    ] {
        let request = Request::get("/v1/artifacts")
            .header(header::AUTHORIZATION, &value)
            .body(Body::empty())?;
        let reply = harness.send(request).await?;
        assert_eq!(
            reply.status,
            StatusCode::OK,
            "{value}: {}",
            error_code(&reply)
        );
    }
    for value in [format!("Bearer{TOKEN}"), format!("Bearer {TOKEN}x")] {
        let request = Request::get("/v1/artifacts")
            .header(header::AUTHORIZATION, &value)
            .body(Body::empty())?;
        let reply = harness.send(request).await?;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{value}");
    }
    Ok(())
}

#[tokio::test]
async fn unsupported_methods_and_routes_return_contract_errors() -> TestResult {
    let harness = Harness::new();
    let id = created_id(&harness).await?;
    for (method, path, allow) in [
        ("DELETE", "/v1/artifacts".to_owned(), "GET,HEAD,POST"),
        ("PUT", format!("/v1/artifacts/{id}"), "GET,HEAD,PATCH"),
        (
            "GET",
            format!("/v1/artifacts/{id}/revisions/1/previews"),
            "POST",
        ),
    ] {
        let request =
            authorized(Request::builder().method(method).uri(&path)).body(Body::empty())?;
        let reply = harness.send(request).await?;
        assert_eq!(
            reply.status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} {path}"
        );
        assert_eq!(error_code(&reply), "invalid_request", "{method} {path}");
        assert_eq!(reply.header(header::ALLOW), allow, "{method} {path}");
    }
    let reply = harness.send(get("/v1/no-such-route")?).await?;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(error_code(&reply), "not_found");
    let artifact = harness.send(get(&format!("/v1/artifacts/{id}"))?).await?;
    assert_eq!(
        artifact.json()?["current_revision"],
        1,
        "nothing was changed"
    );
    Ok(())
}

#[tokio::test]
async fn create_requires_an_idempotency_key() -> TestResult {
    let harness = Harness::new();
    let reply = harness
        .send(post("/v1/artifacts", None, &page("<p>x</p>"))?)
        .await?;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&reply), "invalid_request");
    let list = harness.send(get("/v1/artifacts")?).await?.json()?;
    assert_eq!(list["artifacts"].as_array().map(Vec::len), Some(0));
    Ok(())
}

#[tokio::test]
async fn retried_create_replays_and_reused_key_is_rejected() -> TestResult {
    let harness = Harness::new();
    let first = harness.create("retry-key-1", &page("<p>a</p>")).await?;
    let retry = harness.create("retry-key-1", &page("<p>a</p>")).await?;
    assert_eq!(first.status, StatusCode::CREATED);
    assert_eq!(retry.status, StatusCode::CREATED);
    assert_eq!(first.json()?["replayed"], false);
    assert_eq!(retry.json()?["replayed"], true);
    assert_eq!(
        retry.json()?["artifact"]["id"],
        first.json()?["artifact"]["id"]
    );

    let reused = harness.create("retry-key-1", &page("<p>b</p>")).await?;
    assert_eq!(reused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error_code(&reused), "request_key_reused");
    let list = harness.send(get("/v1/artifacts")?).await?.json()?;
    assert_eq!(list["artifacts"].as_array().map(Vec::len), Some(1));
    Ok(())
}

#[tokio::test]
async fn retried_save_replays_regardless_of_change_order() -> TestResult {
    let harness = Harness::new();
    let id = created_id(&harness).await?;
    let path = format!("/v1/artifacts/{id}/revisions");
    let put = |name: &str| json!({ "op": "put", "path": name, "encoding": "utf8", "content": "x" });
    let delete = json!({ "op": "delete", "path": "assets/logo.png" });
    let first =
        json!({ "base_revision": 1, "changes": [put("a.txt"), put("b.txt"), delete.clone()] });
    let reordered = json!({ "base_revision": 1, "changes": [delete, put("b.txt"), put("a.txt")] });
    let saved = harness
        .send(post(&path, Some("order-key"), &first)?)
        .await?;
    assert_eq!(saved.status, StatusCode::CREATED, "{}", error_code(&saved));
    let retry = harness
        .send(post(&path, Some("order-key"), &reordered)?)
        .await?;
    assert_eq!(retry.status, StatusCode::CREATED, "{}", error_code(&retry));
    assert_eq!(retry.json()?["replayed"], true);
    assert_eq!(retry.json()?["revision"], saved.json()?["revision"]);
    Ok(())
}

#[tokio::test]
async fn stale_base_revision_conflicts_with_current_revision() -> TestResult {
    let harness = Harness::new();
    let id = created_id(&harness).await?;
    let path = format!("/v1/artifacts/{id}/revisions");
    let saved = harness
        .send(post(
            &path,
            Some("save-key-1"),
            &put_index(1, "<h1>v2</h1>"),
        )?)
        .await?;
    assert_eq!(saved.status, StatusCode::CREATED, "{}", error_code(&saved));
    assert_eq!(
        saved.header(header::LOCATION),
        format!("/v1/artifacts/{id}/revisions/2")
    );

    let stale = harness
        .send(post(
            &path,
            Some("save-key-2"),
            &put_index(1, "<h1>lost</h1>"),
        )?)
        .await?;
    assert_eq!(stale.status, StatusCode::CONFLICT);
    assert_eq!(error_code(&stale), "revision_conflict");
    assert_eq!(stale.json()?["error"]["current_revision"], 2);
    // Changes that leave a stale base as it was are a conflict too: the
    // current files differ.
    let unchanged = harness
        .send(post(
            &path,
            Some("save-key-5"),
            &put_index(1, "<h1>v1</h1>"),
        )?)
        .await?;
    assert_eq!(error_code(&unchanged), "revision_conflict");
    let future = harness
        .send(post(
            &path,
            Some("save-key-3"),
            &put_index(9, "<h1>x</h1>"),
        )?)
        .await?;
    assert_eq!(
        future.status,
        StatusCode::CONFLICT,
        "a base newer than current must not be accepted"
    );
    let zero = harness
        .send(post(
            &path,
            Some("save-key-4"),
            &put_index(0, "<h1>x</h1>"),
        )?)
        .await?;
    assert_eq!(zero.status, StatusCode::CONFLICT, "{}", error_code(&zero));
    assert_eq!(zero.json()?["error"]["current_revision"], 2);

    let artifact = harness
        .send(get(&format!("/v1/artifacts/{id}"))?)
        .await?
        .json()?;
    assert_eq!(artifact["current_revision"], 2);
    Ok(())
}

#[tokio::test]
async fn revision_reads_are_pinned() -> TestResult {
    let harness = Harness::new();
    let id = created_id(&harness).await?;
    let path = format!("/v1/artifacts/{id}/revisions");
    harness
        .send(post(
            &path,
            Some("save-key-1"),
            &put_index(1, "<h1>v2</h1>"),
        )?)
        .await?;

    let first = harness
        .send(get(&format!("{path}/1/files/index.html"))?)
        .await?;
    assert_eq!(first.status, StatusCode::OK);
    assert_eq!(first.body, b"<h1>v1</h1>");
    assert_eq!(
        first.header(header::CONTENT_TYPE),
        "text/html; charset=utf-8"
    );
    assert_eq!(first.header(header::CONTENT_DISPOSITION), "attachment");
    let binary = harness
        .send(get(&format!("{path}/2/files/assets/logo.png"))?)
        .await?;
    assert_eq!(
        binary.body,
        [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']
    );
    let history = harness.send(get(&path)?).await?.json()?;
    let numbers: Vec<_> = history["revisions"]
        .as_array()
        .ok_or("missing revisions")?
        .iter()
        .map(|revision| revision["revision"].clone())
        .collect();
    assert_eq!(numbers, [json!(2), json!(1)]);
    for missing in [
        format!("{path}/3/files/index.html"),
        format!("{path}/0/files/index.html"),
        format!("{path}/1/files/absent.css"),
    ] {
        let reply = harness.send(get(&missing)?).await?;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{missing}");
    }
    Ok(())
}

/// A read of the file at `path` with one `Range` line per entry of `ranges`.
fn ranged(path: &str, ranges: &[&str], if_range: bool) -> Result<Request<Body>, axum::http::Error> {
    let mut builder = authorized(Request::get(path));
    for range in ranges {
        builder = builder.header(header::RANGE, *range);
    }
    if if_range {
        builder = builder.header(header::IF_RANGE, "\"anything\"");
    }
    builder.body(Body::empty())
}

/// An artifact whose revision 1 has the 11-byte `index.html` "<h1>v1</h1>"
/// and whose revision 2 adds `notes.txt` ("a\u{e9}") and an empty
/// `empty.txt`. Returns the path of its revisions.
async fn parts_fixture(harness: &Harness) -> Result<String, Box<dyn std::error::Error>> {
    let id = created_id(harness).await?;
    let history = format!("/v1/artifacts/{id}/revisions");
    let saved = harness
        .send(post(
            &history,
            Some("save-key-1"),
            &json!({ "base_revision": 1, "changes": [
                { "op": "put", "path": "notes.txt", "encoding": "utf8", "content": "a\u{e9}" },
                { "op": "put", "path": "empty.txt", "encoding": "utf8", "content": "" }
            ] }),
        )?)
        .await?;
    assert_eq!(saved.status, StatusCode::CREATED, "{}", error_code(&saved));
    Ok(history)
}

#[tokio::test]
async fn one_byte_range_of_a_pinned_file_is_read() -> TestResult {
    let harness = Harness::new();
    let history = parts_fixture(&harness).await?;
    let index = format!("{history}/1/files/index.html");
    for (range, body, content_range) in [
        ("bytes=0-3", &b"<h1>"[..], "bytes 0-3/11"),
        ("bytes=4-", b"v1</h1>", "bytes 4-10/11"),
        ("bytes=-4", b"/h1>", "bytes 7-10/11"),
        ("bytes=10-10", b">", "bytes 10-10/11"),
        // A range that ends past the end of the file stops there.
        ("bytes=8-99", b"h1>", "bytes 8-10/11"),
        ("bytes=-99", b"<h1>v1</h1>", "bytes 0-10/11"),
        (
            "bytes=0-99999999999999999999",
            b"<h1>v1</h1>",
            "bytes 0-10/11",
        ),
    ] {
        let reply = harness.send(ranged(&index, &[range], false)?).await?;
        assert_eq!(
            reply.status,
            StatusCode::PARTIAL_CONTENT,
            "{range}: {}",
            error_code(&reply)
        );
        assert_eq!(reply.body, body, "{range}");
        assert_eq!(
            reply.header(header::CONTENT_RANGE),
            content_range,
            "{range}"
        );
        assert_eq!(
            reply.header(header::CONTENT_TYPE),
            "text/html; charset=utf-8"
        );
        assert_eq!(reply.header(header::CONTENT_DISPOSITION), "attachment");
    }
    // Over HTTP a part is cut at bytes, also inside a UTF-8 character.
    let notes = harness
        .send(ranged(
            &format!("{history}/2/files/notes.txt"),
            &["bytes=1-1"],
            false,
        )?)
        .await?;
    assert_eq!(
        (notes.status, notes.body),
        (StatusCode::PARTIAL_CONTENT, vec![0xC3])
    );

    Ok(())
}

#[tokio::test]
async fn other_range_fields_read_nothing_or_the_whole_file() -> TestResult {
    let harness = Harness::new();
    let history = parts_fixture(&harness).await?;
    let index = format!("{history}/1/files/index.html");
    // A range that selects no bytes is not satisfiable, and says the size.
    let empty = format!("{history}/2/files/empty.txt");
    for (path, range, size) in [
        (&index, "bytes=11-", "11"),
        (&index, "bytes=12-20", "11"),
        (&index, "bytes=99999999999999999999-", "11"),
        (&index, "bytes=-0", "11"),
        (&empty, "bytes=0-", "0"),
        (&empty, "bytes=-1", "0"),
    ] {
        let reply = harness.send(ranged(path, &[range], false)?).await?;
        assert_eq!(reply.status, StatusCode::RANGE_NOT_SATISFIABLE, "{range}");
        assert_eq!(error_code(&reply), "range_not_satisfiable", "{range}");
        assert_eq!(
            reply.header(header::CONTENT_RANGE),
            format!("bytes */{size}"),
            "{range}"
        );
    }
    // A range the API does not serve is refused; nothing is read.
    for ranges in [
        &["bytes=3-1"][..],
        &["bytes=0-1,3-4"],
        &["bytes=0-1", "bytes=3-4"],
        &["bytes=one-two"],
    ] {
        let reply = harness.send(ranged(&index, ranges, false)?).await?;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{ranges:?}");
        assert_eq!(error_code(&reply), "invalid_request", "{ranges:?}");
        assert!(
            reply.headers.get(header::CONTENT_RANGE).is_none(),
            "{ranges:?}"
        );
    }
    // Without a byte range the whole file is read, as before.
    for (ranges, if_range) in [
        (&[][..], false),
        (&["items=0-1"], false),
        (&["bytes=0-1"], true),
    ] {
        let reply = harness.send(ranged(&index, ranges, if_range)?).await?;
        assert_eq!(
            (reply.status, reply.body.as_slice()),
            (StatusCode::OK, &b"<h1>v1</h1>"[..]),
            "{ranges:?}"
        );
        assert!(
            reply.headers.get(header::CONTENT_RANGE).is_none(),
            "{ranges:?}"
        );
        assert_eq!(reply.header(header::ACCEPT_RANGES), "bytes");
    }
    // What a read refuses is refused before any range is considered.
    for (path, status) in [
        (
            format!("{history}/3/files/index.html"),
            StatusCode::NOT_FOUND,
        ),
        (
            format!("{history}/1/files/absent.css"),
            StatusCode::NOT_FOUND,
        ),
        (
            format!("{history}/0/files/index.html"),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let reply = harness.send(ranged(&path, &["bytes=99-"], false)?).await?;
        assert_eq!(reply.status, status, "{path}");
    }
    Ok(())
}

#[tokio::test]
async fn an_export_is_a_zip_of_the_pinned_revision() -> TestResult {
    let harness = Harness::new();
    let id = created_id(&harness).await?;
    let path = format!("/v1/artifacts/{id}/revisions");
    harness
        .send(post(
            &path,
            Some("save-key-1"),
            &put_index(1, "<h1>v2</h1>"),
        )?)
        .await?;

    let first = harness.send(get(&format!("{path}/1/export"))?).await?;
    assert_eq!(first.status, StatusCode::OK);
    assert_eq!(first.header(header::CONTENT_TYPE), "application/zip");
    assert_eq!(
        first.header(header::CONTENT_DISPOSITION),
        format!("attachment; filename=\"{id}-r1.zip\"")
    );
    assert_eq!(first.header(header::X_CONTENT_TYPE_OPTIONS), "nosniff");
    // The manifest comes first; each file is stored as it is.
    assert_eq!(&first.body[..4], b"PK\x03\x04");
    assert_eq!(&first.body[30..51], b".artifree-export.json");
    let contains = |body: &[u8], part: &[u8]| body.windows(part.len()).any(|at| at == part);
    assert!(contains(&first.body, b"<h1>v1</h1>"), "revision 1's page");
    assert!(!contains(&first.body, b"<h1>v2</h1>"), "not revision 2's");
    let again = harness.send(get(&format!("{path}/1/export"))?).await?;
    assert_eq!(again.body, first.body, "the same revision, the same bytes");
    let second = harness.send(get(&format!("{path}/2/export"))?).await?;
    assert!(contains(&second.body, b"<h1>v2</h1>"), "revision 2's page");

    for missing in [format!("{path}/3/export"), format!("{path}/0/export")] {
        let reply = harness.send(get(&missing)?).await?;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{missing}");
        assert_eq!(error_code(&reply), "not_found", "{missing}");
    }
    let malformed = harness.send(get(&format!("{path}/first/export"))?).await?;
    assert_eq!(malformed.status, StatusCode::BAD_REQUEST);
    Ok(())
}

fn restore(
    id: &str,
    revision: u64,
    key: Option<&str>,
    base: u64,
) -> Result<Request<Body>, axum::http::Error> {
    post(
        &format!("/v1/artifacts/{id}/revisions/{revision}/restore"),
        key,
        &json!({ "base_revision": base }),
    )
}

#[tokio::test]
async fn restore_commits_an_earlier_revision_exactly_as_the_next() -> TestResult {
    let harness = Harness::new();
    let id = created_id(&harness).await?;
    let path = format!("/v1/artifacts/{id}/revisions");
    let changed = json!({ "base_revision": 1, "changes": [
        { "op": "put", "path": "index.html", "encoding": "utf8", "content": "<h1>v2</h1>" },
        { "op": "put", "path": "notes.txt", "encoding": "utf8", "content": "new" },
        { "op": "delete", "path": "assets/logo.png" }
    ]});
    let saved = harness
        .send(post(&path, Some("save-key-1"), &changed)?)
        .await?;
    assert_eq!(saved.status, StatusCode::CREATED, "{}", error_code(&saved));
    let renamed = harness
        .send(patch(
            &format!("/v1/artifacts/{id}"),
            Some("rename-key-1"),
            &json!({ "base_metadata_revision": 1, "title": "Renamed" }),
        )?)
        .await?;
    assert_eq!(renamed.status, StatusCode::OK, "{}", error_code(&renamed));

    let restored = harness
        .send(restore(&id, 1, Some("restore-key-1"), 2)?)
        .await?;
    assert_eq!(
        restored.status,
        StatusCode::CREATED,
        "{}",
        error_code(&restored)
    );
    assert_eq!(restored.header(header::LOCATION), format!("{path}/3"));
    let body = restored.json()?;
    assert_eq!(body["replayed"], false);
    assert_eq!(body["revision"]["revision"], 3);
    // The manifest is revision 1's: the same paths, sizes, and digests.
    let first = harness.send(get(&format!("{path}/1"))?).await?.json()?;
    assert_eq!(body["revision"]["files"], first["files"]);
    assert_eq!(
        first["files"][1]["sha256"],
        // SHA-256 of "<h1>v1</h1>".
        "c5484ff4f6706fb9d2418ffdc2b0ba515106dc32c68fbffb365b303bd9a6427e"
    );
    let index = harness
        .send(get(&format!("{path}/3/files/index.html"))?)
        .await?;
    assert_eq!(index.body, b"<h1>v1</h1>");
    let logo = harness
        .send(get(&format!("{path}/3/files/assets/logo.png"))?)
        .await?;
    assert_eq!(
        logo.body,
        [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']
    );
    let notes = harness
        .send(get(&format!("{path}/3/files/notes.txt"))?)
        .await?;
    assert_eq!(notes.status, StatusCode::NOT_FOUND);
    // Earlier revisions and the title and pin stay as they were.
    let kept = harness
        .send(get(&format!("{path}/2/files/notes.txt"))?)
        .await?;
    assert_eq!(kept.body, b"new");
    assert_eq!(body["artifact"]["title"], "Renamed");
    assert_eq!(body["artifact"]["metadata_revision"], 2);

    // A retry replays, also after a later save, and the key stays bound to
    // the restore.
    let later = harness
        .send(post(
            &path,
            Some("save-key-2"),
            &put_index(3, "<h1>v4</h1>"),
        )?)
        .await?;
    assert_eq!(later.status, StatusCode::CREATED, "{}", error_code(&later));
    let retry = harness
        .send(restore(&id, 1, Some("restore-key-1"), 2)?)
        .await?;
    assert_eq!(retry.status, StatusCode::CREATED, "{}", error_code(&retry));
    assert_eq!(retry.json()?["replayed"], true);
    assert_eq!(retry.json()?["revision"]["revision"], 3);
    let reused = harness
        .send(post(
            &path,
            Some("restore-key-1"),
            &put_index(4, "<h1>x</h1>"),
        )?)
        .await?;
    assert_eq!(error_code(&reused), "request_key_reused");
    let other = harness
        .send(restore(&id, 2, Some("restore-key-1"), 2)?)
        .await?;
    assert_eq!(error_code(&other), "request_key_reused");

    Ok(())
}

#[tokio::test]
async fn restore_refusals_write_nothing() -> TestResult {
    let harness = Harness::new();
    let id = created_id(&harness).await?;
    let saved = harness
        .send(post(
            &format!("/v1/artifacts/{id}/revisions"),
            Some("save-key-1"),
            &put_index(1, "<h1>v2</h1>"),
        )?)
        .await?;
    assert_eq!(saved.status, StatusCode::CREATED, "{}", error_code(&saved));
    // A stale base is a conflict, even when its files are the ones restored.
    let stale = harness
        .send(restore(&id, 1, Some("restore-key-1"), 1)?)
        .await?;
    assert_eq!(stale.status, StatusCode::CONFLICT);
    assert_eq!(error_code(&stale), "revision_conflict");
    assert_eq!(stale.json()?["error"]["current_revision"], 2);
    for base in [0, 9] {
        let reply = harness
            .send(restore(&id, 1, Some("restore-key-2"), base)?)
            .await?;
        assert_eq!(reply.status, StatusCode::CONFLICT, "base {base}");
    }
    for missing in [0, 3] {
        let reply = harness
            .send(restore(&id, missing, Some("restore-key-3"), 2)?)
            .await?;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "revision {missing}");
    }
    let unchanged = harness
        .send(restore(&id, 2, Some("restore-key-4"), 2)?)
        .await?;
    assert_eq!(unchanged.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error_code(&unchanged), "invalid_request");
    let keyless = harness.send(restore(&id, 1, None, 2)?).await?;
    assert_eq!(keyless.status, StatusCode::BAD_REQUEST);
    // A revision that cannot exist is reported before a malformed body.
    let malformed = harness
        .send(
            authorized(Request::post(format!(
                "/v1/artifacts/{id}/revisions/0/restore"
            )))
            .header(header::CONTENT_TYPE, "application/json")
            .header("idempotency-key", "restore-key-6")
            .body(Body::from("{"))?,
        )
        .await?;
    assert_eq!(malformed.status, StatusCode::NOT_FOUND);
    let unknown = harness
        .send(restore(
            "art_00000000000000000000000000000000",
            1,
            Some("restore-key-5"),
            1,
        )?)
        .await?;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    let artifact = harness
        .send(get(&format!("/v1/artifacts/{id}"))?)
        .await?
        .json()?;
    assert_eq!(artifact["current_revision"], 2);
    Ok(())
}

fn duplicate(
    id: &str,
    revision: u64,
    key: Option<&str>,
    body: &Value,
) -> Result<Request<Body>, axum::http::Error> {
    post(
        &format!("/v1/artifacts/{id}/revisions/{revision}/duplicate"),
        key,
        body,
    )
}

async fn artifact_count(harness: &Harness) -> Result<usize, Box<dyn std::error::Error>> {
    let list = harness.send(get("/v1/artifacts")?).await?.json()?;
    Ok(list["artifacts"].as_array().ok_or("missing list")?.len())
}

/// An artifact at revision 2, pinned, with the default fixture at revision 1.
async fn pinned_source(harness: &Harness) -> Result<String, Box<dyn std::error::Error>> {
    let id = created_id(harness).await?;
    let saved = harness
        .send(post(
            &format!("/v1/artifacts/{id}/revisions"),
            Some("save-key-1"),
            &put_index(1, "<h1>v2</h1>"),
        )?)
        .await?;
    assert_eq!(saved.status, StatusCode::CREATED, "{}", error_code(&saved));
    let pinned = harness
        .send(patch(
            &format!("/v1/artifacts/{id}"),
            Some("pin-key-1"),
            &json!({ "base_metadata_revision": 1, "pinned": true }),
        )?)
        .await?;
    assert_eq!(pinned.status, StatusCode::OK, "{}", error_code(&pinned));
    Ok(id)
}

#[tokio::test]
async fn a_duplicate_starts_its_own_history_from_the_chosen_revision() -> TestResult {
    let harness = Harness::new();
    let id = pinned_source(&harness).await?;
    let source = format!("/v1/artifacts/{id}");

    // Revision 1 of the source becomes revision 1 of an unpinned copy named
    // after the source.
    let copied = harness
        .send(duplicate(&id, 1, Some("copy-key-1"), &json!({}))?)
        .await?;
    assert_eq!(
        copied.status,
        StatusCode::CREATED,
        "{}",
        error_code(&copied)
    );
    let body = copied.json()?;
    let copy_id = body["artifact"]["id"].as_str().ok_or("missing copy id")?;
    assert_ne!(copy_id, id);
    let copy = format!("/v1/artifacts/{copy_id}");
    assert_eq!(copied.header(header::LOCATION), copy);
    assert_eq!(body["replayed"], false);
    assert_eq!(body["copied"], json!(["files"]));
    assert_eq!(body["artifact"]["title"], "Demo (Copy)");
    assert_eq!(body["artifact"]["pinned"], false);
    assert_eq!(body["artifact"]["current_revision"], 1);
    assert_eq!(body["artifact"]["metadata_revision"], 1);
    assert_eq!(body["revision"]["revision"], 1);
    let first = harness
        .send(get(&format!("{source}/revisions/1"))?)
        .await?
        .json()?;
    assert_eq!(body["revision"]["files"], first["files"]);
    let index = harness
        .send(get(&format!("{copy}/revisions/1/files/index.html"))?)
        .await?;
    assert_eq!(index.body, b"<h1>v1</h1>");
    let history = harness
        .send(get(&format!("{copy}/revisions"))?)
        .await?
        .json()?;
    assert_eq!(history["revisions"].as_array().map(Vec::len), Some(1));

    // Later saves to either artifact leave the other as it is.
    for (path, key, base, html) in [
        (&copy, "save-key-2", 1, "<h1>copy</h1>"),
        (&source, "save-key-3", 2, "<h1>v3</h1>"),
    ] {
        let saved = harness
            .send(post(
                &format!("{path}/revisions"),
                Some(key),
                &put_index(base, html),
            )?)
            .await?;
        assert_eq!(saved.status, StatusCode::CREATED, "{}", error_code(&saved));
    }
    for (path, revision, html, pinned) in [
        (&source, 3, "<h1>v3</h1>", true),
        (&copy, 2, "<h1>copy</h1>", false),
    ] {
        let artifact = harness.send(get(path)?).await?.json()?;
        assert_eq!(artifact["current_revision"], revision, "{path}");
        assert_eq!(artifact["pinned"], pinned, "{path}");
        let index = harness
            .send(get(&format!(
                "{path}/revisions/{revision}/files/index.html"
            ))?)
            .await?;
        assert_eq!(index.body, html.as_bytes(), "{path}");
    }

    // A title of its own, and a copy of the latest revision.
    let titled = harness
        .send(duplicate(
            &id,
            3,
            Some("copy-key-2"),
            &json!({ "title": "Mine" }),
        )?)
        .await?;
    assert_eq!(
        titled.status,
        StatusCode::CREATED,
        "{}",
        error_code(&titled)
    );
    let titled = titled.json()?;
    assert_eq!(titled["artifact"]["title"], "Mine");
    let latest = harness
        .send(get(&format!("{source}/revisions/3"))?)
        .await?
        .json()?;
    assert_eq!(titled["revision"]["files"], latest["files"]);
    assert_eq!(artifact_count(&harness).await?, 3);
    Ok(())
}

#[tokio::test]
async fn a_retried_duplicate_replays_after_the_source_changed() -> TestResult {
    let harness = Harness::new();
    let id = pinned_source(&harness).await?;
    let copied = harness
        .send(duplicate(&id, 1, Some("copy-key-1"), &json!({}))?)
        .await?;
    assert_eq!(
        copied.status,
        StatusCode::CREATED,
        "{}",
        error_code(&copied)
    );
    let copy_id = copied.json()?["artifact"]["id"].clone();
    // The source is renamed and revised; the retry still names the copy
    // as it was created.
    let renamed = harness
        .send(patch(
            &format!("/v1/artifacts/{id}"),
            Some("rename-key-1"),
            &json!({ "base_metadata_revision": 2, "title": "Renamed" }),
        )?)
        .await?;
    assert_eq!(renamed.status, StatusCode::OK, "{}", error_code(&renamed));
    let saved = harness
        .send(post(
            &format!("/v1/artifacts/{id}/revisions"),
            Some("save-key-2"),
            &put_index(2, "<h1>v3</h1>"),
        )?)
        .await?;
    assert_eq!(saved.status, StatusCode::CREATED, "{}", error_code(&saved));
    let retry = harness
        .send(duplicate(&id, 1, Some("copy-key-1"), &json!({}))?)
        .await?;
    assert_eq!(retry.status, StatusCode::CREATED, "{}", error_code(&retry));
    let retried = retry.json()?;
    assert_eq!(retried["replayed"], true);
    assert_eq!(retried["artifact"]["id"], copy_id);
    assert_eq!(retried["artifact"]["title"], "Demo (Copy)");
    assert_eq!(retried["copied"], json!(["files"]));
    // The key stays bound to this duplicate.
    for other in [
        duplicate(&id, 2, Some("copy-key-1"), &json!({}))?,
        duplicate(
            &id,
            1,
            Some("copy-key-1"),
            &json!({ "title": "Demo (Copy)" }),
        )?,
    ] {
        let reply = harness.send(other).await?;
        assert_eq!(error_code(&reply), "request_key_reused");
    }
    assert_eq!(artifact_count(&harness).await?, 2);
    Ok(())
}

#[tokio::test]
async fn duplicate_refusals_create_nothing() -> TestResult {
    let harness = Harness::new();
    let id = created_id(&harness).await?;
    // A revision past the store's range is missing too, not a failure.
    for missing in [0, 2, u64::MAX] {
        let reply = harness
            .send(duplicate(&id, missing, Some("copy-key-1"), &json!({}))?)
            .await?;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "revision {missing}");
    }
    for title in [json!(""), json!(" padded"), json!("x".repeat(201))] {
        let reply = harness
            .send(duplicate(
                &id,
                1,
                Some("copy-key-2"),
                &json!({ "title": title }),
            )?)
            .await?;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{title}");
    }
    let unknown_field = harness
        .send(duplicate(
            &id,
            1,
            Some("copy-key-3"),
            &json!({ "pinned": true }),
        )?)
        .await?;
    assert_eq!(unknown_field.status, StatusCode::BAD_REQUEST);
    let keyless = harness.send(duplicate(&id, 1, None, &json!({}))?).await?;
    assert_eq!(keyless.status, StatusCode::BAD_REQUEST);
    let unknown = harness
        .send(duplicate(
            "art_00000000000000000000000000000000",
            1,
            Some("copy-key-4"),
            &json!({}),
        )?)
        .await?;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    // A revision that cannot exist is reported before a malformed body.
    let malformed = harness
        .send(
            authorized(Request::post(format!(
                "/v1/artifacts/{id}/revisions/0/duplicate"
            )))
            .header(header::CONTENT_TYPE, "application/json")
            .header("idempotency-key", "copy-key-5")
            .body(Body::from("{"))?,
        )
        .await?;
    assert_eq!(malformed.status, StatusCode::NOT_FOUND);
    // Whether a well-formed artifact exists is checked with the request,
    // after its body.
    let unread = harness
        .send(
            authorized(Request::post(
                "/v1/artifacts/art_00000000000000000000000000000000/revisions/1/duplicate",
            ))
            .header(header::CONTENT_TYPE, "application/json")
            .header("idempotency-key", "copy-key-6")
            .body(Body::from("{"))?,
        )
        .await?;
    assert_eq!(unread.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&unread), "invalid_request");
    // A key of another request is refused without a copy.
    let reused = harness
        .send(duplicate(&id, 1, Some("create-key-1"), &json!({}))?)
        .await?;
    assert_eq!(error_code(&reused), "request_key_reused");
    assert_eq!(artifact_count(&harness).await?, 1);
    Ok(())
}

#[tokio::test]
async fn a_full_package_can_be_replaced_in_one_save() -> TestResult {
    let harness = Harness::new();
    let file = |name: String| json!({ "path": name, "encoding": "utf8", "content": "x" });
    let mut files =
        vec![json!({ "path": "index.html", "encoding": "utf8", "content": "<p>1</p>" })];
    files.extend((1..256).map(|n| file(format!("data/{n}.txt"))));
    let reply = harness
        .create("full-package", &json!({ "title": "Full", "files": files }))
        .await?;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", error_code(&reply));
    let id = reply.json()?["artifact"]["id"]
        .as_str()
        .ok_or("missing artifact id")?
        .to_owned();
    let path = format!("/v1/artifacts/{id}/revisions");
    let put =
        |name: String| json!({ "op": "put", "path": name, "encoding": "utf8", "content": "y" });
    let delete = |name: String| json!({ "op": "delete", "path": name });

    // What `artifree artifact save --dir` sends to swap one file of a full
    // package: a put for every desired file and a delete for the dropped one.
    let mut swap: Vec<Value> = (2..256).map(|n| put(format!("data/{n}.txt"))).collect();
    swap.push(put("index.html".into()));
    swap.push(put("data/new.txt".into()));
    swap.push(delete("data/1.txt".into()));
    assert_eq!(swap.len(), 257);
    let body = json!({ "base_revision": 1, "changes": swap });
    let reply = harness.send(post(&path, Some("swap-one"), &body)?).await?;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", error_code(&reply));

    // Replacing every file: delete all but the required index.html, rewrite
    // it, and put a new set. This is the largest valid change set.
    let mut replace: Vec<Value> = (2..256).map(|n| delete(format!("data/{n}.txt"))).collect();
    replace.push(delete("data/new.txt".into()));
    replace.push(put("index.html".into()));
    replace.extend((0..255).map(|n| put(format!("other/{n}.txt"))));
    assert_eq!(replace.len(), 511);
    let body = json!({ "base_revision": 2, "changes": replace });
    let reply = harness
        .send(post(&path, Some("replace-all"), &body)?)
        .await?;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", error_code(&reply));

    let too_many: Vec<Value> = (0..513).map(|n| put(format!("more/{n}.txt"))).collect();
    let body = json!({ "base_revision": 3, "changes": too_many });
    let reply = harness.send(post(&path, Some("too-many"), &body)?).await?;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error_code(&reply), "invalid_request");
    Ok(())
}

#[tokio::test]
async fn unsafe_paths_and_unknown_fields_are_rejected_without_writing() -> TestResult {
    let harness = Harness::new();
    for (index, path) in [
        "../index.html",
        "/etc/passwd.txt",
        "a\\b.html",
        "C:/x.html",
        ".git/config.txt",
        "con.html",
    ]
    .into_iter()
    .enumerate()
    {
        let mut body = page("<p>x</p>");
        body["files"][1]["path"] = json!(path);
        let reply = harness
            .create(&format!("unsafe-key-{index}"), &body)
            .await?;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{path}");
        assert_eq!(error_code(&reply), "invalid_request", "{path}");
    }
    let mut unknown = page("<p>x</p>");
    unknown["owner"] = json!("someone-else");
    let reply = harness.create("unknown-key-1", &unknown).await?;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert!(String::from_utf8_lossy(&reply.body).contains("unknown field `owner`"));
    let mut bad_base64 = page("<p>x</p>");
    bad_base64["files"][1]["content"] = json!("not base64!");
    assert_eq!(
        harness.create("base64-key-1", &bad_base64).await?.status,
        StatusCode::BAD_REQUEST
    );

    let id = created_id(&harness).await?;
    let escape = harness
        .send(get(&format!(
            "/v1/artifacts/{id}/revisions/1/files/..%2Fdb.sqlite3"
        ))?)
        .await?;
    assert_eq!(escape.status, StatusCode::UNPROCESSABLE_ENTITY);
    let list = harness.send(get("/v1/artifacts")?).await?.json()?;
    assert_eq!(
        list["artifacts"].as_array().map(Vec::len),
        Some(1),
        "rejected requests must not create artifacts"
    );
    Ok(())
}

#[tokio::test]
async fn oversized_body_is_rejected_before_parsing() -> TestResult {
    let harness = Harness::new();
    let huge = "a".repeat(http::MAX_BODY_BYTES + 1);
    let reply = harness.create("huge-key-1", &page(&huge)).await?;
    assert_eq!(reply.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(error_code(&reply), "payload_too_large");
    Ok(())
}

#[tokio::test]
async fn unknown_artifacts_are_not_found() -> TestResult {
    let harness = Harness::new();
    for path in [
        format!("/v1/artifacts/art_{}", "0".repeat(32)),
        "/v1/artifacts/not-an-id".to_owned(),
        format!("/v1/artifacts/art_{}/revisions", "0".repeat(32)),
    ] {
        let reply = harness.send(get(&path)?).await?;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(error_code(&reply), "not_found");
    }
    Ok(())
}

async fn preview_url(
    harness: &Harness,
    id: &str,
    revision: u64,
) -> Result<String, Box<dyn std::error::Error>> {
    let reply = harness
        .send(post(
            &format!("/v1/artifacts/{id}/revisions/{revision}/previews"),
            None,
            &json!({}),
        )?)
        .await?;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", error_code(&reply));
    Ok(reply.json()?["url"]
        .as_str()
        .ok_or("missing preview url")?
        .to_owned())
}

#[tokio::test]
async fn preview_serves_a_pinned_revision_in_an_opaque_sandbox() -> TestResult {
    let harness = Harness::new();
    let id = created_id(&harness).await?;
    let url = preview_url(&harness, &id, 1).await?;
    assert!(
        url.starts_with(&format!("{CONTENT_ORIGIN}/p/")) && url.ends_with('/'),
        "{url}"
    );
    harness
        .send(post(
            &format!("/v1/artifacts/{id}/revisions"),
            Some("save-key-1"),
            &put_index(1, "<h1>v2</h1>"),
        )?)
        .await?;

    let entry = harness.content(&url).await?;
    assert_eq!(entry.status, StatusCode::OK);
    assert_eq!(
        entry.body, b"<h1>v1</h1>",
        "a preview link stays on its revision"
    );
    let policy = entry.header(header::CONTENT_SECURITY_POLICY);
    assert!(policy.starts_with("sandbox allow-scripts"), "{policy}");
    assert!(
        !policy.contains("allow-same-origin"),
        "content must run in an opaque origin: {policy}"
    );
    assert!(
        !policy.contains("allow-popups"),
        "a popup would escape the content policy: {policy}"
    );
    assert!(
        policy.contains("form-action 'none'"),
        "forms must not submit anywhere: {policy}"
    );
    assert!(
        policy.contains(&format!("connect-src {CONTENT_ORIGIN};")),
        "{policy}"
    );
    assert!(
        policy.contains("frame-ancestors http://127.0.0.1:*"),
        "{policy}"
    );
    assert_eq!(entry.header(header::X_CONTENT_TYPE_OPTIONS), "nosniff");
    assert_eq!(entry.header(header::REFERRER_POLICY), "no-referrer");

    let asset = harness.content(&format!("{url}assets/logo.png")).await?;
    assert_eq!(
        (asset.status, asset.header(header::CONTENT_TYPE)),
        (StatusCode::OK, "image/png")
    );
    for probe in [
        format!("{url}../index.html"),
        format!("{url}%2e%2e/index.html"),
        format!("{url}absent.css"),
    ] {
        assert_eq!(
            harness.content(&probe).await?.status,
            StatusCode::NOT_FOUND,
            "{probe}"
        );
    }
    let forged = format!("{CONTENT_ORIGIN}/p/{}/", "f".repeat(64));
    assert_eq!(
        harness.content(&forged).await?.status,
        StatusCode::NOT_FOUND
    );
    Ok(())
}

#[tokio::test]
async fn expired_preview_links_stop_working() -> TestResult {
    let harness = Harness::with_ttl(Duration::ZERO);
    let id = created_id(&harness).await?;
    let url = preview_url(&harness, &id, 1).await?;
    assert_eq!(harness.content(&url).await?.status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn preview_of_a_future_revision_is_not_issued() -> TestResult {
    let harness = Harness::new();
    let id = created_id(&harness).await?;
    let reply = harness
        .send(post(
            &format!("/v1/artifacts/{id}/revisions/2/previews"),
            None,
            &json!({}),
        )?)
        .await?;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn metadata_updates_rename_and_pin_without_a_revision() -> TestResult {
    let harness = Harness::new();
    let id = created_id(&harness).await?;
    let path = format!("/v1/artifacts/{id}");
    let rename = json!({ "base_metadata_revision": 1, "title": "Renamed" });
    let renamed = harness
        .send(patch(&path, Some("meta-key-1"), &rename)?)
        .await?;
    assert_eq!(renamed.status, StatusCode::OK, "{}", error_code(&renamed));
    let body = renamed.json()?;
    assert_eq!(body["replayed"], false);
    assert_eq!(body["artifact"]["title"], "Renamed");
    assert_eq!(body["artifact"]["metadata_revision"], 2);
    assert_eq!(body["artifact"]["current_revision"], 1);
    let retried = harness
        .send(patch(&path, Some("meta-key-1"), &rename)?)
        .await?;
    assert_eq!(retried.json()?["replayed"], true);
    assert_eq!(retried.json()?["artifact"], body["artifact"]);

    let stale = harness
        .send(patch(
            &path,
            Some("meta-key-2"),
            &json!({ "base_metadata_revision": 1, "pinned": true }),
        )?)
        .await?;
    assert_eq!(stale.status, StatusCode::CONFLICT);
    assert_eq!(error_code(&stale), "metadata_conflict");
    assert_eq!(stale.json()?["error"]["current_metadata_revision"], 2);
    assert_eq!(stale.json()?["error"].get("current_revision"), None);

    for (body, status) in [
        (
            json!({ "base_metadata_revision": 2 }),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!({ "base_metadata_revision": 2, "title": " padded" }),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!({ "base_metadata_revision": 2, "title": "x", "kind": "app" }),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let reply = harness
            .send(patch(&path, Some("meta-key-3"), &body)?)
            .await?;
        assert_eq!(reply.status, status, "{body}");
        assert_eq!(error_code(&reply), "invalid_request", "{body}");
    }
    let keyless = harness
        .send(patch(
            &path,
            None,
            &json!({ "base_metadata_revision": 2, "pinned": true }),
        )?)
        .await?;
    assert_eq!(keyless.status, StatusCode::BAD_REQUEST);

    // Metadata changes create no revision and do not conflict with saves.
    let saved = harness
        .send(post(
            &format!("{path}/revisions"),
            Some("save-key-1"),
            &put_index(1, "<h1>v2</h1>"),
        )?)
        .await?;
    assert_eq!(saved.status, StatusCode::CREATED, "{}", error_code(&saved));
    assert_eq!(saved.json()?["artifact"]["title"], "Renamed");
    let history = harness
        .send(get(&format!("{path}/revisions"))?)
        .await?
        .json()?;
    assert_eq!(history["revisions"].as_array().map(Vec::len), Some(2));
    let current = harness.send(get(&path)?).await?.json()?;
    assert_eq!(current["title"], "Renamed");
    assert_eq!(current["pinned"], false);
    assert_eq!(current["metadata_revision"], 2);
    Ok(())
}

#[tokio::test]
async fn lists_search_filter_and_page_with_tokens() -> TestResult {
    let harness = Harness::new();
    let mut ids = Vec::new();
    for (index, title) in ["One plan", "Two", "Three plan"].into_iter().enumerate() {
        // Distinct times make the order by time unambiguous.
        tokio::time::sleep(Duration::from_millis(3)).await;
        let mut body = page("<h1>x</h1>");
        body["title"] = json!(title);
        let reply = harness.create(&format!("list-key-{index}"), &body).await?;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", error_code(&reply));
        ids.push(
            reply.json()?["artifact"]["id"]
                .as_str()
                .ok_or("id")?
                .to_owned(),
        );
    }
    let titles = |body: &Value| -> Vec<String> {
        body["artifacts"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item["title"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    // Without parameters, the list is what callers received before pages.
    let all = harness.send(get("/v1/artifacts")?).await?.json()?;
    assert_eq!(titles(&all).len(), 3);
    assert_eq!(all.get("next_page_token"), None);

    let first = harness.send(get("/v1/artifacts?limit=2")?).await?.json()?;
    let token = first["next_page_token"].as_str().ok_or("a next page")?;
    let second = harness
        .send(get(&format!("/v1/artifacts?limit=2&page_token={token}"))?)
        .await?
        .json()?;
    assert_eq!(second.get("next_page_token"), None);
    assert_eq!(
        [titles(&first), titles(&second)].concat(),
        titles(&all),
        "pages continue the same order"
    );

    let plans = harness.send(get("/v1/artifacts?q=PLAN")?).await?.json()?;
    assert_eq!(titles(&plans), ["Three plan", "One plan"]);
    let created = harness
        .send(get("/v1/artifacts?sort=created")?)
        .await?
        .json()?;
    assert_eq!(titles(&created), ["Three plan", "Two", "One plan"]);
    let pinned = harness
        .send(patch(
            &format!("/v1/artifacts/{}", ids[1]),
            Some("pin-key-1"),
            &json!({ "base_metadata_revision": 1, "pinned": true }),
        )?)
        .await?;
    assert_eq!(pinned.status, StatusCode::OK, "{}", error_code(&pinned));
    let only_pinned = harness
        .send(get("/v1/artifacts?pinned=true")?)
        .await?
        .json()?;
    assert_eq!(titles(&only_pinned), ["Two"]);

    let filtered = harness
        .send(get("/v1/artifacts?q=plan&limit=1")?)
        .await?
        .json()?;
    let plan_token = filtered["next_page_token"].as_str().ok_or("a next page")?;
    for query in [
        "limit=0",
        "limit=501",
        "limit=x",
        "sort=title",
        "pinned=yes",
        "q=",
        "unknown=1",
        "page_token=garbage",
        &format!("page_token={plan_token}"),
        &format!("q=plan&sort=created&page_token={plan_token}"),
    ] {
        let reply = harness
            .send(get(&format!("/v1/artifacts?{query}"))?)
            .await?;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{query}");
        assert_eq!(error_code(&reply), "invalid_request", "{query}");
    }
    Ok(())
}

fn set_deleted(
    id: &str,
    key: &str,
    base: u64,
    deleted: bool,
) -> Result<Request<Body>, axum::http::Error> {
    patch(
        &format!("/v1/artifacts/{id}"),
        Some(key),
        &json!({ "base_metadata_revision": base, "deleted": deleted }),
    )
}

/// An artifact at revision 2, with a preview link to it issued before it
/// went to the trash with "delete-key-1". Returns its id and the link.
async fn trashed(harness: &Harness) -> Result<(String, String), Box<dyn std::error::Error>> {
    let id = created_id(harness).await?;
    let history = format!("/v1/artifacts/{id}/revisions");
    let saved = harness
        .send(post(
            &history,
            Some("save-key-1"),
            &put_index(1, "<h1>v2</h1>"),
        )?)
        .await?;
    assert_eq!(saved.status, StatusCode::CREATED, "{}", error_code(&saved));
    let link = preview_url(harness, &id, 2).await?;
    let deleted = harness
        .send(set_deleted(&id, "delete-key-1", 1, true)?)
        .await?;
    assert_eq!(deleted.status, StatusCode::OK, "{}", error_code(&deleted));
    let artifact = &deleted.json()?["artifact"];
    assert!(artifact["deleted_at_ms"].is_i64(), "{artifact}");
    assert_eq!(artifact["metadata_revision"], 2);
    Ok((id, link))
}

async fn listed_ids(
    harness: &Harness,
    query: &str,
) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let list = harness
        .send(get(&format!("/v1/artifacts{query}"))?)
        .await?
        .json()?;
    Ok(list["artifacts"]
        .as_array()
        .ok_or("artifacts")?
        .iter()
        .map(|artifact| artifact["id"].clone())
        .collect())
}

type Requests = Vec<(&'static str, Request<Body>)>;

/// Reads and source writes that name the artifact `id`, as made after the
/// [`trashed`] fixture: none may succeed in the trash.
fn reads_and_saves(id: &str) -> Result<Requests, axum::http::Error> {
    let history = format!("/v1/artifacts/{id}/revisions");
    let save = |key, body: &Value| post(&history, Some(key), body);
    Ok(vec![
        ("read", get(&format!("/v1/artifacts/{id}"))?),
        ("history", get(&history)?),
        ("manifest", get(&format!("{history}/2"))?),
        ("file", get(&format!("{history}/2/files/index.html"))?),
        (
            "part of a file",
            ranged(
                &format!("{history}/2/files/index.html"),
                &["bytes=0-3"],
                false,
            )?,
        ),
        // A range past the end tells nothing of the file's size.
        (
            "part past the end of a file",
            ranged(
                &format!("{history}/2/files/index.html"),
                &["bytes=99-"],
                false,
            )?,
        ),
        ("export", get(&format!("{history}/2/export"))?),
        (
            "preview",
            post(&format!("{history}/2/previews"), None, &json!({}))?,
        ),
        ("save", save("save-key-2", &put_index(2, "<h1>v3</h1>"))?),
        // Checks before the store answer the same, for any request but a
        // retry, and so does a key used for another request.
        (
            "save on revision 0",
            save("save-key-3", &put_index(0, "x"))?,
        ),
        (
            "save on a future base",
            save("save-key-4", &put_index(9, "x"))?,
        ),
        (
            "save that forms no package",
            save(
                "save-key-5",
                &json!({ "base_revision": 2, "changes": [{ "op": "delete", "path": "index.html" }] }),
            )?,
        ),
        (
            "save with a reused key",
            save("save-key-1", &put_index(2, "y"))?,
        ),
        (
            "save on revision 0 with a reused key",
            save("save-key-1", &put_index(0, "y"))?,
        ),
        // A request invalid in itself is refused before any artifact is
        // looked up, so it is answered the same way too.
        (
            "save to an unsafe path",
            save(
                "save-key-6",
                &json!({ "base_revision": 2, "changes": [{ "op": "delete", "path": "../x" }] }),
            )?,
        ),
        (
            "save without a key",
            post(&history, None, &put_index(2, "z"))?,
        ),
    ])
}

/// The other writes that name the artifact `id`, as for [`reads_and_saves`].
fn other_writes(id: &str) -> Result<Requests, axum::http::Error> {
    let metadata = |key, body: Value| patch(&format!("/v1/artifacts/{id}"), Some(key), &body);
    Ok(vec![
        ("restore", restore(id, 1, Some("restore-key-1"), 2)?),
        (
            "restore a future revision",
            restore(id, 9, Some("restore-key-2"), 2)?,
        ),
        (
            "restore onto a future base",
            restore(id, 1, Some("restore-key-3"), 9)?,
        ),
        ("copy", duplicate(id, 2, Some("copy-key-1"), &json!({}))?),
        (
            "copy a future revision",
            duplicate(id, 9, Some("copy-key-2"), &json!({}))?,
        ),
        (
            "copy with a reused key",
            duplicate(id, 9, Some("save-key-1"), &json!({}))?,
        ),
        (
            "copy with an invalid title",
            duplicate(id, 2, Some("copy-key-3"), &json!({ "title": "" }))?,
        ),
        (
            "rename",
            metadata(
                "rename-key-1",
                json!({ "base_metadata_revision": 2, "title": "New" }),
            )?,
        ),
        (
            "pin",
            metadata(
                "pin-key-1",
                json!({ "base_metadata_revision": 2, "pinned": true }),
            )?,
        ),
        ("delete again", set_deleted(id, "delete-key-2", 2, true)?),
        (
            "rename with a reused key",
            metadata(
                "delete-key-1",
                json!({ "base_metadata_revision": 2, "title": "New" }),
            )?,
        ),
        (
            "rename to an invalid title",
            metadata(
                "rename-key-2",
                json!({ "base_metadata_revision": 2, "title": "" }),
            )?,
        ),
        (
            "update without a change",
            metadata("rename-key-3", json!({ "base_metadata_revision": 2 }))?,
        ),
    ])
}

#[tokio::test]
async fn an_artifact_in_the_trash_answers_as_an_unknown_one() -> TestResult {
    let harness = Harness::new();
    let (id, link) = trashed(&harness).await?;
    let unknown = "art_00000000000000000000000000000000";
    let mut missing = 0;
    for ((what, request), (_, control)) in reads_and_saves(&id)?
        .into_iter()
        .chain(other_writes(&id)?)
        .zip(
            reads_and_saves(unknown)?
                .into_iter()
                .chain(other_writes(unknown)?),
        )
    {
        let reply = harness.send(request).await?;
        let expected = harness.send(control).await?;
        assert_eq!(
            (reply.status, reply.json()?),
            (expected.status, expected.json()?),
            "{what}: answered unlike an unknown artifact"
        );
        if reply.status == StatusCode::NOT_FOUND {
            missing += 1;
        }
    }
    assert_eq!(missing, 24, "every request valid in itself is not found");
    assert_eq!(
        harness.content(&link).await?.status,
        StatusCode::NOT_FOUND,
        "a link issued before the delete"
    );
    assert_eq!(listed_ids(&harness, "").await?, Vec::<Value>::new());
    assert_eq!(listed_ids(&harness, "?deleted=true").await?, [json!(id)]);
    Ok(())
}

#[tokio::test]
async fn retries_replay_in_the_trash_and_a_recovery_brings_it_back() -> TestResult {
    let harness = Harness::new();
    let (id, link) = trashed(&harness).await?;
    let again = harness
        .send(set_deleted(&id, "delete-key-1", 1, true)?)
        .await?;
    assert_eq!(again.status, StatusCode::OK, "{}", error_code(&again));
    assert_eq!(again.json()?["replayed"], true);
    let resaved = harness
        .send(post(
            &format!("/v1/artifacts/{id}/revisions"),
            Some("save-key-1"),
            &put_index(1, "<h1>v2</h1>"),
        )?)
        .await?;
    assert_eq!(
        resaved.status,
        StatusCode::CREATED,
        "{}",
        error_code(&resaved)
    );
    let replayed = resaved.json()?;
    assert_eq!(replayed["replayed"], true);
    assert_eq!(
        replayed["artifact"]["deleted_at_ms"],
        Value::Null,
        "as of its request"
    );

    // A recovery is an ordinary conditional update. The delete's key does
    // not recover, even on the delete's own base.
    let reused = harness
        .send(set_deleted(&id, "delete-key-1", 1, false)?)
        .await?;
    assert_eq!(error_code(&reused), "request_key_reused");
    let stale = harness
        .send(set_deleted(&id, "recover-key-1", 1, false)?)
        .await?;
    assert_eq!(error_code(&stale), "metadata_conflict");
    assert_eq!(stale.json()?["error"]["current_metadata_revision"], 2);
    let recovered = harness
        .send(set_deleted(&id, "recover-key-1", 2, false)?)
        .await?;
    assert_eq!(
        recovered.status,
        StatusCode::OK,
        "{}",
        error_code(&recovered)
    );
    let artifact = &recovered.json()?["artifact"];
    assert_eq!(artifact["deleted_at_ms"], Value::Null);
    assert_eq!(
        (
            &artifact["current_revision"],
            &artifact["metadata_revision"]
        ),
        (&json!(2), &json!(3))
    );
    let read = harness.send(get(&format!("/v1/artifacts/{id}"))?).await?;
    assert_eq!(read.status, StatusCode::OK);
    assert_eq!(
        harness.content(&link).await?.status,
        StatusCode::OK,
        "the link works again until it expires"
    );
    assert_eq!(listed_ids(&harness, "").await?, [json!(id)]);
    assert_eq!(
        listed_ids(&harness, "?deleted=true").await?,
        Vec::<Value>::new()
    );
    Ok(())
}
