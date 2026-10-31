//! Integration tests for single-owner token management.
//!
//! Verifies agent token creation, listing, rotation, cascading revocation,
//! administrative boundary enforcement (403 on agent tokens), and persistence across restarts.

use std::error::Error;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use artifree_contracts::api::{CreateTokenResponse, ErrorResponse, RotateTokenResponse, TokenList};
use artifree_providers::storage::SqliteStore;
use artifree_providers::tokens::SqliteTokenStore;
use artifree_server::auth::AccessToken;
use artifree_server::http::{self, AppState, Settings};
use artifree_server::limits::RequestLimits;
use artifree_server::{changes, preview};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use http_body::Body as _;
use serde_json::Value;
use tower::ServiceExt;

const CONTENT_ORIGIN: &str = "http://127.0.0.1:7411";
const OWNER_TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct Harness {
    directory: PathBuf,
    api: Router,
    content: Router,
    clean_on_drop: bool,
}

impl Harness {
    fn new() -> Result<Self, Box<dyn Error>> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!(
            "artifree-tokens-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory)?;
        Self::open_with_cleanup(directory, true)
    }

    fn open(directory: PathBuf) -> Result<Self, Box<dyn Error>> {
        Self::open_with_cleanup(directory, false)
    }

    fn open_with_cleanup(directory: PathBuf, clean_on_drop: bool) -> Result<Self, Box<dyn Error>> {
        std::fs::create_dir_all(&directory)?;
        let db_path = directory.join("db.sqlite3");
        let store = SqliteStore::open(&db_path)?;
        let token_store = SqliteTokenStore::open(&db_path)?;
        let token = AccessToken::parse(OWNER_TOKEN).ok_or("invalid owner token")?;
        let state = AppState::new(
            store,
            token_store,
            token,
            Settings {
                content_origin: CONTENT_ORIGIN.into(),
                preview_ttl: Duration::from_secs(60),
                frame_ancestors: "http://127.0.0.1:*".into(),
                limits: RequestLimits::default(),
                change_streams: changes::DEFAULT_STREAMS_PER_ACTOR,
                ..Settings::default()
            },
        )
        .map_err(|e| format!("app state error: {e}"))?;
        Ok(Self {
            directory,
            api: http::router(state.clone()),
            content: preview::router(state),
            clean_on_drop,
        })
    }

    async fn send(&self, request: Request<Body>) -> Result<Reply, Box<dyn Error>> {
        Reply::from(self.api.clone().oneshot(request).await?).await
    }

    async fn oneshot(
        &self,
        request: Request<Body>,
    ) -> Result<axum::response::Response, Box<dyn Error>> {
        Ok(self.api.clone().oneshot(request).await?)
    }

    async fn content(&self, url: &str) -> Result<Reply, Box<dyn Error>> {
        let path = url
            .strip_prefix(CONTENT_ORIGIN)
            .ok_or("preview URL uses another origin")?;
        let request = Request::get(path).body(Body::empty())?;
        Reply::from(self.content.clone().oneshot(request).await?).await
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if self.clean_on_drop {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }
}

struct Reply {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}

impl Reply {
    async fn from(response: axum::response::Response) -> Result<Self, Box<dyn Error>> {
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), usize::MAX).await?.to_vec();
        Ok(Self {
            status,
            headers,
            body,
        })
    }

    fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T, Box<dyn Error>> {
        Ok(serde_json::from_slice(&self.body)?)
    }
}

fn request(
    method: &str,
    path: &str,
    bearer_token: &str,
    body: Option<&Value>,
) -> Result<Request<Body>, Box<dyn Error>> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {bearer_token}"));
    if let Some(json_val) = body {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
        let bytes = serde_json::to_vec(json_val)?;
        Ok(builder.body(Body::from(bytes))?)
    } else {
        Ok(builder.body(Body::empty())?)
    }
}

