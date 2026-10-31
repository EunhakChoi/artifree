//! Integration tests for production web asset serving, LAN configuration,
//! CORS preflights, and security isolation.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use artifree_providers::storage::SqliteStore;
use artifree_server::auth::AccessToken;
use artifree_server::changes;
use artifree_server::http::{self, AppState, Settings};
use artifree_server::limits::RequestLimits;
use artifree_server::serve;
use artifree_server::web::WebAssets;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::header;
use axum::http::{Method, Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct Harness {
    temp_dir: PathBuf,
    router: Router,
}

impl Harness {
    fn new() -> Self {
        Self::with_limits(RequestLimits::default())
    }

    fn with_limits(limits: RequestLimits) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp_dir =
            std::env::temp_dir().join(format!("artifree-prod-web-{}-{id}", std::process::id()));
        let web_dir = temp_dir.join("dist");
        let data_dir = temp_dir.join("data");

        fs::create_dir_all(&web_dir).unwrap_or_else(|e| panic!("create web dir: {e}"));
        fs::create_dir_all(&data_dir).unwrap_or_else(|e| panic!("create data dir: {e}"));

        // Write sample web assets
        fs::write(
            web_dir.join("index.html"),
            "<!doctype html><html><head><base href=\"/\"><title>Artifree Test</title></head><body><div id=\"root\">App</div></body></html>",
        ).unwrap_or_else(|e| panic!("write index.html: {e}"));

        let assets_dir = web_dir.join("assets");
        fs::create_dir_all(&assets_dir).unwrap_or_else(|e| panic!("create assets dir: {e}"));
        fs::write(assets_dir.join("index.js"), "console.log('artifree');")
            .unwrap_or_else(|e| panic!("write index.js: {e}"));
        fs::write(assets_dir.join("style.css"), "body { margin: 0; }")
            .unwrap_or_else(|e| panic!("write style.css: {e}"));

        let store = SqliteStore::open(&data_dir.join("artifree.sqlite3"))
            .unwrap_or_else(|e| panic!("open store: {e}"));
        let token = AccessToken::parse(TOKEN).unwrap_or_else(|| panic!("parse token"));
        let settings = Settings {
            content_origin: "http://127.0.0.1:7411".into(),
            preview_ttl: Duration::from_secs(900),
            frame_ancestors: serve::DEFAULT_FRAME_ANCESTORS.to_string(),
            limits,
            change_streams: changes::DEFAULT_STREAMS_PER_ACTOR,
        };
        let state = AppState::new(store, token, settings).unwrap_or_else(|e| panic!("state: {e}"));
        let web_assets =
            Some(WebAssets::new(web_dir).unwrap_or_else(|e| panic!("web assets: {e}")));
        let router = http::router_with_web(state, web_assets);

        Self { temp_dir, router }
    }

    fn router(&self) -> Router {
        self.router.clone()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.temp_dir);
    }
}

#[tokio::test]
async fn root_serves_index_html_without_authentication() -> TestResult {
    let harness = Harness::new();
    let req = Request::builder().uri("/").body(Body::empty())?;
    let res = harness.router().oneshot(req).await?;

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

    let body = to_bytes(res.into_body(), 1024 * 1024).await?;
    let html = String::from_utf8(body.to_vec())?;
    assert!(html.contains("Artifree Test"));
    Ok(())
}

#[tokio::test]
async fn static_assets_have_immutable_cache_and_correct_mime() -> TestResult {
    let harness = Harness::new();

    // JS asset
    let req = Request::builder()
        .uri("/assets/index.js")
        .body(Body::empty())?;
    let res = harness.router().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/javascript; charset=utf-8")
    );
    assert_eq!(
        res.headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("public, max-age=31536000, immutable")
    );

    // CSS asset
    let req = Request::builder()
        .uri("/assets/style.css")
        .body(Body::empty())?;
    let res = harness.router().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/css; charset=utf-8")
    );
    assert_eq!(
        res.headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("public, max-age=31536000, immutable")
    );
    Ok(())
}

