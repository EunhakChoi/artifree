//! Build previews and the head status on the wire (decision 0058 rules 14 and
//! 16, slice R03db) through the real router, core, and `SQLite` store:
//! `POST /v1/artifacts/{id}/revisions/{revision}/builds/{profile}/previews`
//! issues a build ticket for a revision with that build, and
//! `GET /v1/artifacts/{id}/builds/{profile}` reads where the head revision's
//! build stands. Both answer `profile_unknown` for a profile the server's
//! build cannot build. No worker runs here: the tests claim and publish a
//! build through the store as the execution worker's store calls do, and
//! write a failure into the database as the worker's store would.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use artifree_domain::identity::UnixMillis;
use artifree_domain::job::{AttemptIdentity, ProfileName, WorkerId};
use artifree_domain::package::{FilePackage, PackagePath};
use artifree_ports::build_store::{BuildPublication, BuildStore};
use artifree_ports::job_store::{ExecutionClaim, JobClaim, JobStore, ProfileBinding};
use artifree_providers::storage::SqliteStore;
use artifree_server::auth::AccessToken;
use artifree_server::http::{self, AppState, Settings};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const OWNER: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const ORIGIN: &str = "http://127.0.0.1:7411";
const TOOL_DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct Harness {
    directory: PathBuf,
    database: PathBuf,
    api: Router,
    /// How many builds the harness published, so each attempt is another.
    published: AtomicU32,
}

