//! The declared shared space of decision 0060 rule 10 (slice S02f1) through
//! the real router, application core and `SQLite` store: the data routes with
//! `space=shared` and the MCP data tools with `space: "shared"`, who may read
//! and write it, and what a key may replay.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use artifree_contracts::api::{CreateTokenResponse, UserInfo};
use artifree_providers::storage::SqliteStore;
use artifree_providers::tokens::SqliteTokenStore;
use artifree_server::auth::AccessToken;
use artifree_server::http::{self, AppState, Settings};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, header};
use serde_json::{Value, json};
use tower::ServiceExt;

type TestError = Box<dyn std::error::Error>;
type TestResult = Result<(), TestError>;

const OWNER: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const EDITORS: &str = r#"{"sdk":"artifree.app.v1","shared":"editors"}"#;

struct Harness {
    directory: PathBuf,
    api: Router,
}

/// A registered user and the secret of its one token.
struct Person {
    id: String,
    secret: String,
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

impl Harness {
    fn new() -> Result<Self, TestError> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!(
            "artifree-shared-data-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory)?;
        let database = directory.join("db.sqlite3");
        let state = AppState::new(
            SqliteStore::open(&database)?,
            SqliteTokenStore::open(&database)?,
            AccessToken::parse(OWNER).ok_or("fixture token")?,
            Settings::default(),
        )?;
        Ok(Self {
            // The real server records the socket peer; /mcp serves loopback
            // clients only, so the tests connect from loopback.
            api: http::router(state).layer(axum::Extension(axum::extract::ConnectInfo(
                std::net::SocketAddr::from(([127, 0, 0, 1], 40_000)),
            ))),
            directory,
        })
    }

    /// Sends a request; returns the status and the body as JSON (`null` for
    /// an empty body).
    async fn send(
        &self,
        method: &str,
        path: &str,
        token: &str,
        key: Option<&str>,
        body: Option<Value>,
    ) -> Result<(u16, Value), TestError> {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(header::AUTHORIZATION, format!("Bearer {token}"));
        if let Some(key) = key {
            builder = builder.header("idempotency-key", key);
        }
        let request = match body {
            Some(body) => builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))?,
            None => builder.body(Body::empty())?,
        };
        let response = self.api.clone().oneshot(request).await?;
        let status = response.status().as_u16();
        let bytes = to_bytes(response.into_body(), usize::MAX).await?;
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)?
        };
        Ok((status, body))
    }

    async fn ok(
        &self,
        status: u16,
        method: &str,
        path: &str,
        token: &str,
        key: Option<&str>,
        body: Option<Value>,
    ) -> Result<Value, TestError> {
        let (answer, body) = self.send(method, path, token, key, body).await?;
        assert_eq!(answer, status, "{method} {path}: {body}");
        Ok(body)
    }

    async fn person(&self, label: &str) -> Result<Person, TestError> {
        let user: UserInfo = serde_json::from_value(
            self.ok(
                201,
                "POST",
                "/v1/users",
                OWNER,
                None,
                Some(json!({ "label": label })),
            )
            .await?,
        )?;
        let issued: CreateTokenResponse = serde_json::from_value(
            self.ok(
                201,
                "POST",
                "/v1/tokens",
                OWNER,
                None,
                Some(json!({ "name": label, "user_id": user.id })),
            )
            .await?,
        )?;
        Ok(Person {
            id: user.id,
            secret: issued.secret,
        })
    }

    /// An artifact of `owner`'s, with `manifest` as its runtime manifest if
    /// any, shared with each `(user, role)` in order.
    async fn artifact(
        &self,
        owner: &Person,
        manifest: Option<&str>,
        members: &[(&Person, &str)],
    ) -> Result<String, TestError> {
        let mut files = vec![json!({ "path": "index.html", "encoding": "utf8", "content": "x" })];
        if let Some(manifest) = manifest {
            files.push(json!({
                "path": "artifree-runtime.json", "encoding": "utf8", "content": manifest
            }));
        }
        let created = self
            .ok(
                201,
                "POST",
                "/v1/artifacts",
                &owner.secret,
                Some("create-shared"),
                Some(json!({ "title": "Shared", "files": files })),
            )
            .await?;
        let id = created["artifact"]["id"]
            .as_str()
            .ok_or("no artifact id")?
            .to_owned();
        for (base, (user, role)) in members.iter().enumerate() {
            self.ok(
                200,
                "PUT",
                &format!("/v1/artifacts/{id}/access/{}", user.id),
                &owner.secret,
                Some(&format!("grant-{}", user.id)),
                Some(json!({ "role": role, "base_access_version": base })),
            )
            .await?;
        }
        Ok(id)
    }

    /// Sets `note` in a space of `artifact` as `token`.
    async fn set(
        &self,
        token: &str,
        key: &str,
        artifact: &str,
        query: &str,
        base: u64,
        value: &str,
    ) -> Result<(u16, Value), TestError> {
        let value: Value = serde_json::from_str(value)?;
        self.send(
            "PUT",
            &format!("/v1/artifacts/{artifact}/data{query}"),
            token,
            Some(key),
            Some(json!({ "path": "note", "base_data_version": base, "value": value })),
        )
        .await
    }

    /// Calls an MCP tool as `token`; returns the JSON-RPC result.
    async fn tool(&self, token: &str, name: &str, arguments: Value) -> Result<Value, TestError> {
        let message = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        });
        let request = Request::post("/mcp")
            .header(header::HOST, "127.0.0.1:7410")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header("mcp-protocol-version", "2025-11-25")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::from(message.to_string()))?;
        let response = self.api.clone().oneshot(request).await?;
        let status = response.status().as_u16();
        let bytes = to_bytes(response.into_body(), usize::MAX).await?;
        let text = String::from_utf8_lossy(&bytes);
        assert_eq!(status, 200, "{name}: {text}");
        let reply: Value = serde_json::from_slice(&bytes)?;
        assert!(reply.get("result").is_some(), "{name}: {text}");
        Ok(reply["result"].clone())
    }
}

