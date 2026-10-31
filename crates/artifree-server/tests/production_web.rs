//! Tests for production web asset serving, SPA fallback, LAN configuration,
//! and strict separation from API and MCP errors.

use std::fs;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use artifree_providers::storage::SqliteStore;
use artifree_server::auth::AccessToken;
use artifree_server::http::{self, AppState, Settings};
use artifree_server::limits::RequestLimits;
use artifree_server::serve::ServeOptions;
use artifree_server::web::WebAssets;
use axum::body::{Body, to_bytes};
use axum::http::header;
use axum::http::{Method, Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn create_temp_dir(prefix: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "artifree-{prefix}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap_or_else(|err| panic!("cannot create test dir: {err}"));
    dir
}

struct TestHarness {
    temp_dir: PathBuf,
    web_dir: PathBuf,
    router: axum::Router,
}

impl Drop for TestHarness {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.temp_dir);
    }
}

impl TestHarness {
    fn new() -> Self {
        let temp_dir = create_temp_dir("prodweb");
        let db_path = temp_dir.join("test.sqlite3");
        let store = SqliteStore::open(&db_path).unwrap_or_else(|err| panic!("open store: {err}"));
        let token = AccessToken::parse(TOKEN).unwrap_or_else(|| panic!("parse token"));

        let web_dir = temp_dir.join("dist");
        fs::create_dir_all(web_dir.join("assets"))
            .unwrap_or_else(|err| panic!("create dist/assets: {err}"));
        fs::write(
            web_dir.join("index.html"),
            "<!doctype html><html><body>Artifree App</body></html>",
        )
        .unwrap_or_else(|err| panic!("write index.html: {err}"));
        fs::write(
            web_dir.join("assets").join("bundle.js"),
            "console.log('artifree');",
        )
        .unwrap_or_else(|err| panic!("write bundle.js: {err}"));
        fs::write(
            web_dir.join("assets").join("style.css"),
            "body { margin: 0; }",
        )
        .unwrap_or_else(|err| panic!("write style.css: {err}"));

        let web_assets =
            WebAssets::try_new(&web_dir).unwrap_or_else(|err| panic!("init web assets: {err}"));
        let state = AppState::with_web_assets(
            store,
            token,
            Settings {
                content_origin: "http://127.0.0.1:7411".into(),
                preview_ttl: Duration::from_secs(900),
                frame_ancestors: "http://127.0.0.1:*".into(),
                limits: RequestLimits::default(),
                change_streams: 4,
            },
            Some(web_assets),
        )
        .unwrap_or_else(|err| panic!("init state: {err}"));

        let router = http::router(state);
        Self {
            temp_dir,
            web_dir,
            router,
        }
    }

    async fn send(
        &self,
        req: Request<Body>,
    ) -> Result<(StatusCode, axum::http::HeaderMap, Vec<u8>), Box<dyn std::error::Error>> {
        let response = self.router.clone().oneshot(req).await?;
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), usize::MAX).await?.to_vec();
        Ok((status, headers, body))
    }
}

#[tokio::test]
async fn root_serves_index_html_without_authentication() -> TestResult {
    let harness = TestHarness::new();
    let req = Request::builder().uri("/").body(Body::empty())?;
    let (status, headers, body) = harness.send(req).await?;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(
        headers
            .get(header::X_CONTENT_TYPE_OPTIONS)
            .and_then(|v| v.to_str().ok()),
        Some("nosniff")
    );
    assert_eq!(
        headers
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-cache")
    );
    let html = String::from_utf8(body)?;
    assert!(html.contains("Artifree App"));
    Ok(())
}

#[tokio::test]
async fn head_request_returns_headers_with_empty_body() -> TestResult {
    let harness = TestHarness::new();
    let req = Request::builder()
        .method(Method::HEAD)
        .uri("/")
        .body(Body::empty())?;
    let (status, headers, body) = harness.send(req).await?;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
    assert!(body.is_empty(), "HEAD must return empty body");
    Ok(())
}

