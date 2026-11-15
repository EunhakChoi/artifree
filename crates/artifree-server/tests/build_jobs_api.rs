//! The public build kind (decision 0058 rule 17, slice R03da) through the real
//! router, core, and `SQLite` store: `POST /v1/artifacts/{id}/jobs` with
//! `kind: run_profile` and `profile: react-v1` enqueues a build on a server
//! whose build knows the profile, and every other profile answers
//! `profile_unknown`. A build is read, listed, replayed, cancelled and bounded
//! as a verification is; a failed build shows its failure and diagnostics, and
//! a succeeded one its counts. No worker runs here: the tests write the end
//! of a job into the database as the worker's store would.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use artifree_domain::job::{MAX_ACTIVE_JOBS_PER_ACTOR, ProfileName};
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

struct Harness {
    directory: PathBuf,
    database: PathBuf,
    api: Router,
}

impl Harness {
    /// A server whose build knows `build_profiles` as its build profiles.
    fn new(build_profiles: &[&str]) -> Result<Self, Box<dyn std::error::Error>> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!(
            "artifree-build-jobs-api-{}-{}",
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
                content_origin: "http://127.0.0.1:7411".into(),
                build_profiles,
                ..Settings::default()
            },
        )?;
        Ok(Self {
            directory,
            database,
            api: http::router(state),
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
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(header::AUTHORIZATION, format!("Bearer {OWNER}"));
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
        let bytes = to_bytes(response.into_body(), usize::MAX).await?;
        Ok(Reply {
            status,
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

    async fn enqueue(
        &self,
        artifact: &str,
        key: &str,
        body: &Value,
    ) -> Result<Reply, Box<dyn std::error::Error>> {
        self.send(
            "POST",
            &format!("/v1/artifacts/{artifact}/jobs"),
            Some(key),
            Some(body),
        )
        .await
    }

    /// Enqueues a `react-v1` build of `revision` and returns its job.
    async fn build(
        &self,
        artifact: &str,
        key: &str,
        revision: u64,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let reply = self
            .enqueue(artifact, key, &build_of(revision, "react-v1"))
            .await?;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
        Ok(reply.json()?["job"].clone())
    }

    async fn read(&self, path: &str) -> Result<Value, Box<dyn std::error::Error>> {
        let reply = self.send("GET", path, None, None).await?;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
        Ok(reply.json()?)
    }

    fn sql(&self) -> Result<rusqlite::Connection, rusqlite::Error> {
        rusqlite::Connection::open(&self.database)
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

fn build_of(revision: u64, profile: &str) -> Value {
    json!({ "kind": "run_profile", "revision": revision, "profile": profile })
}

fn id_of(job: &Value) -> Result<String, Box<dyn std::error::Error>> {
    Ok(job["id"].as_str().ok_or("no job id")?.to_owned())
}

/// Rule 17: a build of any revision, an older one included, is enqueued with
/// its profile, read and listed with it, and replayed by its key.
#[tokio::test]
async fn a_build_is_enqueued_read_listed_and_replayed() -> TestResult {
    let harness = Harness::building()?;
    let artifact = harness.artifact().await?;

    let job = harness.build(&artifact, "build", 1).await?;
    assert_eq!(job["kind"], "run_profile");
    assert_eq!(job["profile"], "react-v1");
    assert_eq!(
        job["revision"], 1,
        "an older revision than the head is built"
    );
    assert_eq!(job["state"], "queued");
    let id = id_of(&job)?;

    let shown = harness.read(&format!("/v1/jobs/{id}")).await?;
    assert_eq!(shown["profile"], "react-v1", "the read shows the profile");
    let listed = harness
        .read(&format!("/v1/artifacts/{artifact}/jobs"))
        .await?;
    assert_eq!(
        listed["jobs"][0]["id"],
        id.as_str(),
        "the list holds the build"
    );

    let replay = harness
        .enqueue(&artifact, "build", &build_of(1, "react-v1"))
        .await?;
    assert_eq!(replay.status, StatusCode::CREATED, "{}", replay.text);
    let replayed = replay.json()?;
    assert_eq!(replayed["replayed"], true);
    assert_eq!(replayed["job"]["id"], id.as_str());

    let reused = harness
        .enqueue(&artifact, "build", &build_of(2, "react-v1"))
        .await?;
    assert_eq!(reused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        reused.code(),
        "request_key_reused",
        "the key names another revision now"
    );
    Ok(())
}

/// Rule 17: a profile this build cannot build for the public answers
/// `profile_unknown` and enqueues nothing: `react-v1` on a build that knows no
/// build profile (a release build for a target other than `x86_64` Linux), a
/// profile known only for tests, and a name that is no profile, which is not
/// quoted back.
#[tokio::test]
async fn a_profile_the_build_cannot_build_is_profile_unknown() -> TestResult {
    let release = Harness::new(&[])?;
    let artifact = release.artifact().await?;
    let refused = release
        .enqueue(&artifact, "release", &build_of(2, "react-v1"))
        .await?;
    assert_eq!(
        refused.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        refused.text
    );
    assert_eq!(
        refused.code(),
        "profile_unknown",
        "a build that knows no profile refuses react-v1"
    );

    let building = Harness::new(&["react-v1", "fixture"])?;
    let artifact = building.artifact().await?;
    for (key, profile) in [("test-only", "fixture"), ("no-name", "Not A Profile")] {
        let refused = building
            .enqueue(&artifact, key, &build_of(2, profile))
            .await?;
        assert_eq!(
            refused.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{profile}"
        );
        assert_eq!(refused.code(), "profile_unknown", "{profile}");
        assert!(
            !refused.text.contains(profile),
            "the refusal does not quote the profile: {}",
            refused.text
        );
    }
    let listed = building
        .read(&format!("/v1/artifacts/{artifact}/jobs"))
        .await?;
    assert_eq!(listed["jobs"], json!([]), "nothing was enqueued");
    // The refused key recorded nothing, so it enqueues a build now.
    building.build(&artifact, "test-only", 2).await?;
    Ok(())
}

/// A build requires a profile, and a verification refuses one.
#[tokio::test]
async fn a_profile_is_required_for_a_build_and_refused_for_a_verification() -> TestResult {
    let harness = Harness::building()?;
    let artifact = harness.artifact().await?;
    for (key, body) in [
        (
            "no-profile",
            json!({ "kind": "run_profile", "revision": 2 }),
        ),
        (
            "verify-profile",
            json!({ "kind": "verify_revision", "revision": 2, "profile": "react-v1" }),
        ),
    ] {
        let refused = harness.enqueue(&artifact, key, &body).await?;
        assert_eq!(
            refused.status,
            StatusCode::BAD_REQUEST,
            "{body}: {}",
            refused.text
        );
        assert_eq!(refused.code(), "invalid_request", "{body}");
    }
    Ok(())
}

/// The open-job bound counts builds and verifications together.
#[tokio::test]
async fn builds_and_verifications_share_the_bound_of_sixteen() -> TestResult {
    let harness = Harness::building()?;
    let artifact = harness.artifact().await?;
    for index in 0..MAX_ACTIVE_JOBS_PER_ACTOR - 1 {
        harness
            .build(&artifact, &format!("build-{index}"), 2)
            .await?;
    }
    let verify = harness
        .enqueue(
            &artifact,
            "verify",
            &json!({ "kind": "verify_revision", "revision": 2 }),
        )
        .await?;
    assert_eq!(verify.status, StatusCode::CREATED, "{}", verify.text);

    let over = harness
        .enqueue(&artifact, "build-over", &build_of(2, "react-v1"))
        .await?;
    assert_eq!(over.status, StatusCode::CONFLICT, "{}", over.text);
    assert_eq!(over.code(), "too_many_jobs");
    Ok(())
}

/// A waiting build is cancelled as a verification is.
#[tokio::test]
async fn a_waiting_build_is_cancelled() -> TestResult {
    let harness = Harness::building()?;
    let artifact = harness.artifact().await?;
    let id = id_of(&harness.build(&artifact, "build", 2).await?)?;
    let cancelled = harness
        .send(
            "POST",
            &format!("/v1/jobs/{id}/cancel"),
            Some("cancel"),
            Some(&json!({})),
        )
        .await?;
    assert_eq!(cancelled.status, StatusCode::OK, "{}", cancelled.text);
    assert_eq!(cancelled.json()?["job"]["state"], "cancelled");
    Ok(())
}

/// Rule 18 on the wire: a failed build shows its failure and its diagnostics,
/// in their order, on the read and the list.
#[tokio::test]
async fn a_failed_build_shows_its_failure_and_diagnostics() -> TestResult {
    let harness = Harness::building()?;
    let artifact = harness.artifact().await?;
    let id = id_of(&harness.build(&artifact, "build", 2).await?)?;
    let sql = harness.sql()?;
    sql.execute(
        "UPDATE jobs SET state = 'failed', failure = 'build_failed', attempts = 1,
             finished_at = 2000 WHERE id = ?1",
        [&id],
    )?;
    sql.execute(
        "INSERT INTO job_diagnostics (job_id, ordinal, path, line, column_number, message)
         VALUES (?1, 0, 'src/main.tsx', 1, 16, 'Could not resolve \"left-pad\"'),
                (?1, 1, NULL, NULL, NULL, 'The build was stopped')",
        [&id],
    )?;

    let expected = json!([
        { "location": { "path": "src/main.tsx", "line": 1, "column": 16 },
          "message": "Could not resolve \"left-pad\"" },
        { "message": "The build was stopped" }
    ]);
    let shown = harness.read(&format!("/v1/jobs/{id}")).await?;
    assert_eq!(shown["state"], "failed");
    assert_eq!(shown["failure"], "build_failed");
    assert_eq!(
        shown["diagnostics"], expected,
        "the read shows the diagnostics"
    );
    let listed = harness.read("/v1/jobs").await?;
    assert_eq!(
        listed["jobs"][0]["diagnostics"], expected,
        "so does the list"
    );
    Ok(())
}

/// A succeeded build shows the counts the store keeps as its build, and no
/// verification result.
#[tokio::test]
async fn a_succeeded_build_shows_its_counts() -> TestResult {
    let harness = Harness::building()?;
    let artifact = harness.artifact().await?;
    let id = id_of(&harness.build(&artifact, "build", 2).await?)?;
    harness.sql()?.execute(
        "UPDATE jobs SET state = 'succeeded', attempts = 1, finished_at = 2000,
             files_checked = 3, bytes_checked = 120 WHERE id = ?1",
        [&id],
    )?;

    let shown = harness.read(&format!("/v1/jobs/{id}")).await?;
    assert_eq!(shown["state"], "succeeded");
    assert_eq!(
        shown["build"],
        json!({ "file_count": 3, "byte_count": 120 })
    );
    assert!(
        shown.get("result").is_none(),
        "a build has no verification result"
    );
    assert!(shown.get("diagnostics").is_none(), "nor diagnostics");
    Ok(())
}
