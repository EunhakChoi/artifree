//! The declared shared asset space of decision 0060 rule 10 (slice S02f2)
//! through the real router, application core and `SQLite` store: the asset
//! routes with `space=shared` and the MCP asset tools with
//! `space: "shared"`, who may read and write it, what a key may replay, and
//! the change every reader receives.

use std::future::poll_fn;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use artifree_contracts::api::{CreateTokenResponse, UserInfo};
use artifree_providers::storage::SqliteStore;
use artifree_providers::tokens::SqliteTokenStore;
use artifree_server::auth::AccessToken;
use artifree_server::http::{self, AppState, Settings};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, header};
use http_body::Body as _;
use serde_json::{Value, json};
use tower::ServiceExt;

type TestError = Box<dyn std::error::Error>;
type TestResult = Result<(), TestError>;

const OWNER: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const EDITORS: &str = r#"{"sdk":"artifree.app.v1","shared":"editors"}"#;
/// `hi` in standard padded base64.
const HI: &str = "aGk=";

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
            "artifree-shared-assets-{}-{}",
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

    /// Uploads `content` (base64) to a space of `artifact` as `token`.
    async fn upload(
        &self,
        token: &str,
        key: Option<&str>,
        (artifact, query): (&str, &str),
        base: u64,
        content: &str,
    ) -> Result<(u16, Value), TestError> {
        self.send(
            "POST",
            &format!("/v1/artifacts/{artifact}/assets{query}"),
            token,
            key,
            Some(json!({
                "base_asset_version": base, "content_type": "text/plain",
                "content_base64": content
            })),
        )
        .await
    }

    /// Deletes an asset from a space of `artifact` as `token`.
    async fn delete(
        &self,
        token: &str,
        key: Option<&str>,
        (artifact, query): (&str, &str),
        base: u64,
        asset: &str,
    ) -> Result<(u16, Value), TestError> {
        self.send(
            "DELETE",
            &format!("/v1/artifacts/{artifact}/assets/{asset}{query}"),
            token,
            key,
            Some(json!({ "base_asset_version": base })),
        )
        .await
    }

    /// Opens the change stream of `token`, once it is ready.
    async fn follow(&self, token: &str) -> Result<Events, TestError> {
        let request = Request::get("/v1/changes")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::ACCEPT, "text/event-stream")
            .body(Body::empty())?;
        let response = self.api.clone().oneshot(request).await?;
        assert_eq!(response.status().as_u16(), 200);
        let mut events = Events {
            body: response.into_body(),
            seen: String::new(),
        };
        events.until("event: ready").await?;
        Ok(events)
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

/// The text of one change stream, read as it arrives.
struct Events {
    body: Body,
    seen: String,
}

impl Events {
    /// Reads until the stream holds `needle`; fails after 5 s without it.
    async fn until(&mut self, needle: &str) -> TestResult {
        while !self.seen.contains(needle) {
            let frame = tokio::time::timeout(
                Duration::from_secs(5),
                poll_fn(|context| Pin::new(&mut self.body).poll_frame(context)),
            )
            .await
            .map_err(|_| format!("no {needle} within 5 s: {}", self.seen))?
            .ok_or("the stream ended")??;
            if let Ok(data) = frame.into_data() {
                self.seen.push_str(std::str::from_utf8(&data)?);
            }
        }
        Ok(())
    }
}

fn code(body: &Value) -> &str {
    body["error"]["code"].as_str().unwrap_or("")
}

/// Every reader reads the shared space, the declaration names who writes it,
/// anyone else does not find it, and each user's own space stays apart.
#[tokio::test]
async fn readers_read_shared_assets_and_the_declaration_names_their_writers() -> TestResult {
    let harness = Harness::new()?;
    let ana = harness.person("Ana").await?;
    let bob = harness.person("Bob").await?;
    let cy = harness.person("Cy").await?;
    let dan = harness.person("Dan").await?;
    let id = harness
        .artifact(&ana, Some(EDITORS), &[(&bob, "editor"), (&cy, "viewer")])
        .await?;
    let shared = (id.as_str(), "?space=shared");
    let (status, uploaded) = harness
        .upload(&bob.secret, Some("bob-shared"), shared, 0, HI)
        .await?;
    assert_eq!(status, 201, "{uploaded}");
    let asset = uploaded["asset"]["id"].as_str().ok_or("no asset id")?;
    let list = format!("/v1/artifacts/{id}/assets?space=shared");
    for reader in [&ana, &bob, &cy] {
        let listed = harness
            .ok(200, "GET", &list, &reader.secret, None, None)
            .await?;
        assert_eq!(listed["assets"][0]["id"], asset, "{listed}");
    }
    let content = harness
        .api
        .clone()
        .oneshot(
            Request::get(format!(
                "/v1/artifacts/{id}/assets/{asset}/content?space=shared"
            ))
            .header(header::AUTHORIZATION, format!("Bearer {}", cy.secret))
            .body(Body::empty())?,
        )
        .await?;
    assert_eq!(content.status().as_u16(), 200);
    assert_eq!(to_bytes(content.into_body(), 64).await?.as_ref(), b"hi");
    let own = format!("/v1/artifacts/{id}/assets");
    let mine = harness
        .ok(200, "GET", &own, &bob.secret, None, None)
        .await?;
    assert_eq!(
        mine["assets"],
        json!([]),
        "the uploader's own space is apart"
    );

    // A viewer may not write a space only editors write; a stranger does not
    // find it at all. The viewer still writes its own space.
    for (who, expected) in [(&cy, 403), (&dan, 404)] {
        let (status, _) = harness
            .upload(&who.secret, Some("refused-up"), shared, 1, HI)
            .await?;
        assert_eq!(status, expected);
        let (status, _) = harness
            .delete(&who.secret, Some("refused-rm"), shared, 1, asset)
            .await?;
        assert_eq!(status, expected);
    }
    let (status, _) = harness.send("GET", &list, &dan.secret, None, None).await?;
    assert_eq!(status, 404);
    let (status, _) = harness
        .upload(&cy.secret, Some("cy-own-asset"), (&id, ""), 0, HI)
        .await?;
    assert_eq!(status, 201);
    let (status, removed) = harness
        .delete(&bob.secret, Some("bob-remove"), shared, 1, asset)
        .await?;
    assert_eq!(
        (status, &removed["deleted"]),
        (200, &json!(true)),
        "{removed}"
    );
    Ok(())
}