#[tokio::test]
async fn token_creation_listing_and_administrative_boundary() -> Result<(), Box<dyn Error>> {
    let harness = Harness::new()?;

    // 1. Initial listing with owner token contains only bootstrap token
    let initial_reply = harness
        .send(request("GET", "/v1/tokens", OWNER_TOKEN, None)?)
        .await?;
    assert_eq!(initial_reply.status, StatusCode::OK);
    let initial_list: TokenList = initial_reply.json()?;
    assert_eq!(initial_list.tokens.len(), 1);
    assert_eq!(initial_list.tokens[0].id, "bootstrap");
    assert!(initial_list.tokens[0].is_admin);
    assert_eq!(initial_list.tokens[0].name, "Bootstrap Owner");

    // 2. Unauthenticated request to /v1/tokens is rejected with 401
    let unauth_query = Request::builder()
        .method("GET")
        .uri("/v1/tokens")
        .body(Body::empty())?;
    let unauth_reply = harness.send(unauth_query).await?;
    assert_eq!(unauth_reply.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        unauth_reply
            .headers
            .get(header::WWW_AUTHENTICATE)
            .ok_or("missing header")?,
        "Bearer"
    );

    // 3. Create a new agent token with owner token
    let create_payload = serde_json::json!({ "name": "Codex Agent" });
    let create_reply = harness
        .send(request(
            "POST",
            "/v1/tokens",
            OWNER_TOKEN,
            Some(&create_payload),
        )?)
        .await?;
    assert_eq!(create_reply.status, StatusCode::CREATED);
    let created: CreateTokenResponse = create_reply.json()?;
    assert!(created.secret.starts_with("art_"));
    assert_eq!(created.token.name, "Codex Agent");
    assert!(!created.token.is_admin);
    assert!(created.token.id.starts_with("tok_"));
    let agent_id = created.token.id.clone();
    let agent_secret = created.secret.clone();

    // 4. Listing now includes both bootstrap and the new agent token
    let updated_reply = harness
        .send(request("GET", "/v1/tokens", OWNER_TOKEN, None)?)
        .await?;
    assert_eq!(updated_reply.status, StatusCode::OK);
    let updated_list: TokenList = updated_reply.json()?;
    assert_eq!(updated_list.tokens.len(), 2);
    assert_eq!(updated_list.tokens[1].id, agent_id);
    assert_eq!(updated_list.tokens[1].name, "Codex Agent");
    assert_eq!(updated_list.tokens[1].prefix, created.token.prefix);

    // 5. Agent token can access regular API (e.g. GET /v1/artifacts)
    let artifacts_reply = harness
        .send(request("GET", "/v1/artifacts", &agent_secret, None)?)
        .await?;
    assert_eq!(artifacts_reply.status, StatusCode::OK);

    // 6. Agent token CANNOT access administrative endpoints (/v1/tokens) -> 403 Forbidden!
    let admin_read_reply = harness
        .send(request("GET", "/v1/tokens", &agent_secret, None)?)
        .await?;
    assert_eq!(admin_read_reply.status, StatusCode::FORBIDDEN);
    let err: ErrorResponse = admin_read_reply.json()?;
    assert_eq!(
        err.error.code,
        artifree_contracts::api::ErrorCode::Forbidden
    );

    let admin_create_reply = harness
        .send(request(
            "POST",
            "/v1/tokens",
            &agent_secret,
            Some(&create_payload),
        )?)
        .await?;
    assert_eq!(admin_create_reply.status, StatusCode::FORBIDDEN);

    let admin_rotate_reply = harness
        .send(request(
            "POST",
            &format!("/v1/tokens/{agent_id}/rotate"),
            &agent_secret,
            None,
        )?)
        .await?;
    assert_eq!(admin_rotate_reply.status, StatusCode::FORBIDDEN);

    let admin_revoke_reply = harness
        .send(request(
            "DELETE",
            &format!("/v1/tokens/{agent_id}"),
            &agent_secret,
            None,
        )?)
        .await?;
    assert_eq!(admin_revoke_reply.status, StatusCode::FORBIDDEN);

    Ok(())
}

