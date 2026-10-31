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
use axum::http::header::{self, HeaderMap};
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
        let token_file = data_dir.join("access-token");
        fs::write(&token_file, TOKEN).unwrap_or_else(|e| panic!("write token: {e}"));
        let token =
            AccessToken::load_or_create(&token_file).unwrap_or_else(|e| panic!("load token: {e}"));

        let settings = Settings {
            content_origin: "http://127.0.0.1:7411".to_string(),
            preview_ttl: Duration::from_secs(900),
            frame_ancestors: serve::DEFAULT_FRAME_ANCESTORS.to_string(),
            limits,
            change_streams: changes::DEFAULT_STREAMS_PER_ACTOR,
        };

        let state =
            AppState::new(store, token, settings).unwrap_or_else(|e| panic!("create state: {e}"));
        let web_assets =
            WebAssets::new(&web_dir).unwrap_or_else(|e| panic!("create web assets: {e}"));
        let router = http::router_with_web(state, Some(web_assets));

        Self { temp_dir, router }
    }

    async fn send(
        &self,
        req: Request<Body>,
    ) -> Result<(StatusCode, HeaderMap, Vec<u8>), Box<dyn std::error::Error>> {
        let res = self.router.clone().oneshot(req).await?;
        let status = res.status();
        let headers = res.headers().clone();
        let body = to_bytes(res.into_body(), usize::MAX).await?.to_vec();
        Ok((status, headers, body))
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.temp_dir);
    }
}

#[tokio::test]
async fn web_assets_validation_fails_on_missing_dir_or_index() {
    let non_existent = PathBuf::from("/nonexistent/directory/artifree");
    assert!(WebAssets::new(&non_existent).is_err());

    let temp_empty = std::env::temp_dir().join(format!("artifree-empty-{}", std::process::id()));
    let _ = fs::create_dir_all(&temp_empty);
    let result = WebAssets::new(&temp_empty);
    let _ = fs::remove_dir_all(&temp_empty);
    assert!(result.is_err());
}

#[tokio::test]
async fn root_serves_index_html_without_authentication() -> TestResult {
    let harness = Harness::new();
    let req = Request::get("/").body(Body::empty())?;
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
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-cache")
    );
    let html = String::from_utf8(body)?;
    assert!(html.contains("Artifree Test"));
    Ok(())
}

#[tokio::test]
async fn head_request_returns_headers_with_empty_body() -> TestResult {
    let harness = Harness::new();
    let req = Request::builder()
        .method(Method::HEAD)
        .uri("/assets/index.js")
        .body(Body::empty())?;
    let (status, headers, body) = harness.send(req).await?;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/javascript; charset=utf-8")
    );
    assert!(headers.get(header::CONTENT_LENGTH).is_some());
    assert!(body.is_empty());
    Ok(())
}

#[tokio::test]
async fn static_assets_have_immutable_cache_and_correct_mime() -> TestResult {
    let harness = Harness::new();

    // JS asset
    let req = Request::get("/assets/index.js").body(Body::empty())?;
    let (status, headers, body) = harness.send(req).await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("public, max-age=31536000, immutable")
    );
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/javascript; charset=utf-8")
    );
    assert_eq!(String::from_utf8(body)?, "console.log('artifree');");

    // CSS asset
    let req = Request::get("/assets/style.css").body(Body::empty())?;
    let (status, headers, body) = harness.send(req).await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/css; charset=utf-8")
    );
    assert_eq!(String::from_utf8(body)?, "body { margin: 0; }");

    Ok(())
}

#[tokio::test]
async fn nested_spa_fallback_serves_root_assets() -> TestResult {
    let harness = Harness::new();

    // 1. Nested SPA route fallback serves index.html
    let req = Request::get("/artifacts/art-12345").body(Body::empty())?;
    let (status, headers, body) = harness.send(req).await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
    let html = String::from_utf8(body)?;
    assert!(html.contains("<base href=\"/\">"));

    // 2. Subpath asset requests (e.g. /artifacts/assets/index.js) serve the root asset
    let req = Request::get("/artifacts/assets/index.js").body(Body::empty())?;
    let (status, headers, body) = harness.send(req).await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/javascript; charset=utf-8")
    );
    assert_eq!(String::from_utf8(body)?, "console.log('artifree');");

    Ok(())
}

#[tokio::test]
async fn spa_fallback_serves_index_html_for_client_routes() -> TestResult {
    let harness = Harness::new();
    for route in ["/artifacts", "/trash", "/settings"] {
        let req = Request::get(route).body(Body::empty())?;
        let (status, headers, body) = harness.send(req).await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            headers
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("text/html; charset=utf-8")
        );
        let html = String::from_utf8(body)?;
        assert!(html.contains("Artifree Test"));
    }
    Ok(())
}

