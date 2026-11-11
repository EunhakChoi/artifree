//! Behavioral tests for `GET /v1/background` (decision 0055, rule 5): the body
//! for each worker state and last exit, the tokens that may read it, the
//! refusals, and the default settings.
//!
//! The expected bodies are written out here from the decision's wire names and
//! not produced by the types under test, so a change of the wire form fails.

use std::collections::BTreeSet;
use std::error::Error;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use artifree_contracts::api::{
    CreateTokenResponse, UserInfo, WorkerExit, WorkerExitOutcome, WorkerMode, WorkerState,
    WorkerStatus,
};
use artifree_providers::storage::SqliteStore;
use artifree_providers::tokens::SqliteTokenStore;
use artifree_server::auth::AccessToken;
use artifree_server::http::{self, AppState, Settings};
use artifree_server::supervisor::WorkerStatusHandle;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::response::Response;
use serde_json::{Value, json};
use tower::ServiceExt;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// The owner's bootstrap token, the administrator's credential.
const OWNER: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
/// A well-formed token that nothing issued.
const UNISSUED: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
/// The start attempts after the first, as the status reports them.
const RESTARTS: u64 = 3;
/// The worker's dropped lines, as the status reports them.
const DROPPED_LINES: u64 = 2;
/// The time of the last exit, in Unix milliseconds.
const AT_MS: i64 = 1_794_000_000_123;
/// The largest response body a test reads.
const MAX_BODY: usize = 1 << 20;

/// The states rule 5 names, with their wire names.
const STATES: [(WorkerState, &str); 3] = [
    (WorkerState::Running, "running"),
    (WorkerState::Restarting, "restarting"),
    (WorkerState::Stopped, "stopped"),
];

/// The shapes of `last_exit`: absent, then each outcome with its wire name.
const LAST_EXITS: [(Option<WorkerExitOutcome>, Option<&str>); 4] = [
    (None, None),
    (Some(WorkerExitOutcome::Clean), Some("clean")),
    (Some(WorkerExitOutcome::Failed), Some("failed")),
    (Some(WorkerExitOutcome::NotStarted), Some("not_started")),
];

/// The body rule 5 gives for a supervised worker in `state`, with this file's
/// counters and, when there was an exit, its wire outcome.
fn expected_body(state: &str, last_exit: Option<&str>) -> Value {
    let mut worker = json!({
        "mode": "supervised",
        "state": state,
        "restarts": RESTARTS,
        "dropped_lines": DROPPED_LINES,
    });
    if let Some(outcome) = last_exit {
        worker["last_exit"] = json!({ "at_ms": AT_MS, "outcome": outcome });
    }
    json!({ "worker": worker })
}

/// The keys rule 5 gives `worker`: `last_exit` only once there has been an exit.
fn expected_keys(last_exit: Option<&str>) -> BTreeSet<&'static str> {
    let mut keys = BTreeSet::from(["dropped_lines", "mode", "restarts", "state"]);
    if last_exit.is_some() {
        keys.insert("last_exit");
    }
    keys
}

/// The keys of the `worker` object in a response body, or none without one.
fn worker_keys(body: &Value) -> BTreeSet<&str> {
    body.get("worker")
        .and_then(Value::as_object)
        .map_or_else(BTreeSet::new, |worker| {
            worker.keys().map(String::as_str).collect()
        })
}

/// A server over a fresh store, with `settings` as its settings.
struct Harness {
    directory: PathBuf,
    api: Router,
}

/// A user the owner registered, with the token issued to that user.
struct Person {
    id: String,
    secret: String,
}

/// A response: its status, its headers, and its body read as JSON.
struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    json: Value,
}

impl Harness {
    fn open(settings: Settings) -> TestResult<Self> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!(
            "artifree-background-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory)?;
        let database = directory.join("db.sqlite3");
        let state = AppState::new(
            SqliteStore::open(&database)?,
            SqliteTokenStore::open(&database)?,
            AccessToken::parse(OWNER).ok_or("the fixture owner token is invalid")?,
            settings,
        )?;
        Ok(Self {
            directory,
            api: http::router(state),
        })
    }

