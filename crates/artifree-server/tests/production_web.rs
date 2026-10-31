//! Tests for production web asset serving, async streaming, SPA fallback,
//! origin normalization, CORS policies, healthz, and LAN loopback derivation.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use artifree_providers::storage::SqliteStore;
use artifree_server::auth::AccessToken;
use artifree_server::changes;
use artifree_server::http::{self, AppState, Settings};
use artifree_server::limits::RequestLimits;
use artifree_server::serve::{ServeOptions, derive_loopback_address};
use artifree_server::web::WebAssets;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use tower::ServiceExt;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct WebTestHarness {
    temp_dir: PathBuf,
    web_dir: PathBuf,
    router: Router,
}

impl WebTestHarness {
    fn new(content_origin: &str) -> Result<Self, Box<dyn std::error::Error>> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp_dir =
            std::env::temp_dir().join(format!("artifree-prod-web-{}-{}", std::process::id(), id));
        let _ = std::fs::remove_dir_all(&temp_dir);
        let web_dir = temp_dir.join("web_dist");
        let data_dir = temp_dir.join("data");
        std::fs::create_dir_all(&web_dir)?;
        std::fs::create_dir_all(&data_dir)?;

        // Write index.html
        std::fs::write(
            web_dir.join("index.html"),
            "<!doctype html><title>App</title>",
        )?;

        // Write nested asset
        let assets_dir = web_dir.join("assets");
        std::fs::create_dir_all(&assets_dir)?;
        std::fs::write(
            assets_dir.join("index.abcd1234.js"),
            "console.log('artifree');",
        )?;

        let db_path = data_dir.join("db.sqlite3");
        let store = SqliteStore::open(&db_path)?;
        let token_store = artifree_providers::tokens::SqliteTokenStore::open(&db_path)?;
        let token = AccessToken::parse(TOKEN).ok_or("invalid token")?;
        let state = AppState::new(
            store,
            token_store,
            token,
            Settings {
                content_origin: content_origin.to_owned(),
                preview_ttl: Duration::from_secs(60),
                frame_ancestors: "http://127.0.0.1:*".into(),
                limits: RequestLimits::default(),
                change_streams: changes::DEFAULT_STREAMS_PER_ACTOR,
                ..Settings::default()
            },
        )
        .map_err(|e| format!("cannot create app state: {e}"))?;

        let web_assets = WebAssets::new(&web_dir).map_err(|e| format!("web assets: {e}"))?;
        let router = http::router_with_web(state, Some(web_assets));
        Ok(Self {
            temp_dir,
            web_dir,
            router,
        })
    }
}

#[tokio::test]
async fn static_assets_serve_with_correct_content_type_and_caching() -> TestResult {
    let harness = WebTestHarness::new("http://127.0.0.1:7411")?;

    // 1. Root index.html
    let req = Request::get("/").body(Body::empty())?;
    let res = harness.router.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(
        res.headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-cache")
    );
    let body = to_bytes(res.into_body(), usize::MAX).await?;
    assert_eq!(body, "<!doctype html><title>App</title>");

    // 2. Fingerprinted asset under /assets/*
    let req = Request::get("/assets/index.abcd1234.js").body(Body::empty())?;
    let res = harness.router.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::OK);
    let ct = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.starts_with("application/javascript") || ct.starts_with("text/javascript"));
    assert_eq!(
        res.headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("public, max-age=31536000, immutable")
    );
    assert_eq!(
        res.headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok()),
        Some("24")
    );
    let body = to_bytes(res.into_body(), usize::MAX).await?;
    assert_eq!(body, "console.log('artifree');");
    Ok(())
}

