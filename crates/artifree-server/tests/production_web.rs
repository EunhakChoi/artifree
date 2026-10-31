//! Production web asset serving, SPA fallback, CORS, /healthz, and MCP peer loopback tests.

use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use artifree_providers::storage::SqliteStore;
use artifree_server::auth::AccessToken;
use artifree_server::changes;
use artifree_server::http::{self, AppState, Settings};
use artifree_server::limits::RequestLimits;
use artifree_server::web::WebAssets;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::header;
use axum::http::{Method, Request, StatusCode};
use tower::ServiceExt;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const PROTOCOL: &str = "2025-11-25";

struct Harness {
    directory: PathBuf,
    router: Router,
}

impl Harness {
    fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!(
            "artifree-web-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory)
            .unwrap_or_else(|error| panic!("cannot create test dir: {error}"));

        let web_dir = directory.join("web");
        fs::create_dir_all(web_dir.join("assets"))
            .unwrap_or_else(|error| panic!("cannot create assets dir: {error}"));

        fs::write(
            web_dir.join("index.html"),
            b"<!doctype html><div id=\"root\"></div>",
        )
        .unwrap_or_else(|error| panic!("cannot write index.html: {error}"));
        fs::write(
            web_dir.join("assets").join("bundle.js"),
            b"console.log('bundle');",
        )
        .unwrap_or_else(|error| panic!("cannot write bundle.js: {error}"));
        fs::write(web_dir.join("style.css"), b"body { margin: 0; }")
            .unwrap_or_else(|error| panic!("cannot write style.css: {error}"));

        let database = directory.join("db.sqlite3");
        let store =
            SqliteStore::open(&database).unwrap_or_else(|error| panic!("cannot open db: {error}"));
        let token = AccessToken::parse(TOKEN).unwrap_or_else(|| panic!("fixture token invalid"));

        let state = AppState::new(
            store,
            token,
            Settings {
                content_origin: "http://127.0.0.1:7411".into(),
                preview_ttl: Duration::from_secs(60),
                frame_ancestors: "http://127.0.0.1:*".into(),
                limits: RequestLimits::default(),
                change_streams: changes::DEFAULT_STREAMS_PER_ACTOR,
            },
        )
        .unwrap_or_else(|error| panic!("cannot build state: {error}"));

        let assets = WebAssets::new(&web_dir)
            .unwrap_or_else(|error| panic!("cannot build web assets: {error}"));

        let router = http::router_with_web(state, Some(assets));

        Self { directory, router }
    }

    fn web_dir(&self) -> PathBuf {
        self.directory.join("web")
    }

    async fn send(&self, request: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
        let response = self
            .router
            .clone()
            .oneshot(request)
            .await
            .unwrap_or_else(|error| panic!("router failed: {error}"));
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap_or_else(|error| panic!("body read failed: {error}"))
            .to_vec();
        (status, headers, body)
    }
}

type HeaderMap = axum::http::HeaderMap;