    /// Sends `GET /v1/background`, with `token` as the bearer token when given.
    async fn background(&self, token: Option<&str>) -> TestResult<Reply> {
        let mut builder = Request::get("/v1/background");
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        self.send(builder.body(Body::empty())?).await
    }

    /// Sends a `POST` as the owner, with `body` as JSON when there is one.
    async fn post(&self, path: &str, body: Option<Value>) -> TestResult<Reply> {
        let mut builder =
            Request::post(path).header(header::AUTHORIZATION, format!("Bearer {OWNER}"));
        let body = match body {
            Some(value) => {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
                Body::from(value.to_string())
            }
            None => Body::empty(),
        };
        self.send(builder.body(body)?).await
    }

    async fn send(&self, request: Request<Body>) -> TestResult<Reply> {
        let response = self.api.clone().oneshot(request).await?;
        Reply::read(response).await
    }

    /// An agent token: one the owner issues without a user.
    async fn agent_token(&self) -> TestResult<String> {
        let reply = self
            .post("/v1/tokens", Some(json!({ "name": "Codex Agent" })))
            .await?;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.json);
        let created: CreateTokenResponse = serde_json::from_value(reply.json)?;
        Ok(created.secret)
    }

    /// A user the owner registers, with one token issued to that user.
    async fn person(&self, label: &str) -> TestResult<Person> {
        let reply = self
            .post("/v1/users", Some(json!({ "label": label })))
            .await?;
        assert_eq!(
            reply.status,
            StatusCode::CREATED,
            "registering {label}: {}",
            reply.json
        );
        let user: UserInfo = serde_json::from_value(reply.json)?;
        let reply = self
            .post(
                "/v1/tokens",
                Some(json!({ "name": label, "user_id": user.id })),
            )
            .await?;
        assert_eq!(
            reply.status,
            StatusCode::CREATED,
            "issuing the token of {label}: {}",
            reply.json
        );
        let issued: CreateTokenResponse = serde_json::from_value(reply.json)?;
        Ok(Person {
            id: user.id,
            secret: issued.secret,
        })
    }

    /// Disables `user`, as the owner does.
    async fn disable(&self, user: &str) -> TestResult<()> {
        let reply = self
            .post(&format!("/v1/users/{user}/disable"), None)
            .await?;
        assert_eq!(
            reply.status,
            StatusCode::OK,
            "disabling {user}: {}",
            reply.json
        );
        Ok(())
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

impl Reply {
    async fn read(response: Response) -> TestResult<Self> {
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = to_bytes(response.into_body(), MAX_BODY).await?;
        Ok(Self {
            status,
            headers,
            json: serde_json::from_slice(&bytes)?,
        })
    }
}

#[tokio::test]
async fn every_accepted_token_reads_the_rule_five_body_in_every_state_and_outcome() -> TestResult {
    for (state, state_name) in STATES {
        for (outcome, outcome_name) in LAST_EXITS {
            let worker = WorkerStatus {
                mode: WorkerMode::Supervised,
                state,
                restarts: RESTARTS,
                last_exit: outcome.map(|outcome| WorkerExit {
                    at_ms: AT_MS,
                    outcome,
                }),
                dropped_lines: DROPPED_LINES,
            };
            let harness = Harness::open(Settings {
                worker_status: WorkerStatusHandle::new(worker),
                ..Settings::default()
            })?;
            let agent = harness.agent_token().await?;
            let bob = harness.person("bob").await?;
            let expected = expected_body(state_name, outcome_name);
            let keys = expected_keys(outcome_name);
            for (kind, token) in [
                ("administrator", OWNER),
                ("agent", agent.as_str()),
                ("user", bob.secret.as_str()),
            ] {
                let reply = harness.background(Some(token)).await?;
                let case = format!("{kind} token, state {state_name}, last_exit {outcome_name:?}");
                assert_eq!(reply.status, StatusCode::OK, "{case}: {}", reply.json);
                assert_eq!(worker_keys(&reply.json), keys, "{case}: the keys of worker");
                assert_eq!(reply.json, expected, "{case}");
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn the_body_reads_the_status_as_it_is_at_each_request() -> TestResult {
    let worker = WorkerStatusHandle::new(WorkerStatus {
        mode: WorkerMode::Supervised,
        state: WorkerState::Running,
        restarts: 0,
        last_exit: None,
        dropped_lines: 0,
    });
    let harness = Harness::open(Settings {
        worker_status: worker.clone(),
        ..Settings::default()
    })?;
    let before = harness.background(Some(OWNER)).await?;
    assert_eq!(
        before.json,
        json!({ "worker": { "mode": "supervised", "state": "running", "restarts": 0, "dropped_lines": 0 } }),
        "before the supervisor changes the status"
    );
    // The supervisor changes the status the server holds; a clone of the handle shares it.
    worker.update(|status| {
        status.state = WorkerState::Restarting;
        status.restarts = RESTARTS;
        status.last_exit = Some(WorkerExit {
            at_ms: AT_MS,
            outcome: WorkerExitOutcome::Failed,
        });
        status.dropped_lines = DROPPED_LINES;
    });
    let after = harness.background(Some(OWNER)).await?;
    assert_eq!(
        after.json,
        expected_body("restarting", Some("failed")),
        "after the supervisor changed the status"
    );
    Ok(())
}

#[tokio::test]
async fn a_request_without_an_accepted_token_is_refused_and_reads_no_worker() -> TestResult {
    let harness = Harness::open(Settings::default())?;
    for (case, token) in [
        ("no token", None),
        ("a token nobody issued", Some(UNISSUED)),
    ] {
        let reply = harness.background(token).await?;
        assert_eq!(
            reply.status,
            StatusCode::UNAUTHORIZED,
            "{case}: {}",
            reply.json
        );
        assert_eq!(
            reply
                .headers
                .get(header::WWW_AUTHENTICATE)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer"),
            "{case}: the refusal names the bearer scheme"
        );
        assert_eq!(reply.json["error"]["code"], "unauthenticated", "{case}");
        assert!(
            reply.json.get("worker").is_none(),
            "{case}: a refusal carries no worker status, got {}",
            reply.json
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_disabled_users_token_is_refused_once_the_user_is_disabled() -> TestResult {
    let harness = Harness::open(Settings::default())?;
    let bob = harness.person("bob").await?;
    let before = harness.background(Some(bob.secret.as_str())).await?;
    assert_eq!(
        before.status,
        StatusCode::OK,
        "before the disable, bob's token reads the status: {}",
        before.json
    );
    harness.disable(&bob.id).await?;
    let after = harness.background(Some(bob.secret.as_str())).await?;
    assert_eq!(
        after.status,
        StatusCode::UNAUTHORIZED,
        "after the disable, bob's token is refused: {}",
        after.json
    );
    assert_eq!(after.json["error"]["code"], "unauthenticated");
    assert!(
        after.json.get("worker").is_none(),
        "after the disable, a refusal carries no worker status: {}",
        after.json
    );
    Ok(())
}

#[tokio::test]
async fn default_settings_report_mode_none_stopped_and_no_last_exit() -> TestResult {
    let harness = Harness::open(Settings::default())?;
    let reply = harness.background(Some(OWNER)).await?;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.json);
    assert_eq!(
        reply.json,
        json!({ "worker": { "mode": "none", "state": "stopped", "restarts": 0, "dropped_lines": 0 } }),
        "the default status"
    );
    assert_eq!(
        worker_keys(&reply.json),
        BTreeSet::from(["dropped_lines", "mode", "restarts", "state"]),
        "the default status has no last_exit"
    );
    Ok(())
}
