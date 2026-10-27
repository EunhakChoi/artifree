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
        (
            "GET",
            format!("/v1/artifacts/{id}/revisions/1/files/index.html"),
        ),
        ("POST", format!("/v1/artifacts/{id}/revisions/1/previews")),
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
        ("PUT", format!("/v1/artifacts/{id}"), "GET,HEAD"),
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