#[tokio::test]
async fn root_and_head_requests_serve_index_html() {
    let harness = Harness::new();

    let get_request = Request::builder()
        .uri("/")
        .method(Method::GET)
        .body(Body::empty())
        .unwrap_or_else(|error| panic!("req: {error}"));
    let (get_status, get_headers, get_body) = harness.send(get_request).await;
    assert_eq!(get_status, StatusCode::OK);
    assert_eq!(
        get_headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(
        get_headers
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-cache")
    );
    assert_eq!(get_body, b"<!doctype html><div id=\"root\"></div>");

    let head_request = Request::builder()
        .uri("/")
        .method(Method::HEAD)
        .body(Body::empty())
        .unwrap_or_else(|error| panic!("req: {error}"));
    let (head_status, head_headers, head_body) = harness.send(head_request).await;
    assert_eq!(head_status, StatusCode::OK);
    assert_eq!(
        head_headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(
        head_headers
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok()),
        Some("36")
    );
    assert!(head_body.is_empty());
}

#[tokio::test]
async fn static_assets_served_with_correct_caching() {
    let harness = Harness::new();

    let asset_request = Request::builder()
        .uri("/assets/bundle.js")
        .method(Method::GET)
        .body(Body::empty())
        .unwrap_or_else(|error| panic!("req: {error}"));
    let (asset_status, asset_headers, asset_body) = harness.send(asset_request).await;
    assert_eq!(asset_status, StatusCode::OK);
    assert_eq!(
        asset_headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/javascript; charset=utf-8")
    );
    assert_eq!(
        asset_headers
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("public, max-age=31536000, immutable")
    );
    assert_eq!(asset_body, b"console.log('bundle');");

    let css_request = Request::builder()
        .uri("/style.css")
        .method(Method::GET)
        .body(Body::empty())
        .unwrap_or_else(|error| panic!("req: {error}"));
    let (css_status, css_headers, _) = harness.send(css_request).await;
    assert_eq!(css_status, StatusCode::OK);
    assert_eq!(
        css_headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/css; charset=utf-8")
    );
    assert_eq!(
        css_headers
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-cache")
    );
}

#[tokio::test]
async fn path_traversal_is_blocked() {
    let harness = Harness::new();

    let traversal_request = Request::builder()
        .uri("/../db.sqlite3")
        .method(Method::GET)
        .body(Body::empty())
        .unwrap_or_else(|error| panic!("req: {error}"));
    let (status, _, _) = harness.send(traversal_request).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn symlink_outside_web_root_is_refused() {
    let outside_dir =
        std::env::temp_dir().join(format!("artifree-symlink-src-{}", std::process::id()));
    let web_dir = std::env::temp_dir().join(format!("artifree-symlink-web-{}", std::process::id()));
    let _ = fs::remove_dir_all(&outside_dir);
    let _ = fs::remove_dir_all(&web_dir);
    fs::create_dir_all(&outside_dir).unwrap_or_else(|error| panic!("err: {error}"));
    fs::create_dir_all(&web_dir).unwrap_or_else(|error| panic!("err: {error}"));

    let outside_file = outside_dir.join("secret.html");
    fs::write(&outside_file, b"secret").unwrap_or_else(|error| panic!("err: {error}"));

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside_file, web_dir.join("index.html"))
            .unwrap_or_else(|error| panic!("err: {error}"));
        let result = WebAssets::new(&web_dir);
        assert!(result.is_err());
    }
}