#[tokio::test]
async fn token_rotation_and_revocation() -> Result<(), Box<dyn Error>> {
    let harness = Harness::new()?;

    // Create an agent token to rotate and revoke
    let create_payload = serde_json::json!({ "name": "Agent Lifecycle" });
    let create_reply = harness
        .send(request(
            "POST",
            "/v1/tokens",
            OWNER_TOKEN,
            Some(&create_payload),
        )?)
        .await?;
    let created: CreateTokenResponse = create_reply.json()?;
    let agent_id = created.token.id;
    let original_secret = created.secret;

    // Rotate agent token with owner token
    let rotate_reply = harness
        .send(request(
            "POST",
            &format!("/v1/tokens/{agent_id}/rotate"),
            OWNER_TOKEN,
            None,
        )?)
        .await?;
    assert_eq!(rotate_reply.status, StatusCode::OK);
    let rotated: RotateTokenResponse = rotate_reply.json()?;
    let next_secret = rotated.secret;
    assert_ne!(next_secret, original_secret);
    assert!(next_secret.starts_with("art_"));

    // Old secret returns 401 Unauthorized
    let old_reply = harness
        .send(request("GET", "/v1/artifacts", &original_secret, None)?)
        .await?;
    assert_eq!(old_reply.status, StatusCode::UNAUTHORIZED);

    // New secret returns 200 OK
    let new_reply = harness
        .send(request("GET", "/v1/artifacts", &next_secret, None)?)
        .await?;
    assert_eq!(new_reply.status, StatusCode::OK);

    // Bootstrap token rotation and revocation are forbidden
    let rot_boot = request("POST", "/v1/tokens/bootstrap/rotate", OWNER_TOKEN, None)?;
    assert_eq!(harness.send(rot_boot).await?.status, StatusCode::FORBIDDEN);

    let rev_boot = request("DELETE", "/v1/tokens/bootstrap", OWNER_TOKEN, None)?;
    assert_eq!(harness.send(rev_boot).await?.status, StatusCode::FORBIDDEN);

    // Revoke the agent token with owner token
    let delete_reply = harness
        .send(request(
            "DELETE",
            &format!("/v1/tokens/{agent_id}"),
            OWNER_TOKEN,
            None,
        )?)
        .await?;
    assert_eq!(delete_reply.status, StatusCode::NO_CONTENT);

    // Revoked secret now returns 401 Unauthorized
    let post_revoke_reply = harness
        .send(request("GET", "/v1/artifacts", &next_secret, None)?)
        .await?;
    assert_eq!(post_revoke_reply.status, StatusCode::UNAUTHORIZED);

    // Listing shows the token is revoked
    let list_reply = harness
        .send(request("GET", "/v1/tokens", OWNER_TOKEN, None)?)
        .await?;
    let list: TokenList = list_reply.json()?;
    assert!(list.tokens[1].revoked_at.is_some());

    Ok(())
}

#[tokio::test]
async fn cascading_revocation_invalidates_previews() -> Result<(), Box<dyn Error>> {
    let harness = Harness::new()?;

    // Create an artifact first
    let mut create_artifact = request(
        "POST",
        "/v1/artifacts",
        OWNER_TOKEN,
        Some(&serde_json::json!({
            "title": "Preview Test",
            "files": [{ "path": "index.html", "encoding": "utf8", "content": "<h1>Hello</h1>" }]
        })),
    )?;
    create_artifact
        .headers_mut()
        .insert("idempotency-key", "key-artifact-1".parse()?);
    let artifact_reply = harness.send(create_artifact).await?;
    assert_eq!(artifact_reply.status, StatusCode::CREATED);
    let artifact_body: Value = artifact_reply.json()?;
    let artifact_id = artifact_body["artifact"]["id"]
        .as_str()
        .ok_or("missing artifact id")?;

    // Create an agent token
    let create_tok = request(
        "POST",
        "/v1/tokens",
        OWNER_TOKEN,
        Some(&serde_json::json!({ "name": "Preview Agent" })),
    )?;
    let created: CreateTokenResponse = harness.send(create_tok).await?.json()?;

    // Agent issues a preview
    let prev_req = request(
        "POST",
        &format!("/v1/artifacts/{artifact_id}/revisions/1/previews"),
        &created.secret,
        None,
    )?;
    let prev_reply = harness.send(prev_req).await?;
    assert_eq!(prev_reply.status, StatusCode::CREATED);
    let prev_body: Value = prev_reply.json()?;
    let preview_url = prev_body["url"].as_str().ok_or("missing preview url")?;

    // Content server serves the preview
    let view_reply = harness.content(preview_url).await?;
    assert_eq!(view_reply.status, StatusCode::OK);

    // Revoke the agent token
    let rev_req = request(
        "DELETE",
        &format!("/v1/tokens/{}", created.token.id),
        OWNER_TOKEN,
        None,
    )?;
    assert_eq!(harness.send(rev_req).await?.status, StatusCode::NO_CONTENT);

    // Previously valid preview ticket is now invalidated!
    let view_after_revoke = harness.content(preview_url).await?;
    assert_eq!(view_after_revoke.status, StatusCode::NOT_FOUND);

    Ok(())
}