#[tokio::test]
async fn head_request_returns_headers_with_empty_body() -> TestResult {
    let harness = Harness::new();
    let req = Request::builder()
        .method(Method::HEAD)
        .uri("/assets/index.js")
        .body(Body::empty())?;
    let res = harness.router().oneshot(req).await?;

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/javascript; charset=utf-8")
    );
    let body = to_bytes(res.into_body(), 1024).await?;
    assert!(body.is_empty());
    Ok(())
}

#[tokio::test]
async fn invalid_methods_on_web_assets_return_405() -> TestResult {
    let harness = Harness::new();
    for method in [Method::POST, Method::PUT, Method::DELETE, Method::PATCH] {
        let req = Request::builder()
            .method(method)
            .uri("/assets/index.js")
            .body(Body::empty())?;
        let res = harness.router().oneshot(req).await?;
        assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            res.headers()
                .get(header::ALLOW)
                .and_then(|v| v.to_str().ok()),
            Some("GET, HEAD")
        );
    }
    Ok(())
}

#[tokio::test]
async fn path_traversal_is_prevented() -> TestResult {
    let harness = Harness::new();
    let attacks = [
        "/../Cargo.toml",
        "/assets/../../Cargo.toml",
        "/..%2F..%2FCargo.toml",
        "/.hidden",
        "/assets/.private",
    ];

    for path in attacks {
        let req = Request::builder().uri(path).body(Body::empty())?;
        let res = harness.router().oneshot(req).await?;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }
    Ok(())
}

#[tokio::test]
async fn spa_fallback_serves_index_html_for_client_routes() -> TestResult {
    let harness = Harness::new();
    let client_routes = [
        "/artifacts",
        "/artifacts/art_0123456789abcdef",
        "/trash",
        "/settings",
    ];

    for route in client_routes {
        let req = Request::builder().uri(route).body(Body::empty())?;
        let res = harness.router().oneshot(req).await?;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("text/html; charset=utf-8")
        );
        let body = to_bytes(res.into_body(), 1024 * 1024).await?;
        let html = String::from_utf8(body.to_vec())?;
        assert!(html.contains("Artifree Test"));
    }
    Ok(())
}

#[tokio::test]
async fn nested_spa_fallback_serves_root_assets() -> TestResult {
    let harness = Harness::new();
    let req = Request::builder()
        .uri("/artifacts/assets/index.js")
        .body(Body::empty())?;
    let res = harness.router().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/javascript; charset=utf-8")
    );
    Ok(())
}

#[tokio::test]
async fn missing_asset_with_extension_returns_404_not_spa_fallback() -> TestResult {
    let harness = Harness::new();
    let missing_assets = ["/assets/nonexistent.js", "/missing.css", "/favicon.ico"];

    for asset in missing_assets {
        let req = Request::builder().uri(asset).body(Body::empty())?;
        let res = harness.router().oneshot(req).await?;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let body = to_bytes(res.into_body(), 1024).await?;
        let text = String::from_utf8(body.to_vec())?;
        assert_eq!(text, "404 Not Found");
    }
    Ok(())
}

#[tokio::test]
async fn api_errors_remain_json_and_never_fallback_to_spa() -> TestResult {
    let harness = Harness::new();

    // 1. Unauthenticated request to /v1/artifacts
    let req = Request::builder()
        .uri("/v1/artifacts")
        .body(Body::empty())?;
    let res = harness.router().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        res.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );

    // 2. Unauthenticated request to missing /v1/nonexistent
    let req = Request::builder()
        .uri("/v1/nonexistent")
        .body(Body::empty())?;
    let res = harness.router().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        res.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );

    // 3. Authenticated request to missing /v1/nonexistent
    let req = Request::builder()
        .uri("/v1/nonexistent")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::empty())?;
    let res = harness.router().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        res.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
    let body = to_bytes(res.into_body(), 1024).await?;
    let json: Value = serde_json::from_slice(&body)?;
    assert_eq!(json["error"]["code"], "not_found");
    Ok(())
}

