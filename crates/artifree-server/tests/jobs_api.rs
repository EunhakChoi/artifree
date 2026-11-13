//! The job routes (decision 0030) through the real router, core, and `SQLite`
//! store, with a worker run in the test process where a job must progress.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};

use artifree_contracts::api::CreateTokenResponse;
use artifree_core::jobs::Clock;
use artifree_core::worker::{Pace, Step, WorkerConfig, run_next};
use artifree_domain::identity::UnixMillis;
use artifree_domain::job::WorkerId;
use artifree_ports::job_store::{JobClaim, JobStore};
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
const UNKNOWN_JOB: &str = "job_00000000000000000000000000000000";

#[derive(Debug)]
struct ManualClock(AtomicI64);

impl ManualClock {
    fn set(&self, millis: i64) {
        self.0.store(millis, Ordering::SeqCst);
    }

    fn advance(&self, millis: i64) {
        self.0.fetch_add(millis, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> UnixMillis {
        UnixMillis(self.0.load(Ordering::SeqCst))
    }
}

struct Harness {
    directory: PathBuf,
    database: PathBuf,
    api: Router,
    clock: Arc<ManualClock>,
}

impl Harness {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!(
            "artifree-jobs-api-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory)?;
        let database = directory.join("db.sqlite3");
        let clock = Arc::new(ManualClock(AtomicI64::new(1_000)));
        let state = AppState::new(
            SqliteStore::open(&database)?,
            artifree_providers::tokens::SqliteTokenStore::open(&database)?,
            AccessToken::parse(OWNER).ok_or("fixture token")?,
            Settings {
                content_origin: "http://127.0.0.1:7411".into(),
                clock: clock.clone(),
                ..Settings::default()
            },
        )?;
        Ok(Self {
            directory,
            database,
            api: http::router(state),
            clock,
        })
    }

    async fn send(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        key: Option<&str>,
        body: Option<&Value>,
    ) -> Result<Reply, Box<dyn std::error::Error>> {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
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
        let headers = response.headers().clone();
        let bytes = to_bytes(response.into_body(), usize::MAX).await?;
        Ok(Reply {
            status,
            headers,
            text: String::from_utf8_lossy(&bytes).into_owned(),
        })
    }

    async fn owner(
        &self,
        method: &str,
        path: &str,
        key: Option<&str>,
        body: Option<&Value>,
    ) -> Result<Reply, Box<dyn std::error::Error>> {
        self.send(method, path, Some(OWNER), key, body).await
    }

    /// An artifact with two revisions; returns its identifier.
    async fn artifact(&self, name: &str) -> Result<String, Box<dyn std::error::Error>> {
        let created = self
            .owner(
                "POST",
                "/v1/artifacts",
                Some(&format!("create-{name}")),
                Some(&json!({
                    "title": name,
                    "files": [
                        { "path": "index.html", "encoding": "utf8", "content": "<h1>one</h1>" },
                        { "path": "app.js", "encoding": "utf8", "content": "let a = 1;" }
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
            .owner(
                "POST",
                &format!("/v1/artifacts/{id}/revisions"),
                Some(&format!("save-{name}")),
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
        revision: u64,
    ) -> Result<Reply, Box<dyn std::error::Error>> {
        self.owner(
            "POST",
            &format!("/v1/artifacts/{artifact}/jobs"),
            Some(key),
            Some(&json!({ "kind": "verify_revision", "revision": revision })),
        )
        .await
    }

    async fn enqueued(
        &self,
        artifact: &str,
        key: &str,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let reply = self.enqueue(artifact, key, 2).await?;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
        Ok(reply.json()?["job"]["id"]
            .as_str()
            .ok_or("no job id")?
            .to_owned())
    }

    async fn state_of(&self, job: &str) -> Result<String, Box<dyn std::error::Error>> {
        let reply = self
            .owner("GET", &format!("/v1/jobs/{job}"), None, None)
            .await?;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
        Ok(reply.json()?["state"]
            .as_str()
            .ok_or("no state")?
            .to_owned())
    }

    /// Runs the worker once with the same manual clock as the API.
    fn work(&self) -> Result<Step, Box<dyn std::error::Error>> {
        struct Never;
        impl Pace for Never {
            fn shutdown_requested(&self) -> bool {
                false
            }
        }
        let store = SqliteStore::open(&self.database)?;
        let worker: WorkerId = "worker-test".parse()?;
        Ok(run_next(
            &store,
            &*self.clock,
            &worker,
            &WorkerConfig::with_lease_ms(60_000),
            &Never,
            &mut |_| {},
        )?)
    }

    fn sql(&self) -> Result<rusqlite::Connection, rusqlite::Error> {
        rusqlite::Connection::open(&self.database)
    }

    async fn agent_token(&self) -> Result<String, Box<dyn std::error::Error>> {
        let reply = self
            .owner(
                "POST",
                "/v1/tokens",
                None,
                Some(&json!({ "name": "Agent" })),
            )
            .await?;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
        Ok(serde_json::from_str::<CreateTokenResponse>(&reply.text)?.secret)
    }

    async fn trash(&self, artifact: &str, key: &str, base: u64, deleted: bool) -> TestResult {
        let reply = self
            .owner(
                "PATCH",
                &format!("/v1/artifacts/{artifact}"),
                Some(key),
                Some(&json!({ "base_metadata_revision": base, "deleted": deleted })),
            )
            .await?;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
        Ok(())
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

struct Reply {
    status: StatusCode,
    headers: axum::http::HeaderMap,
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

// --- access ----------------------------------------------------------------

#[tokio::test]
async fn every_job_route_requires_the_bearer_token() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    let job = harness.enqueued(&id, "k1").await?;
    let wrong = "f".repeat(64);
    let body = json!({ "kind": "verify_revision", "revision": 1 });
    let empty = json!({});
    for (method, path, payload) in [
        ("POST", format!("/v1/artifacts/{id}/jobs"), Some(&body)),
        ("GET", format!("/v1/artifacts/{id}/jobs"), None),
        ("GET", "/v1/jobs".to_owned(), None),
        ("GET", format!("/v1/jobs/{job}"), None),
        ("POST", format!("/v1/jobs/{job}/cancel"), Some(&empty)),
    ] {
        for token in [None, Some(wrong.as_str())] {
            let reply = harness
                .send(method, &path, token, Some("unauthenticated-1"), payload)
                .await?;
            assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{method} {path}");
        }
    }
    assert_eq!(harness.state_of(&job).await?, "queued", "nothing changed");
    Ok(())
}

#[tokio::test]
async fn an_agent_token_may_enqueue_read_list_and_cancel() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    let agent = harness.agent_token().await?;
    let agent_call =
        |method: &'static str, path: String, key: Option<&'static str>, body: Value| {
            let harness = &harness;
            let agent = agent.clone();
            async move {
                harness
                    .send(method, &path, Some(&agent), key, Some(&body))
                    .await
            }
        };

    let enqueued = agent_call(
        "POST",
        format!("/v1/artifacts/{id}/jobs"),
        Some("agent-enqueue"),
        json!({ "kind": "verify_revision", "revision": 2 }),
    )
    .await?;
    assert_eq!(enqueued.status, StatusCode::CREATED, "{}", enqueued.text);
    let job = enqueued.json()?["job"]["id"]
        .as_str()
        .ok_or("no job id")?
        .to_owned();
    let shown = harness
        .send("GET", &format!("/v1/jobs/{job}"), Some(&agent), None, None)
        .await?;
    assert_eq!(shown.status, StatusCode::OK, "{}", shown.text);
    for path in ["/v1/jobs".to_owned(), format!("/v1/artifacts/{id}/jobs")] {
        let listed = harness.send("GET", &path, Some(&agent), None, None).await?;
        assert_eq!(listed.status, StatusCode::OK, "{path}: {}", listed.text);
        assert_eq!(listed.json()?["jobs"].as_array().map(Vec::len), Some(1));
    }
    let cancelled = agent_call(
        "POST",
        format!("/v1/jobs/{job}/cancel"),
        Some("agent-cancel"),
        json!({}),
    )
    .await?;
    assert_eq!(cancelled.status, StatusCode::OK, "{}", cancelled.text);
    assert_eq!(cancelled.json()?["job"]["state"], "cancelled");
    // The agent and the owner are one actor: the owner sees the agent's job.
    assert_eq!(harness.state_of(&job).await?, "cancelled");
    Ok(())
}

// --- enqueue ---------------------------------------------------------------

#[tokio::test]
async fn an_enqueued_job_is_pinned_located_and_replayed_by_its_key() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;

    let first = harness.enqueue(&id, "job-key-1", 1).await?;

    assert_eq!(first.status, StatusCode::CREATED, "{}", first.text);
    let body = first.json()?;
    let job = &body["job"];
    let job_id = job["id"].as_str().ok_or("no job id")?;
    assert!(job_id.starts_with("job_") && job_id.len() == 36, "{job_id}");
    assert_eq!(
        first
            .headers
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok()),
        Some(format!("/v1/jobs/{job_id}").as_str())
    );
    assert_eq!(body["replayed"], false);
    assert_eq!(job["artifact_id"], id.as_str());
    assert_eq!(job["revision"], 1);
    assert_eq!(job["kind"], "verify_revision");
    assert_eq!(job["state"], "queued");
    assert_eq!(
        (job["attempt"].clone(), job["max_attempts"].clone()),
        (json!(0), json!(3))
    );
    assert_eq!(job["cancel_requested"], false);
    for absent in [
        "result",
        "failure",
        "started_at_ms",
        "finished_at_ms",
        "retry_at_ms",
    ] {
        assert!(
            job.get(absent).is_none_or(Value::is_null),
            "{absent}: {job}"
        );
    }

    let again = harness.enqueue(&id, "job-key-1", 1).await?;
    assert_eq!(again.status, StatusCode::CREATED);
    assert_eq!(again.json()?["job"]["id"], job_id);
    assert_eq!(again.json()?["replayed"], true);
    let listed = harness.owner("GET", "/v1/jobs", None, None).await?.json()?;
    assert_eq!(listed["jobs"].as_array().map(Vec::len), Some(1));
    Ok(())
}

#[tokio::test]
async fn a_bad_enqueue_is_refused_with_the_right_code() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    harness.enqueue(&id, "job-key-1", 1).await?;
    let path = format!("/v1/artifacts/{id}/jobs");
    let valid = json!({ "kind": "verify_revision", "revision": 1 });

    // The key is required, and cannot serve a different request.
    let missing_key = harness.owner("POST", &path, None, Some(&valid)).await?;
    assert_eq!(
        (missing_key.status, missing_key.code()),
        (StatusCode::BAD_REQUEST, "invalid_request".into())
    );
    let other = harness.enqueue(&id, "job-key-1", 2).await?;
    assert_eq!(
        (other.status, other.code()),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            "request_key_reused".into()
        )
    );
    // The key space is shared with the requests that change artifacts.
    let shared = harness.enqueue(&id, "create-A", 1).await?;
    assert_eq!(
        (shared.status, shared.code()),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            "request_key_reused".into()
        )
    );

    // Bodies are closed and typed.
    for body in [
        json!({ "kind": "build", "revision": 1 }),
        json!({ "kind": "verify_revision" }),
        json!({ "kind": "verify_revision", "revision": "1" }),
        json!({ "kind": "verify_revision", "revision": -1 }),
        json!({ "kind": "verify_revision", "revision": 1, "priority": 9 }),
    ] {
        let reply = harness
            .owner("POST", &path, Some("bad-body"), Some(&body))
            .await?;
        assert_eq!(
            (reply.status, reply.code()),
            (StatusCode::BAD_REQUEST, "invalid_request".into()),
            "{body}"
        );
    }

    // A revision that does not exist is not found; so is an artifact.
    for revision in [0, 3, 1_000_000] {
        let reply = harness.enqueue(&id, "no-such-revision", revision).await?;
        assert_eq!(
            (reply.status, reply.code()),
            (StatusCode::NOT_FOUND, "not_found".into()),
            "{revision}"
        );
    }
    let unknown = "art_00000000000000000000000000000000";
    let reply = harness.enqueue(unknown, "no-such-artifact", 1).await?;
    assert_eq!(
        (reply.status, reply.code()),
        (StatusCode::NOT_FOUND, "not_found".into())
    );
    // A malformed identifier is not found and is not repeated back.
    let reply = harness
        .enqueue("not-an-artifact-id-zz9", "malformed", 1)
        .await?;
    assert_eq!(
        (reply.status, reply.code()),
        (StatusCode::NOT_FOUND, "not_found".into())
    );
    assert!(!reply.text.contains("zz9"), "{}", reply.text);
    assert_eq!(
        harness
            .sql()?
            .query_row("SELECT count(*) FROM jobs", [], |row| row.get::<_, i64>(0))?,
        1,
        "only the first request created a job"
    );
    Ok(())
}

#[tokio::test]
async fn jobs_of_an_artifact_in_the_trash_leave_room_for_work_elsewhere() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    for number in 0..16 {
        harness.enqueued(&id, &format!("job-{number}")).await?;
    }
    harness.trash(&id, "trash-a", 1, true).await?;

    let other = harness.artifact("B").await?;
    let reply = harness.enqueue(&other, "on-b", 2).await?;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);

    harness.trash(&id, "recover-a", 2, false).await?;
    let listed = harness
        .owner("GET", &format!("/v1/artifacts/{id}/jobs"), None, None)
        .await?;
    assert_eq!(
        listed.json()?["jobs"].as_array().map(Vec::len),
        Some(16),
        "recovery brings every job back"
    );
    Ok(())
}

#[tokio::test]
async fn at_most_sixteen_jobs_may_wait_or_run() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    let mut jobs = Vec::new();
    for number in 0..16 {
        jobs.push(harness.enqueued(&id, &format!("job-{number}")).await?);
    }

    let refused = harness.enqueue(&id, "one-too-many", 2).await?;
    assert_eq!(
        (refused.status, refused.code()),
        (StatusCode::CONFLICT, "too_many_jobs".into())
    );
    let retry = harness.enqueue(&id, "job-0", 2).await?;
    assert_eq!(retry.status, StatusCode::CREATED, "a retry needs no room");
    assert_eq!(retry.json()?["replayed"], true);
    let cancelled = harness
        .owner(
            "POST",
            &format!("/v1/jobs/{}/cancel", jobs[0]),
            Some("free-room"),
            Some(&json!({})),
        )
        .await?;
    assert_eq!(cancelled.status, StatusCode::OK);
    let room = harness.enqueue(&id, "one-too-many", 2).await?;
    assert_eq!(room.status, StatusCode::CREATED, "{}", room.text);
    Ok(())
}

// --- not found -------------------------------------------------------------

#[tokio::test]
async fn jobs_of_an_artifact_in_the_trash_or_purged_are_not_found() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    let other = harness.artifact("B").await?;
    let job = harness.enqueued(&id, "job-a").await?;
    let kept = harness.enqueued(&other, "job-b").await?;

