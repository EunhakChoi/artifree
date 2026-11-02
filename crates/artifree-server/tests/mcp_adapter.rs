//! MCP endpoint guards and owner checks, exercised in process through the real
//! router, MCP SDK, application core, and `SQLite` store. Tool behavior with
//! a real MCP client is covered by `scripts/tests/mcp.test.mjs`.
use std::fmt::Display;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use artifree_core::artifacts::ArtifactService;
use artifree_domain::SourceRevision;
use artifree_domain::identity::{ActorId, RequestKey, Title, UnixMillis};
use artifree_domain::job::JobKind;
use artifree_domain::package::{FilePackage, PackagePath};
use artifree_providers::storage::SqliteStore;
use artifree_server::auth::AccessToken;
use artifree_server::changes;
use artifree_server::http::{self, AppState, Settings};
use artifree_server::limits::RequestLimits;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
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
        let token_store = artifree_providers::tokens::SqliteTokenStore::open(&database)
            .unwrap_or_else(|error| panic!("cannot open token store: {error}"));
        let token = AccessToken::parse(TOKEN).unwrap_or_else(|| panic!("fixture token is invalid"));
        let state = AppState::new(
            store,
            token_store,
            token,
            Settings {
                content_origin: "http://127.0.0.1:7411".into(),
                preview_ttl: Duration::from_secs(60),
                frame_ancestors: "http://127.0.0.1:*".into(),
                limits: RequestLimits::default(),
                change_streams: changes::DEFAULT_STREAMS_PER_ACTOR,
                ..Settings::default()
            },
        )
        .unwrap_or_else(|error| panic!("cannot build state: {error}"));
        Self {
            directory,
            database,
            // The real server records the socket peer; with none, the client
            // is unknown and /mcp is refused, so tests connect from loopback.
            api: http::router(state).layer(axum::Extension(ConnectInfo(SocketAddr::from((
                [127, 0, 0, 1],
                40_000,
            ))))),
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

/// A purge and the content cleanup cannot be undone (decision 0029), so no
/// agent tool offers them: not in the catalog, and not callable by name, even
/// with the owner's own token.
#[tokio::test]
async fn no_tool_purges_or_cleans_up() -> TestResult {
    let harness = Harness::new();
    let (status, body) = harness.post(rpc(Some(TOKEN), &list_tools())?).await?;
    assert_eq!(status, StatusCode::OK);
    let listed: Value = serde_json::from_slice(&body)?;
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .ok_or("tools")?
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(names.contains(&"update_artifact"), "{names:?}");
    for name in &names {
        for word in ["purge", "cleanup", "clean_up", "unreferenced", "erase"] {
            assert!(!name.contains(word), "tool {name} must not be offered");
        }
    }
    for name in [
        "purge_artifact",
        "cleanup_content",
        "remove_unreferenced_content",
    ] {
        let (status, body) = harness
            .post(rpc(
                Some(TOKEN),
                &json!({
                    "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": { "name": name, "arguments": {} }
                }),
            )?)
            .await?;
        let reply: Value = serde_json::from_slice(&body)?;
        let refused = reply.get("error").is_some() || reply["result"]["isError"] == true;
        assert!(refused, "{name} must not be a tool: {status} {reply}");
    }
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
    let jobs = harness.call("list_jobs", json!({})).await?;
    assert_eq!(jobs["structuredContent"]["jobs"], json!([]), "{jobs}");
    let file = json!({ "artifact_id": id, "revision": 1, "path": "index.html" });
    let pinned = json!({ "artifact_id": id, "revision": 1 });
    let foreign_job = ArtifactService::new(SqliteStore::open(&harness.database)?)
        .enqueue_job(
            &parsed::<ActorId>("someone-else"),
            &parsed::<RequestKey>("foreign-job-key"),
            &parsed(&id),
            SourceRevision(1),
            JobKind::VerifyRevision,
            UnixMillis(1),
        )?
        .job
        .id
        .to_string();
    for (tool, arguments) in [
        (
            "enqueue_job",
            json!({
                "artifact_id": id, "revision": 1, "kind": "verify_revision",
                "idempotency_key": "mine-job-key"
            }),
        ),
        ("get_job", json!({ "job_id": foreign_job })),
        ("list_jobs", json!({ "artifact_id": id })),
        (
            "cancel_job",
            json!({ "job_id": foreign_job, "idempotency_key": "mine-job-cancel" }),
        ),
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
        (
            "restore_revision",
            json!({
                "artifact_id": id, "revision": 1, "base_revision": 1,
                "idempotency_key": "mine-key-3"
            }),
        ),
        (
            "duplicate_artifact",
            json!({ "artifact_id": id, "revision": 1, "idempotency_key": "mine-key-4" }),
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
async fn another_owners_data_is_not_found_through_every_data_tool() -> TestResult {
    let harness = Harness::new();
    let id = harness.foreign_artifact("shared-key-1")?;
    for (tool, arguments) in [
        ("get_data", json!({ "artifact_id": id })),
        (
            "set_data",
            json!({
                "artifact_id": id, "path": "a", "base_data_version": 0, "value": "1",
                "idempotency_key": "mine-data-1"
            }),
        ),
        (
            "update_data",
            json!({
                "artifact_id": id, "path": "a", "base_data_version": 0, "patch": "{}",
                "idempotency_key": "mine-data-2"
            }),
        ),
        (
            "delete_data",
            json!({
                "artifact_id": id, "path": "a", "base_data_version": 0,
                "idempotency_key": "mine-data-3"
            }),
        ),
    ] {
        let result = harness.call(tool, arguments).await?;
        assert_eq!(error_code(&result), "not_found", "{tool}: {result}");
    }
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

#[tokio::test]
async fn agents_enqueue_read_list_and_cancel_jobs_with_the_http_semantics() -> TestResult {
    let harness = Harness::new();
    let created = harness
        .call(
            "create_artifact",
            json!({
                "title": "Page", "idempotency_key": "job-create-1",
                "files": [{ "path": "index.html", "encoding": "utf8", "content": "<p>x</p>" }]
            }),
        )
        .await?;
    let artifact = created["structuredContent"]["artifact"]["id"].clone();
    let enqueue = json!({
        "artifact_id": artifact, "revision": 1, "kind": "verify_revision",
        "idempotency_key": "job-enqueue-1"
    });

    let queued = harness.call("enqueue_job", enqueue.clone()).await?;
    assert_eq!(queued["isError"], false, "{queued}");
    let job = &queued["structuredContent"]["job"];
    assert_eq!(
        (
            job["state"].clone(),
            job["revision"].clone(),
            job["attempt"].clone()
        ),
        (json!("queued"), json!(1), json!(0))
    );
    assert_eq!(queued["structuredContent"]["replayed"], false);
    let replay = harness.call("enqueue_job", enqueue).await?;
    assert_eq!(replay["structuredContent"]["replayed"], true);
    assert_eq!(replay["structuredContent"]["job"]["id"], job["id"]);

    let shown = harness
        .call("get_job", json!({ "job_id": job["id"] }))
        .await?;
    assert_eq!(shown["structuredContent"]["state"], "queued");
    let listed = harness
        .call("list_jobs", json!({ "artifact_id": artifact }))
        .await?;
    assert_eq!(listed["structuredContent"]["jobs"][0]["id"], job["id"]);
    let everything = harness.call("list_jobs", json!({ "limit": 1 })).await?;
    assert_eq!(
        everything["structuredContent"]["jobs"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );

    let cancelled = harness
        .call(
            "cancel_job",
            json!({ "job_id": job["id"], "idempotency_key": "job-cancel-1" }),
        )
        .await?;
    assert_eq!(
        cancelled["structuredContent"]["job"]["state"], "cancelled",
        "{cancelled}"
    );
    let ended = harness
        .call(
            "cancel_job",
            json!({ "job_id": job["id"], "idempotency_key": "job-cancel-2" }),
        )
        .await?;
    assert_eq!(error_code(&ended), "job_ended");
    let gone = harness
        .call("get_job", json!({ "job_id": "job_zzzz-not-a-job" }))
        .await?;
    assert_eq!(error_code(&gone), "not_found");
    assert!(!gone.to_string().contains("zzzz"), "{gone}");
    Ok(())
}

#[tokio::test]
async fn job_requests_that_cannot_be_served_get_the_http_error_codes() -> TestResult {
    let harness = Harness::new();
    let mut artifacts = Vec::new();
    for (name, key) in [("Page", "job-create-1"), ("Other", "job-create-2")] {
        let created = harness
            .call(
                "create_artifact",
                json!({
                    "title": name, "idempotency_key": key,
                    "files": [{ "path": "index.html", "encoding": "utf8", "content": "<p>x</p>" }]
                }),
            )
            .await?;
        artifacts.push(created["structuredContent"]["artifact"]["id"].clone());
    }
    let (artifact, other) = (&artifacts[0], &artifacts[1]);
    let first = harness
        .call(
            "enqueue_job",
            json!({
                "artifact_id": artifact, "revision": 1, "kind": "verify_revision",
                "idempotency_key": "job-enqueue-1"
            }),
        )
        .await?;
    assert_eq!(first["isError"], false, "{first}");
    for (arguments, code) in [
        // The key serves one request only.
        (
            json!({ "artifact_id": artifact, "revision": 1, "kind": "verify_revision",
                    "idempotency_key": "job-enqueue-1", "extra": 1 }),
            "invalid_request",
        ),
        (
            json!({ "artifact_id": other, "revision": 1,
                    "kind": "verify_revision", "idempotency_key": "job-enqueue-1" }),
            "request_key_reused",
        ),
        (
            json!({ "artifact_id": artifact, "revision": 9, "kind": "verify_revision",
                    "idempotency_key": "job-enqueue-2" }),
            "not_found",
        ),
        (
            json!({ "artifact_id": artifact, "revision": 1, "kind": "build",
                    "idempotency_key": "job-enqueue-3" }),
            "invalid_request",
        ),
        (
            json!({ "artifact_id": artifact, "revision": 1, "kind": "verify_revision" }),
            "invalid_request",
        ),
    ] {
        let refused = harness.call("enqueue_job", arguments.clone()).await?;
        assert_eq!(error_code(&refused), code, "{arguments}: {refused}");
    }

    Ok(())
}

#[tokio::test]
async fn the_job_tools_say_what_they_change() -> TestResult {
    let harness = Harness::new();
    let (_, body) = harness.post(rpc(Some(TOKEN), &list_tools())?).await?;
    let listed: Value = serde_json::from_slice(&body)?;
    let tools = listed["result"]["tools"].as_array().ok_or("tools")?;
    for (name, read_only) in [
        ("enqueue_job", false),
        ("get_job", true),
        ("list_jobs", true),
        ("cancel_job", false),
    ] {
        let tool = tools
            .iter()
            .find(|tool| tool["name"] == name)
            .ok_or_else(|| format!("{name} is not offered"))?;
        assert_eq!(tool["annotations"]["readOnlyHint"], read_only, "{name}");
        assert_eq!(tool["annotations"]["destructiveHint"], false, "{name}");
        assert_eq!(tool["annotations"]["openWorldHint"], false, "{name}");
        let required = tool["inputSchema"]["required"].to_string();
        assert_eq!(required.contains("idempotency_key"), !read_only, "{name}");
    }
    Ok(())
}

/// The tools of application data (decision 0031) answer as the HTTP routes do,
/// and take their values as JSON text so that a repeated member name is seen.
#[tokio::test]
async fn agents_read_and_write_application_data_with_the_http_semantics() -> TestResult {
    let harness = Harness::new();
    let created = harness
        .call(
            "create_artifact",
            json!({
                "title": "Page", "idempotency_key": "data-create-1",
                "files": [{ "path": "index.html", "encoding": "utf8", "content": "<p>x</p>" }]
            }),
        )
        .await?;
    let artifact = created["structuredContent"]["artifact"]["id"].clone();

    let empty = harness
        .call("get_data", json!({ "artifact_id": artifact }))
        .await?;
    assert_eq!(
        empty["structuredContent"],
        json!({ "artifact_id": artifact, "path": "", "data_version": 0, "exists": true, "value": {} })
    );

    let set = json!({
        "artifact_id": artifact, "path": "todos/first", "base_data_version": 0,
        "value": "{\"done\":false,\"tags\":[\"a\"]}", "idempotency_key": "data-set-1"
    });
    let written = harness.call("set_data", set.clone()).await?;
    assert_eq!(written["isError"], false, "{written}");
    assert_eq!(
        written["structuredContent"],
        json!({ "artifact_id": artifact, "path": "todos/first", "data_version": 1,
                "changed": true, "replayed": false })
    );
    let replay = harness.call("set_data", set).await?;
    assert_eq!(replay["structuredContent"]["replayed"], true, "{replay}");

    let patched = harness
        .call(
            "update_data",
            json!({
                "artifact_id": artifact, "path": "todos/first", "base_data_version": 1,
                "patch": "{\"done\":true,\"tags\":null}", "idempotency_key": "data-update-1"
            }),
        )
        .await?;
    assert_eq!(patched["structuredContent"]["data_version"], 2, "{patched}");
    let read = harness
        .call(
            "get_data",
            json!({ "artifact_id": artifact, "path": "todos" }),
        )
        .await?;
    assert_eq!(
        read["structuredContent"]["value"],
        json!({ "first": { "done": true } })
    );

    let stale = harness
        .call(
            "set_data",
            json!({
                "artifact_id": artifact, "path": "todos/first", "base_data_version": 1,
                "value": "1", "idempotency_key": "data-set-stale"
            }),
        )
        .await?;
    assert_eq!(error_code(&stale), "data_conflict");
    assert_eq!(
        stale["structuredContent"]["error"]["current_data_version"], 2,
        "{stale}"
    );

    let removed = harness
        .call(
            "delete_data",
            json!({
                "artifact_id": artifact, "path": "todos/first", "base_data_version": 2,
                "idempotency_key": "data-delete-1"
            }),
        )
        .await?;
    assert_eq!(removed["structuredContent"]["data_version"], 3, "{removed}");
    let gone = harness
        .call(
            "get_data",
            json!({ "artifact_id": artifact, "path": "todos/first" }),
        )
        .await?;
    assert_eq!(gone["structuredContent"]["exists"], false);
    assert!(gone["structuredContent"].get("value").is_none(), "{gone}");
    Ok(())
}

#[tokio::test]
async fn data_requests_that_cannot_be_served_get_the_http_error_codes() -> TestResult {
    let harness = Harness::new();
    let created = harness
        .call(
            "create_artifact",
            json!({
                "title": "Page", "idempotency_key": "data-create-1",
                "files": [{ "path": "index.html", "encoding": "utf8", "content": "<p>x</p>" }]
            }),
        )
        .await?;
    let artifact = created["structuredContent"]["artifact"]["id"].clone();
    let first = harness
        .call(
            "set_data",
            json!({
                "artifact_id": artifact, "path": "a", "base_data_version": 0, "value": "5",
                "idempotency_key": "data-set-1"
            }),
        )
        .await?;
    assert_eq!(first["isError"], false, "{first}");
    for (tool, arguments, code) in [
        (
            "set_data",
            json!({ "artifact_id": artifact, "path": "a", "base_data_version": 0, "value": "6",
                    "idempotency_key": "data-set-1" }),
            "request_key_reused",
        ),
        (
            "set_data",
            json!({ "artifact_id": artifact, "path": "a/b", "base_data_version": 1, "value": "1",
                    "idempotency_key": "data-set-3" }),
            "data_path_conflict",
        ),
        (
            "set_data",
            json!({ "artifact_id": artifact, "path": "b", "base_data_version": 1,
                    "value": "{\"x\":1,\"x\":2}", "idempotency_key": "data-set-4" }),
            "invalid_request",
        ),
        (
            "set_data",
            json!({ "artifact_id": artifact, "path": "b", "base_data_version": 1,
                    "value": "9007199254740993", "idempotency_key": "data-set-5" }),
            "invalid_request",
        ),
        (
            "set_data",
            json!({ "artifact_id": artifact, "path": "b", "base_data_version": 1,
                    "value": format!("{}1{}", "[".repeat(100), "]".repeat(100)), "idempotency_key": "data-set-6" }),
            "data_too_large",
        ),
        (
            "set_data",
            json!({ "artifact_id": artifact, "path": "b", "base_data_version": 1,
                    "value": {"x": 1}, "idempotency_key": "data-set-7" }),
            "invalid_request",
        ),
        (
            "set_data",
            json!({ "artifact_id": artifact, "path": "../b", "base_data_version": 1,
                    "value": "1", "idempotency_key": "data-set-8" }),
            "invalid_request",
        ),
        (
            "update_data",
            json!({ "artifact_id": artifact, "path": "", "base_data_version": 1,
                    "patch": "[1]", "idempotency_key": "data-update-1" }),
            "invalid_request",
        ),
        (
            "delete_data",
            json!({ "artifact_id": artifact, "path": "a", "base_data_version": 1 }),
            "invalid_request",
        ),
        (
            "get_data",
            json!({ "artifact_id": "art_00000000000000000000000000000000" }),
            "not_found",
        ),
    ] {
        let refused = harness.call(tool, arguments.clone()).await?;
        assert_eq!(error_code(&refused), code, "{tool} {arguments}: {refused}");
    }
    Ok(())
}

/// Only the tools that overwrite data without keeping it are marked
/// destructive; the rest of the catalog keeps its hints.
#[tokio::test]
async fn the_data_tools_say_what_they_change() -> TestResult {
    let harness = Harness::new();
    let (_, body) = harness.post(rpc(Some(TOKEN), &list_tools())?).await?;
    let listed: Value = serde_json::from_slice(&body)?;
    let tools = listed["result"]["tools"].as_array().ok_or("tools")?;
    for (name, read_only) in [
        ("get_data", true),
        ("set_data", false),
        ("update_data", false),
        ("delete_data", false),
    ] {
        let tool = tools
            .iter()
            .find(|tool| tool["name"] == name)
            .ok_or_else(|| format!("{name} is not offered"))?;
        assert_eq!(tool["annotations"]["readOnlyHint"], read_only, "{name}");
        assert_eq!(tool["annotations"]["destructiveHint"], !read_only, "{name}");
        assert_eq!(tool["annotations"]["openWorldHint"], false, "{name}");
        let required = tool["inputSchema"]["required"].to_string();
        assert_eq!(required.contains("idempotency_key"), !read_only, "{name}");
    }
    let destructive: Vec<&str> = tools
        .iter()
        .filter(|tool| tool["annotations"]["destructiveHint"] == true)
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert_eq!(destructive, ["set_data", "update_data", "delete_data"]);
    Ok(())
}

/// Duplicating with `copy_data` copies the data in the same call, as over HTTP.
#[tokio::test]
async fn duplicate_artifact_copies_data_only_when_asked() -> TestResult {
    let harness = Harness::new();
    let created = harness
        .call(
            "create_artifact",
            json!({
                "title": "Page", "idempotency_key": "dup-create-1",
                "files": [{ "path": "index.html", "encoding": "utf8", "content": "<p>x</p>" }]
            }),
        )
        .await?;
    let artifact = created["structuredContent"]["artifact"]["id"].clone();
    harness
        .call(
            "set_data",
            json!({
                "artifact_id": artifact, "path": "a", "base_data_version": 0, "value": "1",
                "idempotency_key": "dup-set-1"
            }),
        )
        .await?;
    let plain = harness
        .call(
            "duplicate_artifact",
            json!({ "artifact_id": artifact, "revision": 1, "idempotency_key": "dup-plain-1" }),
        )
        .await?;
    assert_eq!(
        plain["structuredContent"]["copied"],
        json!(["files"]),
        "{plain}"
    );
    let with = harness
        .call(
            "duplicate_artifact",
            json!({ "artifact_id": artifact, "revision": 1, "copy_data": true,
                    "idempotency_key": "dup-data-1" }),
        )
        .await?;
    assert_eq!(
        with["structuredContent"]["copied"],
        json!(["files", "data"]),
        "{with}"
    );
    let copy = with["structuredContent"]["artifact"]["id"].clone();
    let copied = harness
        .call("get_data", json!({ "artifact_id": copy }))
        .await?;
    assert_eq!(copied["structuredContent"]["value"], json!({ "a": 1 }));
    let uncopied = harness
        .call(
            "get_data",
            json!({ "artifact_id": plain["structuredContent"]["artifact"]["id"] }),
        )
        .await?;
    assert_eq!(uncopied["structuredContent"]["data_version"], 0);
    Ok(())
}