#[tokio::test]
async fn missing_asset_with_extension_returns_404_not_spa_fallback() -> TestResult {
    let harness = Harness::new();
    let req = Request::get("/assets/missing.js").body(Body::empty())?;
    let (status, _, _) = harness.send(req).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let req_png = Request::get("/logo.png").body(Body::empty())?;
    let (status_png, _, _) = harness.send(req_png).await?;
    assert_eq!(status_png, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn api_errors_remain_json_and_never_fallback_to_spa() -> TestResult {
    let harness = Harness::new();

    // 1. Unauthenticated API request -> 401 JSON
    let req = Request::get("/v1/artifacts").body(Body::empty())?;
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

    // 2. Nonexistent API route -> 404 JSON, never index.html
    let req_unknown = Request::get("/v1/nonexistent")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::empty())?;
    let (status_unrec, headers_unrec, body_unrec) = harness.send(req_unknown).await?;
    assert_eq!(status_unrec, StatusCode::NOT_FOUND);
    assert_eq!(
        headers_unrec
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
    let json_unrec: Value = serde_json::from_slice(&body_unrec)?;
    assert_eq!(json_unrec["error"]["code"], "not_found");

    Ok(())
}

#[tokio::test]
async fn invalid_methods_on_web_assets_return_405() -> TestResult {
    let harness = Harness::new();
    let req = Request::post("/assets/index.js").body(Body::empty())?;
    let (status, headers, _) = harness.send(req).await?;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(
        headers.get(header::ALLOW).and_then(|v| v.to_str().ok()),
        Some("GET, HEAD")
    );
    Ok(())
}

#[tokio::test]
async fn path_traversal_is_prevented() -> TestResult {
    let harness = Harness::new();
    let req = Request::get("/assets/../../secret.txt").body(Body::empty())?;
    let (status, _, _) = harness.send(req).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let req_enc = Request::get("/assets/%2e%2e/%2e%2e/secret.txt").body(Body::empty())?;
    let (status_enc, _, _) = harness.send(req_enc).await?;
    assert_eq!(status_enc, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn cors_preflight_and_response_headers_on_api_v1() -> TestResult {
    let harness = Harness::new();

    // 1. Preflight OPTIONS on registered route -> 204 No Content with CORS headers
    let preflight = Request::builder()
        .method(Method::OPTIONS)
        .uri("/v1/artifacts")
        .header(header::ORIGIN, "http://localhost:5173")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
        .body(Body::empty())?;
    let (status, headers, _) = harness.send(preflight).await?;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        headers
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|v| v.to_str().ok()),
        Some("http://localhost:5173")
    );
    assert!(headers.get(header::ACCESS_CONTROL_ALLOW_METHODS).is_some());

    // 2. Preflight on unrecognized API endpoint -> 204 No Content with CORS headers
    let unrec_preflight = Request::builder()
        .method(Method::OPTIONS)
        .uri("/v1/nonexistent-endpoint")
        .header(header::ORIGIN, "http://localhost:5173")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
        .body(Body::empty())?;
    let (status, headers, _) = harness.send(unrec_preflight).await?;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        headers
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|v| v.to_str().ok()),
        Some("http://localhost:5173")
    );

    // 3. Cross-origin request to unrecognized API endpoint -> 404 with CORS headers
    let unrec_req = Request::get("/v1/nonexistent-endpoint")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::ORIGIN, "http://localhost:5173")
        .body(Body::empty())?;
    let (status, headers, body) = harness.send(unrec_req).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        headers
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|v| v.to_str().ok()),
        Some("http://localhost:5173")
    );
    let json: Value = serde_json::from_slice(&body)?;
    assert_eq!(json["error"]["code"], "not_found");

    // 4. Preflight from content origin (preview) -> 403 Forbidden
    let preview_preflight = Request::builder()
        .method(Method::OPTIONS)
        .uri("/v1/artifacts")
        .header(header::ORIGIN, "http://127.0.0.1:7411")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
        .body(Body::empty())?;
    let (status, _, _) = harness.send(preview_preflight).await?;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // 5. Preflight from null origin -> 403 Forbidden
    let null_preflight = Request::builder()
        .method(Method::OPTIONS)
        .uri("/v1/artifacts")
        .header(header::ORIGIN, "null")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
        .body(Body::empty())?;
    let (status, _, _) = harness.send(null_preflight).await?;
    assert_eq!(status, StatusCode::FORBIDDEN);

    Ok(())
}

#[tokio::test]
async fn mcp_loopback_policy_is_enforced_even_in_lan_setup() -> TestResult {
    let harness = Harness::new();

    // Rejection of foreign Origin on /mcp
    let origin_req = Request::post("/mcp")
        .header(header::HOST, "127.0.0.1:7410")
        .header(header::ORIGIN, "http://192.168.1.50:7410")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::from(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        ))?;
    let (status, _, _) = harness.send(origin_req).await?;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Rejection of foreign Host on /mcp
    let host_req = Request::post("/mcp")
        .header(header::HOST, "192.168.1.50:7410")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::from(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        ))?;
    let (status_host, _, _) = harness.send(host_req).await?;
    assert_eq!(status_host, StatusCode::FORBIDDEN);

    Ok(())
}

#[tokio::test]
async fn unmatched_api_routes_are_subject_to_admission_limits() -> TestResult {
    let limits = RequestLimits {
        per_minute: 1,
        burst: 1,
        concurrent: 10,
    };
    let harness = Harness::with_limits(limits);

    // 1. First unmatched API route succeeds in reaching admission, returning 404 Not Found
    let req1 = Request::get("/v1/nonexistent-route-1")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::empty())?;
    let (status1, _, body1) = harness.send(req1).await?;
    assert_eq!(status1, StatusCode::NOT_FOUND);
    let val1: Value = serde_json::from_slice(&body1)?;
    assert_eq!(val1["error"]["code"], "not_found");

    // 2. Second request immediately exceeds the burst of 1, so admission refuses with 429 RateLimited
    let req2 = Request::get("/v1/nonexistent-route-2")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::empty())?;
    let (status2, _, body2) = harness.send(req2).await?;
    assert_eq!(status2, StatusCode::TOO_MANY_REQUESTS);
    let val2: Value = serde_json::from_slice(&body2)?;
    assert_eq!(val2["error"]["code"], "rate_limited");

    Ok(())
}