impl Harness {
    /// A server whose build knows `build_profiles` as its build profiles.
    fn new(build_profiles: &[&str]) -> Result<Self, Box<dyn std::error::Error>> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!(
            "artifree-build-status-api-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        // A directory left by an earlier run with the same name is cleared first.
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory)?;
        let database = directory.join("db.sqlite3");
        let build_profiles = build_profiles
            .iter()
            .map(|name| name.parse::<ProfileName>())
            .collect::<Result<Vec<_>, _>>()?;
        let state = AppState::new(
            SqliteStore::open(&database)?,
            artifree_providers::tokens::SqliteTokenStore::open(&database)?,
            AccessToken::parse(OWNER).ok_or("fixture token")?,
            Settings {
                content_origin: ORIGIN.into(),
                build_profiles,
                ..Settings::default()
            },
        )?;
        Ok(Self {
            directory,
            database,
            api: http::router(state),
            published: AtomicU32::new(0),
        })
    }

    /// A server whose build knows `react-v1`.
    fn building() -> Result<Self, Box<dyn std::error::Error>> {
        Self::new(&["react-v1"])
    }

    async fn send(
        &self,
        method: &str,
        path: &str,
        key: Option<&str>,
        body: Option<&Value>,
    ) -> Result<Reply, Box<dyn std::error::Error>> {
        self.send_as(OWNER, method, path, key, body).await
    }

    async fn send_as(
        &self,
        token: &str,
        method: &str,
        path: &str,
        key: Option<&str>,
        body: Option<&Value>,
    ) -> Result<Reply, Box<dyn std::error::Error>> {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(header::AUTHORIZATION, format!("Bearer {token}"));
        if let Some(key) = key {
            // A key is 8-128 characters; the tests name them briefly.
            builder = builder.header("idempotency-key", format!("test-key-{key}"));
        }
        let request = match body {
            Some(body) => builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))?,
            None => builder.body(Body::empty())?,
        };
        let response = self.api.clone().oneshot(request).await?;
        let status = response.status();
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let bytes = to_bytes(response.into_body(), usize::MAX).await?;
        Ok(Reply {
            status,
            location,
            text: String::from_utf8_lossy(&bytes).into_owned(),
        })
    }

    /// An artifact with two revisions; returns its identifier.
    async fn artifact(&self) -> Result<String, Box<dyn std::error::Error>> {
        let created = self
            .send(
                "POST",
                "/v1/artifacts",
                Some("create"),
                Some(&json!({
                    "title": "Build",
                    "files": [
                        { "path": "index.html", "encoding": "utf8", "content": "<h1>one</h1>" }
                    ]
                })),
            )
            .await?;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
        let id = created.json()?["artifact"]["id"]
            .as_str()
            .ok_or("no artifact id")?
            .to_owned();
        let saved = self
            .send(
                "POST",
                &format!("/v1/artifacts/{id}/revisions"),
                Some("save"),
                Some(&json!({
                    "base_revision": 1,
                    "changes": [{ "op": "put", "path": "index.html", "encoding": "utf8",
                                  "content": "<h1>two</h1>" }]
                })),
            )
            .await?;
        assert_eq!(saved.status, StatusCode::CREATED, "{}", saved.text);
        Ok(id)
    }

    /// Enqueues a `react-v1` build of `revision` and returns its job id.
    async fn build(
        &self,
        artifact: &str,
        key: &str,
        revision: u64,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let reply = self
            .send(
                "POST",
                &format!("/v1/artifacts/{artifact}/jobs"),
                Some(key),
                Some(&json!({ "kind": "run_profile", "revision": revision,
                              "profile": "react-v1" })),
            )
            .await?;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
        Ok(reply.json()?["job"]["id"]
            .as_str()
            .ok_or("no job id")?
            .to_owned())
    }

    /// Claims the job that is due and publishes a build of it, as the
    /// execution worker's store calls do.
    fn publish_due_build(&self) -> TestResult {
        let store = SqliteStore::open(&self.database)?;
        let attempt_number = self.published.fetch_add(1, Ordering::Relaxed);
        let attempt =
            AttemptIdentity::from_random([u8::try_from(attempt_number)?.wrapping_add(1); 16]);
        let profile: ProfileName = "react-v1".parse()?;
        let known = [profile.clone()];
        let worker: WorkerId = "worker-a".parse()?;
        let now = UnixMillis(i64::try_from(
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
        )?);
        let claimed = store
            .claim_job(JobClaim {
                worker: &worker,
                now,
                lease_ms: 30_000,
                known_profiles: &known,
                execution: Some(ExecutionClaim {
                    attempt,
                    tool_digest: TOOL_DIGEST,
                    machine: "machine-a",
                    boot: "boot-a",
                    cgroupns: "cgroupns-a",
                    directory: "0123456789abcdef0123456789abcdef",
                }),
            })?
            .claimed
            .ok_or("no job was due")?;
        store.settle_attempt(&attempt, now)?;
        let files = FilePackage::new(vec![(
            PackagePath::entry(),
            b"<!doctype html><p>built</p>".to_vec(),
        )])?;
        store.publish_build(BuildPublication {
            lease: &claimed.lease,
            attempt: &attempt,
            binding: ProfileBinding {
                profile: &profile,
                tool_digest: TOOL_DIGEST,
            },
            files: &files,
            now,
        })?;
        Ok(())
    }

    async fn status(&self, artifact: &str) -> Result<Value, Box<dyn std::error::Error>> {
        let reply = self
            .send(
                "GET",
                &format!("/v1/artifacts/{artifact}/builds/react-v1"),
                None,
                None,
            )
            .await?;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
        Ok(reply.json()?)
    }

    async fn build_preview(
        &self,
        artifact: &str,
        revision: u64,
        profile: &str,
    ) -> Result<Reply, Box<dyn std::error::Error>> {
        self.send(
            "POST",
            &format!("/v1/artifacts/{artifact}/revisions/{revision}/builds/{profile}/previews"),
            Some("preview"),
            Some(&json!({})),
        )
        .await
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        // Drop cannot report a failure; a directory left behind is cleared by
        // the next run that uses the same name.
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

struct Reply {
    status: StatusCode,
    location: Option<String>,
    text: String,
}

impl Reply {
    fn json(&self) -> Result<Value, serde_json::Error> {
        serde_json::from_str(&self.text)
    }

    fn code(&self) -> String {
        self.json()
            .ok()
            .and_then(|body| body["error"]["code"].as_str().map(str::to_owned))
            .unwrap_or_else(|| format!("<no error body: {}>", self.text))
    }
}

/// Rule 14: a revision with a build gets a build ticket on the `/b/` route,
/// and one without that build is refused at issue.
#[tokio::test]
async fn a_build_preview_is_issued_only_for_a_built_revision() -> TestResult {
    let harness = Harness::building()?;
    let artifact = harness.artifact().await?;
    harness.build(&artifact, "build-1", 1).await?;
    harness.publish_due_build()?;

    let issued = harness.build_preview(&artifact, 1, "react-v1").await?;
    assert_eq!(issued.status, StatusCode::CREATED, "{}", issued.text);
    let preview = issued.json()?;
    assert_eq!(preview["revision"], 1);
    let url = preview["url"].as_str().ok_or("no preview url")?;
    assert!(
        url.starts_with(&format!("{ORIGIN}/b/")) && url.ends_with('/'),
        "a build ticket is served on the build route: {url}"
    );
    assert_eq!(issued.location.as_deref(), Some(url));
    assert!(preview["expires_at_ms"].as_i64().is_some(), "{preview}");

    let unbuilt = harness.build_preview(&artifact, 2, "react-v1").await?;
    assert_eq!(
        (unbuilt.status, unbuilt.code()),
        (StatusCode::NOT_FOUND, "not_found".to_owned()),
        "revision 2 has no build: {}",
        unbuilt.text
    );
    let missing = harness.build_preview(&artifact, 3, "react-v1").await?;
    assert_eq!(missing.status, StatusCode::NOT_FOUND, "{}", missing.text);
    Ok(())
}

/// The R03d split note: both build routes refuse a profile the server's
/// build cannot build for the public, before they read anything, without
/// quoting the name back.
#[tokio::test]
async fn the_build_routes_refuse_a_profile_the_build_cannot_build() -> TestResult {
    let harness = Harness::new(&["react-v1", "fixture"])?;
    let artifact = harness.artifact().await?;
    for profile in ["fixture", "react-v1-check", "NOT-a-profile"] {
        let preview = harness.build_preview(&artifact, 1, profile).await?;
        let status = harness
            .send(
                "GET",
                &format!("/v1/artifacts/{artifact}/builds/{profile}"),
                None,
                None,
            )
            .await?;
        for reply in [preview, status] {
            assert_eq!(
                (reply.status, reply.code()),
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "profile_unknown".to_owned()
                ),
                "{profile}: {}",
                reply.text
            );
            assert!(!reply.text.contains(profile), "{profile}: {}", reply.text);
        }
    }

    // A build that knows no build profile, such as a release build for a
    // target other than x86_64 Linux.
    let release = Harness::new(&[])?;
    let artifact = release.artifact().await?;
    let preview = release.build_preview(&artifact, 1, "react-v1").await?;
    let status = release
        .send(
            "GET",
            &format!("/v1/artifacts/{artifact}/builds/react-v1"),
            None,
            None,
        )
        .await?;
    for reply in [preview, status] {
        assert_eq!(
            (reply.status, reply.code()),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "profile_unknown".to_owned()
            ),
            "{}",
            reply.text
        );
    }
    Ok(())
}

