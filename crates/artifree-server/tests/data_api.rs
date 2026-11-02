//! The application data routes (decision 0031) through the real router, core,
//! and `SQLite` store.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use artifree_contracts::api::CreateTokenResponse;
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
    api: Router,
}

impl Harness {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!(
            "artifree-data-api-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory)?;
        let database = directory.join("db.sqlite3");
        let state = AppState::new(
            SqliteStore::open(&database)?,
            artifree_providers::tokens::SqliteTokenStore::open(&database)?,
            AccessToken::parse(OWNER).ok_or("fixture token")?,
            Settings {
                content_origin: "http://127.0.0.1:7411".into(),
                ..Settings::default()
            },
        )?;
        Ok(Self {
            directory,
            api: http::router(state),
        })
    }

    /// Sends a request with the body exactly as written.
    async fn send_text(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        key: Option<&str>,
        body: Option<String>,
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
                .body(Body::from(body))?,
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

    async fn send(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        key: Option<&str>,
        body: Option<&Value>,
    ) -> Result<Reply, Box<dyn std::error::Error>> {
        self.send_text(method, path, token, key, body.map(Value::to_string))
            .await
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
                    "files": [{ "path": "index.html", "encoding": "utf8", "content": "<h1>one</h1>" }]
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

    /// Reads a path of the data space; the path is written as it is sent, so
    /// a test can pass it already percent-encoded.
    async fn read(&self, id: &str, query: &str) -> Result<Reply, Box<dyn std::error::Error>> {
        self.owner(
            "GET",
            &format!("/v1/artifacts/{id}/data{query}"),
            None,
            None,
        )
        .await
    }

    /// Reads a well-formed path of the data space.
    async fn at(&self, id: &str, path: &str) -> Result<Value, Box<dyn std::error::Error>> {
        let reply = self
            .read(id, &format!("?path={}", path.replace('/', "%2F")))
            .await?;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
        reply.json().map_err(Into::into)
    }

    async fn write(
        &self,
        method: &str,
        id: &str,
        key: &str,
        body: &Value,
    ) -> Result<Reply, Box<dyn std::error::Error>> {
        self.owner(
            method,
            &format!("/v1/artifacts/{id}/data"),
            Some(key),
            Some(body),
        )
        .await
    }

    async fn set(
        &self,
        id: &str,
        key: &str,
        base: u64,
        path: &str,
        value: &Value,
    ) -> Result<Reply, Box<dyn std::error::Error>> {
        self.write(
            "PUT",
            id,
            key,
            &json!({ "path": path, "base_data_version": base, "value": value }),
        )
        .await
    }

    async fn update(
        &self,
        id: &str,
        key: &str,
        base: u64,
        path: &str,
        merge: &Value,
    ) -> Result<Reply, Box<dyn std::error::Error>> {
        self.write(
            "PATCH",
            id,
            key,
            &json!({ "path": path, "base_data_version": base, "patch": merge }),
        )
        .await
    }

    async fn remove(
        &self,
        id: &str,
        key: &str,
        base: u64,
        path: &str,
    ) -> Result<Reply, Box<dyn std::error::Error>> {
        self.write(
            "DELETE",
            id,
            key,
            &json!({ "path": path, "base_data_version": base }),
        )
        .await
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

/// A write that must have succeeded; returns its response.
fn accepted(reply: &Reply) -> Result<Value, Box<dyn std::error::Error>> {
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    reply.json().map_err(Into::into)
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

    fn is(&self, status: StatusCode, code: &str) -> bool {
        self.status == status && self.code() == code
    }
}

// --- access ----------------------------------------------------------------

#[tokio::test]
async fn every_data_route_requires_the_bearer_token() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    let wrong = "f".repeat(64);
    let path = format!("/v1/artifacts/{id}/data");
    let set = json!({ "path": "a", "base_data_version": 0, "value": 1 });
    let update = json!({ "path": "a", "base_data_version": 0, "patch": { "x": 1 } });
    let remove = json!({ "path": "a", "base_data_version": 0 });
    for (method, body) in [
        ("GET", None),
        ("PUT", Some(&set)),
        ("PATCH", Some(&update)),
        ("DELETE", Some(&remove)),
    ] {
        for token in [None, Some(wrong.as_str())] {
            let reply = harness
                .send(method, &path, token, Some("unauthenticated-1"), body)
                .await?;
            assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{method}");
        }
    }
    assert_eq!(
        harness.at(&id, "").await?["data_version"],
        0,
        "nothing written"
    );
    Ok(())
}

#[tokio::test]
async fn an_agent_token_may_read_and_write_every_data_route() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    let agent = harness.agent_token().await?;
    let path = format!("/v1/artifacts/{id}/data");
    let agent_call = |method: &'static str, key: Option<&'static str>, body: Option<Value>| {
        let (harness, agent, path) = (&harness, agent.clone(), path.clone());
        async move {
            harness
                .send(method, &path, Some(&agent), key, body.as_ref())
                .await
        }
    };

    let set = agent_call(
        "PUT",
        Some("agent-set"),
        Some(json!({ "path": "a", "base_data_version": 0, "value": { "x": 1 } })),
    )
    .await?;
    assert_eq!(set.status, StatusCode::OK, "{}", set.text);
    let update = agent_call(
        "PATCH",
        Some("agent-update"),
        Some(json!({ "path": "a", "base_data_version": 1, "patch": { "y": 2 } })),
    )
    .await?;
    assert_eq!(update.status, StatusCode::OK, "{}", update.text);
    let shown = harness
        .send("GET", &format!("{path}?path=a"), Some(&agent), None, None)
        .await?;
    assert_eq!(shown.status, StatusCode::OK, "{}", shown.text);
    assert_eq!(shown.json()?["value"], json!({ "x": 1, "y": 2 }));
    // The agent and the owner are one actor: the owner reads the same space.
    assert_eq!(
        harness.at(&id, "a").await?["value"],
        json!({ "x": 1, "y": 2 })
    );
    let removed = agent_call(
        "DELETE",
        Some("agent-delete"),
        Some(json!({ "path": "a/x", "base_data_version": 2 })),
    )
    .await?;
    assert_eq!(removed.status, StatusCode::OK, "{}", removed.text);
    assert_eq!(harness.at(&id, "a").await?["value"], json!({ "y": 2 }));
    Ok(())
}

// --- reading and writing ----------------------------------------------------

#[tokio::test]
async fn a_new_artifact_has_an_empty_space_at_version_zero() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    assert_eq!(
        harness.at(&id, "").await?,
        json!({ "artifact_id": id, "path": "", "data_version": 0, "exists": true, "value": {} })
    );
    let missing = harness.at(&id, "a/b").await?;
    assert_eq!(missing["exists"], false);
    assert_eq!(missing["data_version"], 0);
    assert!(missing.get("value").is_none(), "{missing}");
    // The path is optional when reading: the root is the default.
    let reply = harness.read(&id, "").await?;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(reply.json()?["path"], "");
    Ok(())
}

#[tokio::test]
async fn set_update_and_delete_follow_their_rules_and_count_versions() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;

