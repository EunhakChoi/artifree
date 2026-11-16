//! The owner's changes of access of decision 0060 rule 6 (slice S02e2),
//! through the real router, application core and `SQLite` store:
//! `PUT` and `DELETE /v1/artifacts/{id}/access/{user_id}`, who may call them,
//! their conflicts, replays and refusals, and their order.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use artifree_contracts::api::{
    AccessChangeResponse, ArtifactAccess, CreateTokenResponse, UserInfo,
};
use artifree_domain::identity::ActorId;
use artifree_providers::storage::SqliteStore;
use artifree_providers::tokens::SqliteTokenStore;
use artifree_server::auth::AccessToken;
use artifree_server::http::{self, AppState, Settings};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, header};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tower::ServiceExt;

type TestError = Box<dyn std::error::Error>;
type TestResult = Result<(), TestError>;

const OWNER: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

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
            "artifree-access-changes-{}-{}",
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
            api: http::router(state),
            directory,
        })
    }

    async fn send(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        key: Option<&str>,
        body: Option<Value>,
    ) -> Result<(u16, String), TestError> {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
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
        Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
    }

    async fn expect<T: DeserializeOwned>(
        &self,
        status: u16,
        method: &str,
        path: &str,
        token: &str,
        key: Option<&str>,
        body: Option<Value>,
    ) -> Result<T, TestError> {
        let (answer, text) = self.send(method, path, Some(token), key, body).await?;
        assert_eq!(answer, status, "{method} {path}: {text}");
        Ok(serde_json::from_str(&text)?)
    }

    async fn person(&self, label: &str) -> Result<Person, TestError> {
        let user: UserInfo = self
            .expect(
                201,
                "POST",
                "/v1/users",
                OWNER,
                None,
                Some(json!({ "label": label })),
            )
            .await?;
        let issued: CreateTokenResponse = self
            .expect(
                201,
                "POST",
                "/v1/tokens",
                OWNER,
                None,
                Some(json!({ "name": label, "user_id": user.id })),
            )
            .await?;
        Ok(Person {
            id: user.id,
            secret: issued.secret,
        })
    }

    /// An artifact created with `token`, under `key`.
    async fn artifact(&self, token: &str, key: &str) -> Result<String, TestError> {
        let created: Value = self
            .expect(
                201,
                "POST",
                "/v1/artifacts",
                token,
                Some(key),
                Some(json!({
                    "title": "Shared",
                    "files": [{ "path": "index.html", "encoding": "utf8", "content": "x" }],
                })),
            )
            .await?;
        Ok(created["artifact"]["id"]
            .as_str()
            .ok_or("no artifact id")?
            .to_owned())
    }

    async fn put(
        &self,
        token: &str,
        key: Option<&str>,
        artifact: &str,
        user: &str,
        body: Value,
    ) -> Result<(u16, String), TestError> {
        self.send(
            "PUT",
            &format!("/v1/artifacts/{artifact}/access/{user}"),
            Some(token),
            key,
            Some(body),
        )
        .await
    }

    async fn delete(
        &self,
        token: &str,
        key: Option<&str>,
        artifact: &str,
        user: &str,
        base: u64,
    ) -> Result<(u16, String), TestError> {
        self.send(
            "DELETE",
            &format!("/v1/artifacts/{artifact}/access/{user}"),
            Some(token),
            key,
            Some(json!({ "base_access_version": base })),
        )
        .await
    }

    async fn list(&self, artifact: &str, token: &str) -> Result<ArtifactAccess, TestError> {
        self.expect(
            200,
            "GET",
            &format!("/v1/artifacts/{artifact}/access"),
            token,
            None,
            None,
        )
        .await
    }
}

fn role(role: &str, base: u64) -> Value {
    json!({ "role": role, "base_access_version": base })
}

fn changed(text: &str) -> Result<AccessChangeResponse, TestError> {
    Ok(serde_json::from_str(text)?)
}

fn code(text: &str) -> Result<String, TestError> {
    let body: Value = serde_json::from_str(text)?;
    Ok(body["error"]["code"].as_str().ok_or("no code")?.to_owned())
}