    harness.trash(&id, "trash-a", 1, true).await?;

    let hidden = |reply: &Reply| (reply.status, reply.code());
    let gone = (StatusCode::NOT_FOUND, "not_found".to_owned());
    assert_eq!(
        hidden(
            &harness
                .owner("GET", &format!("/v1/jobs/{job}"), None, None)
                .await?
        ),
        gone
    );
    assert_eq!(hidden(&harness.enqueue(&id, "new-key", 2).await?), gone);
    assert_eq!(
        hidden(&harness.enqueue(&id, "job-a", 2).await?),
        gone,
        "a retry too"
    );
    assert_eq!(
        hidden(
            &harness
                .owner(
                    "POST",
                    &format!("/v1/jobs/{job}/cancel"),
                    Some("c1"),
                    Some(&json!({}))
                )
                .await?
        ),
        gone
    );
    assert_eq!(
        hidden(
            &harness
                .owner("GET", &format!("/v1/artifacts/{id}/jobs"), None, None)
                .await?
        ),
        gone
    );
    let global = harness.owner("GET", "/v1/jobs", None, None).await?.json()?;
    let listed: Vec<_> = global["jobs"]
        .as_array()
        .ok_or("jobs")?
        .iter()
        .map(|j| j["id"].clone())
        .collect();
    assert_eq!(
        listed,
        [json!(kept)],
        "the global list leaves out the trash"
    );

