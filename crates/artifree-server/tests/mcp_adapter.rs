//! MCP endpoint guards and owner checks, exercised in process through the real
//! router, MCP SDK, application core, and `SQLite` store. Tool behavior with
//! a real MCP client is covered by `scripts/tests/mcp.test.mjs`.
use std::fmt::Display;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use artifree_core::artifacts::ArtifactService;
use artifree_domain::identity::{ActorId, RequestKey, Title};
use artifree_domain::package::{FilePackage, PackagePath};
use artifree_providers::storage::SqliteStore;
use artifree_server::auth::AccessToken;
use artifree_server::changes;
use artifree_server::http::{self, AppState, Settings};
use artifree_server::limits::RequestLimits;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const PROTOCOL: &str = "2025-11-25";

fn parsed<T: FromStr>(value: &str) -> T
where
    T::Err: Display,
{
    value
        .parse()
        .unwrap_or_else(|error| panic!("invalid fixture {value:?}: {error}"))
}

struct Harness {
    directory: PathBuf,
    database: PathBuf,
    api: Router,
}

impl Harness {
    fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!(
            "artifree-mcp-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory)
            .unwrap_or_else(|error| panic!("cannot create test directory: {error}"));
        let database = directory.join("db.sqlite3");
        let store =
            SqliteStore::open(&database).unwrap_or_else(|error| panic!("cannot open: {error}"));
        let token = AccessToken::parse(TOKEN).unwrap_or_else(|| panic!("fixture token is invalid"));
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
        Self {
            directory,
            database,
            api: http::router(state),
        }
    }

    /// Stores an artifact owned by someone other than the token's actor.
    fn foreign_artifact(&self, key: &str) -> Result<String, Box<dyn std::error::Error>> {
        let service = ArtifactService::new(SqliteStore::open(&self.database)?);
        let package = FilePackage::new(vec![(PackagePath::entry(), b"<p>secret</p>".to_vec())])?;
        let receipt = service.create(
            &parsed::<ActorId>("someone-else"),
            &parsed::<RequestKey>(key),
            &parsed::<Title>("Private"),
            &package,
        )?;
        Ok(receipt.artifact.id.to_string())
    }

    async fn post(&self, request: Request<Body>) -> Result<(StatusCode, Vec<u8>), TestError> {
        let response = self.api.clone().oneshot(request).await?;
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await?.to_vec();
        Ok((status, body))
    }

    /// Calls a tool and returns the JSON-RPC result.
    async fn call(&self, name: &str, arguments: Value) -> Result<Value, TestError> {
        let (status, body) = self
            .post(rpc(
                Some(TOKEN),
                &json!({
                    "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": { "name": name, "arguments": arguments }
                }),
            )?)
            .await?;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let reply: Value = serde_json::from_slice(&body)?;
        Ok(reply["result"].clone())
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

type TestError = Box<dyn std::error::Error>;

fn rpc(token: Option<&str>, message: &Value) -> Result<Request<Body>, axum::http::Error> {
    let mut builder = Request::post("/mcp")
        .header(header::HOST, "127.0.0.1:7410")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("mcp-protocol-version", PROTOCOL);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    builder.body(Body::from(message.to_string()))
}

fn list_tools() -> Value {
    json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })
}

fn error_code(result: &Value) -> &str {
    assert_eq!(result["isError"], true, "expected a tool error: {result}");
    result["structuredContent"]["error"]["code"]
        .as_str()
        .unwrap_or("")
}