/// Each change raises the access version by one, the list follows it, a
/// grant of the role a member has changes nothing, and a key replays its
/// answer or is refused for another change.
#[tokio::test]
async fn the_owner_grants_changes_and_revokes_a_role() -> TestResult {
    let harness = Harness::new()?;
    let (ana, bob) = (harness.person("Ana").await?, harness.person("Bob").await?);
    let shared = harness.artifact(&ana.secret, "create-shared").await?;

    let (status, text) = harness
        .put(
            &ana.secret,
            Some("key-grant"),
            &shared,
            &bob.id,
            role("viewer", 0),
        )
        .await?;
    assert_eq!(status, 200, "{text}");
    let granted = changed(&text)?;
    assert_eq!(
        (granted.artifact_id.as_str(), granted.user_id.as_str()),
        (shared.as_str(), bob.id.as_str())
    );
    assert_eq!((granted.access_version, granted.replayed), (1, false));
    let listed = harness.list(&shared, &ana.secret).await?;
    assert_eq!(listed.access_version, 1);
    assert_eq!(listed.members.len(), 1);
    assert_eq!(
        serde_json::to_value(listed.members[0].role)?,
        json!("viewer")
    );
    // Bob reads it now as a member.
    assert_eq!(harness.list(&shared, &bob.secret).await?.access_version, 1);

    let (_, text) = harness
        .put(
            &ana.secret,
            Some("key-grant"),
            &shared,
            &bob.id,
            role("viewer", 0),
        )
        .await?;
    assert_eq!(
        (changed(&text)?.access_version, changed(&text)?.replayed),
        (1, true)
    );
    let (status, text) = harness
        .put(
            &ana.secret,
            Some("key-grant"),
            &shared,
            &bob.id,
            role("editor", 0),
        )
        .await?;
    assert_eq!((status, code(&text)?.as_str()), (422, "request_key_reused"));

    let (_, text) = harness
        .put(
            &ana.secret,
            Some("key-same"),
            &shared,
            &bob.id,
            role("viewer", 1),
        )
        .await?;
    assert_eq!(
        changed(&text)?.access_version,
        1,
        "the same role changes nothing"
    );
    let (_, text) = harness
        .put(
            &ana.secret,
            Some("key-promote"),
            &shared,
            &bob.id,
            role("editor", 1),
        )
        .await?;
    assert_eq!(changed(&text)?.access_version, 2);
    let listed = harness.list(&shared, &ana.secret).await?;
    assert_eq!(
        serde_json::to_value(listed.members[0].role)?,
        json!("editor")
    );

    let (status, text) = harness
        .delete(&ana.secret, Some("key-revoke"), &shared, &bob.id, 2)
        .await?;
    assert_eq!(status, 200, "{text}");
    assert_eq!(changed(&text)?.access_version, 3);
    assert!(harness.list(&shared, &ana.secret).await?.members.is_empty());
    let (status, _) = harness
        .send(
            "GET",
            &format!("/v1/artifacts/{shared}/access"),
            Some(&bob.secret),
            None,
            None,
        )
        .await?;
    assert_eq!(status, 404, "a revoked user finds no artifact");
    Ok(())
}

/// A stale base is `access_conflict` with the current version, and writes
/// nothing.
#[tokio::test]
async fn a_stale_base_is_an_access_conflict() -> TestResult {
    let harness = Harness::new()?;
    let (ana, bob) = (harness.person("Ana").await?, harness.person("Bob").await?);
    let shared = harness.artifact(&ana.secret, "create-shared").await?;
    harness
        .put(
            &ana.secret,
            Some("key-grant"),
            &shared,
            &bob.id,
            role("viewer", 0),
        )
        .await?;
    for (status, text) in [
        harness
            .put(
                &ana.secret,
                Some("key-stale"),
                &shared,
                &bob.id,
                role("editor", 0),
            )
            .await?,
        harness
            .delete(&ana.secret, Some("stale-revoke"), &shared, &bob.id, 0)
            .await?,
    ] {
        assert_eq!(
            (status, code(&text)?.as_str()),
            (409, "access_conflict"),
            "{text}"
        );
        let body: Value = serde_json::from_str(&text)?;
        assert_eq!(body["error"]["current_access_version"], json!(1));
    }
    let listed = harness.list(&shared, &ana.secret).await?;
    assert_eq!(listed.access_version, 1);
    assert_eq!(
        serde_json::to_value(listed.members[0].role)?,
        json!("viewer")
    );
    Ok(())
}