/// The MCP asset tools name the same space, and check the role before they
/// read a write's key or content (decision 0060 rule 4).
#[tokio::test]
async fn the_asset_tools_name_the_shared_space() -> TestResult {
    let harness = Harness::new()?;
    let ana = harness.person("Ana").await?;
    let cy = harness.person("Cy").await?;
    let id = harness
        .artifact(&ana, Some(EDITORS), &[(&cy, "viewer")])
        .await?;
    let uploaded = harness
        .tool(
            &ana.secret,
            "upload_asset",
            json!({
                "artifact_id": id, "base_asset_version": 0, "content_type": "text/plain",
                "content_base64": HI, "idempotency_key": "ana-mcp-shared", "space": "shared"
            }),
        )
        .await?;
    let asset = uploaded["structuredContent"]["asset"]["id"].clone();
    let listed = harness
        .tool(
            &cy.secret,
            "list_assets",
            json!({ "artifact_id": id, "space": "shared" }),
        )
        .await?;
    assert_eq!(
        listed["structuredContent"]["assets"][0]["id"], asset,
        "{listed}"
    );
    let read = harness
        .tool(
            &cy.secret,
            "read_asset",
            json!({ "artifact_id": id, "asset_id": asset, "space": "shared" }),
        )
        .await?;
    assert_eq!(read["structuredContent"]["content_base64"], HI, "{read}");
    let own = harness
        .tool(&ana.secret, "list_assets", json!({ "artifact_id": id }))
        .await?;
    assert_eq!(own["structuredContent"]["assets"], json!([]), "{own}");
    // A malformed key and content are not read before the role refuses.
    for (name, arguments) in [
        (
            "upload_asset",
            json!({
                "artifact_id": id, "base_asset_version": 1, "content_type": "no",
                "content_base64": "!", "idempotency_key": "x", "space": "shared"
            }),
        ),
        (
            "delete_asset",
            json!({
                "artifact_id": id, "asset_id": asset, "base_asset_version": 1,
                "idempotency_key": "x", "space": "shared"
            }),
        ),
    ] {
        let refused = harness.tool(&cy.secret, name, arguments).await?;
        assert_eq!(
            refused["structuredContent"]["error"]["code"], "forbidden",
            "{name}: {refused}"
        );
    }
    Ok(())
}

/// Access, the declaration and the writer role are judged before a shared
/// write's key and body (decision 0060 rule 4).
#[tokio::test]
async fn a_refused_shared_asset_write_learns_nothing_of_its_key_or_body() -> TestResult {
    let harness = Harness::new()?;
    let ana = harness.person("Ana").await?;
    let cy = harness.person("Cy").await?;
    let dan = harness.person("Dan").await?;
    let id = harness
        .artifact(&ana, Some(EDITORS), &[(&cy, "viewer")])
        .await?;
    let asset = "0".repeat(64);
    for (who, expected) in [(&cy, 403), (&dan, 404)] {
        for (key, body) in [
            (Some("bad-shared-body"), json!(["not", "an", "object"])),
            (None, json!({ "base_asset_version": 0 })),
        ] {
            for path in [
                format!("/v1/artifacts/{id}/assets?space=shared"),
                format!("/v1/artifacts/{id}/assets/{asset}?space=shared"),
            ] {
                let method = if path.contains(&asset) {
                    "DELETE"
                } else {
                    "POST"
                };
                let (status, answer) = harness
                    .send(method, &path, &who.secret, key, Some(body.clone()))
                    .await?;
                assert_eq!(status, expected, "{method} {key:?} {body}: {answer}");
            }
        }
    }
    // The caller's own space keeps its order: a missing key is refused first.
    let (status, answer) = harness.upload(&cy.secret, None, (&id, ""), 0, HI).await?;
    assert_eq!(status, 400, "{answer}");
    Ok(())
}