#[tokio::test]
async fn cors_preflight_and_response_headers_on_api_v1() -> TestResult {
    let harness = Harness::new();

    // 1. Preflight on /v1/artifacts
    let preflight = Request::builder()
        .method(Method::OPTIONS)
        .uri("/v1/artifacts")
        .header(header::ORIGIN, "http://localhost:5173")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
        .body(Body::empty())?;

    let res = harness.router().oneshot(preflight).await?;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    let headers = res.headers();
    assert_eq!(
        headers
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|v| v.to_str().ok()),
        Some("http://localhost:5173")
    );
    let methods = headers
        .get(header::ACCESS_CONTROL_ALLOW_METHODS)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(methods.contains("PATCH"));
    assert!(methods.contains("HEAD"));
    assert!(methods.contains("GET"));
    assert!(methods.contains("POST"));
    assert!(methods.contains("PUT"));
    assert!(methods.contains("DELETE"));

    let allowed_headers = headers
        .get(header::ACCESS_CONTROL_ALLOW_HEADERS)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(allowed_headers.contains("Last-Event-ID"));
    assert!(allowed_headers.contains("Authorization"));

    let exposed_headers = headers
        .get(header::ACCESS_CONTROL_EXPOSE_HEADERS)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(exposed_headers.contains("Content-Range"));
    assert!(exposed_headers.contains("Retry-After"));
    assert!(exposed_headers.contains("Location"));

    // 2. Preflight on unrecognized API endpoint -> 204 No Content with CORS headers
    let unrec_preflight = Request::builder()
        .method(Method::OPTIONS)
        .uri("/v1/unrecognized")
        .header(header::ORIGIN, "http://localhost:5173")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
        .body(Body::empty())?;

    let res = harness.router().oneshot(unrec_preflight).await?;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        res.headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|v| v.to_str().ok()),
        Some("http://localhost:5173")
    );

    // 3. Authenticated request to unrecognized API endpoint -> 404 JSON with CORS headers
    let unrec_req = Request::builder()
        .uri("/v1/unrecognized")
        .header(header::ORIGIN, "http://localhost:5173")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::empty())?;

    let res = harness.router().oneshot(unrec_req).await?;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        res.headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|v| v.to_str().ok()),
        Some("http://localhost:5173")
    );
    let exposed = res
        .headers()
        .get(header::ACCESS_CONTROL_EXPOSE_HEADERS)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(exposed.contains("Content-Range"));
    assert!(exposed.contains("Retry-After"));
    assert!(exposed.contains("Location"));
    let body = to_bytes(res.into_body(), 1024).await?;
    let json: Value = serde_json::from_slice(&body)?;
    assert_eq!(json["error"]["code"], "not_found");

    // 4. Forbidden origins: preview origin and null
    for bad_origin in ["http://127.0.0.1:7411", "null"] {
        let req = Request::builder()
            .uri("/v1/artifacts")
            .header(header::ORIGIN, bad_origin)
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .body(Body::empty())?;
        let res = harness.router().oneshot(req).await?;
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    Ok(())
}