#[tokio::test]
async fn persistence_across_restart() -> Result<(), Box<dyn Error>> {
    static COUNTER: AtomicU32 = AtomicU32::new(100);
    let directory = std::env::temp_dir().join(format!(
        "artifree-persist-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory)?;

    let (active_secret, active_id, revoked_id) = {
        let harness = Harness::open(directory.clone())?;

        // Create token 1
        let tok1_req = request(
            "POST",
            "/v1/tokens",
            OWNER_TOKEN,
            Some(&serde_json::json!({ "name": "Agent Persistent" })),
        )?;
        let tok1: CreateTokenResponse = harness.send(tok1_req).await?.json()?;

        // Create token 2 and revoke it
        let tok2_req = request(
            "POST",
            "/v1/tokens",
            OWNER_TOKEN,
            Some(&serde_json::json!({ "name": "Agent Revoked" })),
        )?;
        let tok2: CreateTokenResponse = harness.send(tok2_req).await?.json()?;

        let rev_req = request(
            "DELETE",
            &format!("/v1/tokens/{}", tok2.token.id),
            OWNER_TOKEN,
            None,
        )?;
        assert_eq!(harness.send(rev_req).await?.status, StatusCode::NO_CONTENT);

        (tok1.secret, tok1.token.id, tok2.token.id)
    };

    // Reopen server on the exact same directory (simulating restart)
    let reopened = Harness::open(directory.clone())?;

    // Active token still works
    let query = request("GET", "/v1/artifacts", &active_secret, None)?;
    assert_eq!(reopened.send(query).await?.status, StatusCode::OK);

    // Revoked token remains rejected with 401
    // (We don't know tok2 secret, but query token list to verify it's marked revoked)
    let list_req = request("GET", "/v1/tokens", OWNER_TOKEN, None)?;
    let list: TokenList = reopened.send(list_req).await?.json()?;
    assert_eq!(list.tokens.len(), 3); // bootstrap + persistent + revoked

    let persistent_entry = list
        .tokens
        .iter()
        .find(|t| t.id == active_id)
        .ok_or("missing persistent token")?;
    assert_eq!(persistent_entry.name, "Agent Persistent");
    assert!(persistent_entry.revoked_at.is_none());

    let revoked_entry = list
        .tokens
        .iter()
        .find(|t| t.id == revoked_id)
        .ok_or("missing revoked token")?;
    assert_eq!(revoked_entry.name, "Agent Revoked");
    assert!(revoked_entry.revoked_at.is_some());

    let _ = std::fs::remove_dir_all(&directory);
    Ok(())
}

#[tokio::test]
async fn cascading_revocation_closes_active_change_stream() -> Result<(), Box<dyn Error>> {
    use std::future::poll_fn;
    use std::pin::Pin;

    let harness = Harness::new()?;

    // Create an agent token
    let create_tok = request(
        "POST",
        "/v1/tokens",
        OWNER_TOKEN,
        Some(&serde_json::json!({ "name": "Stream Agent" })),
    )?;
    let created: CreateTokenResponse = harness.send(create_tok).await?.json()?;

    // Open change stream with the agent token
    let stream_req = Request::get("/v1/changes")
        .header(header::AUTHORIZATION, format!("Bearer {}", created.secret))
        .body(Body::empty())?;
    let response = harness.oneshot(stream_req).await?;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();

    // Read the initial ready event frame
    let initial_frame = tokio::time::timeout(
        Duration::from_secs(5),
        poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)),
    )
    .await
    .map_err(|_| "stream timed out waiting for initial frame")?;
    assert!(initial_frame.is_some());

    // Revoke the agent token with owner token
    let rev_req = request(
        "DELETE",
        &format!("/v1/tokens/{}", created.token.id),
        OWNER_TOKEN,
        None,
    )?;
    assert_eq!(harness.send(rev_req).await?.status, StatusCode::NO_CONTENT);

    // Active change stream must terminate promptly
    let next_frame = tokio::time::timeout(
        Duration::from_secs(5),
        poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)),
    )
    .await
    .map_err(|_| "stream timed out waiting for stream closure")?;
    assert!(
        next_frame.is_none(),
        "stream must be closed after token revocation"
    );

    Ok(())
}

