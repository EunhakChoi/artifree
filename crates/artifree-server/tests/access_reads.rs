//! The access reads of decision 0060 rule 6 (slice S02e1), through the real
//! router, application core and `SQLite` store: `GET /v1/me` and
//! `GET /v1/artifacts/{id}/access`. Members are granted in the store; the
//! routes that change access are a later slice.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use artifree_contracts::api::{ArtifactAccess, CreateTokenResponse, Me, MemberRole, UserInfo};
use artifree_domain::access::{AccessVersion, Role};
use artifree_domain::identity::{ActorId, ArtifactId, UnixMillis};
use artifree_ports::access_store::{AccessChange, AccessRequest, AccessStore};
use artifree_ports::artifact_store::{RequestFingerprint, RequestIdentity};
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
    access: SqliteStore,
    version: u64,
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
            "artifree-access-reads-{}-{}",
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
            access: SqliteStore::open(&database)?,
            directory,
            version: 0,
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

    /// An artifact `owner` creates.
    async fn artifact(&self, owner: &Person) -> Result<String, TestError> {
        let created: Value = self
            .expect(
                201,
                "POST",
                "/v1/artifacts",
                &owner.secret,
                Some("create-shared"),
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

    /// Gives `user` `role` on `artifact` in the store, as an owner's grant
    /// would.
    fn grant(&mut self, owner: &Person, artifact: &str, user: &Person, role: Role) -> TestResult {
        let (owner, artifact, user): (ActorId, ArtifactId, ActorId) =
            (owner.id.parse()?, artifact.parse()?, user.id.parse()?);
        self.access.change_access(AccessRequest {
            request: RequestIdentity {
                actor: &owner,
                key: &format!("grant-access-{}", self.version).parse()?,
                fingerprint: RequestFingerprint([7; 32]),
            },
            artifact: &artifact,
            user: &user,
            change: AccessChange::Grant(role),
            base: AccessVersion(self.version),
            now: UnixMillis(1),
        })?;
        self.version += 1;
        Ok(())
    }

    async fn access(&self, artifact: &str, token: &str) -> Result<ArtifactAccess, TestError> {
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

#[tokio::test]
async fn me_names_the_caller_and_needs_a_credential() -> TestResult {
    let harness = Harness::new()?;
    let ana = harness.person("Ana").await?;
    let me: Me = harness
        .expect(200, "GET", "/v1/me", &ana.secret, None, None)
        .await?;
    assert_eq!(
        (me.id.as_str(), me.label.as_str()),
        (ana.id.as_str(), "Ana")
    );
    let owner: Me = harness
        .expect(200, "GET", "/v1/me", OWNER, None, None)
        .await?;
    assert_eq!(owner.id, ActorId::owner().to_string());
    let (status, _) = harness.send("GET", "/v1/me", None, None, None).await?;
    assert_eq!(status, 401);
    Ok(())
}

/// Rules 6, 8 and 18: the owner and the members read who may reach the
/// artifact, each with id, label and role; only the owner is told who is
/// disabled; anyone else gets the `404` of an unknown artifact.
#[tokio::test]
async fn the_access_list_is_read_by_the_owner_and_members_only() -> TestResult {
    let mut harness = Harness::new()?;
    let (ana, bob, cy, dee) = (
        harness.person("Ana").await?,
        harness.person("Bob").await?,
        harness.person("Cy").await?,
        harness.person("Dee").await?,
    );
    let shared = harness.artifact(&ana).await?;
    harness.grant(&ana, &shared, &bob, Role::Viewer)?;
    harness.grant(&ana, &shared, &cy, Role::Editor)?;
    harness
        .expect::<Value>(
            200,
            "POST",
            &format!("/v1/users/{}/disable", cy.id),
            OWNER,
            None,
            None,
        )
        .await?;

    let mut members = [
        (bob.id.clone(), "Bob", MemberRole::Viewer, false),
        (cy.id.clone(), "Cy", MemberRole::Editor, true),
    ];
    members.sort_by(|left, right| left.0.cmp(&right.0));
    let owners = harness.access(&shared, &ana.secret).await?;
    assert_eq!(owners.artifact_id, shared);
    assert_eq!(
        (owners.owner.id.as_str(), owners.owner.label.as_str()),
        (ana.id.as_str(), "Ana")
    );
    assert_eq!(owners.access_version, 2);
    let seen: Vec<_> = owners
        .members
        .iter()
        .map(|member| {
            (
                member.id.clone(),
                member.label.as_str(),
                member.role,
                member.disabled,
            )
        })
        .collect();
    let told: Vec<_> = members
        .iter()
        .map(|(id, label, role, disabled)| (id.clone(), *label, *role, Some(*disabled)))
        .collect();
    assert_eq!(seen, told);

    let viewers = harness.access(&shared, &bob.secret).await?;
    assert_eq!(viewers.owner, owners.owner);
    assert_eq!(viewers.access_version, 2);
    assert!(
        viewers
            .members
            .iter()
            .all(|member| member.disabled.is_none())
    );
    assert_eq!(
        viewers
            .members
            .iter()
            .map(|member| &member.id)
            .collect::<Vec<_>>(),
        owners
            .members
            .iter()
            .map(|member| &member.id)
            .collect::<Vec<_>>()
    );

    let path = format!("/v1/artifacts/{shared}/access");
    let unknown = format!("/v1/artifacts/{}/access", "a".repeat(26));
    let stranger = harness
        .send("GET", &path, Some(&dee.secret), None, None)
        .await?;
    let nothing = harness
        .send("GET", &unknown, Some(&dee.secret), None, None)
        .await?;
    assert_eq!(stranger.0, 404);
    assert_eq!(stranger, nothing);
    Ok(())
}

/// Rule 4: in the trash only the owner reaches the artifact, its access list
/// included.
#[tokio::test]
async fn in_the_trash_only_the_owner_reads_the_access_list() -> TestResult {
    let mut harness = Harness::new()?;
    let (ana, bob) = (harness.person("Ana").await?, harness.person("Bob").await?);
    let shared = harness.artifact(&ana).await?;
    harness.grant(&ana, &shared, &bob, Role::Viewer)?;
    harness
        .expect::<Value>(
            200,
            "PATCH",
            &format!("/v1/artifacts/{shared}"),
            &ana.secret,
            Some("trash-shared"),
            Some(json!({ "base_metadata_revision": 1, "deleted": true })),
        )
        .await?;
    assert_eq!(harness.access(&shared, &ana.secret).await?.members.len(), 1);
    let (status, _) = harness
        .send(
            "GET",
            &format!("/v1/artifacts/{shared}/access"),
            Some(&bob.secret),
            None,
            None,
        )
        .await?;
    assert_eq!(status, 404);
    Ok(())
}