    // Recovery brings the job back, untouched.
    harness.trash(&id, "recover-a", 2, false).await?;
    assert_eq!(harness.state_of(&job).await?, "queued");

    // Purging ends it for good: the job and its retry are not found.
    harness.trash(&id, "trash-a-again", 3, true).await?;
    let purged = harness
        .owner(
            "POST",
            &format!("/v1/artifacts/{id}/purge"),
            Some("purge-a"),
            Some(&json!({ "base_metadata_revision": 4 })),
        )
        .await?;
    assert_eq!(purged.status, StatusCode::OK, "{}", purged.text);
    assert_eq!(
        hidden(
            &harness
                .owner("GET", &format!("/v1/jobs/{job}"), None, None)
                .await?
        ),
        gone
    );
    assert_eq!(hidden(&harness.enqueue(&id, "job-a", 2).await?), gone);
    assert_eq!(
        harness.state_of(&kept).await?,
        "queued",
        "another artifact's job"
    );
    Ok(())
}

#[tokio::test]
async fn an_unknown_or_malformed_job_is_not_found_without_echoing_the_input() -> TestResult {
    let harness = Harness::new()?;
    for path in [
        format!("/v1/jobs/{UNKNOWN_JOB}"),
        "/v1/jobs/not-a-job-yy7".to_owned(),
        "/v1/jobs/JOB_00000000000000000000000000000000".to_owned(),
    ] {
        let shown = harness.owner("GET", &path, None, None).await?;
        assert_eq!(
            (shown.status, shown.code()),
            (StatusCode::NOT_FOUND, "not_found".into()),
            "{path}"
        );
        assert!(!shown.text.contains("yy7"));
        let cancelled = harness
            .owner(
                "POST",
                &format!("{path}/cancel"),
                Some("cancel-unknown"),
                Some(&json!({})),
            )
            .await?;
        assert_eq!(
            (cancelled.status, cancelled.code()),
            (StatusCode::NOT_FOUND, "not_found".into()),
            "{path}"
        );
        assert!(!cancelled.text.contains("yy7"));
    }
    Ok(())
}