#[tokio::test]
async fn the_endpoint_authenticates_before_anything_else() -> TestResult {
    let harness = Harness::new();
    let wrong = "f".repeat(64);
    for token in [None, Some(wrong.as_str())] {
        // A disallowed Host and Origin still answer 401: callers without the
        // token learn nothing about the MCP layer behind it.
        let mut request = rpc(token, &list_tools())?;
        request
            .headers_mut()
            .insert(header::HOST, "attacker.example".parse()?);
        request
            .headers_mut()
            .insert(header::ORIGIN, "http://attacker.example".parse()?);
        let response = harness.api.clone().oneshot(request).await?;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
        let body = to_bytes(response.into_body(), usize::MAX).await?;
        let body: Value = serde_json::from_slice(&body)?;
        assert_eq!(body["error"]["code"], "unauthenticated");
    }
    let (status, _) = harness.post(rpc(Some(TOKEN), &list_tools())?).await?;
    assert_eq!(status, StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn browser_origins_and_foreign_hosts_are_refused() -> TestResult {
    let harness = Harness::new();
    let mut from_page = rpc(Some(TOKEN), &list_tools())?;
    from_page
        .headers_mut()
        .insert(header::ORIGIN, "http://127.0.0.1:5173".parse()?);
    assert_eq!(harness.post(from_page).await?.0, StatusCode::FORBIDDEN);
    // DNS rebinding: a page on another name resolving to loopback.
    let mut rebound = rpc(Some(TOKEN), &list_tools())?;
    rebound
        .headers_mut()
        .insert(header::HOST, "attacker.example:7410".parse()?);
    assert_eq!(harness.post(rebound).await?.0, StatusCode::FORBIDDEN);
    let stream = Request::get("/mcp")
        .header(header::HOST, "127.0.0.1:7410")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::ACCEPT, "text/event-stream")
        .body(Body::empty())?;
    let response = harness.api.clone().oneshot(stream).await?;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    Ok(())
}

#[tokio::test]
async fn another_owners_artifact_is_not_found_through_every_tool() -> TestResult {
    let harness = Harness::new();
    let id = harness.foreign_artifact("shared-key-1")?;
    let listed = harness.call("list_artifacts", json!({})).await?;
    assert_eq!(listed["structuredContent"]["artifacts"], json!([]));
    let file = json!({ "artifact_id": id, "revision": 1, "path": "index.html" });
    let pinned = json!({ "artifact_id": id, "revision": 1 });
    for (tool, arguments) in [
        ("get_artifact", json!({ "artifact_id": id })),
        ("list_revisions", json!({ "artifact_id": id })),
        ("get_revision", pinned.clone()),
        ("read_file", file),
        ("create_preview", pinned),
        (
            "update_artifact",
            json!({
                "artifact_id": id, "base_metadata_revision": 1, "title": "Mine now",
                "idempotency_key": "mine-key-2"
            }),
        ),
        (
            "save_revision",
            json!({
                "artifact_id": id, "base_revision": 1, "idempotency_key": "mine-key-1",
                "changes": [{ "op": "delete", "path": "index.html" }]
            }),
        ),
    ] {
        let result = harness.call(tool, arguments).await?;
        assert_eq!(error_code(&result), "not_found", "{tool}: {result}");
        assert!(
            !result.to_string().contains("secret"),
            "{tool} leaked content"
        );
    }
    // Retry keys are scoped to the actor: another owner's key is free here.
    let created = harness
        .call(
            "create_artifact",
            json!({
                "title": "Mine", "idempotency_key": "shared-key-1",
                "files": [{ "path": "index.html", "encoding": "utf8", "content": "<p>mine</p>" }]
            }),
        )
        .await?;
    assert_eq!(created["isError"], false, "{created}");
    assert_eq!(created["structuredContent"]["replayed"], false);
    assert_ne!(created["structuredContent"]["artifact"]["id"], json!(id));
    Ok(())
}

#[tokio::test]
async fn an_unknown_tool_is_a_protocol_error() -> TestResult {
    let harness = Harness::new();
    let (status, body) = harness
        .post(rpc(
            Some(TOKEN),
            &json!({
                "jsonrpc": "2.0", "id": 7, "method": "tools/call",
                "params": { "name": "delete_everything", "arguments": {} }
            }),
        )?)
        .await?;
    assert_eq!(status, StatusCode::OK);
    let reply: Value = serde_json::from_slice(&body)?;
    assert_eq!(reply["error"]["code"], -32602, "{reply}");
    assert!(reply.get("result").is_none());
    Ok(())
}

#[tokio::test]
async fn agents_rename_pin_and_page_through_artifacts() -> TestResult {
    let harness = Harness::new();
    let mut ids = Vec::new();
    for (index, title) in ["First", "Second", "Third"].into_iter().enumerate() {
        tokio::time::sleep(Duration::from_millis(3)).await;
        let created = harness
            .call(
                "create_artifact",
                json!({
                    "title": title, "idempotency_key": format!("agent-create-{index}"),
                    "files": [{ "path": "index.html", "encoding": "utf8", "content": "<p>x</p>" }]
                }),
            )
            .await?;
        assert_eq!(created["isError"], false, "{created}");
        ids.push(created["structuredContent"]["artifact"]["id"].clone());
    }
    let update = json!({
        "artifact_id": ids[0], "base_metadata_revision": 1, "title": "First, renamed",
        "pinned": true, "idempotency_key": "agent-update-1"
    });
    let updated = harness.call("update_artifact", update.clone()).await?;
    assert_eq!(updated["isError"], false, "{updated}");
    let artifact = &updated["structuredContent"]["artifact"];
    assert_eq!(
        (artifact["title"].clone(), artifact["pinned"].clone()),
        (json!("First, renamed"), json!(true))
    );
    assert_eq!(artifact["current_revision"], 1, "no revision is committed");
    let replayed = harness.call("update_artifact", update).await?;
    assert_eq!(replayed["structuredContent"]["replayed"], true);
    let stale = harness
        .call(
            "update_artifact",
            json!({
                "artifact_id": ids[0], "base_metadata_revision": 1, "pinned": false,
                "idempotency_key": "agent-update-2"
            }),
        )
        .await?;
    assert_eq!(error_code(&stale), "metadata_conflict");
    assert_eq!(
        stale["structuredContent"]["error"]["current_metadata_revision"],
        2
    );

    let first = harness
        .call("list_artifacts", json!({ "limit": 2 }))
        .await?;
    let page = &first["structuredContent"];
    assert_eq!(page["artifacts"].as_array().map(Vec::len), Some(2));
    let rest = harness
        .call(
            "list_artifacts",
            json!({ "limit": 2, "page_token": page["next_page_token"] }),
        )
        .await?;
    assert_eq!(rest["structuredContent"]["artifacts"][0]["id"], ids[0]);
    assert_eq!(rest["structuredContent"].get("next_page_token"), None);
    let pinned = harness
        .call("list_artifacts", json!({ "pinned": true, "q": "renamed" }))
        .await?;
    assert_eq!(pinned["structuredContent"]["artifacts"][0]["id"], ids[0]);
    let invalid = harness
        .call("list_artifacts", json!({ "limit": 0 }))
        .await?;
    assert_eq!(error_code(&invalid), "invalid_request");
    Ok(())
}
