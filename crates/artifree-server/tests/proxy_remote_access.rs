//! Integration tests for HTTPS reverse proxy integration, trusted proxy evaluation,
//! Host and Origin allowlists, spoofed forwarded header rejection, and MCP loopback enforcement.

use std::net::SocketAddr;
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
use artifree_server::proxy::IpNetwork;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use tower::ServiceExt;

const OWNER_TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct ProxyHarness {
    _temp_dir: PathBuf,
    api: Router,
    preview: Router,
}

impl ProxyHarness {
    fn new(
        trusted_proxies: Vec<IpNetwork>,
        allowed_hosts: Vec<String>,
        allowed_origins: Vec<String>,
        api_origin: Option<String>,
        content_origin: String,
        allow_remote_mcp: bool,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp_dir =
            std::env::temp_dir().join(format!("artifree-proxy-test-{}-{}", std::process::id(), id));
        let _ = std::fs::remove_dir_all(&temp_dir);
        let data_dir = temp_dir.join("data");
        std::fs::create_dir_all(&data_dir)?;

        let db_path = data_dir.join("db.sqlite3");
        let store = SqliteStore::open(&db_path)?;
        let token_store = SqliteTokenStore::open(&db_path)?;
        let token = AccessToken::parse(OWNER_TOKEN).ok_or("invalid owner token")?;

        let settings = Settings {
            content_origin,
            preview_ttl: Duration::from_secs(60),
            frame_ancestors: "'self'".into(),
            limits: RequestLimits::default(),
            change_streams: changes::DEFAULT_STREAMS_PER_ACTOR,
            api_origin,
            trusted_proxies,
            allowed_hosts,
            allowed_origins,
            allow_remote_mcp,
        };

        let state = AppState::new(store, token_store, token, settings)
            .map_err(|e| format!("cannot create app state: {e}"))?;

        let api = http::router(state.clone());
        let preview = preview::router(state);

        Ok(Self {
            _temp_dir: temp_dir,
            api,
            preview,
        })
    }
}

#[tokio::test]
async fn untrusted_peer_forwarded_headers_rejected() -> TestResult {
    // Only 127.0.0.1 is trusted as proxy
    let harness = ProxyHarness::new(
        vec!["127.0.0.1/32".parse()?],
        vec!["artifree.example.com".into()],
        vec![],
        Some("https://artifree.example.com".into()),
        "https://preview.example.com".into(),
        false,
    )?;

    // Direct caller from untrusted peer (e.g. 192.168.1.100) attempts to spoof X-Forwarded-For
    let untrusted_peer: SocketAddr = "192.168.1.100:45678".parse()?;

    // 1. Spoofed X-Forwarded-For on API
    let mut req = Request::get("/healthz")
        .header(header::HOST, "artifree.example.com")
        .header("x-forwarded-for", "127.0.0.1")
        .body(Body::empty())?;
    req.extensions_mut().insert(ConnectInfo(untrusted_peer));

    let res = harness.api.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // 2. Spoofed Forwarded header on API
    let mut req = Request::get("/healthz")
        .header(header::HOST, "artifree.example.com")
        .header("forwarded", "for=127.0.0.1;proto=https")
        .body(Body::empty())?;
    req.extensions_mut().insert(ConnectInfo(untrusted_peer));

    let res = harness.api.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // 3. Spoofed X-Forwarded-Host on Preview listener
    let mut req = Request::get("/p/deadbeef1234/")
        .header("x-forwarded-host", "attacker.com")
        .body(Body::empty())?;
    req.extensions_mut().insert(ConnectInfo(untrusted_peer));

    let res = harness.preview.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // 4. Legitimate direct request without forwarded headers succeeds
    let mut req = Request::get("/healthz")
        .header(header::HOST, "artifree.example.com")
        .body(Body::empty())?;
    req.extensions_mut().insert(ConnectInfo(untrusted_peer));

    let res = harness.api.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::OK);

    Ok(())
}