// --- the worker's result ---------------------------------------------------

#[tokio::test]
async fn a_job_waits_after_a_clock_rollback_until_its_enqueue_time() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    let job = harness.enqueued(&id, "job-a").await?;

    harness.clock.set(999);
    let step = harness.work()?;
    assert!(matches!(step, Step::Idle), "job is not due yet: {step:?}");
    let waiting = harness
        .owner("GET", &format!("/v1/jobs/{job}"), None, None)
        .await?
        .json()?;
    assert_eq!(waiting["created_at_ms"], 1_000);
    assert_eq!(waiting["state"], "queued");
    assert_eq!(waiting["attempt"], 0, "an idle poll uses no attempt");
    assert_eq!(waiting["started_at_ms"], Value::Null);

    harness.clock.advance(1);
    let step = harness.work()?;
    assert!(matches!(step, Step::Finished(_)), "job is due: {step:?}");
    let finished = harness
        .owner("GET", &format!("/v1/jobs/{job}"), None, None)
        .await?
        .json()?;
    assert_eq!(finished["state"], "succeeded");
    assert_eq!(finished["attempt"], 1);
    assert_eq!(finished["started_at_ms"], 1_000);
    assert_eq!(finished["finished_at_ms"], 1_000);
    assert_eq!(finished["result"]["verdict"], "ok");
    Ok(())
}