    // Set creates the missing parents and counts one version.
    let set = accepted(
        &harness
            .set(
                &id,
                "set-1",
                0,
                "scores/ada",
                &json!({ "points": 3, "tags": ["a", "b"] }),
            )
            .await?,
    )?;
    assert_eq!(
        set,
        json!({ "artifact_id": id, "path": "scores/ada", "data_version": 1,
                "changed": true, "replayed": false })
    );
    assert_eq!(
        harness.at(&id, "").await?["value"],
        json!({ "scores": { "ada": { "points": 3, "tags": ["a", "b"] } } })
    );
    // A path through a value that is not an object does not exist.
    assert_eq!(
        harness.at(&id, "scores/ada/points/deeper").await?["exists"],
        false
    );
    assert_eq!(harness.at(&id, "scores/ada/tags/0").await?["exists"], false);

    // Update is a merge patch: members merge, null removes, an array replaces.
    let updated = accepted(
        &harness
            .update(
                &id,
                "upd-1",
                1,
                "scores/ada",
                &json!({ "points": null, "level": 2, "tags": ["z"] }),
            )
            .await?,
    )?;
    assert_eq!(updated["data_version"], 2);
    assert_eq!(
        harness.at(&id, "scores/ada").await?["value"],
        json!({ "level": 2, "tags": ["z"] })
    );

    // Set stores null where a merge patch cannot.
    accepted(
        &harness
            .set(&id, "set-2", 2, "scores/bob", &Value::Null)
            .await?,
    )?;
    let bob = harness.at(&id, "scores/bob").await?;
    assert_eq!(
        (bob["exists"].clone(), bob["value"].clone()),
        (json!(true), Value::Null)
    );
    assert_eq!(
        bob.get("value"),
        Some(&Value::Null),
        "a stored null is present, unlike a value that is not there: {bob}"
    );

    // Delete removes a subtree; a path that is not there is a no-op.
    let deleted = accepted(&harness.remove(&id, "del-1", 3, "scores/ada").await?)?;
    assert_eq!(
        (deleted["data_version"].clone(), deleted["changed"].clone()),
        (json!(4), json!(true))
    );
    let again = accepted(&harness.remove(&id, "del-2", 4, "scores/ada").await?)?;
    assert_eq!(
        (again["data_version"].clone(), again["changed"].clone()),
        (json!(4), json!(false))
    );
    let nowhere = accepted(&harness.remove(&id, "del-3", 4, "never/was").await?)?;
    assert_eq!(nowhere["changed"], false);