#[tokio::test]
async fn async_chunked_streaming_for_large_assets() -> TestResult {
    let harness = WebTestHarness::new("http://127.0.0.1:7411")?;

    // Create a 150 KiB file to exceed a single 64 KiB buffer
    let size = 150 * 1024;
    #[expect(clippy::cast_possible_truncation, reason = "modulo 256 fits into u8")]
    let large_data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
    let large_path = harness.web_dir.join("large.bin");
    std::fs::write(&large_path, &large_data)?;

    let req = Request::get("/large.bin").body(Body::empty())?;
    let res = harness.router.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok()),
        Some(size.to_string().as_str())
    );

    let body = to_bytes(res.into_body(), usize::MAX).await?;
    assert_eq!(body.len(), size);
    assert_eq!(body.as_ref(), large_data.as_slice());
    Ok(())
}

#[tokio::test]
async fn spa_client_side_routing_fallback_and_missing_asset_isolation() -> TestResult {
    let harness = WebTestHarness::new("http://127.0.0.1:7411")?;

    // Client route without dot: /artifacts/art-12345 -> fallback to index.html
    let req = Request::get("/artifacts/art-12345").body(Body::empty())?;
    let res = harness.router.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
    let body = to_bytes(res.into_body(), usize::MAX).await?;
    assert_eq!(body, "<!doctype html><title>App</title>");

    // Missing file with dot: /assets/missing.js -> 404 Not Found (NOT index.html)
    let req = Request::get("/assets/missing.js").body(Body::empty())?;
    let res = harness.router.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // Missing image: /favicon.ico -> 404 Not Found
    let req = Request::get("/favicon.ico").body(Body::empty())?;
    let res = harness.router.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn path_traversal_and_symlink_escape_are_prevented() -> TestResult {
    let harness = WebTestHarness::new("http://127.0.0.1:7411")?;

    // Path traversal with ..
    let req = Request::get("/../Cargo.toml").body(Body::empty())?;
    let res = harness.router.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // Symlink escape
    let outside_file = harness.temp_dir.join("outside_secret.txt");
    std::fs::write(&outside_file, "secret-info")?;

    #[cfg(unix)]
    {
        let symlink_path = harness.web_dir.join("symlink_escape.txt");
        let _ = std::os::unix::fs::symlink(&outside_file, &symlink_path);

        let req = Request::get("/symlink_escape.txt").body(Body::empty())?;
        let res = harness.router.clone().oneshot(req).await?;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);

        // Replacing index.html after startup with a symlink escaping web root is safely rejected
        let index_path = harness.web_dir.join("index.html");
        std::fs::remove_file(&index_path)?;
        std::os::unix::fs::symlink(&outside_file, &index_path)?;

        // Root request must NOT serve the external secret!
        let root_request = Request::get("/").body(Body::empty())?;
        let root_response = harness.router.clone().oneshot(root_request).await?;
        assert_eq!(root_response.status(), StatusCode::NOT_FOUND);

        // Direct /index.html must also return NOT_FOUND
        let direct_request = Request::get("/index.html").body(Body::empty())?;
        let direct_response = harness.router.clone().oneshot(direct_request).await?;
        assert_eq!(direct_response.status(), StatusCode::NOT_FOUND);

        // SPA route must also return NOT_FOUND
        let spa_request = Request::get("/artifacts/test").body(Body::empty())?;
        let spa_response = harness.router.clone().oneshot(spa_request).await?;
        assert_eq!(spa_response.status(), StatusCode::NOT_FOUND);
    }
    Ok(())
}