#[tokio::test]
async fn a_worker_run_shows_in_the_job() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    let job = harness.enqueued(&id, "job-a").await?;

    assert!(matches!(harness.work()?, Step::Finished(_)));

    let shown = harness
        .owner("GET", &format!("/v1/jobs/{job}"), None, None)
        .await?
        .json()?;
    assert_eq!(shown["state"], "succeeded");
    assert_eq!(shown["attempt"], 1);
    assert_eq!(shown["result"]["verdict"], "ok");
    assert_eq!(shown["result"]["files_checked"], 2);
    assert_eq!(shown["result"]["mismatch_count"], 0);
    assert_eq!(shown["result"]["mismatches"], json!([]));
    assert_eq!(shown["result"]["truncated"], false);
    assert!(shown["finished_at_ms"].is_i64() && shown["started_at_ms"].is_i64());
    assert!(matches!(harness.work()?, Step::Idle));
    Ok(())
}

#[tokio::test]
async fn a_damaged_file_is_a_mismatch_in_a_job_that_succeeded() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    let job = harness.enqueued(&id, "job-a").await?;
    harness.sql()?.execute(
        "UPDATE blobs SET content = CAST('<h1>TWO</h1>' AS BLOB)
         WHERE digest = (SELECT digest FROM revision_files
                         WHERE artifact_id = ?1 AND revision = 2 AND path = 'index.html')",
        [&id],
    )?;

    harness.work()?;

    let shown = harness
        .owner("GET", &format!("/v1/jobs/{job}"), None, None)
        .await?;
    let body = shown.json()?;
    assert_eq!(body["state"], "succeeded", "the job worked");
    assert_eq!(body["result"]["verdict"], "mismatch");
    assert_eq!(body["result"]["mismatch_count"], 1);
    assert_eq!(
        body["result"]["mismatches"],
        json!([{ "path": "index.html", "problem": "digest_mismatch" }])
    );
    assert!(
        !shown.text.contains("TWO") && !shown.text.contains("<h1>"),
        "no content: {}",
        shown.text
    );
    Ok(())
}