    // Deleting the root empties the space; the version still counts on.
    let emptied = accepted(&harness.remove(&id, "del-4", 4, "").await?)?;
    assert_eq!(emptied["data_version"], 5);
    assert_eq!(harness.at(&id, "").await?["value"], json!({}));
    let next = accepted(&harness.set(&id, "set-3", 5, "a", &json!(1)).await?)?;
    assert_eq!(next["data_version"], 6);
    Ok(())
}

#[tokio::test]
async fn a_write_that_changes_nothing_keeps_the_version_but_not_a_stale_base() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    accepted(
        &harness
            .set(&id, "first-1", 0, "a", &json!({ "x": [1, 2] }))
            .await?,
    )?;

    // The same value, written in a different spelling: nothing changes.
    let same = accepted(
        &harness
            .write(
                "PUT",
                &id,
                "same-1",
                &json!({ "path": "a", "base_data_version": 1, "value": { "x": [1, 2] } }),
            )
            .await?,
    )?;
    assert_eq!(
        (same["data_version"].clone(), same["changed"].clone()),
        (json!(1), json!(false))
    );
    let empty_patch = accepted(&harness.update(&id, "same-2", 1, "a", &json!({})).await?)?;
    assert_eq!(empty_patch["changed"], false);

    // A stale base is refused even when the write would change nothing.
    accepted(&harness.set(&id, "next-1", 1, "b", &json!(true)).await?)?;
    let stale = harness
        .set(&id, "same-3", 1, "a", &json!({ "x": [1, 2] }))
        .await?;
    assert!(
        stale.is(StatusCode::CONFLICT, "data_conflict"),
        "{}",
        stale.text
    );
    Ok(())
}

#[tokio::test]
async fn values_are_stored_in_their_canonical_form() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    // Raw text, so the spelling reaches the server as written.
    let body =
        r#"{"path":"n","base_data_version":0,"value":{"z":1.0,"a":[1e2,-0,0.5,"é\n"],"m":{}}}"#;
    let reply = harness
        .send_text(
            "PUT",
            &format!("/v1/artifacts/{id}/data"),
            Some(OWNER),
            Some("canon-1"),
            Some(body.to_owned()),
        )
        .await?;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let read = harness.read(&id, "?path=n").await?;
    assert!(
        read.text
            .contains(r#""value":{"a":[100,0,0.5,"é\n"],"m":{},"z":1}"#),
        "{}",
        read.text
    );
    Ok(())
}

// --- versions and keys --------------------------------------------------------

#[tokio::test]
async fn a_stale_or_future_base_is_a_conflict_that_names_the_current_version() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    accepted(&harness.set(&id, "one-1", 0, "a", &json!(1)).await?)?;
    accepted(&harness.set(&id, "two-1", 1, "a", &json!(2)).await?)?;

    for (call, reply) in [
        ("set", harness.set(&id, "stale-1", 0, "a", &json!(3)).await?),
        (
            "update",
            harness
                .update(&id, "stale-2", 1, "a", &json!({ "k": 1 }))
                .await?,
        ),
        ("delete", harness.remove(&id, "stale-3", 5, "a").await?),
    ] {
        assert!(
            reply.is(StatusCode::CONFLICT, "data_conflict"),
            "{call}: {}",
            reply.text
        );
        assert_eq!(reply.json()?["error"]["current_data_version"], 2, "{call}");
    }
    assert_eq!(harness.at(&id, "a").await?["value"], 2, "nothing changed");

    // A refused request records no key: the same key works with the right base.
    let retried = accepted(&harness.set(&id, "stale-1", 2, "a", &json!(3)).await?)?;
    assert_eq!(retried["data_version"], 3);
    Ok(())
}