#[tokio::test]
async fn spa_client_routes_fall_back_to_index_html() {
    let harness = Harness::new();

    let spa_request = Request::builder()
        .uri("/artifacts/art-12345")
        .method(Method::GET)
        .body(Body::empty())
        .unwrap_or_else(|error| panic!("req: {error}"));
    let (status, headers, body) = harness.send(spa_request).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(body, b"<!doctype html><div id=\"root\"></div>");

    let missing_asset = Request::builder()
        .uri("/assets/missing.js")
        .method(Method::GET)
        .body(Body::empty())
        .unwrap_or_else(|error| panic!("req: {error}"));
    let (missing_status, _, _) = harness.send(missing_asset).await;
    assert_eq!(missing_status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn nested_spa_subpath_asset_recovery() {
    let harness = Harness::new();

    let nested_asset = Request::builder()
        .uri("/artifacts/art-12345/assets/bundle.js")
        .method(Method::GET)
        .body(Body::empty())
        .unwrap_or_else(|error| panic!("req: {error}"));
    let (status, headers, body) = harness.send(nested_asset).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/javascript; charset=utf-8")
    );
    assert_eq!(body, b"console.log('bundle');");
}

#[tokio::test]
async fn healthz_endpoint_outside_admit_and_spa() {
    let harness = Harness::new();

    let get_healthz = Request::builder()
        .uri("/healthz")
        .method(Method::GET)
        .body(Body::empty())
        .unwrap_or_else(|error| panic!("req: {error}"));
    let (status, headers, body) = harness.send(get_healthz).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/plain; charset=utf-8")
    );
    assert_eq!(body, b"ok\n");

    let head_healthz = Request::builder()
        .uri("/healthz")
        .method(Method::HEAD)
        .body(Body::empty())
        .unwrap_or_else(|error| panic!("req: {error}"));
    let (head_status, head_headers, head_body) = harness.send(head_healthz).await;
    assert_eq!(head_status, StatusCode::OK);
    assert_eq!(
        head_headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/plain; charset=utf-8")
    );
    assert!(head_body.is_empty());

    let post_healthz = Request::builder()
        .uri("/healthz")
        .method(Method::POST)
        .body(Body::empty())
        .unwrap_or_else(|error| panic!("req: {error}"));
    let (post_status, _, _) = harness.send(post_healthz).await;
    assert_eq!(post_status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn cors_preflight_and_response_headers() {
    let harness = Harness::new();

    let preflight = Request::builder()
        .uri("/v1/artifacts")
        .method(Method::OPTIONS)
        .header(header::ORIGIN, "http://localhost:5173")
        .body(Body::empty())
        .unwrap_or_else(|error| panic!("req: {error}"));
    let (status, headers, body) = harness.send(preflight).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        headers
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|v| v.to_str().ok()),
        Some("http://localhost:5173")
    );
    assert!(
        headers
            .get(header::ACCESS_CONTROL_ALLOW_METHODS)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .contains("PATCH")
    );
    assert!(
        headers
            .get(header::ACCESS_CONTROL_ALLOW_HEADERS)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .contains("Last-Event-ID")
    );
    assert!(
        headers
            .get(header::ACCESS_CONTROL_EXPOSE_HEADERS)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .contains("Content-Range")
    );
    assert!(body.is_empty());

    let forbidden_null = Request::builder()
        .uri("/v1/artifacts")
        .method(Method::OPTIONS)
        .header(header::ORIGIN, "null")
        .body(Body::empty())
        .unwrap_or_else(|error| panic!("req: {error}"));
    let (forbidden_status, _, _) = harness.send(forbidden_null).await;
    assert_eq!(forbidden_status, StatusCode::FORBIDDEN);

    let forbidden_content = Request::builder()
        .uri("/v1/artifacts")
        .method(Method::OPTIONS)
        .header(header::ORIGIN, "http://127.0.0.1:7411")
        .body(Body::empty())
        .unwrap_or_else(|error| panic!("req: {error}"));
    let (content_status, _, _) = harness.send(forbidden_content).await;
    assert_eq!(content_status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn api_route_probing_is_refused_unauthenticated() {
    let harness = Harness::new();

    let unauth_request = Request::builder()
        .uri("/v1/nonexistent-route")
        .method(Method::GET)
        .body(Body::empty())
        .unwrap_or_else(|error| panic!("req: {error}"));
    let (unauth_status, _, _) = harness.send(unauth_request).await;
    assert_eq!(unauth_status, StatusCode::UNAUTHORIZED);

    let auth_request = Request::builder()
        .uri("/v1/nonexistent-route")
        .method(Method::GET)
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::ORIGIN, "http://localhost:5173")
        .body(Body::empty())
        .unwrap_or_else(|error| panic!("req: {error}"));
    let (auth_status, headers, _) = harness.send(auth_request).await;
    assert_eq!(auth_status, StatusCode::NOT_FOUND);
    assert_eq!(
        headers
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|v| v.to_str().ok()),
        Some("http://localhost:5173")
    );
}