// --- cancel ----------------------------------------------------------------

#[tokio::test]
async fn cancelling_a_waiting_job_ends_it_and_a_second_cancel_is_job_ended() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    let job = harness.enqueued(&id, "job-a").await?;
    let path = format!("/v1/jobs/{job}/cancel");

    let first = harness
        .owner("POST", &path, Some("cancel-1"), Some(&json!({})))
        .await?;
    assert_eq!(first.status, StatusCode::OK, "{}", first.text);
    assert_eq!(first.json()?["job"]["state"], "cancelled");
    assert_eq!(first.json()?["replayed"], false);
    assert!(first.json()?["job"]["finished_at_ms"].is_i64());

    let replay = harness
        .owner("POST", &path, Some("cancel-1"), Some(&json!({})))
        .await?;
    assert_eq!(
        (replay.status, replay.json()?["replayed"].clone()),
        (StatusCode::OK, json!(true))
    );
    let again = harness
        .owner("POST", &path, Some("cancel-2"), Some(&json!({})))
        .await?;
    assert_eq!(
        (again.status, again.code()),
        (StatusCode::CONFLICT, "job_ended".into())
    );
    // The key the refused request named is still free.
    let other = harness.enqueued(&id, "job-b").await?;
    let reused = harness
        .owner(
            "POST",
            &format!("/v1/jobs/{other}/cancel"),
            Some("cancel-2"),
            Some(&json!({})),
        )
        .await?;
    assert_eq!(reused.status, StatusCode::OK, "{}", reused.text);
    // A key that served another request cannot cancel a different job.
    let third = harness.enqueued(&id, "job-c").await?;
    let wrong = harness
        .owner(
            "POST",
            &format!("/v1/jobs/{third}/cancel"),
            Some("cancel-1"),
            Some(&json!({})),
        )
        .await?;
    assert_eq!(
        (wrong.status, wrong.code()),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            "request_key_reused".into()
        )
    );
    // The body is `{}` and the key is required.
    let no_key = harness
        .owner(
            "POST",
            &format!("/v1/jobs/{third}/cancel"),
            None,
            Some(&json!({})),
        )
        .await?;
    assert_eq!(no_key.status, StatusCode::BAD_REQUEST);
    let extra = harness
        .owner(
            "POST",
            &format!("/v1/jobs/{third}/cancel"),
            Some("extra-1"),
            Some(&json!({ "force": true })),
        )
        .await?;
    assert_eq!(extra.status, StatusCode::BAD_REQUEST);
    assert_eq!(harness.state_of(&third).await?, "queued");
    Ok(())
}