#[tokio::test]
async fn trusted_proxy_forwarded_headers_accepted_and_extracted() -> TestResult {
    let harness = ProxyHarness::new(
        vec!["127.0.0.1/32".parse()?, "10.0.0.0/8".parse()?],
        vec!["artifree.example.com".into()],
        vec![],
        Some("https://artifree.example.com".into()),
        "https://preview.example.com".into(),
        false,
    )?;

    let proxy_peer: SocketAddr = "10.1.2.3:54321".parse()?;

    // Trusted proxy forwards client request with headers
    let mut req = Request::get("/healthz")
        .header("host", "artifree.example.com")
        .header("x-forwarded-for", "203.0.113.195, 10.1.2.3")
        .header("x-forwarded-host", "artifree.example.com")
        .header("x-forwarded-proto", "https")
        .body(Body::empty())?;
    req.extensions_mut().insert(ConnectInfo(proxy_peer));

    let res = harness.api.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await?;
    assert_eq!(body, "ok\n");

    Ok(())
}

#[tokio::test]
async fn host_validation_enforces_allowlist() -> TestResult {
    let harness = ProxyHarness::new(
        vec!["127.0.0.1/32".parse()?],
        vec!["artifree.example.com".into(), "localhost".into()],
        vec![],
        Some("https://artifree.example.com".into()),
        "https://preview.example.com".into(),
        false,
    )?;

    let peer: SocketAddr = "127.0.0.1:45678".parse()?;

    // Allowed host
    let mut req = Request::get("/healthz")
        .header(header::HOST, "artifree.example.com")
        .body(Body::empty())?;
    req.extensions_mut().insert(ConnectInfo(peer));
    let res = harness.api.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::OK);

    // Allowed host with default port (:443 for https / :80 for http)
    let mut req = Request::get("/healthz")
        .header(header::HOST, "artifree.example.com:443")
        .body(Body::empty())?;
    req.extensions_mut().insert(ConnectInfo(peer));
    let res = harness.api.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::OK);

    // Allowed host case-insensitive
    let mut req = Request::get("/healthz")
        .header(header::HOST, "ARTIFREE.EXAMPLE.COM")
        .body(Body::empty())?;
    req.extensions_mut().insert(ConnectInfo(peer));
    let res = harness.api.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::OK);

    // Disallowed host is rejected with 400 Bad Request
    let mut req = Request::get("/healthz")
        .header(header::HOST, "evil.attacker.org")
        .body(Body::empty())?;
    req.extensions_mut().insert(ConnectInfo(peer));
    let res = harness.api.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    Ok(())
}

#[tokio::test]
async fn cors_origin_allowlist_in_remote_mode() -> TestResult {
    let harness = ProxyHarness::new(
        vec!["127.0.0.1/32".parse()?],
        vec!["artifree.example.com".into()],
        vec!["https://dashboard.example.com".into()],
        Some("https://artifree.example.com".into()),
        "https://preview.example.com".into(),
        false,
    )?;

    // 1. Preflight from configured api_origin is permitted
    let req = Request::builder()
        .method(Method::OPTIONS)
        .uri("/v1/artifacts")
        .header(header::HOST, "artifree.example.com")
        .header(header::ORIGIN, "https://artifree.example.com")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
        .body(Body::empty())?;
    let res = harness.api.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        res.headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|v| v.to_str().ok()),
        Some("https://artifree.example.com")
    );

    // 2. Preflight from allowed_origins list is permitted
    let req = Request::builder()
        .method(Method::OPTIONS)
        .uri("/v1/artifacts")
        .header(header::HOST, "artifree.example.com")
        .header(header::ORIGIN, "https://dashboard.example.com")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
        .body(Body::empty())?;
    let res = harness.api.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    // 3. Preflight from preview origin is strictly rejected
    let req = Request::builder()
        .method(Method::OPTIONS)
        .uri("/v1/artifacts")
        .header(header::HOST, "artifree.example.com")
        .header(header::ORIGIN, "https://preview.example.com")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
        .body(Body::empty())?;
    let res = harness.api.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // 4. Preflight from attacker origin is rejected
    let req = Request::builder()
        .method(Method::OPTIONS)
        .uri("/v1/artifacts")
        .header(header::HOST, "artifree.example.com")
        .header(header::ORIGIN, "https://attacker.org")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
        .body(Body::empty())?;
    let res = harness.api.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // 5. Preflight from null origin is rejected
    let req = Request::builder()
        .method(Method::OPTIONS)
        .uri("/v1/artifacts")
        .header(header::HOST, "artifree.example.com")
        .header(header::ORIGIN, "null")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
        .body(Body::empty())?;
    let res = harness.api.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    Ok(())
}