#[tokio::test]
async fn cors_origin_normalization_and_preview_isolation() -> TestResult {
    // Configured content origin has non-canonical default port 80 and trailing slash
    let harness = WebTestHarness::new("http://preview.local:80/")?;

    // 1. Preflight OPTIONS from legitimate origin
    let req = Request::builder()
        .method(Method::OPTIONS)
        .uri("/v1/artifacts")
        .header(header::ORIGIN, "http://192.168.1.100:5173")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
        .body(Body::empty())?;
    let res = harness.router.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        res.headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|v| v.to_str().ok()),
        Some("http://192.168.1.100:5173")
    );
    assert!(
        res.headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_METHODS)
    );
    assert!(
        res.headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_HEADERS)
    );
    assert!(
        res.headers()
            .contains_key(header::ACCESS_CONTROL_EXPOSE_HEADERS)
    );

    // 2. Preflight from 'null' origin is refused
    let req = Request::builder()
        .method(Method::OPTIONS)
        .uri("/v1/artifacts")
        .header(header::ORIGIN, "null")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
        .body(Body::empty())?;
    let res = harness.router.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // 3. Preflight from preview origin (browser serializes without :80 and without slash)
    let req = Request::builder()
        .method(Method::OPTIONS)
        .uri("/v1/artifacts")
        .header(header::ORIGIN, "http://preview.local")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
        .body(Body::empty())?;
    let res = harness.router.clone().oneshot(req).await?;
    // Normalized comparison correctly detects it matches configured content origin!
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // 4. Bracketed IPv6 preview origin configured with default port matches browser Origin without port
    let ipv6_harness = WebTestHarness::new("http://[::1]:80/")?;
    let req = Request::builder()
        .method(Method::OPTIONS)
        .uri("/v1/artifacts")
        .header(header::ORIGIN, "http://[::1]")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
        .body(Body::empty())?;
    let res = ipv6_harness.router.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // 4b. Expanded IPv6 preview origin matches compressed browser Origin
    let ipv6_expanded_harness = WebTestHarness::new("http://[0:0:0:0:0:0:0:1]:80/")?;
    let req = Request::builder()
        .method(Method::OPTIONS)
        .uri("/v1/artifacts")
        .header(header::ORIGIN, "http://[::1]")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
        .body(Body::empty())?;
    let res = ipv6_expanded_harness.router.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // 5. Exact normalize_origin behavior across IPv4 and IPv6 variations
    assert_eq!(
        artifree_server::http::normalize_origin("http://[::1]:80/").as_deref(),
        Ok("http://[::1]")
    );
    assert_eq!(
        artifree_server::http::normalize_origin("http://[::1]").as_deref(),
        Ok("http://[::1]")
    );
    assert_eq!(
        artifree_server::http::normalize_origin("http://[0:0:0:0:0:0:0:1]:80/").as_deref(),
        Ok("http://[::1]")
    );
    assert_eq!(
        artifree_server::http::normalize_origin("http://[0:0:0:0:0:0:0:1]:7411/").as_deref(),
        Ok("http://[::1]:7411")
    );
    assert_eq!(
        artifree_server::http::normalize_origin("http://[::1]:7411/").as_deref(),
        Ok("http://[::1]:7411")
    );
    assert_eq!(
        artifree_server::http::normalize_origin("https://[2001:db8::1]:443/").as_deref(),
        Ok("https://[2001:db8::1]")
    );
    assert_eq!(
        artifree_server::http::normalize_origin(
            "https://[2001:0db8:0000:0000:0000:0000:0000:0001]:443/"
        )
        .as_deref(),
        Ok("https://[2001:db8::1]")
    );
    assert_eq!(
        artifree_server::http::normalize_origin("http://localhost:80/").as_deref(),
        Ok("http://localhost")
    );
    assert!(artifree_server::http::normalize_origin("invalid-origin").is_err());
    assert!(artifree_server::http::normalize_origin("data:text/html,secret").is_err());
    assert!(artifree_server::http::normalize_origin("file:///etc/passwd").is_err());
    Ok(())
}

#[tokio::test]
async fn healthz_endpoint_is_unauthenticated_and_outside_spa_fallback() -> TestResult {
    let harness = WebTestHarness::new("http://127.0.0.1:7411")?;

    // GET /healthz
    let req = Request::get("/healthz").body(Body::empty())?;
    let res = harness.router.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::OK);
    let ct = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.starts_with("text/plain"));
    let body = to_bytes(res.into_body(), usize::MAX).await?;
    assert_eq!(body, "ok\n");

    // HEAD /healthz
    let req = Request::head("/healthz").body(Body::empty())?;
    let res = harness.router.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await?;
    assert!(body.is_empty());
    Ok(())
}