#[tokio::test]
async fn cancelling_a_running_job_asks_and_the_worker_confirms() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    let job = harness.enqueued(&id, "job-a").await?;
    let store = SqliteStore::open(&harness.database)?;
    let claimed = store
        .claim_job(JobClaim {
            worker: &"worker-held".parse()?,
            now: harness.clock.now(),
            lease_ms: 60_000,
            known_profiles: &[],
            execution: None,
        })?
        .claimed
        .ok_or("nothing claimed")?;
    assert_eq!(harness.state_of(&job).await?, "running");

    let asked = harness
        .owner(
            "POST",
            &format!("/v1/jobs/{job}/cancel"),
            Some("cancel-1"),
            Some(&json!({})),
        )
        .await?;

    assert_eq!(asked.status, StatusCode::OK, "{}", asked.text);
    assert_eq!(asked.json()?["job"]["state"], "running", "not stopped yet");
    assert_eq!(asked.json()?["job"]["cancel_requested"], true);
    let again = harness
        .owner(
            "POST",
            &format!("/v1/jobs/{job}/cancel"),
            Some("cancel-2"),
            Some(&json!({})),
        )
        .await?;
    assert_eq!(again.status, StatusCode::OK, "asking twice is not an error");
    store.confirm_cancel(&claimed.lease, harness.clock.now())?;
    assert_eq!(harness.state_of(&job).await?, "cancelled");
    let ended = harness
        .owner(
            "POST",
            &format!("/v1/jobs/{job}/cancel"),
            Some("cancel-3"),
            Some(&json!({})),
        )
        .await?;
    assert_eq!(
        (ended.status, ended.code()),
        (StatusCode::CONFLICT, "job_ended".into())
    );
    Ok(())
}

// --- lists -----------------------------------------------------------------