#[tokio::test]
async fn mcp_loopback_confinement_behind_trusted_proxy() -> TestResult {
    let loopback_proxy_peer: SocketAddr = "127.0.0.1:44444".parse()?;

    // Harness A: allow_remote_mcp is FALSE (default)
    let harness_local_mcp = ProxyHarness::new(
        vec!["127.0.0.1/32".parse()?],
        vec!["artifree.example.com".into()],
        vec![],
        Some("https://artifree.example.com".into()),
        "https://preview.example.com".into(),
        false, // default: no remote MCP
    )?;

    // Remote client (203.0.113.195) forwarded via local proxy is rejected with 403 Forbidden!
    let mut req = Request::post("/mcp")
        .header(header::HOST, "artifree.example.com")
        .header(header::AUTHORIZATION, format!("Bearer {OWNER_TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-11-25")
        .header("x-forwarded-for", "203.0.113.195")
        .header("x-forwarded-host", "artifree.example.com")
        .body(Body::from(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        ))?;
    req.extensions_mut()
        .insert(ConnectInfo(loopback_proxy_peer));

    let res = harness_local_mcp.api.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // Injected/prepended leftmost loopback IP (127.0.0.1, 203.0.113.195) is rejected:
    // right-to-left traversal identifies 203.0.113.195 as untrusted client IP
    let mut req = Request::post("/mcp")
        .header(header::HOST, "artifree.example.com")
        .header(header::AUTHORIZATION, format!("Bearer {OWNER_TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-11-25")
        .header("x-forwarded-for", "127.0.0.1, 203.0.113.195")
        .header("x-forwarded-host", "artifree.example.com")
        .body(Body::from(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        ))?;
    req.extensions_mut()
        .insert(ConnectInfo(loopback_proxy_peer));

    let res = harness_local_mcp.api.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // Direct non-loopback client (198.51.100.5) is rejected
    let non_loopback_peer: SocketAddr = "198.51.100.5:12345".parse()?;
    let mut req = Request::post("/mcp")
        .header(header::HOST, "artifree.example.com")
        .header(header::AUTHORIZATION, format!("Bearer {OWNER_TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-11-25")
        .body(Body::from(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        ))?;
    req.extensions_mut().insert(ConnectInfo(non_loopback_peer));
    let res = harness_local_mcp.api.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // A chain made only of trusted proxies names no client (ADR 0024
    // amendment): a local client behind a local proxy is unknown, so 403.
    let mut req = Request::post("/mcp")
        .header(header::HOST, "artifree.example.com")
        .header(header::AUTHORIZATION, format!("Bearer {OWNER_TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-11-25")
        .header("x-forwarded-for", "127.0.0.1")
        .header("x-forwarded-host", "artifree.example.com")
        .body(Body::from(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        ))?;
    req.extensions_mut()
        .insert(ConnectInfo(loopback_proxy_peer));

    let res = harness_local_mcp.api.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // Harness B: allow_remote_mcp is TRUE
    let harness_remote_mcp = ProxyHarness::new(
        vec!["127.0.0.1/32".parse()?],
        vec!["artifree.example.com".into()],
        vec![],
        Some("https://artifree.example.com".into()),
        "https://preview.example.com".into(),
        true, // remote MCP explicitly allowed
    )?;

    // Remote client (203.0.113.195) forwarded via local proxy is now accepted
    let mut req = Request::post("/mcp")
        .header(header::HOST, "artifree.example.com")
        .header(header::AUTHORIZATION, format!("Bearer {OWNER_TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-11-25")
        .header("x-forwarded-for", "203.0.113.195")
        .header("x-forwarded-host", "artifree.example.com")
        .body(Body::from(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        ))?;
    req.extensions_mut()
        .insert(ConnectInfo(loopback_proxy_peer));

    let res = harness_remote_mcp.api.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::OK);

    Ok(())
}

/// Builds an authenticated `tools/list` request to `/mcp` from `peer`, with
/// the given extra headers.
fn mcp_request(
    peer: SocketAddr,
    extra: &[(&str, &str)],
) -> Result<Request<Body>, Box<dyn std::error::Error>> {
    let mut builder = Request::post("/mcp")
        .header(header::HOST, "artifree.example.com")
        .header(header::AUTHORIZATION, format!("Bearer {OWNER_TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-11-25");
    for (name, value) in extra {
        builder = builder.header(*name, *value);
    }
    let mut req = builder.body(Body::from(
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
    ))?;
    req.extensions_mut().insert(ConnectInfo(peer));
    Ok(req)
}

fn mcp_harness(allow_remote_mcp: bool) -> Result<ProxyHarness, Box<dyn std::error::Error>> {
    ProxyHarness::new(
        vec!["127.0.0.1/32".parse()?],
        vec!["artifree.example.com".into()],
        vec![],
        Some("https://artifree.example.com".into()),
        "https://preview.example.com".into(),
        allow_remote_mcp,
    )
}

#[tokio::test]
async fn mcp_rejects_trusted_proxy_request_without_forwarding_information() -> TestResult {
    // A same-host tunnel or proxy (peer 127.0.0.1, trusted) that strips the
    // visitor address leaves the real client unknown. That must not be
    // treated as a loopback client (ADR 0024 amendment).
    let peer: SocketAddr = "127.0.0.1:44444".parse()?;
    let harness = mcp_harness(false)?;

    let res = harness.api.clone().oneshot(mcp_request(peer, &[])?).await?;
    assert_eq!(res.status(), StatusCode::FORBIDDEN, "no headers at all");

    // Forwarded host/proto without a client address is still unknown.
    let res = harness
        .api
        .clone()
        .oneshot(mcp_request(
            peer,
            &[
                ("x-forwarded-host", "artifree.example.com"),
                ("x-forwarded-proto", "https"),
            ],
        )?)
        .await?;
    assert_eq!(res.status(), StatusCode::FORBIDDEN, "host/proto only");

    // The explicit remote opt-in still admits the unknown client.
    let remote = mcp_harness(true)?;
    let res = remote.api.clone().oneshot(mcp_request(peer, &[])?).await?;
    assert_eq!(res.status(), StatusCode::OK, "--allow-remote-mcp");

    Ok(())
}

#[tokio::test]
async fn mcp_allows_direct_loopback_client_without_proxy() -> TestResult {
    // No trusted proxies configured: the socket peer is the client.
    let harness = ProxyHarness::new(
        vec![],
        vec!["artifree.example.com".into()],
        vec![],
        Some("https://artifree.example.com".into()),
        "https://preview.example.com".into(),
        false,
    )?;
    let peer: SocketAddr = "127.0.0.1:44444".parse()?;
    let res = harness.api.clone().oneshot(mcp_request(peer, &[])?).await?;
    assert_eq!(res.status(), StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn untrusted_peer_forwarded_headers_rejected_on_mcp() -> TestResult {
    let harness = mcp_harness(false)?;
    let peer: SocketAddr = "198.51.100.5:12345".parse()?;
    let res = harness
        .api
        .clone()
        .oneshot(mcp_request(peer, &[("x-forwarded-for", "127.0.0.1")])?)
        .await?;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    Ok(())
}

#[tokio::test]
async fn mcp_reads_every_forwarded_for_line_behind_trusted_proxy() -> TestResult {
    // The client injects a loopback address on its own line; the trusted proxy
    // adds the real peer on a separate line instead of appending to it.
    let peer: SocketAddr = "127.0.0.1:44444".parse()?;
    let harness = mcp_harness(false)?;

    let res = harness
        .api
        .clone()
        .oneshot(mcp_request(
            peer,
            &[
                ("x-forwarded-for", "127.0.0.1"),
                ("x-forwarded-for", "203.0.113.195"),
            ],
        )?)
        .await?;
    assert_eq!(res.status(), StatusCode::FORBIDDEN, "two XFF lines");

    // Same shape with RFC 7239 `Forwarded`.
    let res = harness
        .api
        .clone()
        .oneshot(mcp_request(
            peer,
            &[
                ("forwarded", "for=127.0.0.1"),
                ("forwarded", "for=203.0.113.195"),
            ],
        )?)
        .await?;
    assert_eq!(res.status(), StatusCode::FORBIDDEN, "two Forwarded lines");

    // A proxy that appends to a single line still works for both outcomes.
    let res = harness
        .api
        .clone()
        .oneshot(mcp_request(
            peer,
            &[("x-forwarded-for", "127.0.0.1, 203.0.113.195")],
        )?)
        .await?;
    assert_eq!(res.status(), StatusCode::FORBIDDEN, "single line, remote");
    let res = harness
        .api
        .clone()
        .oneshot(mcp_request(peer, &[("x-forwarded-for", "127.0.0.1")])?)
        .await?;
    assert_eq!(
        res.status(),
        StatusCode::FORBIDDEN,
        "single line, all trusted"
    );

    // A non-ASCII line anywhere is still a 400.
    let mut req = mcp_request(peer, &[("x-forwarded-for", "203.0.113.195")])?;
    req.headers_mut().append(
        "x-forwarded-for",
        axum::http::HeaderValue::from_bytes(b"\xff")?,
    );
    let res = harness.api.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST, "non-ASCII line");

    Ok(())
}

#[tokio::test]
async fn mcp_rejects_chains_made_only_of_trusted_proxies() -> TestResult {
    // Tunnel -> local proxy -> artifree, with 127.0.0.1 trusted: an attacker
    // sends `X-Forwarded-For: 127.0.0.1` and the local proxy appends the
    // tunnel's 127.0.0.1. Nothing in the chain proves a loopback client.
    let peer: SocketAddr = "127.0.0.1:44444".parse()?;
    let harness = mcp_harness(false)?;
    for (name, value) in [
        ("x-forwarded-for", "127.0.0.1, 127.0.0.1"),
        ("forwarded", "for=127.0.0.1, for=127.0.0.1"),
    ] {
        let res = harness
            .api
            .clone()
            .oneshot(mcp_request(peer, &[(name, value)])?)
            .await?;
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "{name}: {value}");
    }
    Ok(())
}

#[tokio::test]
async fn mcp_rejects_disagreeing_xff_and_forwarded() -> TestResult {
    // The proxy sets only `Forwarded`; the attacker's own XFF is passed through.
    let peer: SocketAddr = "127.0.0.1:44444".parse()?;
    let harness = mcp_harness(false)?;
    let res = harness
        .api
        .clone()
        .oneshot(mcp_request(
            peer,
            &[
                ("x-forwarded-for", "127.0.0.1"),
                ("forwarded", "for=203.0.113.9"),
            ],
        )?)
        .await?;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // And the reverse: only XFF is trusted, a spoofed Forwarded says loopback.
    let res = harness
        .api
        .clone()
        .oneshot(mcp_request(
            peer,
            &[
                ("forwarded", "for=127.0.0.1"),
                ("x-forwarded-for", "203.0.113.9"),
            ],
        )?)
        .await?;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    Ok(())
}

#[tokio::test]
async fn mcp_rejects_disagreement_when_loopback_is_not_a_trusted_proxy() -> TestResult {
    // Trusted 10.0.0.0/8 does not contain loopback, so each header on its own
    // names a known client; only the disagreement rule can reject the spoof.
    let peer: SocketAddr = "10.0.0.5:44444".parse()?;
    let harness = ProxyHarness::new(
        vec!["10.0.0.0/8".parse()?],
        vec!["artifree.example.com".into()],
        vec![],
        Some("https://artifree.example.com".into()),
        "https://preview.example.com".into(),
        false,
    )?;
    for (xff, forwarded) in [
        ("127.0.0.1", "for=203.0.113.9"),
        ("203.0.113.9", "for=127.0.0.1"),
    ] {
        let res = harness
            .api
            .clone()
            .oneshot(mcp_request(
                peer,
                &[("x-forwarded-for", xff), ("forwarded", forwarded)],
            )?)
            .await?;
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "{xff} vs {forwarded}");
    }

    // Sanity: the same loopback client, named by a single header, is admitted.
    let res = harness
        .api
        .clone()
        .oneshot(mcp_request(peer, &[("x-forwarded-for", "127.0.0.1")])?)
        .await?;
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "single header, loopback client"
    );
    Ok(())
}

#[tokio::test]
async fn mcp_ipv6_and_mapped_loopback_clients() -> TestResult {
    let harness = mcp_harness(false)?;
    let proxy: SocketAddr = "127.0.0.1:44444".parse()?;
    let res = harness
        .api
        .clone()
        .oneshot(mcp_request(proxy, &[("x-forwarded-for", "[2001:db8::1]")])?)
        .await?;
    assert_eq!(res.status(), StatusCode::FORBIDDEN, "remote IPv6 via proxy");

    // Direct clients, no trusted proxy configured.
    let direct = ProxyHarness::new(
        vec![],
        vec!["artifree.example.com".into()],
        vec![],
        Some("https://artifree.example.com".into()),
        "https://preview.example.com".into(),
        false,
    )?;
    for peer in ["[::1]:44444", "[::ffff:127.0.0.1]:44444"] {
        let res = direct
            .api
            .clone()
            .oneshot(mcp_request(peer.parse()?, &[])?)
            .await?;
        assert_eq!(res.status(), StatusCode::OK, "direct {peer}");
    }
    let res = direct
        .api
        .clone()
        .oneshot(mcp_request("[::ffff:198.51.100.5]:1".parse()?, &[])?)
        .await?;
    assert_eq!(res.status(), StatusCode::FORBIDDEN, "mapped non-loopback");
    Ok(())
}

#[tokio::test]
async fn mcp_denies_request_without_peer_address() -> TestResult {
    // No socket peer is known: that must not be taken for loopback.
    let harness = ProxyHarness::new(
        vec![],
        vec!["artifree.example.com".into()],
        vec![],
        Some("https://artifree.example.com".into()),
        "https://preview.example.com".into(),
        false,
    )?;
    let mut req = mcp_request("127.0.0.1:1".parse()?, &[])?;
    req.extensions_mut().remove::<ConnectInfo<SocketAddr>>();
    let res = harness.api.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    Ok(())
}

#[tokio::test]
async fn api_origin_and_content_origin_conflict_rejected() -> TestResult {
    let temp_dir = std::env::temp_dir().join(format!("artifree-conflict-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(&temp_dir)?;
    let db_path = temp_dir.join("db.sqlite3");
    let store = SqliteStore::open(&db_path)?;
    let token_store = SqliteTokenStore::open(&db_path)?;
    let token = AccessToken::parse(OWNER_TOKEN).ok_or("invalid token")?;

    let settings = Settings {
        content_origin: "https://artifree.example.com".into(),
        api_origin: Some("https://artifree.example.com".into()),
        ..Settings::default()
    };

    let result = AppState::new(store, token_store, token, settings);
    let Err(err_msg) = result else {
        panic!("expected conflict error");
    };
    assert!(err_msg.contains("preview content origin must differ from API origin"));

    Ok(())
}
