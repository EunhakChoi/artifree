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
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
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
        self.call_as(TOKEN, name, arguments).await
    }

    /// Calls a tool with `token` and returns the JSON-RPC result.
    async fn call_as(&self, token: &str, name: &str, arguments: Value) -> Result<Value, TestError> {
        let (status, body) = self
            .post(rpc(
                Some(token),
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

    /// Sends a request with the owner's token to the HTTP API; returns the
    /// status and the body.
    async fn owner_request(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Result<(StatusCode, Vec<u8>), TestError> {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .header(header::CONTENT_TYPE, "application/json");
        let request = match body {
            Some(body) => request.body(Body::from(body.to_string()))?,
            None => request.body(Body::empty())?,
        };
        self.post(request).await
    }

    /// Registers a user as the owner would, and returns a token that acts as
    /// that user.
    async fn user_token(&self, label: &str) -> Result<String, TestError> {
        let (status, body) = self
            .owner_request("POST", "/v1/users", Some(json!({ "label": label })))
            .await?;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "{}",
            String::from_utf8_lossy(&body)
        );
        let user: Value = serde_json::from_slice(&body)?;
        let (status, body) = self
            .owner_request(
                "POST",
                "/v1/tokens",
                Some(json!({ "name": label, "user_id": user["id"] })),
            )
            .await?;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "{}",
            String::from_utf8_lossy(&body)
        );
        let issued: Value = serde_json::from_slice(&body)?;
        Ok(issued["secret"].as_str().ok_or("secret")?.to_owned())
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
            "edit_files",
            json!({
                "artifact_id": id, "base_revision": 1, "idempotency_key": "mine-key-5",
                "edits": [{
                    "op": "put", "path": "index.html", "encoding": "utf8",
                    "content": "x", "expect": "absent"
                }]
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

/// `edit_files` answers as `POST /v1/artifacts/{id}/edits` does (decision 0035).
#[tokio::test]
async fn agents_edit_files_with_the_http_semantics() -> TestResult {
    let harness = Harness::new();
    let created = harness
        .call(
            "create_artifact",
            json!({
                "title": "Page", "idempotency_key": "edit-create-1",
                "files": [
                    { "path": "index.html", "encoding": "utf8", "content": "<p>1</p>" },
                    { "path": "notes.txt", "encoding": "utf8", "content": "n1" }
                ]
            }),
        )
        .await?;
    let id = created["structuredContent"]["artifact"]["id"].clone();
    let notes = created["structuredContent"]["revision"]["files"]
        .as_array()
        .and_then(|files| files.iter().find(|file| file["path"] == "notes.txt"))
        .map(|file| file["sha256"].clone())
        .ok_or("notes.txt is listed")?;
    let index = created["structuredContent"]["revision"]["files"]
        .as_array()
        .and_then(|files| files.iter().find(|file| file["path"] == "index.html"))
        .map(|file| file["sha256"].clone())
        .ok_or("index.html is listed")?;
    let moved = harness
        .call(
            "save_revision",
            json!({
                "artifact_id": id, "base_revision": 1, "idempotency_key": "edit-save-1",
                "changes": [{ "op": "put", "path": "index.html", "encoding": "utf8", "content": "<p>2</p>" }]
            }),
        )
        .await?;
    assert_eq!(moved["isError"], false, "{moved}");
    let edit = |key: &str, path: &str, expect: &Value| {
        json!({
            "artifact_id": id, "base_revision": 1, "idempotency_key": key,
            "edits": [{
                "op": "put", "path": path, "encoding": "utf8", "content": "changed",
                "expect": expect
            }]
        })
    };

    let stale_index = harness
        .call("edit_files", edit("edit-key-1", "index.html", &index))
        .await?;
    assert_eq!(error_code(&stale_index), "file_conflict", "{stale_index}");
    let error = &stale_index["structuredContent"]["error"];
    assert_eq!(error["current_revision"], 2);
    assert_eq!(error["conflicting_files_total"], 1);
    assert_eq!(error["conflicting_files"][0]["path"], "index.html");
    assert_ne!(error["conflicting_files"][0]["current_sha256"], index);

    // The same stale base, a file nobody touched: the edit applies.
    let call = edit("edit-key-2", "notes.txt", &notes);
    let edited = harness.call("edit_files", call.clone()).await?;
    assert_eq!(edited["isError"], false, "{edited}");
    assert_eq!(edited["structuredContent"]["revision"]["revision"], 3);
    assert_eq!(edited["structuredContent"]["replayed"], false);
    let replayed = harness.call("edit_files", call).await?;
    assert_eq!(replayed["structuredContent"]["replayed"], true);

    let strict = harness
        .call(
            "save_revision",
            json!({
                "artifact_id": id, "base_revision": 1, "idempotency_key": "edit-save-2",
                "changes": [{ "op": "put", "path": "notes.txt", "encoding": "utf8", "content": "x" }]
            }),
        )
        .await?;
    assert_eq!(error_code(&strict), "revision_conflict", "{strict}");

    for (what, arguments) in [
        (
            "a digest in upper case",
            edit("edit-key-3", "notes.txt", &json!("A".repeat(64))),
        ),
        (
            "no edit",
            json!({
                "artifact_id": id, "base_revision": 1, "idempotency_key": "edit-key-4", "edits": []
            }),
        ),
    ] {
        let refused = harness.call("edit_files", arguments).await?;
        assert_eq!(error_code(&refused), "invalid_request", "{what}: {refused}");
    }
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
        ("edit_files", false),
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

/// Only the writes that replace or remove data or assets without keeping the
/// earlier state are marked destructive: the data writes and the asset writes,
/// and no other tool. The data tools also say so in their own hints.
#[tokio::test]
async fn only_data_and_asset_writes_are_destructive() -> TestResult {
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
    assert_eq!(
        destructive,
        [
            "set_data",
            "update_data",
            "delete_data",
            "upload_asset",
            "delete_asset"
        ]
    );
    Ok(())
}

#[tokio::test]
async fn data_tools_preserve_unicode_identity_without_url_decoding() -> TestResult {
    let harness = Harness::new();
    let created = harness
        .call(
            "create_artifact",
            json!({
                "title": "Unicode data", "idempotency_key": "unicode-create",
                "files": [{"path": "index.html", "encoding": "utf8", "content": "<p>Unicode</p>"}]
            }),
        )
        .await?;
    let id = created["structuredContent"]["artifact"]["id"]
        .as_str()
        .ok_or("artifact id")?;
    let written = harness
        .call(
            "set_data",
            json!({
                "artifact_id": id, "path": "한글/e\u{301}", "base_data_version": 0,
                "value": "7", "idempotency_key": "unicode-set"
            }),
        )
        .await?;
    assert_eq!(
        written["structuredContent"]["path"], "한글/e\u{301}",
        "{written}"
    );
    let read = harness
        .call(
            "get_data",
            json!({"artifact_id": id, "path": "한글/e\u{301}"}),
        )
        .await?;
    assert_eq!(read["structuredContent"]["value"], 7, "{read}");
    let distinct = harness
        .call("get_data", json!({"artifact_id": id, "path": "한글/é"}))
        .await?;
    assert_eq!(distinct["structuredContent"]["exists"], false, "{distinct}");
    for bad in ["한글%2Fe\u{301}", "bad\u{200d}name", "𝄞\u{fe0f}"] {
        let rejected = harness
            .call("get_data", json!({"artifact_id": id, "path": bad}))
            .await?;
        assert_eq!(error_code(&rejected), "invalid_request", "{rejected}");
    }
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

/// Duplicating with `copy_assets` copies the caller's asset space with its
/// IDs, as over HTTP; without it the copy has no assets.
#[tokio::test]
async fn duplicate_artifact_copies_assets_only_when_asked() -> TestResult {
    use artifree_core::assets::AssetUpload;
    use artifree_domain::assets::AssetVersion;
    use artifree_domain::identity::ArtifactId;
    let harness = Harness::new();
    let created = harness
        .call(
            "create_artifact",
            json!({
                "title": "Page", "idempotency_key": "dup-asset-create-1",
                "files": [{ "path": "index.html", "encoding": "utf8", "content": "<p>x</p>" }]
            }),
        )
        .await?;
    let artifact = created["structuredContent"]["artifact"]["id"]
        .as_str()
        .ok_or("artifact")?
        .to_owned();
    // The upload goes through the service, which the MCP asset tools also call.
    let service = ArtifactService::new(SqliteStore::open(&harness.database)?);
    let actor: ActorId = parsed("local");
    let uploaded = service.upload_asset(
        &actor,
        &parsed("dup-asset-upload-1"),
        &parsed(&artifact),
        AssetUpload {
            base: AssetVersion(0),
            content_type: &parsed("text/plain"),
            bytes: b"asset bytes",
            now: UnixMillis(1),
        },
    )?;
    let duplicate = |key: &'static str, copy_assets: Option<bool>| {
        let mut arguments =
            json!({ "artifact_id": artifact, "revision": 1, "idempotency_key": key });
        if let Some(copy_assets) = copy_assets {
            arguments["copy_assets"] = json!(copy_assets);
        }
        harness.call("duplicate_artifact", arguments)
    };
    let copy_of = |reply: &Value| -> Result<ArtifactId, Box<dyn std::error::Error>> {
        Ok(parsed(
            reply["structuredContent"]["artifact"]["id"]
                .as_str()
                .ok_or("copy")?,
        ))
    };

    let plain = duplicate("dup-asset-plain-1", None).await?;
    assert_eq!(
        plain["structuredContent"]["copied"],
        json!(["files"]),
        "{plain}"
    );
    assert_eq!(service.assets(&actor, &copy_of(&plain)?)?.assets, []);

    let with = duplicate("dup-asset-with-1", Some(true)).await?;
    assert_eq!(
        with["structuredContent"]["copied"],
        json!(["files", "assets"]),
        "{with}"
    );
    let copied = service.asset_content(&actor, &copy_of(&with)?, &uploaded.id)?;
    assert_eq!(copied.bytes, b"asset bytes");
    Ok(())
}

/// Text that is not JSON, and numbers outside what a double holds exactly in
/// any spelling, are `invalid_request` through both tools that take text, with
/// no panic (`i64::MIN` once overflowed `abs`).
#[tokio::test]
async fn data_tools_refuse_text_that_is_not_data_without_panicking() -> TestResult {
    let harness = Harness::new();
    let created = harness
        .call(
            "create_artifact",
            json!({
                "title": "Page", "idempotency_key": "num-create-1",
                "files": [{ "path": "index.html", "encoding": "utf8", "content": "<p>x</p>" }]
            }),
        )
        .await?;
    let artifact = created["structuredContent"]["artifact"]["id"].clone();
    // Not JSON at all, whichever tool takes it.
    let not_json = ["{", "1 2", "", "NaN"];
    // JSON, but a number the rules refuse: alone for `set_data`, and as a
    // member of an object patch for `update_data`.
    let numbers = [
        "-9223372036854775808",
        "9223372036854775807",
        "18446744073709551615",
        "9007199254740992",
        "-9007199254740992",
        "9007199254740993.0",
        "9.007199254740993e15",
        "1e16",
        "1e20",
        "-1.5e300",
        "1e999",
        "[-9223372036854775808]",
    ];
    let mut cases: Vec<(&str, &str, String)> = Vec::new();
    for text in not_json {
        cases.push(("set_data", "value", text.to_owned()));
        cases.push(("update_data", "patch", text.to_owned()));
    }
    for text in numbers {
        cases.push(("set_data", "value", text.to_owned()));
        cases.push(("update_data", "patch", format!(r#"{{"x":{text}}}"#)));
    }
    for (index, (tool, field, text)) in cases.into_iter().enumerate() {
        let mut arguments = json!({
            "artifact_id": artifact, "path": "n", "base_data_version": 0,
            "idempotency_key": format!("num-refused-{index}")
        });
        arguments[field] = json!(text);
        // A handler that panics never answers; bound the wait so that it is a
        // failure of this test and not a hang.
        let result = tokio::time::timeout(Duration::from_secs(30), harness.call(tool, arguments))
            .await
            .map_err(|_| format!("{tool} {text:?} was never answered"))??;
        assert_eq!(
            error_code(&result),
            "invalid_request",
            "{tool} {text:?}: {result}"
        );
    }
    // The bounds themselves are accepted, in either spelling.
    for (index, text) in [
        "9007199254740991",
        "-9007199254740991",
        "9007199254740991.0",
        "1.5e3",
    ]
    .into_iter()
    .enumerate()
    {
        let result = harness
            .call(
                "set_data",
                json!({
                    "artifact_id": artifact, "path": "ok", "base_data_version": index,
                    "value": text, "idempotency_key": format!("num-ok-{index}")
                }),
            )
            .await?;
        assert_eq!(result["isError"], false, "{text}: {result}");
    }
    Ok(())
}

/// The bytes of the round-trip upload, and their SHA-256 digest as `sha256sum`
/// prints it. The digest comes from outside this crate, so the check does not
/// trust the server's own hashing.
const ROUND_TRIP: &[u8] = "round trip: 한글\r\n".as_bytes();
const ROUND_TRIP_SHA256: &str = "ac5c78c73a01e13e1d7705aeb76418c297d85ca8730434548177ba7e818d09cc";

/// The asset ID that an upload result names.
fn uploaded_asset_id(upload: &Value) -> Result<String, TestError> {
    Ok(upload["structuredContent"]["asset"]["id"]
        .as_str()
        .ok_or("asset id")?
        .to_owned())
}

/// Creates an artifact for the owner and returns its ID.
async fn new_artifact(harness: &Harness, key: &str) -> Result<String, TestError> {
    let created = harness
        .call(
            "create_artifact",
            json!({
                "title": "Assets", "idempotency_key": key,
                "files": [{ "path": "index.html", "encoding": "utf8", "content": "<p>x</p>" }]
            }),
        )
        .await?;
    assert_eq!(created["isError"], false, "{created}");
    Ok(created["structuredContent"]["artifact"]["id"]
        .as_str()
        .ok_or("artifact id")?
        .to_owned())
}

/// Uploads `bytes` as `text/plain` and returns the tool result.
async fn upload_text(
    harness: &Harness,
    artifact: &str,
    base: u64,
    key: &str,
    bytes: &[u8],
) -> Result<Value, TestError> {
    harness
        .call(
            "upload_asset",
            json!({
                "artifact_id": artifact, "base_asset_version": base,
                "content_type": "text/plain", "content_base64": STANDARD.encode(bytes),
                "idempotency_key": key
            }),
        )
        .await
}

async fn list_assets(harness: &Harness, artifact: &str) -> Result<Value, TestError> {
    harness
        .call("list_assets", json!({ "artifact_id": artifact }))
        .await
}

async fn read_asset(harness: &Harness, artifact: &str, asset: &str) -> Result<Value, TestError> {
    harness
        .call(
            "read_asset",
            json!({ "artifact_id": artifact, "asset_id": asset }),
        )
        .await
}

async fn delete_asset(
    harness: &Harness,
    artifact: &str,
    asset: &str,
    base: u64,
    key: &str,
) -> Result<Value, TestError> {
    harness
        .call(
            "delete_asset",
            json!({
                "artifact_id": artifact, "asset_id": asset,
                "base_asset_version": base, "idempotency_key": key
            }),
        )
        .await
}

/// Reads are read-only and not destructive; uploads and deletes are destructive
/// and idempotent. Every write needs a base version and a retry key.
#[tokio::test]
async fn the_asset_tools_say_what_they_change() -> TestResult {
    let harness = Harness::new();
    let (_, body) = harness.post(rpc(Some(TOKEN), &list_tools())?).await?;
    let listed: Value = serde_json::from_slice(&body)?;
    let tools = listed["result"]["tools"].as_array().ok_or("tools")?;
    for (name, read_only, required) in [
        ("list_assets", true, vec!["artifact_id"]),
        ("read_asset", true, vec!["artifact_id", "asset_id"]),
        (
            "upload_asset",
            false,
            vec![
                "artifact_id",
                "base_asset_version",
                "content_base64",
                "content_type",
                "idempotency_key",
            ],
        ),
        (
            "delete_asset",
            false,
            vec![
                "artifact_id",
                "asset_id",
                "base_asset_version",
                "idempotency_key",
            ],
        ),
    ] {
        let tool = tools
            .iter()
            .find(|tool| tool["name"] == name)
            .ok_or_else(|| format!("{name} is not offered"))?;
        let hints = &tool["annotations"];
        assert_eq!(hints["readOnlyHint"], read_only, "{name}");
        assert_eq!(hints["destructiveHint"], !read_only, "{name}");
        assert_eq!(hints["idempotentHint"], true, "{name}");
        assert_eq!(hints["openWorldHint"], false, "{name}");
        let mut declared: Vec<&str> = tool["inputSchema"]["required"]
            .as_array()
            .ok_or("required")?
            .iter()
            .filter_map(Value::as_str)
            .collect();
        declared.sort_unstable();
        assert_eq!(declared, required, "{name}");
    }
    Ok(())
}

/// An upload is listed and read back byte for byte, and a delete made with the
/// version the upload left removes it.
#[tokio::test]
async fn agents_upload_list_read_and_delete_an_asset() -> TestResult {
    let harness = Harness::new();
    let artifact = new_artifact(&harness, "asset-create-1").await?;
    let empty = list_assets(&harness, &artifact).await?;
    assert_eq!(
        empty["structuredContent"],
        json!({
            "asset_version": 0, "assets": [], "bytes": 0,
            "max_assets": 64, "max_bytes": 26_214_400
        }),
        "{empty}"
    );

    let uploaded = upload_text(&harness, &artifact, 0, "asset-upload-1", ROUND_TRIP).await?;
    assert_eq!(uploaded["isError"], false, "{uploaded}");
    let receipt = &uploaded["structuredContent"];
    assert_eq!(receipt["asset_version"], 1, "{uploaded}");
    assert_eq!(receipt["replayed"], false, "{uploaded}");
    assert_eq!(receipt["asset"]["content_type"], "text/plain", "{uploaded}");
    assert_eq!(
        receipt["asset"]["size_bytes"],
        ROUND_TRIP.len(),
        "{uploaded}"
    );
    assert_eq!(receipt["asset"]["sha256"], ROUND_TRIP_SHA256, "{uploaded}");
    let asset = uploaded_asset_id(&uploaded)?;

    let listed = list_assets(&harness, &artifact).await?;
    assert_eq!(listed["structuredContent"]["asset_version"], 1, "{listed}");
    assert_eq!(
        listed["structuredContent"]["bytes"],
        ROUND_TRIP.len(),
        "{listed}"
    );
    assert_eq!(
        listed["structuredContent"]["assets"],
        json!([receipt["asset"]]),
        "{listed}"
    );

    let read = read_asset(&harness, &artifact, &asset).await?;
    assert_eq!(read["isError"], false, "{read}");
    assert_eq!(read["structuredContent"]["asset_version"], 1, "{read}");
    assert_eq!(
        read["structuredContent"]["asset"], receipt["asset"],
        "{read}"
    );
    assert_eq!(
        read["structuredContent"]["content_base64"],
        STANDARD.encode(ROUND_TRIP),
        "read_asset must return the stored bytes"
    );

    let removed = delete_asset(&harness, &artifact, &asset, 1, "asset-delete-1").await?;
    assert_eq!(removed["isError"], false, "{removed}");
    assert_eq!(
        removed["structuredContent"],
        json!({ "asset_id": asset, "asset_version": 2, "deleted": true, "replayed": false }),
        "{removed}"
    );
    let gone = read_asset(&harness, &artifact, &asset).await?;
    assert_eq!(error_code(&gone), "not_found", "{gone}");
    let after = list_assets(&harness, &artifact).await?;
    assert_eq!(after["structuredContent"]["asset_version"], 2, "{after}");
    assert_eq!(after["structuredContent"]["assets"], json!([]), "{after}");
    Ok(())
}

/// A same-key resend replays the first receipt even after the space moved on,
/// and creates no second asset.
#[tokio::test]
async fn a_same_key_upload_replays_its_receipt_after_the_space_moves_on() -> TestResult {
    let harness = Harness::new();
    let artifact = new_artifact(&harness, "asset-create-2").await?;
    let first = upload_text(&harness, &artifact, 0, "asset-upload-2", b"first\n").await?;
    assert_eq!(first["isError"], false, "{first}");
    // Another upload and its deletion move the version to 3 and leave one asset.
    let other = upload_text(&harness, &artifact, 1, "asset-upload-3", b"other\n").await?;
    assert_eq!(other["structuredContent"]["asset_version"], 2, "{other}");
    let other_asset = uploaded_asset_id(&other)?;
    let removed = delete_asset(&harness, &artifact, &other_asset, 2, "asset-delete-2").await?;
    assert_eq!(
        removed["structuredContent"]["asset_version"], 3,
        "{removed}"
    );

    // The same request as before, with its original base, which is no longer current.
    let replay = upload_text(&harness, &artifact, 0, "asset-upload-2", b"first\n").await?;
    assert_eq!(replay["isError"], false, "{replay}");
    assert_eq!(replay["structuredContent"]["replayed"], true, "{replay}");
    assert_eq!(replay["structuredContent"]["asset_version"], 1, "{replay}");
    assert_eq!(
        replay["structuredContent"]["asset"], first["structuredContent"]["asset"],
        "{replay}"
    );
    let listed = list_assets(&harness, &artifact).await?;
    assert_eq!(listed["structuredContent"]["asset_version"], 3, "{listed}");
    assert_eq!(
        listed["structuredContent"]["assets"],
        json!([first["structuredContent"]["asset"]]),
        "{listed}"
    );
    Ok(())
}

/// A replayed upload does not bring back an asset that was deleted: the receipt
/// replays, reads are `not_found`, and deleting it again changes nothing.
#[tokio::test]
async fn a_replayed_upload_does_not_recreate_a_deleted_asset() -> TestResult {
    let harness = Harness::new();
    let artifact = new_artifact(&harness, "asset-create-3").await?;
    let first = upload_text(&harness, &artifact, 0, "asset-upload-4", b"gone\n").await?;
    assert_eq!(first["isError"], false, "{first}");
    let asset = uploaded_asset_id(&first)?;
    let removed = delete_asset(&harness, &artifact, &asset, 1, "asset-delete-3").await?;
    assert_eq!(removed["structuredContent"]["deleted"], true, "{removed}");

    let replay = upload_text(&harness, &artifact, 0, "asset-upload-4", b"gone\n").await?;
    assert_eq!(replay["isError"], false, "{replay}");
    assert_eq!(replay["structuredContent"]["replayed"], true, "{replay}");
    assert_eq!(
        replay["structuredContent"]["asset"]["id"], asset,
        "{replay}"
    );
    let gone = read_asset(&harness, &artifact, &asset).await?;
    assert_eq!(error_code(&gone), "not_found", "{gone}");
    let listed = list_assets(&harness, &artifact).await?;
    assert_eq!(listed["structuredContent"]["asset_version"], 2, "{listed}");
    assert_eq!(listed["structuredContent"]["assets"], json!([]), "{listed}");

    let again = delete_asset(&harness, &artifact, &asset, 2, "asset-delete-4").await?;
    assert_eq!(again["structuredContent"]["deleted"], false, "{again}");
    assert_eq!(again["structuredContent"]["asset_version"], 2, "{again}");
    Ok(())
}

/// A stale base is refused with the current version and writes nothing. The
/// same key then still works with the current base, so the refusal consumed
/// nothing.
#[tokio::test]
async fn a_stale_base_is_a_conflict_that_writes_nothing() -> TestResult {
    let harness = Harness::new();
    let artifact = new_artifact(&harness, "asset-create-4").await?;
    let kept = upload_text(&harness, &artifact, 0, "asset-upload-5", b"kept\n").await?;
    assert_eq!(kept["isError"], false, "{kept}");
    let asset = uploaded_asset_id(&kept)?;
    let before = list_assets(&harness, &artifact).await?;

    // The space is at version 1; both writes were prepared against version 0.
    let stale = upload_text(&harness, &artifact, 0, "asset-upload-6", b"stale\n").await?;
    assert_eq!(error_code(&stale), "asset_conflict", "{stale}");
    assert_eq!(
        stale["structuredContent"]["error"]["current_asset_version"], 1,
        "{stale}"
    );
    let stale_delete = delete_asset(&harness, &artifact, &asset, 0, "asset-delete-5").await?;
    assert_eq!(
        error_code(&stale_delete),
        "asset_conflict",
        "{stale_delete}"
    );
    assert_eq!(
        stale_delete["structuredContent"]["error"]["current_asset_version"], 1,
        "{stale_delete}"
    );

    let after = list_assets(&harness, &artifact).await?;
    assert_eq!(
        after["structuredContent"], before["structuredContent"],
        "{after}"
    );
    let read = read_asset(&harness, &artifact, &asset).await?;
    assert_eq!(read["isError"], false, "{read}");

    let current = upload_text(&harness, &artifact, 1, "asset-upload-6", b"stale\n").await?;
    assert_eq!(current["isError"], false, "{current}");
    assert_eq!(
        current["structuredContent"]["asset_version"], 2,
        "{current}"
    );
    assert_eq!(current["structuredContent"]["replayed"], false, "{current}");
    Ok(())
}

/// Refused uploads answer with their codes and change nothing. None of them
/// consumes its key, and a different request under a used key is refused.
#[tokio::test]
async fn refused_uploads_answer_with_their_codes_and_change_nothing() -> TestResult {
    let harness = Harness::new();
    let artifact = new_artifact(&harness, "asset-create-5").await?;
    let key = "asset-refused-1";
    let over_limit = vec![b'a'; 5 * 1024 * 1024 + 1];
    for (what, content_type, content, code) in [
        (
            "a truncated PNG",
            "image/png",
            STANDARD.encode(b"\x89PNG\r\n\x1a\n"),
            "invalid_asset",
        ),
        (
            "a JPEG",
            "image/jpeg",
            STANDARD.encode(b"\xff\xd8\xff\xd9"),
            "asset_type_unsupported",
        ),
        (
            "a GIF",
            "image/gif",
            STANDARD.encode(b"GIF89a"),
            "asset_type_unsupported",
        ),
        ("empty text", "text/plain", String::new(), "invalid_asset"),
        (
            "invalid UTF-8",
            "text/plain",
            STANDARD.encode([0xff]),
            "invalid_asset",
        ),
        (
            "a NUL byte",
            "text/csv",
            STANDARD.encode(b"a\0b"),
            "invalid_asset",
        ),
        (
            "unpadded base64",
            "text/plain",
            "YQ".to_owned(),
            "invalid_request",
        ),
        (
            "5 MiB plus one byte",
            "text/plain",
            STANDARD.encode(&over_limit),
            "asset_limit_exceeded",
        ),
    ] {
        let refused = harness
            .call(
                "upload_asset",
                json!({
                    "artifact_id": artifact, "base_asset_version": 0,
                    "content_type": content_type, "content_base64": content,
                    "idempotency_key": key
                }),
            )
            .await?;
        assert_eq!(error_code(&refused), code, "{what}: {refused}");
        let listed = list_assets(&harness, &artifact).await?;
        assert_eq!(
            listed["structuredContent"]["asset_version"], 0,
            "{what}: {listed}"
        );
        assert_eq!(
            listed["structuredContent"]["assets"],
            json!([]),
            "{what}: {listed}"
        );
    }

    let stored = upload_text(&harness, &artifact, 0, key, b"valid\n").await?;
    assert_eq!(stored["isError"], false, "{stored}");
    assert_eq!(stored["structuredContent"]["replayed"], false, "{stored}");
    let reused = upload_text(&harness, &artifact, 1, key, b"other\n").await?;
    assert_eq!(error_code(&reused), "request_key_reused", "{reused}");
    let listed = list_assets(&harness, &artifact).await?;
    assert_eq!(listed["structuredContent"]["asset_version"], 1, "{listed}");
    assert_eq!(
        listed["structuredContent"]["assets"],
        json!([stored["structuredContent"]["asset"]]),
        "{listed}"
    );
    Ok(())
}

/// Another user's token reaches none of this user's assets, and an artifact in
/// the trash reaches none of them through any tool.
#[tokio::test]
async fn other_users_and_the_trash_reach_no_asset() -> TestResult {
    let harness = Harness::new();
    let artifact = new_artifact(&harness, "asset-create-6").await?;
    let kept = upload_text(&harness, &artifact, 0, "asset-upload-7", b"mine\n").await?;
    assert_eq!(kept["isError"], false, "{kept}");
    let asset = uploaded_asset_id(&kept)?;
    let before = list_assets(&harness, &artifact).await?;
    // Every write carries the current base, so a refusal can only be about access.
    let calls = |artifact_id: &str, asset_id: &str| -> [(&'static str, Value); 4] {
        [
            ("list_assets", json!({ "artifact_id": artifact_id })),
            (
                "read_asset",
                json!({ "artifact_id": artifact_id, "asset_id": asset_id }),
            ),
            (
                "upload_asset",
                json!({
                    "artifact_id": artifact_id, "base_asset_version": 1,
                    "content_type": "text/plain", "content_base64": STANDARD.encode(b"theirs\n"),
                    "idempotency_key": "asset-probe-1"
                }),
            ),
            (
                "delete_asset",
                json!({
                    "artifact_id": artifact_id, "asset_id": asset_id,
                    "base_asset_version": 1, "idempotency_key": "asset-probe-2"
                }),
            ),
        ]
    };

    let other = harness.user_token("someone").await?;
    for (tool, arguments) in calls(&artifact, &asset) {
        let result = harness.call_as(&other, tool, arguments).await?;
        assert_eq!(
            error_code(&result),
            "not_found",
            "another user's {tool}: {result}"
        );
    }
    let after = list_assets(&harness, &artifact).await?;
    assert_eq!(
        after["structuredContent"], before["structuredContent"],
        "{after}"
    );

    let trashed = harness
        .call(
            "update_artifact",
            json!({
                "artifact_id": artifact, "base_metadata_revision": 1, "deleted": true,
                "idempotency_key": "asset-trash-1"
            }),
        )
        .await?;
    assert_eq!(trashed["isError"], false, "{trashed}");
    for (tool, arguments) in calls(&artifact, &asset) {
        let result = harness.call(tool, arguments).await?;
        assert_eq!(error_code(&result), "not_found", "trashed {tool}: {result}");
    }
    Ok(())
}

/// Over HTTP and over MCP, one asset has the same bytes and the same digest.
#[tokio::test]
async fn http_and_mcp_read_the_same_asset() -> TestResult {
    let harness = Harness::new();
    let artifact = new_artifact(&harness, "asset-create-7").await?;
    let uploaded = upload_text(&harness, &artifact, 0, "asset-upload-8", ROUND_TRIP).await?;
    assert_eq!(uploaded["isError"], false, "{uploaded}");
    let asset = uploaded_asset_id(&uploaded)?;
    let mcp = read_asset(&harness, &artifact, &asset).await?;
    let mcp_bytes = STANDARD.decode(
        mcp["structuredContent"]["content_base64"]
            .as_str()
            .ok_or("content_base64")?,
    )?;

    let path = format!("/v1/artifacts/{artifact}/assets/{asset}");
    let content = harness
        .api
        .clone()
        .oneshot(
            Request::get(format!("{path}/content"))
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(content.status(), StatusCode::OK);
    let http_bytes = to_bytes(content.into_body(), usize::MAX).await?;
    let (status, body) = harness.owner_request("GET", &path, None).await?;
    assert_eq!(status, StatusCode::OK);
    let metadata: Value = serde_json::from_slice(&body)?;

    assert_eq!(&http_bytes[..], ROUND_TRIP, "HTTP content");
    assert_eq!(mcp_bytes, ROUND_TRIP, "MCP content");
    assert_eq!(
        metadata["asset"]["sha256"], ROUND_TRIP_SHA256,
        "HTTP metadata"
    );
    assert_eq!(
        mcp["structuredContent"]["asset"]["sha256"], ROUND_TRIP_SHA256,
        "MCP metadata"
    );
    assert_eq!(metadata["asset"], mcp["structuredContent"]["asset"]);
    Ok(())
}