/// Rule 6: a grant to an id nobody registered, to a disabled user or to the
/// owner is one answer, so the route does not tell them apart; revoking a
/// user who is not a member is `404`, a spelling no user can have too.
#[tokio::test]
async fn refused_users_get_one_answer() -> TestResult {
    let harness = Harness::new()?;
    let (ana, dee) = (harness.person("Ana").await?, harness.person("Dee").await?);
    let shared = harness.artifact(&ana.secret, "create-shared").await?;
    harness
        .expect::<Value>(
            200,
            "POST",
            &format!("/v1/users/{}/disable", dee.id),
            OWNER,
            None,
            None,
        )
        .await?;
    let unknown = format!("u-{}", "e".repeat(32));
    let mut answers = Vec::new();
    for (index, user) in [
        unknown.as_str(),
        dee.id.as_str(),
        ana.id.as_str(),
        "not-a-user",
    ]
    .into_iter()
    .enumerate()
    {
        let key = format!("key-refused-{index}");
        answers.push(
            harness
                .put(&ana.secret, Some(&key), &shared, user, role("viewer", 0))
                .await?,
        );
    }
    assert_eq!(answers[0].0, 400, "{}", answers[0].1);
    assert_eq!(code(&answers[0].1)?, "invalid_request");
    assert!(
        answers.iter().all(|answer| *answer == answers[0]),
        "{answers:?}"
    );
    assert_eq!(harness.list(&shared, &ana.secret).await?.access_version, 0);

    let not_member = harness
        .delete(&ana.secret, Some("key-revoke"), &shared, &dee.id, 0)
        .await?;
    let unspellable = harness
        .delete(&ana.secret, Some("revoke-2"), &shared, "not-a-user", 0)
        .await?;
    assert_eq!(not_member.0, 404);
    assert_eq!(not_member, unspellable);
    Ok(())
}

/// Rule 6: the owner profile's agent token is refused before the key and
/// the body are read; the bootstrap token changes the owner profile's own.
#[tokio::test]
async fn the_owner_profiles_agent_token_changes_no_access() -> TestResult {
    let harness = Harness::new()?;
    let bob = harness.person("Bob").await?;
    // The owner profile's artifact, with its bootstrap token and an agent's.
    let local = harness.artifact(OWNER, "create-local").await?;
    let agent: CreateTokenResponse = harness
        .expect(
            201,
            "POST",
            "/v1/tokens",
            OWNER,
            None,
            Some(json!({ "name": "agent" })),
        )
        .await?;
    let refused = harness
        .put(&agent.secret, None, &local, &bob.id, json!({}))
        .await?;
    assert_eq!((refused.0, code(&refused.1)?.as_str()), (403, "forbidden"));
    let (status, text) = harness
        .put(
            OWNER,
            Some("grant-local"),
            &local,
            &bob.id,
            role("viewer", 0),
        )
        .await?;
    assert_eq!(status, 200, "{text}");
    assert_eq!(
        ActorId::owner().to_string(),
        harness.list(&local, OWNER).await?.owner.id
    );

    Ok(())
}

/// Rules 4 and 6: a member is refused for its role before its body is read;
/// anyone else, and the owner in the trash, find no artifact.
#[tokio::test]
async fn members_and_strangers_change_no_access() -> TestResult {
    let harness = Harness::new()?;
    let (ana, bob, cy) = (
        harness.person("Ana").await?,
        harness.person("Bob").await?,
        harness.person("Cy").await?,
    );
    let shared = harness.artifact(&ana.secret, "create-shared").await?;
    harness
        .put(
            &ana.secret,
            Some("grant-bob"),
            &shared,
            &bob.id,
            role("editor", 0),
        )
        .await?;
    let member = harness
        .put(
            &bob.secret,
            Some("key-bob-1"),
            &shared,
            &cy.id,
            role("viewer", 1),
        )
        .await?;
    assert_eq!((member.0, code(&member.1)?.as_str()), (403, "forbidden"));
    let malformed = harness
        .put(&bob.secret, Some("key-bob-2"), &shared, &cy.id, json!({}))
        .await?;
    assert_eq!(malformed, member, "the role is checked before the body");
    let revoke = harness
        .delete(&bob.secret, Some("key-bob-3"), &shared, &bob.id, 1)
        .await?;
    assert_eq!(revoke, member);

    let stranger = harness
        .put(
            &cy.secret,
            Some("key-cy-01"),
            &shared,
            &cy.id,
            role("viewer", 1),
        )
        .await?;
    let nothing = harness
        .put(
            &cy.secret,
            Some("key-cy-2"),
            &"a".repeat(26),
            &cy.id,
            role("viewer", 1),
        )
        .await?;
    assert_eq!(stranger.0, 404);
    assert_eq!(stranger, nothing);
    let strangers_body = harness
        .put(&cy.secret, Some("key-cy-3"), &shared, &cy.id, json!({}))
        .await?;
    assert_eq!(strangers_body, nothing, "access is checked before the body");

    harness
        .expect::<Value>(
            200,
            "PATCH",
            &format!("/v1/artifacts/{shared}"),
            &ana.secret,
            Some("key-trash"),
            Some(json!({ "base_metadata_revision": 1, "deleted": true })),
        )
        .await?;
    let trashed = harness
        .put(
            &ana.secret,
            Some("in-trash"),
            &shared,
            &cy.id,
            role("viewer", 1),
        )
        .await?;
    assert_eq!(trashed.0, 404, "{}", trashed.1);
    Ok(())
}