/// Rules 15 and 16: the status names the head, the latest build, and the
/// state of the newest build job of the head.
#[tokio::test]
async fn the_head_status_follows_the_newest_build_job_of_the_head() -> TestResult {
    let harness = Harness::building()?;
    let artifact = harness.artifact().await?;
    assert_eq!(
        harness.status(&artifact).await?,
        json!({ "profile": "react-v1", "head_revision": 2, "status": "none" }),
        "nothing was built or enqueued"
    );

    harness.build(&artifact, "build-1", 1).await?;
    harness.publish_due_build()?;
    assert_eq!(
        harness.status(&artifact).await?,
        json!({ "profile": "react-v1", "head_revision": 2, "latest_build_revision": 1,
                "status": "none" }),
        "an older revision's build is the latest, and the head has none"
    );

    let job = harness.build(&artifact, "build-2", 2).await?;
    assert_eq!(
        harness.status(&artifact).await?,
        json!({ "profile": "react-v1", "head_revision": 2, "latest_build_revision": 1,
                "status": "pending", "job_id": job }),
    );

    let sql = rusqlite::Connection::open(&harness.database)?;
    sql.execute(
        "UPDATE jobs SET state = 'failed', failure = 'build_failed', attempts = 1,
             finished_at = 2000 WHERE id = ?1",
        [&job],
    )?;
    sql.execute(
        "INSERT INTO job_diagnostics (job_id, ordinal, path, line, column_number, message)
         VALUES (?1, 0, 'src/main.tsx', 1, 16, 'Could not resolve \"left-pad\"'),
                (?1, 1, NULL, NULL, NULL, 'The build was stopped')",
        [&job],
    )?;
    assert_eq!(
        harness.status(&artifact).await?,
        json!({ "profile": "react-v1", "head_revision": 2, "latest_build_revision": 1,
                "status": "failed", "job_id": job, "failure": "build_failed",
                "diagnostics": [
                    { "location": { "path": "src/main.tsx", "line": 1, "column": 16 },
                      "message": "Could not resolve \"left-pad\"" },
                    { "message": "The build was stopped" }
                ] }),
        "the failure keeps the last good build as the latest"
    );

    harness.build(&artifact, "build-3", 2).await?;
    harness.publish_due_build()?;
    assert_eq!(
        harness.status(&artifact).await?,
        json!({ "profile": "react-v1", "head_revision": 2, "latest_build_revision": 2,
                "status": "built" }),
    );
    Ok(())
}