/// A key replays only the upload of the space it was sent to, and an asset
/// route names its space with nothing else in its query.
#[tokio::test]
async fn an_asset_key_belongs_to_one_space_and_a_route_takes_only_space() -> TestResult {
    let harness = Harness::new()?;
    let ana = harness.person("Ana").await?;
    let id = harness.artifact(&ana, Some(EDITORS), &[]).await?;
    let shared = (id.as_str(), "?space=shared");
    let (status, first) = harness
        .upload(&ana.secret, Some("ana-asset"), shared, 0, HI)
        .await?;
    assert_eq!(status, 201, "{first}");
    let (status, replay) = harness
        .upload(&ana.secret, Some("ana-asset"), shared, 0, HI)
        .await?;
    assert_eq!(
        (status, &replay["replayed"]),
        (201, &json!(true)),
        "{replay}"
    );
    // The same key, base and content for the owner's own space is another
    // request.
    let (status, reused) = harness
        .upload(&ana.secret, Some("ana-asset"), (&id, ""), 0, HI)
        .await?;
    assert_eq!(
        (status, code(&reused)),
        (422, "request_key_reused"),
        "{reused}"
    );
    for query in [
        "?space=shared&path=x",
        "?space=everyone",
        "?space=shared&space=own",
    ] {
        let (status, answer) = harness
            .send(
                "GET",
                &format!("/v1/artifacts/{id}/assets{query}"),
                &ana.secret,
                None,
                None,
            )
            .await?;
        assert_eq!(status, 400, "{query}: {answer}");
        let (status, answer) = harness
            .upload(&ana.secret, Some("ana-bad-query"), (&id, query), 1, HI)
            .await?;
        assert_eq!(status, 400, "{query}: {answer}");
    }
    Ok(())
}

/// An artifact whose head declares no shared space has none to read or write.
#[tokio::test]
async fn an_undeclared_shared_asset_space_is_not_found() -> TestResult {
    let harness = Harness::new()?;
    let ana = harness.person("Ana").await?;
    let id = harness.artifact(&ana, None, &[]).await?;
    let (status, _) = harness
        .send(
            "GET",
            &format!("/v1/artifacts/{id}/assets?space=shared"),
            &ana.secret,
            None,
            None,
        )
        .await?;
    assert_eq!(status, 404);
    let (status, _) = harness
        .upload(
            &ana.secret,
            Some("ana-undeclared"),
            (&id, "?space=shared"),
            0,
            HI,
        )
        .await?;
    assert_eq!(status, 404);
    Ok(())
}

/// A shared upload and delete reach the owner's and every member's change
/// stream as `shared_assets_changed` (decision 0060 rule 13).
#[tokio::test]
async fn a_shared_asset_change_reaches_every_readers_stream() -> TestResult {
    let harness = Harness::new()?;
    let ana = harness.person("Ana").await?;
    let cy = harness.person("Cy").await?;
    let id = harness
        .artifact(&ana, Some(EDITORS), &[(&cy, "viewer")])
        .await?;
    let mut streams = [
        harness.follow(&ana.secret).await?,
        harness.follow(&cy.secret).await?,
    ];
    let shared = (id.as_str(), "?space=shared");
    let (status, uploaded) = harness
        .upload(&ana.secret, Some("ana-streamed"), shared, 0, HI)
        .await?;
    assert_eq!(status, 201);
    let changed = r#""change":"shared_assets_changed""#;
    for stream in &mut streams {
        stream.until(changed).await?;
        stream.seen.clear();
    }
    // A delete wakes the streams too.
    let asset = uploaded["asset"]["id"].as_str().ok_or("no asset id")?;
    let (status, _) = harness
        .delete(&ana.secret, Some("ana-unstreamed"), shared, 1, asset)
        .await?;
    assert_eq!(status, 200);
    for stream in &mut streams {
        stream.until(changed).await?;
    }
    Ok(())
}

/// A shared upload's `Location` names the asset in the shared space, where a
/// read finds it.
#[tokio::test]
async fn a_shared_upload_is_located_in_the_shared_space() -> TestResult {
    let harness = Harness::new()?;
    let ana = harness.person("Ana").await?;
    let id = harness.artifact(&ana, Some(EDITORS), &[]).await?;
    let body = json!({
        "base_asset_version": 0, "content_type": "text/plain", "content_base64": HI
    });
    let request = Request::post(format!("/v1/artifacts/{id}/assets?space=shared"))
        .header(header::AUTHORIZATION, format!("Bearer {}", ana.secret))
        .header(header::CONTENT_TYPE, "application/json")
        .header("idempotency-key", "ana-located")
        .body(Body::from(body.to_string()))?;
    let response = harness.api.clone().oneshot(request).await?;
    assert_eq!(response.status().as_u16(), 201);
    let location = response
        .headers()
        .get(header::LOCATION)
        .ok_or("no location")?
        .to_str()?
        .to_owned();
    assert!(location.ends_with("?space=shared"), "{location}");
    harness
        .ok(200, "GET", &location, &ana.secret, None, None)
        .await?;
    Ok(())
}