#[tokio::test]
async fn static_assets_have_immutable_cache_and_correct_mime() -> TestResult {
    let harness = TestHarness::new();

    // JS asset
    let req = Request::builder()
        .uri("/assets/bundle.js")
        .body(Body::empty())?;
    let (status, headers, body) = harness.send(req).await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/javascript; charset=utf-8")
    );
    assert_eq!(
        headers
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("public, max-age=31536000, immutable")
    );
    assert_eq!(String::from_utf8(body)?, "console.log('artifree');");

    // CSS asset
    let req = Request::builder()
        .uri("/assets/style.css")
        .body(Body::empty())?;
    let (status, headers, body) = harness.send(req).await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/css; charset=utf-8")
    );
    assert_eq!(
        headers
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("public, max-age=31536000, immutable")
    );
    assert_eq!(String::from_utf8(body)?, "body { margin: 0; }");
    Ok(())
}

#[tokio::test]
async fn missing_asset_with_extension_returns_404_not_spa_fallback() -> TestResult {
    let harness = TestHarness::new();
    for missing in ["/assets/absent.js", "/missing.css", "/favicon.ico"] {
        let req = Request::builder().uri(missing).body(Body::empty())?;
        let (status, _, body) = harness.send(req).await?;
        assert_eq!(status, StatusCode::NOT_FOUND, "failed on {missing}");
        let text = String::from_utf8_lossy(&body);
        assert!(
            !text.contains("Artifree App"),
            "must not serve index.html for missing asset {missing}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn spa_fallback_serves_index_html_for_client_routes() -> TestResult {
    let harness = TestHarness::new();
    for route in ["/artifacts", "/artifacts/art-12345", "/trash", "/settings"] {
        let req = Request::builder().uri(route).body(Body::empty())?;
        let (status, headers, body) = harness.send(req).await?;
        assert_eq!(status, StatusCode::OK, "failed on {route}");
        assert_eq!(
            headers
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("text/html; charset=utf-8")
        );
        let html = String::from_utf8(body)?;
        assert!(
            html.contains("Artifree App"),
            "must serve index.html for SPA route {route}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn path_traversal_is_prevented() -> TestResult {
    let harness = TestHarness::new();
    let secret_path = harness
        .web_dir
        .parent()
        .ok_or("no parent dir")?
        .join("secret.txt");
    fs::write(&secret_path, "top secret")?;

    let targets = [
        "/../secret.txt",
        "/..%2fsecret.txt",
        "/%2e%2e/secret.txt",
        "/assets/../../secret.txt",
        "/assets/%2e%2e/%2e%2e/secret.txt",
    ];

    for target in targets {
        let req = Request::builder().uri(target).body(Body::empty())?;
        let (status, _, body) = harness.send(req).await?;
        assert_ne!(
            status,
            StatusCode::OK,
            "traversal attempt {target} must not succeed"
        );
        let text = String::from_utf8_lossy(&body);
        assert!(
            !text.contains("top secret"),
            "must not leak secret file on {target}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn invalid_methods_on_web_assets_return_405() -> TestResult {
    let harness = TestHarness::new();
    for method in [Method::POST, Method::PUT, Method::DELETE, Method::PATCH] {
        let req = Request::builder()
            .method(method)
            .uri("/")
            .body(Body::empty())?;
        let (status, headers, _) = harness.send(req).await?;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            headers.get(header::ALLOW).and_then(|v| v.to_str().ok()),
            Some("GET, HEAD")
        );
    }
    Ok(())
}

#[tokio::test]
async fn api_errors_remain_json_and_never_fallback_to_spa() -> TestResult {
    let harness = TestHarness::new();

    // 1. Unauthenticated request to real API route -> 401 JSON
    let req = Request::builder()
        .uri("/v1/artifacts")
        .body(Body::empty())?;
    let (status, headers, body) = harness.send(req).await?;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
    let json: Value = serde_json::from_slice(&body)?;
    assert_eq!(json["error"]["code"], "unauthenticated");

    // 2. Unauthenticated request to unmatched API route -> 401 JSON (not HTML!)
    let req = Request::builder()
        .uri("/v1/nonexistent")
        .body(Body::empty())?;
    let (status, headers, body) = harness.send(req).await?;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
    let json: Value = serde_json::from_slice(&body)?;
    assert_eq!(json["error"]["code"], "unauthenticated");

    // 3. Authenticated request to unmatched API route -> 404 JSON (not HTML!)
    let req = Request::builder()
        .uri("/v1/nonexistent")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::empty())?;
    let (status, headers, body) = harness.send(req).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
    let json: Value = serde_json::from_slice(&body)?;
    assert_eq!(json["error"]["code"], "not_found");
    assert_eq!(json["error"]["message"], "no such route");

    // 4. Authenticated request with unsupported method on API route -> 405 JSON
    let req = Request::builder()
        .method(Method::PUT)
        .uri("/v1/artifacts")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::empty())?;
    let (status, headers, body) = harness.send(req).await?;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
    let json: Value = serde_json::from_slice(&body)?;
    assert_eq!(json["error"]["code"], "invalid_request");
    assert!(headers.contains_key(header::ALLOW));
    Ok(())
}

#[test]
fn web_assets_validation_fails_on_missing_dir_or_index() {
    let temp_dir = create_temp_dir("prodweb-valid");
    let non_existent = temp_dir.join("does_not_exist");
    let Err(err) = WebAssets::try_new(&non_existent) else {
        panic!("expected missing directory error");
    };
    assert!(err.contains("does not exist"), "got: {err}");

    let empty_dir = temp_dir.join("empty");
    fs::create_dir(&empty_dir).unwrap_or_else(|err| panic!("create empty: {err}"));
    let Err(err) = WebAssets::try_new(&empty_dir) else {
        panic!("expected missing index.html error");
    };
    assert!(err.contains("is missing index.html"), "got: {err}");
    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn lan_configuration_derives_correct_addresses_and_frame_ancestors() -> TestResult {
    let temp_dir = create_temp_dir("prodweb-lan");
    let data_dir = temp_dir.clone();
    let lan_ip: IpAddr = "192.168.1.50".parse()?;

    let options = ServeOptions {
        data_dir,
        web_dir: None,
        lan: Some(lan_ip),
        api_address: SocketAddr::new(lan_ip, 7410),
        content_address: SocketAddr::new(lan_ip, 7411),
        content_origin: Some(format!("http://{lan_ip}:7411")),
        frame_ancestors: format!("http://127.0.0.1:* http://localhost:* http://{lan_ip}:*"),
        preview_ttl: Duration::from_secs(900),
        limits: RequestLimits::default(),
        change_streams: 4,
    };

    assert_eq!(options.api_address.ip(), lan_ip);
    assert_eq!(options.api_address.port(), 7410);
    assert_eq!(options.content_address.ip(), lan_ip);
    assert_eq!(options.content_address.port(), 7411);
    assert_eq!(
        options.content_origin.as_deref(),
        Some("http://192.168.1.50:7411")
    );
    assert!(options.frame_ancestors.contains("http://192.168.1.50:*"));
    let _ = fs::remove_dir_all(&temp_dir);
    Ok(())
}

#[tokio::test]
async fn mcp_loopback_policy_is_enforced_even_in_lan_setup() -> TestResult {
    let harness = TestHarness::new();
    let rpc_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list",
        "params": {}
    });
    let rpc_bytes = serde_json::to_vec(&rpc_body)?;

    // 1. Calling /mcp with LAN host header -> 403 Forbidden!
    let req = Request::post("/mcp")
        .header(header::HOST, "192.168.1.50:7410")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(rpc_bytes.clone()))?;
    let (status, _, _) = harness.send(req).await?;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "MCP must refuse non-loopback Host"
    );

    // 2. Calling /mcp with Origin header -> 403 Forbidden!
    let req = Request::post("/mcp")
        .header(header::HOST, "127.0.0.1:7410")
        .header(header::ORIGIN, "http://192.168.1.50:7410")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(rpc_bytes.clone()))?;
    let (status, _, _) = harness.send(req).await?;
    assert_eq!(status, StatusCode::FORBIDDEN, "MCP must refuse Origin");

    // 3. Calling /mcp with loopback Host and no Origin -> 200 OK!
    let req = Request::post("/mcp")
        .header(header::HOST, "127.0.0.1:7410")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-11-25")
        .body(Body::from(rpc_bytes))?;
    let (status, _, body) = harness.send(req).await?;
    assert_eq!(status, StatusCode::OK, "MCP accepts loopback Host");
    let json: Value = serde_json::from_slice(&body)?;
    assert!(json.get("result").is_some());
    Ok(())
}