/// Rule 13 as R03cc implements it: another user, who holds no name, gets the
/// answer for an artifact nobody holds, though the revision has a build.
#[tokio::test]
async fn another_user_finds_no_build_of_the_owners_artifact() -> TestResult {
    let harness = Harness::building()?;
    let artifact = harness.artifact().await?;
    harness.build(&artifact, "build-1", 1).await?;
    harness.publish_due_build()?;
    let user = harness
        .send("POST", "/v1/users", None, Some(&json!({ "label": "bob" })))
        .await?;
    assert_eq!(user.status, StatusCode::CREATED, "{}", user.text);
    let issued = harness
        .send(
            "POST",
            "/v1/tokens",
            None,
            Some(&json!({ "name": "bob", "user_id": user.json()?["id"] })),
        )
        .await?;
    assert_eq!(issued.status, StatusCode::CREATED, "{}", issued.text);
    let bob = issued.json()?["secret"]
        .as_str()
        .ok_or("no token secret")?
        .to_owned();

    let absent = "art_00000000000000000000000000000000";
    let mut answers = Vec::new();
    for id in [artifact.as_str(), absent] {
        let preview = harness
            .send_as(
                &bob,
                "POST",
                &format!("/v1/artifacts/{id}/revisions/1/builds/react-v1/previews"),
                Some("bob-preview"),
                Some(&json!({})),
            )
            .await?;
        let status = harness
            .send_as(
                &bob,
                "GET",
                &format!("/v1/artifacts/{id}/builds/react-v1"),
                None,
                None,
            )
            .await?;
        for reply in [preview, status] {
            assert_eq!(
                (reply.status, reply.code()),
                (StatusCode::NOT_FOUND, "not_found".to_owned()),
                "{id}: {}",
                reply.text
            );
            answers.push(reply.text.replace(id, "<id>"));
        }
    }
    assert_eq!(
        answers[..2],
        answers[2..],
        "bob tells a held name from none"
    );
    // The owner still previews it.
    let owned = harness.build_preview(&artifact, 1, "react-v1").await?;
    assert_eq!(owned.status, StatusCode::CREATED, "{}", owned.text);
    Ok(())
}

/// Rule 13 as R03cc implements it: in the trash, and for an artifact nobody
/// holds, both routes answer `not_found`.
#[tokio::test]
async fn the_build_routes_find_nothing_in_the_trash() -> TestResult {
    let harness = Harness::building()?;
    let artifact = harness.artifact().await?;
    harness.build(&artifact, "build-1", 1).await?;
    harness.publish_due_build()?;
    let deleted = harness
        .send(
            "PATCH",
            &format!("/v1/artifacts/{artifact}"),
            Some("trash"),
            Some(&json!({ "base_metadata_revision": 1, "deleted": true })),
        )
        .await?;
    assert_eq!(deleted.status, StatusCode::OK, "{}", deleted.text);

    let absent = "art_00000000000000000000000000000000";
    for id in [artifact.as_str(), absent] {
        let preview = harness.build_preview(id, 1, "react-v1").await?;
        let status = harness
            .send(
                "GET",
                &format!("/v1/artifacts/{id}/builds/react-v1"),
                None,
                None,
            )
            .await?;
        for reply in [preview, status] {
            assert_eq!(
                (reply.status, reply.code()),
                (StatusCode::NOT_FOUND, "not_found".to_owned()),
                "{id}: {}",
                reply.text
            );
        }
    }
    Ok(())
}