#[tokio::test]
async fn lists_page_newest_first_and_tokens_belong_to_their_list() -> TestResult {
    let harness = Harness::new()?;
    let first = harness.artifact("A").await?;
    let second = harness.artifact("B").await?;
    let mut made = Vec::new();
    for (number, artifact) in [&first, &second, &first, &second, &first]
        .into_iter()
        .enumerate()
    {
        // Creation times order the list; each job gets its own millisecond.
        harness.clock.advance(1);
        made.push(harness.enqueued(artifact, &format!("job-{number}")).await?);
    }

    let page = harness
        .owner("GET", "/v1/jobs?limit=2", None, None)
        .await?
        .json()?;
    let ids = |page: &Value| -> Vec<String> {
        page["jobs"]
            .as_array()
            .map(|jobs| {
                jobs.iter()
                    .filter_map(|j| j["id"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    assert_eq!(ids(&page), [made[4].clone(), made[3].clone()]);
    let token = page["next_page_token"]
        .as_str()
        .ok_or("no token")?
        .to_owned();
    let next = harness
        .owner(
            "GET",
            &format!("/v1/jobs?limit=2&page_token={token}"),
            None,
            None,
        )
        .await?
        .json()?;
    assert_eq!(ids(&next), [made[2].clone(), made[1].clone()]);
    let last = harness
        .owner(
            "GET",
            &format!(
                "/v1/jobs?limit=2&page_token={}",
                next["next_page_token"].as_str().ok_or("token")?
            ),
            None,
            None,
        )
        .await?
        .json()?;
    assert_eq!(ids(&last), [made[0].clone()]);
    assert!(last.get("next_page_token").is_none_or(Value::is_null));
    let all: BTreeSet<_> = [ids(&page), ids(&next), ids(&last)]
        .concat()
        .into_iter()
        .collect();
    assert_eq!(all.len(), 5, "no job twice, none missing");

    // One artifact's list holds only its own jobs.
    let own = harness
        .owner("GET", &format!("/v1/artifacts/{first}/jobs"), None, None)
        .await?
        .json()?;
    assert_eq!(
        ids(&own),
        [made[4].clone(), made[2].clone(), made[0].clone()]
    );
    Ok(())
}

#[tokio::test]
async fn a_page_token_continues_only_the_list_it_came_from_as_written() -> TestResult {
    let harness = Harness::new()?;
    let first = harness.artifact("A").await?;
    for number in 0..3 {
        harness.clock.advance(1);
        harness.enqueued(&first, &format!("job-{number}")).await?;
    }
    let page = harness
        .owner("GET", "/v1/jobs?limit=1", None, None)
        .await?
        .json()?;
    let token = page["next_page_token"]
        .as_str()
        .ok_or("no token")?
        .to_owned();
    let invalid = |reply: Reply| (reply.status, reply.code());
    let bad = (StatusCode::BAD_REQUEST, "invalid_request".to_owned());
    assert_eq!(
        invalid(
            harness
                .owner(
                    "GET",
                    &format!("/v1/artifacts/{first}/jobs?page_token={token}"),
                    None,
                    None
                )
                .await?
        ),
        bad,
        "another list's token"
    );
    let parts: Vec<&str> = token.split('.').collect();
    for tampered in [
        format!("{}.{}.{}", parts[0], parts[1], parts[2]),
        format!("x1.{}.{}.{}", parts[1], parts[2], parts[3]),
        format!("{}.{}.0{}.{}", parts[0], parts[1], parts[2], parts[3]),
        format!("{}.{}.+{}.{}", parts[0], parts[1], parts[2], parts[3]),
        format!("{}.{}.{}.{}", parts[0], "0".repeat(16), parts[2], parts[3]),
        format!("{}.{}.{}.job_zz", parts[0], parts[1], parts[2]),
        String::new(),
    ] {
        assert_eq!(
            invalid(
                harness
                    .owner(
                        "GET",
                        &format!("/v1/jobs?page_token={tampered}"),
                        None,
                        None
                    )
                    .await?
            ),
            bad,
            "{tampered}"
        );
    }
    for limit in ["0", "101", "-1", "x"] {
        assert_eq!(
            invalid(
                harness
                    .owner("GET", &format!("/v1/jobs?limit={limit}"), None, None)
                    .await?
            ),
            bad,
            "limit {limit}"
        );
    }
    let unknown = harness
        .owner("GET", "/v1/jobs?sort=oldest", None, None)
        .await?;
    assert_eq!((unknown.status, unknown.code()), bad);
    Ok(())
}