#[tokio::test]
async fn unmatched_api_routes_are_subject_to_admission_limits() -> TestResult {
    let limits = RequestLimits {
        burst: 1,
        per_minute: 1,
        concurrent: 1,
    };
    let harness = Harness::with_limits(limits);

    // First request consumes the single slot
    let first_req = Request::builder()
        .uri("/v1/unrecognized-admission-1")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::empty())?;
    let first_resp = harness.router().oneshot(first_req).await?;
    assert_eq!(first_resp.status(), StatusCode::NOT_FOUND);
    let first_body = to_bytes(first_resp.into_body(), 1024).await?;
    let first_json: Value = serde_json::from_slice(&first_body)?;
    assert_eq!(first_json["error"]["code"], "not_found");

    // Second request immediately exceeds the rate limit
    let second_req = Request::builder()
        .uri("/v1/unrecognized-admission-2")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::empty())?;
    let second_resp = harness.router().oneshot(second_req).await?;
    assert_eq!(second_resp.status(), StatusCode::TOO_MANY_REQUESTS);
    let second_body = to_bytes(second_resp.into_body(), 1024).await?;
    let second_json: Value = serde_json::from_slice(&second_body)?;
    assert_eq!(second_json["error"]["code"], "rate_limited");

    Ok(())
}

#[tokio::test]
async fn mcp_loopback_policy_is_enforced_even_in_lan_setup() -> TestResult {
    let harness = Harness::new();

    // 1. Non-loopback Host is rejected
    let req = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::HOST, "192.168.1.100:7410")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#))?;

    let res = harness.router().oneshot(req).await?;
    assert_ne!(res.status(), StatusCode::OK);

    // 2. Origin header is strictly rejected on /mcp
    let req = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::HOST, "127.0.0.1:7410")
        .header(header::ORIGIN, "http://127.0.0.1:5173")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#))?;

    let res = harness.router().oneshot(req).await?;
    assert_ne!(res.status(), StatusCode::OK);

    Ok(())
}

#[tokio::test]
async fn web_assets_validation_fails_on_missing_dir_or_index() {
    let temp = std::env::temp_dir().join(format!("artifree-empty-web-{}", std::process::id()));
    let _ = fs::create_dir_all(&temp);

    // Missing index.html
    let res = WebAssets::new(temp.clone());
    assert!(res.is_err());

    // Nonexistent directory
    let res = WebAssets::new(temp.join("nonexistent"));
    assert!(res.is_err());

    let _ = fs::remove_dir_all(&temp);
}

#[cfg(unix)]
#[tokio::test]
async fn index_symlink_pointing_outside_web_root_is_rejected() {
    use std::os::unix::fs::symlink;
    let temp = std::env::temp_dir().join(format!("artifree-symlink-web-{}", std::process::id()));
    let target = std::env::temp_dir().join(format!(
        "artifree-symlink-target-{}.html",
        std::process::id()
    ));
    let _ = fs::create_dir_all(&temp);
    let _ = fs::write(&target, "secret content outside root");

    let symlink_path = temp.join("index.html");
    let _ = symlink(&target, &symlink_path);

    let res = WebAssets::new(&temp);
    match res {
        Err(err) => assert!(err.contains("must be contained within the web directory")),
        Ok(_) => panic!("expected WebAssets::new to fail on outward symlink"),
    }

    let _ = fs::remove_dir_all(&temp);
    let _ = fs::remove_file(&target);
}

#[tokio::test]
async fn healthz_returns_200_ok_and_does_not_fallback_to_spa() -> TestResult {
    let harness = Harness::new();

    // 1. GET /healthz returns 200 OK without authentication
    let req = Request::builder().uri("/healthz").body(Body::empty())?;
    let res = harness.router().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), 1024).await?;
    let text = String::from_utf8(body.to_vec())?;
    assert_eq!(text, "ok\n");

    // 2. HEAD /healthz returns 200 OK with empty body
    let head_request = Request::builder()
        .method(Method::HEAD)
        .uri("/healthz")
        .body(Body::empty())?;
    let head_response = harness.router().oneshot(head_request).await?;
    assert_eq!(head_response.status(), StatusCode::OK);
    let head_body = to_bytes(head_response.into_body(), 1024).await?;
    assert!(head_body.is_empty());

    // 3. /healthz never falls back to SPA index.html
    assert!(!text.contains("<!doctype html>"));
    assert!(!text.contains("Artifree Test"));

    Ok(())
}