#[tokio::test]
async fn cascading_revocation_closes_active_runtime_session() -> Result<(), Box<dyn Error>> {
    let harness = Harness::new()?;

    // Create an agent token
    let create_tok = request(
        "POST",
        "/v1/tokens",
        OWNER_TOKEN,
        Some(&serde_json::json!({ "name": "Runtime Agent" })),
    )?;
    let created: CreateTokenResponse = harness.send(create_tok).await?.json()?;

    // Open a runtime session using the agent token
    let open_req = Request::post("/v1/runtime-sessions")
        .header(header::AUTHORIZATION, format!("Bearer {}", created.secret))
        .body(Body::empty())?;
    let open_reply = harness.send(open_req).await?;
    assert_eq!(open_reply.status, StatusCode::OK);
    let session_body: Value = open_reply.json()?;
    let session_id = session_body["session_id"]
        .as_str()
        .ok_or("missing session_id")?;

    // Create an artifact to test grant operations
    let mut create_art = request(
        "POST",
        "/v1/artifacts",
        OWNER_TOKEN,
        Some(&serde_json::json!({
            "title": "Session Test",
            "files": [{ "path": "index.html", "encoding": "utf8", "content": "<!doctype html><html><head><!-- artifree-runtime --></head><body><h1>Session</h1></body></html>" }]
        })),
    )?;
    create_art
        .headers_mut()
        .insert("idempotency-key", "key-session-1".parse()?);
    let art_reply = harness.send(create_art).await?;
    assert_eq!(art_reply.status, StatusCode::CREATED);
    let art_body: Value = art_reply.json()?;
    let artifact_id = art_body["artifact"]["id"]
        .as_str()
        .ok_or("missing artifact_id")?;

    // Setting a grant initially succeeds
    let grant_req = Request::put(format!(
        "/v1/runtime-sessions/{session_id}/artifacts/{artifact_id}/revisions/1/grant"
    ))
    .header(header::AUTHORIZATION, format!("Bearer {OWNER_TOKEN}"))
    .header(header::CONTENT_TYPE, "application/json")
    .body(Body::from(serde_json::to_vec(&serde_json::json!({
        "head_revision": 1,
        "expected_version": 0,
        "capabilities": ["metadata.read"]
    }))?))?;
    let grant_reply = harness.send(grant_req).await?;
    assert_eq!(grant_reply.status, StatusCode::OK);

    // Revoke the agent token with owner token
    let rev_req = request(
        "DELETE",
        &format!("/v1/tokens/{}", created.token.id),
        OWNER_TOKEN,
        None,
    )?;
    assert_eq!(harness.send(rev_req).await?.status, StatusCode::NO_CONTENT);

    // Setting a grant on the revoked session now fails with 404 Not Found!
    let grant_after_revoke = Request::put(format!(
        "/v1/runtime-sessions/{session_id}/artifacts/{artifact_id}/revisions/1/grant"
    ))
    .header(header::AUTHORIZATION, format!("Bearer {OWNER_TOKEN}"))
    .header(header::CONTENT_TYPE, "application/json")
    .body(Body::from(serde_json::to_vec(&serde_json::json!({
        "head_revision": 1,
        "expected_version": 1,
        "capabilities": ["metadata.read"]
    }))?))?;
    let reply_after_revoke = harness.send(grant_after_revoke).await?;
    assert_eq!(reply_after_revoke.status, StatusCode::NOT_FOUND);

    Ok(())
}