/// Decision 0060 rule 7 over HTTP: a member finds the artifact in their list
/// with their role and the owner's id and label, under the shared filter and
/// not the owned one; the trash lists only the caller's own; and reading the
/// artifact reports the caller's role.
#[tokio::test]
async fn a_member_finds_the_shared_artifact_in_their_list() -> TestResult {
    let harness = Harness::new()?;
    let (ana, bob) = (harness.person("Ana").await?, harness.person("Bob").await?);
    let shared = harness.artifact(&ana.secret, "create-listed").await?;
    let (status, text) = harness
        .put(
            &ana.secret,
            Some("key-list-grant"),
            &shared,
            &bob.id,
            role("viewer", 0),
        )
        .await?;
    assert_eq!(status, 200, "{text}");

    let listed: Value = harness
        .expect(
            200,
            "GET",
            "/v1/artifacts?access=shared",
            &bob.secret,
            None,
            None,
        )
        .await?;
    assert_eq!(
        listed["artifacts"],
        json!([{
            "id": shared,
            "kind": listed["artifacts"][0]["kind"],
            "type": null,
            "title": "Shared",
            "pinned": false,
            "current_revision": 1,
            "metadata_revision": 1,
            "deleted_at_ms": null,
            "created_at_ms": listed["artifacts"][0]["created_at_ms"],
            "updated_at_ms": listed["artifacts"][0]["updated_at_ms"],
            "role": "viewer",
            "owner": { "id": ana.id, "label": "Ana" },
        }])
    );
    for path in ["/v1/artifacts?access=owned", "/v1/artifacts?deleted=true"] {
        let none: Value = harness
            .expect(200, "GET", path, &bob.secret, None, None)
            .await?;
        assert_eq!(none["artifacts"], json!([]), "{path}");
    }
    let mine: Value = harness
        .expect(200, "GET", "/v1/artifacts", &ana.secret, None, None)
        .await?;
    assert_eq!(mine["artifacts"][0]["role"], "owner");
    let read: Value = harness
        .expect(
            200,
            "GET",
            &format!("/v1/artifacts/{shared}"),
            &bob.secret,
            None,
            None,
        )
        .await?;
    assert_eq!(read["role"], "viewer");
    assert_eq!(read.get("owner"), None);

    let (status, text) = harness
        .send(
            "PATCH",
            &format!("/v1/artifacts/{shared}"),
            Some(&ana.secret),
            Some("key-list-trash"),
            Some(json!({ "base_metadata_revision": 1, "deleted": true })),
        )
        .await?;
    assert_eq!(status, 200, "{text}");
    let after: Value = harness
        .expect(200, "GET", "/v1/artifacts", &bob.secret, None, None)
        .await?;
    assert_eq!(after["artifacts"], json!([]));
    let trash: Value = harness
        .expect(
            200,
            "GET",
            "/v1/artifacts?deleted=true",
            &ana.secret,
            None,
            None,
        )
        .await?;
    assert_eq!(trash["artifacts"][0]["role"], "owner");
    Ok(())
}

/// A filter the contract does not name is refused, as every bad query is.
#[tokio::test]
async fn an_unknown_list_filter_is_an_invalid_request() -> TestResult {
    let harness = Harness::new()?;
    let bob = harness.person("Bob").await?;
    let (status, text) = harness
        .send(
            "GET",
            "/v1/artifacts?access=everyone",
            Some(&bob.secret),
            None,
            None,
        )
        .await?;
    assert_eq!(
        (status, code(&text)?.as_str()),
        (400, "invalid_request"),
        "{text}"
    );
    Ok(())
}