#[tokio::test]
async fn a_retry_with_the_same_key_returns_the_first_answer_without_writing_again() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    let first = accepted(
        &harness
            .set(&id, "retry-1", 0, "a", &json!({ "n": 1 }))
            .await?,
    )?;
    assert_eq!(
        (first["data_version"].clone(), first["replayed"].clone()),
        (json!(1), json!(false))
    );
    accepted(
        &harness
            .set(&id, "other-1", 1, "a", &json!({ "n": 2 }))
            .await?,
    )?;

    // The response was lost; the client repeats the request.
    let again = accepted(
        &harness
            .set(&id, "retry-1", 0, "a", &json!({ "n": 1 }))
            .await?,
    )?;
    assert_eq!(
        (
            again["data_version"].clone(),
            again["changed"].clone(),
            again["replayed"].clone()
        ),
        (json!(1), json!(true), json!(true)),
        "the original answer, not the current state"
    );
    assert_eq!(
        harness.at(&id, "a").await?["value"],
        json!({ "n": 2 }),
        "not written again"
    );

    // The same key for another request is a reuse, however it differs.
    for (what, reply) in [
        (
            "value",
            harness
                .set(&id, "retry-1", 0, "a", &json!({ "n": 9 }))
                .await?,
        ),
        (
            "path",
            harness
                .set(&id, "retry-1", 0, "b", &json!({ "n": 1 }))
                .await?,
        ),
        (
            "base",
            harness
                .set(&id, "retry-1", 2, "a", &json!({ "n": 1 }))
                .await?,
        ),
        (
            "operation",
            harness
                .update(&id, "retry-1", 0, "a", &json!({ "n": 1 }))
                .await?,
        ),
    ] {
        assert!(
            reply.is(StatusCode::UNPROCESSABLE_ENTITY, "request_key_reused"),
            "{what}: {}",
            reply.text
        );
    }
    Ok(())
}

#[tokio::test]
async fn keys_are_one_space_across_data_and_other_requests() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    accepted(&harness.set(&id, "shared-data", 0, "a", &json!(1)).await?)?;

    // A key a data write used does not serve an artifact request ...
    let create = harness
        .owner(
            "POST",
            "/v1/artifacts",
            Some("shared-data"),
            Some(&json!({
                "title": "B",
                "files": [{ "path": "index.html", "encoding": "utf8", "content": "x" }]
            })),
        )
        .await?;
    assert!(
        create.is(StatusCode::UNPROCESSABLE_ENTITY, "request_key_reused"),
        "{}",
        create.text
    );
    let job = harness
        .owner(
            "POST",
            &format!("/v1/artifacts/{id}/jobs"),
            Some("shared-data"),
            Some(&json!({ "kind": "verify_revision", "revision": 2 })),
        )
        .await?;
    assert!(
        job.is(StatusCode::UNPROCESSABLE_ENTITY, "request_key_reused"),
        "{}",
        job.text
    );

    // ... and a key an artifact request used does not serve a data write.
    for key in ["create-A", "save-A"] {
        let reply = harness.set(&id, key, 1, "b", &json!(2)).await?;
        assert!(
            reply.is(StatusCode::UNPROCESSABLE_ENTITY, "request_key_reused"),
            "{key}: {}",
            reply.text
        );
    }
    let queued = harness
        .owner(
            "POST",
            &format!("/v1/artifacts/{id}/jobs"),
            Some("shared-job"),
            Some(&json!({ "kind": "verify_revision", "revision": 2 })),
        )
        .await?;
    assert_eq!(queued.status, StatusCode::CREATED, "{}", queued.text);
    let reply = harness.set(&id, "shared-job", 1, "b", &json!(2)).await?;
    assert!(
        reply.is(StatusCode::UNPROCESSABLE_ENTITY, "request_key_reused"),
        "{}",
        reply.text
    );
    assert_eq!(
        harness.at(&id, "b").await?["exists"],
        false,
        "nothing written"
    );
    Ok(())
}

#[tokio::test]
async fn two_writers_from_one_version_cannot_both_win() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    for round in 0..8u64 {
        // Different every round: writing what is already there changes nothing.
        let (left_value, right_value) = (
            json!(format!("left-{round}")),
            json!(format!("right-{round}")),
        );
        let (left_key, right_key) = (format!("left-{round}"), format!("right-{round}"));
        let (left, right) = tokio::join!(
            harness.set(&id, &left_key, round, "who", &left_value),
            harness.set(&id, &right_key, round, "who", &right_value),
        );
        let (left, right) = (left?, right?);
        let statuses = [left.status, right.status];
        assert!(
            statuses.contains(&StatusCode::OK) && statuses.contains(&StatusCode::CONFLICT),
            "round {round}: {statuses:?}"
        );
        let loser = if left.status == StatusCode::OK {
            &right
        } else {
            &left
        };
        assert_eq!(loser.code(), "data_conflict");
        assert_eq!(harness.at(&id, "who").await?["data_version"], round + 1);
    }
    Ok(())
}

#[tokio::test]
async fn one_request_sent_twice_at_once_is_written_once() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    let value = json!(1);
    let (one, two) = tokio::join!(
        harness.set(&id, "twice-1", 0, "a", &value),
        harness.set(&id, "twice-1", 0, "a", &value),
    );
    let answers = [accepted(&one?)?, accepted(&two?)?];
    assert_eq!(answers[0]["data_version"], 1);
    assert_eq!(answers[1]["data_version"], 1);
    let replayed = answers
        .iter()
        .filter(|answer| answer["replayed"] == true)
        .count();
    assert_eq!(replayed, 1, "{answers:?}");
    assert_eq!(harness.at(&id, "").await?["data_version"], 1);
    Ok(())
}