#[tokio::test]
async fn mcp_endpoint_enforces_loopback_socket_peer() -> TestResult {
    let harness = WebTestHarness::new("http://127.0.0.1:7411")?;

    // Request to /mcp from remote LAN peer is rejected with 403 Forbidden
    let remote_peer: SocketAddr = "192.168.1.50:54321".parse()?;
    let mut req = Request::post("/mcp")
        .header(header::HOST, "127.0.0.1:7410")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-11-25")
        .body(Body::from(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        ))?;
    req.extensions_mut().insert(ConnectInfo(remote_peer));

    let res = harness.router.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // Request to /mcp from loopback peer is permitted
    let loopback_peer: SocketAddr = "127.0.0.1:54321".parse()?;
    let mut req = Request::post("/mcp")
        .header(header::HOST, "127.0.0.1:7410")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-11-25")
        .body(Body::from(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        ))?;
    req.extensions_mut().insert(ConnectInfo(loopback_peer));

    let res = harness.router.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::OK);
    Ok(())
}

fn dummy_options(
    api: SocketAddr,
    loopback: Option<SocketAddr>,
) -> Result<ServeOptions, Box<dyn std::error::Error>> {
    Ok(ServeOptions {
        data_dir: PathBuf::from("/tmp"),
        web_dir: None,
        api_address: api,
        loopback_api_address: loopback,
        content_address: "127.0.0.1:7411".parse()?,
        content_origin: None,
        frame_ancestors: "http://127.0.0.1:*".to_string(),
        preview_ttl: Duration::from_secs(60),
        limits: RequestLimits::default(),
        change_streams: changes::DEFAULT_STREAMS_PER_ACTOR,
        api_origin: None,
        trusted_proxies: Vec::new(),
        allowed_hosts: Vec::new(),
        allowed_origins: Vec::new(),
        allow_remote_mcp: false,
    })
}

#[test]
fn derive_loopback_address_behavior() -> TestResult {
    // 1. Loopback addresses do not derive secondary listener
    let v4_loopback: SocketAddr = "127.0.0.1:7410".parse()?;
    assert_eq!(
        derive_loopback_address(&dummy_options(v4_loopback, None)?),
        None
    );

    let v6_loopback: SocketAddr = "[::1]:7410".parse()?;
    assert_eq!(
        derive_loopback_address(&dummy_options(v6_loopback, None)?),
        None
    );

    // 2. Wildcard addresses do not derive secondary listener (avoids EADDRINUSE)
    let v4_wildcard: SocketAddr = "0.0.0.0:7410".parse()?;
    assert_eq!(
        derive_loopback_address(&dummy_options(v4_wildcard, None)?),
        None
    );

    let v6_wildcard: SocketAddr = "[::]:7410".parse()?;
    assert_eq!(
        derive_loopback_address(&dummy_options(v6_wildcard, None)?),
        None
    );

    // 3. IPv4 LAN address derives IPv4 loopback on same port
    let v4_lan: SocketAddr = "192.168.1.100:7410".parse()?;
    let expected_v4: SocketAddr = "127.0.0.1:7410".parse()?;
    assert_eq!(
        derive_loopback_address(&dummy_options(v4_lan, None)?),
        Some(expected_v4)
    );

    // 4. IPv6 LAN address derives IPv6 loopback on same port
    let v6_lan: SocketAddr = "[2001:db8::1]:7410".parse()?;
    let expected_v6: SocketAddr = "[::1]:7410".parse()?;
    assert_eq!(
        derive_loopback_address(&dummy_options(v6_lan, None)?),
        Some(expected_v6)
    );

    // 5. Explicit loopback option overrides derivation
    let explicit_loopback: SocketAddr = "127.0.0.1:9999".parse()?;
    assert_eq!(
        derive_loopback_address(&dummy_options(v4_lan, Some(explicit_loopback))?),
        Some(explicit_loopback)
    );
    Ok(())
}