fn code(body: &Value) -> &str {
    body["error"]["code"].as_str().unwrap_or("")
}

/// Every reader reads the shared space, the declaration names who writes it,
/// anyone else does not find it, and each user's own space stays apart.
#[tokio::test]
async fn readers_read_the_shared_space_and_the_declaration_names_its_writers() -> TestResult {
    let harness = Harness::new()?;
    let ana = harness.person("Ana").await?;
    let bob = harness.person("Bob").await?;
    let cy = harness.person("Cy").await?;
    let dan = harness.person("Dan").await?;
    let id = harness
        .artifact(&ana, Some(EDITORS), &[(&bob, "editor"), (&cy, "viewer")])
        .await?;
    let shared = format!("/v1/artifacts/{id}/data?space=shared&path=note");

    let (status, written) = harness
        .set(&bob.secret, "bob-shared", &id, "?space=shared", 0, "\"hi\"")
        .await?;
    assert_eq!(status, 200, "{written}");
    assert_eq!(written["data_version"], 1);
    for reader in [&ana, &bob, &cy] {
        let read = harness
            .ok(200, "GET", &shared, &reader.secret, None, None)
            .await?;
        assert_eq!(
            (&read["value"], &read["data_version"]),
            (&json!("hi"), &json!(1))
        );
    }
    let own = harness
        .ok(
            200,
            "GET",
            &format!("/v1/artifacts/{id}/data"),
            &cy.secret,
            None,
            None,
        )
        .await?;
    assert_eq!(own["data_version"], 0, "the viewer's own space is apart");

    // A viewer may not write a space only editors write; a stranger does not
    // find it at all.
    let (status, refused) = harness
        .set(&cy.secret, "cy-shared", &id, "?space=shared", 1, "\"no\"")
        .await?;
    assert_eq!((status, code(&refused)), (403, "forbidden"), "{refused}");
    let (status, _) = harness
        .send("GET", &shared, &dan.secret, None, None)
        .await?;
    assert_eq!(status, 404);
    let (status, _) = harness
        .set(&dan.secret, "dan-shared", &id, "?space=shared", 1, "\"no\"")
        .await?;
    assert_eq!(status, 404);
    // The viewer still writes its own space.
    let (status, _) = harness
        .set(&cy.secret, "cy-own-space", &id, "", 0, "\"mine\"")
        .await?;
    assert_eq!(status, 200);

    // The MCP tools name the same space.
    let read = harness
        .tool(
            &cy.secret,
            "get_data",
            json!({ "artifact_id": id, "path": "note", "space": "shared" }),
        )
        .await?;
    assert_eq!(read["structuredContent"]["value"], "hi", "{read}");
    let refused = harness
        .tool(
            &cy.secret,
            "set_data",
            json!({
                "artifact_id": id, "path": "note", "base_data_version": 1, "value": "\"no\"",
                "idempotency_key": "cy-mcp-shared", "space": "shared"
            }),
        )
        .await?;
    assert_eq!(
        refused["structuredContent"]["error"]["code"], "forbidden",
        "{refused}"
    );
    Ok(())
}

/// A key replays only the write of the space it was sent to, and a write
/// names its space with nothing else in its query.
#[tokio::test]
async fn a_key_belongs_to_one_space_and_a_write_takes_only_space() -> TestResult {
    let harness = Harness::new()?;
    let ana = harness.person("Ana").await?;
    let id = harness.artifact(&ana, Some(EDITORS), &[]).await?;

    let (status, _) = harness
        .set(&ana.secret, "ana-note", &id, "?space=shared", 0, "1")
        .await?;
    assert_eq!(status, 200);
    let (status, replay) = harness
        .set(&ana.secret, "ana-note", &id, "?space=shared", 0, "1")
        .await?;
    assert_eq!(
        (status, &replay["replayed"]),
        (200, &json!(true)),
        "{replay}"
    );
    // The same key, base and value for the owner's own space is another
    // request.
    let (status, reused) = harness
        .set(&ana.secret, "ana-note", &id, "", 0, "1")
        .await?;
    assert_eq!(
        (status, code(&reused)),
        (422, "request_key_reused"),
        "{reused}"
    );

    for query in [
        "?space=shared&path=note",
        "?space=everyone",
        "?space=shared&space=own",
    ] {
        let (status, answer) = harness
            .set(&ana.secret, "ana-bad-query", &id, query, 1, "2")
            .await?;
        assert_eq!(status, 400, "{query}: {answer}");
    }
    Ok(())
}

/// An artifact whose head declares no shared space has none to read or write.
#[tokio::test]
async fn an_undeclared_shared_space_is_not_found() -> TestResult {
    let harness = Harness::new()?;
    let ana = harness.person("Ana").await?;
    let id = harness.artifact(&ana, None, &[]).await?;
    let (status, _) = harness
        .send(
            "GET",
            &format!("/v1/artifacts/{id}/data?space=shared"),
            &ana.secret,
            None,
            None,
        )
        .await?;
    assert_eq!(status, 404);
    let (status, _) = harness
        .set(&ana.secret, "ana-shared", &id, "?space=shared", 0, "1")
        .await?;
    assert_eq!(status, 404);
    let members = harness
        .send(
            "GET",
            &format!("/v1/artifacts/{id}/data/members?space=shared"),
            &ana.secret,
            None,
            None,
        )
        .await?;
    assert_eq!(members.0, 404);
    Ok(())
}