// --- what is refused -----------------------------------------------------------

#[tokio::test]
async fn paths_that_break_the_grammar_are_refused_without_echoing_them() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    let sixteen = vec!["s"; 16].join("/");
    let seventeen = vec!["s"; 17].join("/");
    let long = "x".repeat(65);
    let bad = [
        "/leading",
        "trailing/",
        "double//slash",
        ".",
        "..",
        "a/../b",
        "has space",
        "per%cent",
        "naïve",
        "tab\t",
        "semi;colon",
        &seventeen,
        &long,
    ];
    for path in bad {
        let reply = harness.set(&id, "bad-path", 0, path, &json!(1)).await?;
        assert!(
            reply.is(StatusCode::UNPROCESSABLE_ENTITY, "invalid_request"),
            "{path:?}: {}",
            reply.text
        );
        assert!(
            !reply.text.contains("semi") && !reply.text.contains("naïve"),
            "echoed: {}",
            reply.text
        );
        let reply = harness.update(&id, "bad-path", 0, path, &json!({})).await?;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{path:?}");
        let reply = harness.remove(&id, "bad-path", 0, path).await?;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{path:?}");
    }
    // The same grammar when reading.
    for query in [
        "?path=%2Fleading",
        "?path=a%2F%2Fb",
        "?path=..",
        "?path=a%20b",
        "?path=na%C3%AFve",
    ] {
        let reply = harness.read(&id, query).await?;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{query}: {}",
            reply.text
        );
    }
    // The longest allowed path works.
    accepted(&harness.set(&id, "deep-1", 0, &sixteen, &json!(1)).await?)?;
    accepted(
        &harness
            .set(&id, "wide-1", 1, &"y".repeat(64), &json!(1))
            .await?,
    )?;
    assert_eq!(
        harness.at(&id, "").await?["data_version"],
        2,
        "only the good ones were written"
    );
    Ok(())
}

#[tokio::test]
async fn a_path_through_a_value_that_is_not_an_object_is_a_conflict() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    accepted(
        &harness
            .set(&id, "base-1", 0, "a", &json!({ "list": [1], "n": 5 }))
            .await?,
    )?;
    for (call, reply) in [
        (
            "set into a number",
            harness.set(&id, "conf-1", 1, "a/n/x", &json!(1)).await?,
        ),
        (
            "set into an array",
            harness.set(&id, "conf-2", 1, "a/list/x", &json!(1)).await?,
        ),
        (
            "update into a number",
            harness
                .update(&id, "conf-3", 1, "a/n/x", &json!({ "k": 1 }))
                .await?,
        ),
    ] {
        assert!(
            reply.is(StatusCode::CONFLICT, "data_path_conflict"),
            "{call}: {}",
            reply.text
        );
    }
    // Deleting through one is the no-op of a path that is not there.
    let removed = accepted(&harness.remove(&id, "conf-4", 1, "a/n/x").await?)?;
    assert_eq!(removed["changed"], false);
    assert_eq!(
        harness.at(&id, "a").await?["value"],
        json!({ "list": [1], "n": 5 })
    );
    Ok(())
}

#[tokio::test]
async fn the_root_holds_an_object() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    for value in [json!([1]), json!(1), json!("s"), Value::Null] {
        let reply = harness.set(&id, "root-1", 0, "", &value).await?;
        assert!(
            reply.is(StatusCode::UNPROCESSABLE_ENTITY, "invalid_request"),
            "{value}: {}",
            reply.text
        );
    }
    let reply = harness.update(&id, "root-2", 0, "", &Value::Null).await?;
    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        reply.text
    );
    let set = accepted(
        &harness
            .set(&id, "root-3", 0, "", &json!({ "a": 1 }))
            .await?,
    )?;
    assert_eq!(set["data_version"], 1);
    Ok(())
}