#[tokio::test]
async fn mcp_loopback_peer_enforcement() {
    let harness = Harness::new();

    // Remote LAN client attempting to forge Host: 127.0.0.1:7410
    let mut remote_req = Request::builder()
        .uri("/mcp")
        .method(Method::POST)
        .header(header::HOST, "127.0.0.1:7410")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("mcp-protocol-version", PROTOCOL)
        .body(Body::from(
            r#"{"jsonrpc":"2.0","method":"tools/list","id":1}"#,
        ))
        .unwrap_or_else(|error| panic!("req: {error}"));
    remote_req
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([192, 168, 1, 55], 12345))));
    let (remote_status, _, _) = harness.send(remote_req).await;
    assert_eq!(remote_status, StatusCode::FORBIDDEN);

    // Loopback client with ConnectInfo succeeds through peer check
    let mut loopback_req = Request::builder()
        .uri("/mcp")
        .method(Method::POST)
        .header(header::HOST, "127.0.0.1:7410")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("mcp-protocol-version", PROTOCOL)
        .body(Body::from(
            r#"{"jsonrpc":"2.0","method":"tools/list","id":1}"#,
        ))
        .unwrap_or_else(|error| panic!("req: {error}"));
    loopback_req
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 12345))));
    let (loopback_status, _, _) = harness.send(loopback_req).await;
    assert_eq!(loopback_status, StatusCode::OK);
}

#[tokio::test]
async fn streaming_large_asset_serves_complete_bytes() {
    let harness = Harness::new();
    let large_file = harness.web_dir().join("assets").join("large.js");
    let content: Vec<u8> = vec![b'x'; 150 * 1024];
    fs::write(&large_file, &content).unwrap_or_else(|error| panic!("write: {error}"));

    // GET request streams full body
    let req = Request::builder()
        .uri("/assets/large.js")
        .method(Method::GET)
        .body(Body::empty())
        .unwrap_or_else(|error| panic!("req: {error}"));
    let (status, headers, body) = harness.send(req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok()),
        Some((150 * 1024).to_string().as_str())
    );
    assert_eq!(body, content);

    // HEAD request returns length with empty body
    let head_req = Request::builder()
        .uri("/assets/large.js")
        .method(Method::HEAD)
        .body(Body::empty())
        .unwrap_or_else(|error| panic!("head_req: {error}"));
    let (head_status, head_headers, head_body) = harness.send(head_req).await;
    assert_eq!(head_status, StatusCode::OK);
    assert_eq!(
        head_headers
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok()),
        Some((150 * 1024).to_string().as_str())
    );
    assert!(head_body.is_empty());
}

#[test]
fn serve_options_loopback_listener_in_lan_mode() {
    use artifree_server::serve::{self, ServeOptions};
    static SERVE_COUNTER: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "artifree-serve-test-{}-{}",
        std::process::id(),
        SERVE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);

    let options = ServeOptions {
        data_dir: dir.clone(),
        web_dir: None,
        api_address: SocketAddr::from(([127, 0, 0, 1], 0)),
        loopback_api_address: Some(SocketAddr::from(([127, 0, 0, 1], 0))),
        content_address: SocketAddr::from(([127, 0, 0, 1], 0)),
        content_origin: None,
        frame_ancestors: serve::DEFAULT_FRAME_ANCESTORS.to_owned(),
        preview_ttl: serve::DEFAULT_PREVIEW_TTL,
        limits: RequestLimits::default(),
        change_streams: changes::DEFAULT_STREAMS_PER_ACTOR,
    };

    let (tx, rx) = std::sync::mpsc::channel();
    let _thread = std::thread::spawn(move || {
        let _ = serve::run(options, |ready| {
            let _ = tx.send(ready.clone());
        });
    });

    let Ok(ready) = rx.recv_timeout(Duration::from_secs(5)) else {
        panic!("server did not report ready");
    };
    assert!(ready.api_url.starts_with("http://127.0.0.1:"));
    let Some(loopback) = ready.loopback_api_url else {
        panic!("missing loopback_api_url");
    };
    assert!(loopback.starts_with("http://127.0.0.1:"));

    // Verify both listeners accept TCP connections
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|error| panic!("rt: {error}"));
    rt.block_on(async {
        let api_addr = ready.api_url.trim_start_matches("http://");
        let loopback_addr = loopback.trim_start_matches("http://");
        assert!(tokio::net::TcpStream::connect(api_addr).await.is_ok());
        assert!(tokio::net::TcpStream::connect(loopback_addr).await.is_ok());
    });

    let _ = fs::remove_dir_all(&dir);
}