#[tokio::test]
async fn values_that_break_the_rules_are_refused() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    let put = |value: String| {
        let (harness, uri) = (&harness, format!("/v1/artifacts/{id}/data"));
        async move {
            harness
                .send_text(
                    "PUT",
                    &uri,
                    Some(OWNER),
                    Some("rules-1"),
                    Some(format!(
                        r#"{{"path":"a","base_data_version":0,"value":{value}}}"#
                    )),
                )
                .await
        }
    };
    for (what, value) in [
        ("duplicate member", r#"{"x":1,"x":2}"#.to_owned()),
        ("duplicate in depth", r#"{"o":[{"x":1,"x":1}]}"#.to_owned()),
        ("duplicate by escape", r#"{"x":1,"x":2}"#.to_owned()),
        ("integer beyond 2^53", "9007199254740993".to_owned()),
        ("number beyond a double", "1e999".to_owned()),
        ("lone surrogate", r#""\ud800""#.to_owned()),
        ("empty member name", r#"{"":1}"#.to_owned()),
        (
            "long member name",
            format!(r#"{{"{}":1}}"#, "k".repeat(257)),
        ),
    ] {
        let reply = put(value).await?;
        assert!(
            reply.is(StatusCode::UNPROCESSABLE_ENTITY, "invalid_request"),
            "{what}: {}",
            reply.text
        );
    }
    // The bound of a safe integer itself is allowed.
    let edge = put("9007199254740991".to_owned()).await?;
    assert_eq!(edge.status, StatusCode::OK, "{}", edge.text);
    assert_eq!(
        harness.at(&id, "").await?["data_version"],
        1,
        "only the last was written"
    );
    Ok(())
}

#[tokio::test]
async fn a_body_that_is_not_the_request_is_refused() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    let uri = format!("/v1/artifacts/{id}/data");
    for (what, body) in [
        ("not json", "{".to_owned()),
        (
            "missing path",
            r#"{"base_data_version":0,"value":1}"#.to_owned(),
        ),
        ("missing base", r#"{"path":"a","value":1}"#.to_owned()),
        (
            "missing value",
            r#"{"path":"a","base_data_version":0}"#.to_owned(),
        ),
        (
            "unknown field",
            r#"{"path":"a","base_data_version":0,"value":1,"extra":1}"#.to_owned(),
        ),
        (
            "negative base",
            r#"{"path":"a","base_data_version":-1,"value":1}"#.to_owned(),
        ),
        (
            "path of the wrong type",
            r#"{"path":1,"base_data_version":0,"value":1}"#.to_owned(),
        ),
        (
            "trailing text",
            r#"{"path":"a","base_data_version":0,"value":1} x"#.to_owned(),
        ),
    ] {
        let reply = harness
            .send_text("PUT", &uri, Some(OWNER), Some("body-1"), Some(body))
            .await?;
        assert!(
            reply.is(StatusCode::BAD_REQUEST, "invalid_request"),
            "{what}: {}",
            reply.text
        );
    }
    // A missing key is refused before the body is looked at.
    let reply = harness
        .send_text("PUT", &uri, Some(OWNER), None, Some("{".to_owned()))
        .await?;
    assert!(
        reply.is(StatusCode::BAD_REQUEST, "invalid_request"),
        "{}",
        reply.text
    );
    let reply = harness
        .owner(
            "PUT",
            &uri,
            None,
            Some(&json!({ "path": "a", "base_data_version": 0, "value": 1 })),
        )
        .await?;
    assert!(
        reply.is(StatusCode::BAD_REQUEST, "invalid_request"),
        "{}",
        reply.text
    );
    // An unknown or repeated query parameter is refused too.
    for query in ["?paths=a", "?path=a&path=b", "?path=a&extra=1"] {
        let reply = harness.read(&id, query).await?;
        assert_eq!(
            reply.status,
            StatusCode::BAD_REQUEST,
            "{query}: {}",
            reply.text
        );
    }
    assert_eq!(harness.at(&id, "").await?["data_version"], 0);
    Ok(())
}

#[tokio::test]
async fn an_artifact_that_is_not_an_identifier_is_not_found() -> TestResult {
    let harness = Harness::new()?;
    for id in ["nothing", "art_00000000000000000000000000000000"] {
        let reply = harness.read(id, "").await?;
        assert!(
            reply.is(StatusCode::NOT_FOUND, "not_found"),
            "{id}: {}",
            reply.text
        );
        let reply = harness.set(id, "unknown-1", 0, "a", &json!(1)).await?;
        assert!(
            reply.is(StatusCode::NOT_FOUND, "not_found"),
            "{id}: {}",
            reply.text
        );
    }
    Ok(())
}

// --- limits ----------------------------------------------------------------------

#[tokio::test]
async fn a_value_deeper_than_the_limit_is_refused_even_when_absurdly_deep() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    let nested = |depth: usize| format!("{}1{}", "[".repeat(depth), "]".repeat(depth));
    let uri = format!("/v1/artifacts/{id}/data");
    let put = |value: String| {
        let uri = uri.clone();
        let harness = &harness;
        async move {
            harness
                .send_text(
                    "PUT",
                    &uri,
                    Some(OWNER),
                    Some("deep-1"),
                    Some(format!(
                        r#"{{"path":"a","base_data_version":0,"value":{value}}}"#
                    )),
                )
                .await
        }
    };
    for depth in [33, 100, 100_000] {
        let reply = put(nested(depth)).await?;
        assert!(
            reply.is(StatusCode::UNPROCESSABLE_ENTITY, "data_too_large"),
            "{depth}: {}",
            reply.text
        );
    }
    let ok = put(nested(10)).await?;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text);
    Ok(())
}

#[tokio::test]
async fn a_value_with_too_many_parts_is_refused() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    let many = json!(vec![0; 10_001]);
    let reply = harness.set(&id, "many-1", 0, "a", &many).await?;
    assert!(
        reply.is(StatusCode::UNPROCESSABLE_ENTITY, "data_too_large"),
        "{}",
        reply.text
    );
    let fits = json!(vec![0; 9_999]);
    accepted(&harness.set(&id, "many-2", 0, "a", &fits).await?)?;
    // The count is of the whole space: the next ones do not fit beside it.
    let reply = harness
        .set(&id, "many-3", 1, "b", &json!(vec![0; 10]))
        .await?;
    assert!(
        reply.is(StatusCode::UNPROCESSABLE_ENTITY, "data_too_large"),
        "{}",
        reply.text
    );
    Ok(())
}

#[tokio::test]
async fn the_size_of_a_value_and_of_the_space_is_bounded() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    // A value over its bound, within what the route reads: refused as data.
    let reply = harness
        .set(&id, "big-1", 0, "a", &json!("a".repeat(265_000)))
        .await?;
    assert!(
        reply.is(StatusCode::UNPROCESSABLE_ENTITY, "data_too_large"),
        "{}",
        reply.text
    );
    // A body over the route's bound is refused as a body.
    let reply = harness
        .set(&id, "big-2", 0, "a", &json!("a".repeat(300_000)))
        .await?;
    assert!(
        reply.is(StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large"),
        "{}",
        reply.text
    );

    // Five values of 200 000 bytes fit in the 1 MiB space; the sixth does not.
    for (index, name) in ["a", "b", "c", "d", "e"].into_iter().enumerate() {
        let reply = harness
            .set(
                &id,
                &format!("fill-{name}"),
                index as u64,
                name,
                &json!("v".repeat(200_000)),
            )
            .await?;
        assert_eq!(reply.status, StatusCode::OK, "{name}: {}", reply.text);
    }
    let reply = harness
        .set(&id, "fill-f", 5, "f", &json!("v".repeat(200_000)))
        .await?;
    assert!(
        reply.is(StatusCode::UNPROCESSABLE_ENTITY, "data_too_large"),
        "{}",
        reply.text
    );
    assert_eq!(harness.at(&id, "").await?["data_version"], 5);
    // Removing makes room again.
    accepted(&harness.remove(&id, "fill-del", 5, "a").await?)?;
    accepted(
        &harness
            .set(&id, "fill-g", 6, "f", &json!("v".repeat(200_000)))
            .await?,
    )?;
    Ok(())
}

// --- artifact lifecycle ---------------------------------------------------------

#[tokio::test]
async fn data_of_an_artifact_in_the_trash_or_purged_is_not_found() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    let other = harness.artifact("B").await?;
    accepted(
        &harness
            .set(&id, "keep-1", 0, "a", &json!({ "n": 1 }))
            .await?,
    )?;
    accepted(
        &harness
            .set(&other, "keep-2", 0, "a", &json!({ "n": 2 }))
            .await?,
    )?;

    harness.trash(&id, "trash-a", 1, true).await?;
    let gone = |reply: &Reply| reply.is(StatusCode::NOT_FOUND, "not_found");
    assert!(gone(&harness.read(&id, "").await?));
    assert!(gone(
        &harness.set(&id, "new-key-1", 1, "a", &json!(1)).await?
    ));
    assert!(gone(
        &harness.update(&id, "new-key-2", 1, "a", &json!({})).await?
    ));
    assert!(gone(&harness.remove(&id, "new-key-3", 1, "a").await?));
    assert!(
        gone(
            &harness
                .set(&id, "keep-1", 0, "a", &json!({ "n": 1 }))
                .await?
        ),
        "a retry too"
    );

    // Recovery brings the data back, at its version.
    harness.trash(&id, "recover-a", 2, false).await?;
    let back = harness.at(&id, "a").await?;
    assert_eq!(
        (back["data_version"].clone(), back["value"].clone()),
        (json!(1), json!({ "n": 1 }))
    );
    accepted(
        &harness
            .set(&id, "after-1", 1, "a", &json!({ "n": 3 }))
            .await?,
    )?;

    // Purging ends it for good.
    harness.trash(&id, "trash-again", 3, true).await?;
    let purged = harness
        .owner(
            "POST",
            &format!("/v1/artifacts/{id}/purge"),
            Some("purge-a"),
            Some(&json!({ "base_metadata_revision": 4 })),
        )
        .await?;
    assert_eq!(purged.status, StatusCode::OK, "{}", purged.text);
    assert!(gone(&harness.read(&id, "").await?));
    assert!(gone(
        &harness
            .set(&id, "keep-1", 0, "a", &json!({ "n": 1 }))
            .await?
    ));
    assert_eq!(
        harness.at(&other, "a").await?["value"],
        json!({ "n": 2 }),
        "another artifact's"
    );
    Ok(())
}

#[tokio::test]
async fn restoring_a_revision_leaves_the_data_and_its_version_alone() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    accepted(&harness.set(&id, "r-1", 0, "a", &json!(1)).await?)?;
    let restored = harness
        .owner(
            "POST",
            &format!("/v1/artifacts/{id}/revisions/1/restore"),
            Some("restore-1"),
            Some(&json!({ "base_revision": 2 })),
        )
        .await?;
    assert_eq!(restored.status, StatusCode::CREATED, "{}", restored.text);
    let read = harness.at(&id, "").await?;
    assert_eq!(
        (read["data_version"].clone(), read["value"].clone()),
        (json!(1), json!({ "a": 1 }))
    );
    // And a source change is not a data change: the base is still 1.
    accepted(&harness.set(&id, "r-2", 1, "a", &json!(2)).await?)?;
    Ok(())
}

// --- duplicating an artifact ---------------------------------------------------

#[tokio::test]
async fn a_duplicate_receives_a_copy_of_the_data_only_when_asked() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    accepted(
        &harness
            .set(&id, "src-1", 0, "a", &json!({ "n": 1 }))
            .await?,
    )?;
    accepted(&harness.set(&id, "src-2", 1, "b", &json!([true])).await?)?;
    let duplicate = |key: &'static str, body: Value| {
        let uri = format!("/v1/artifacts/{id}/revisions/1/duplicate");
        let harness = &harness;
        async move { harness.owner("POST", &uri, Some(key), Some(&body)).await }
    };

    // By default: files only, an empty space.
    let plain = duplicate("dup-plain", json!({})).await?;
    assert_eq!(plain.status, StatusCode::CREATED, "{}", plain.text);
    assert_eq!(plain.json()?["copied"], json!(["files"]));
    let plain_id = plain.json()?["artifact"]["id"]
        .as_str()
        .ok_or("id")?
        .to_owned();
    let empty = harness.at(&plain_id, "").await?;
    assert_eq!(
        (empty["data_version"].clone(), empty["value"].clone()),
        (json!(0), json!({}))
    );

    // Asked for: the source's data at the time, as an independent space.
    let copy = duplicate("dup-data", json!({ "copy_data": true })).await?;
    assert_eq!(copy.status, StatusCode::CREATED, "{}", copy.text);
    assert_eq!(copy.json()?["copied"], json!(["files", "data"]));
    let copy_id = copy.json()?["artifact"]["id"]
        .as_str()
        .ok_or("id")?
        .to_owned();
    let copied = harness.at(&copy_id, "").await?;
    assert_eq!(copied["value"], json!({ "a": { "n": 1 }, "b": [true] }));
    assert_eq!(
        copied["data_version"], 1,
        "the copy starts its own history of versions"
    );

    // Neither space follows the other afterwards.
    accepted(
        &harness
            .set(&copy_id, "copy-1", 1, "a", &json!("changed"))
            .await?,
    )?;
    accepted(&harness.set(&id, "src-3", 2, "c", &json!(3)).await?)?;
    assert_eq!(harness.at(&id, "a").await?["value"], json!({ "n": 1 }));
    assert_eq!(harness.at(&copy_id, "c").await?["exists"], false);

    // A retry of the request answers with the same copy and does not copy again.
    let replay = duplicate("dup-data", json!({ "copy_data": true })).await?;
    assert_eq!(replay.status, StatusCode::CREATED, "{}", replay.text);
    assert_eq!(
        replay.json()?["artifact"]["id"].as_str(),
        Some(copy_id.as_str())
    );
    assert_eq!(replay.json()?["replayed"], true);
    assert_eq!(harness.at(&copy_id, "a").await?["value"], "changed");

    // Another request under the same key, without the data, is a reuse.
    let reuse = duplicate("dup-data", json!({})).await?;
    assert!(
        reuse.is(StatusCode::UNPROCESSABLE_ENTITY, "request_key_reused"),
        "{}",
        reuse.text
    );

    // A source with no data: asking is allowed and copies nothing.
    let other = harness.artifact("B").await?;
    let none = harness
        .owner(
            "POST",
            &format!("/v1/artifacts/{other}/revisions/1/duplicate"),
            Some("dup-none"),
            Some(&json!({ "copy_data": true })),
        )
        .await?;
    assert_eq!(none.status, StatusCode::CREATED, "{}", none.text);
    let none_id = none.json()?["artifact"]["id"]
        .as_str()
        .ok_or("id")?
        .to_owned();
    assert_eq!(harness.at(&none_id, "").await?["data_version"], 0);
    Ok(())
}
