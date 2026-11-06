//! The application data routes (decision 0031) through the real router, core,
//! and `SQLite` store.

use std::fmt;
use std::fmt::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use artifree_contracts::api::CreateTokenResponse;
use artifree_providers::storage::SqliteStore;
use artifree_server::auth::AccessToken;
use artifree_server::http::{self, AppState, Settings};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use base64::Engine;
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use tower::ServiceExt;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const OWNER: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// Encodes every fixture byte so query transport cannot normalize its text.
fn encode_query_path(text: &str) -> Result<String, fmt::Error> {
    let mut encoded = String::with_capacity(text.len() * 3);
    for byte in text.bytes() {
        write!(encoded, "%{byte:02X}")?;
    }
    Ok(encoded)
}

#[test]
fn query_fixture_encoding_preserves_each_utf8_byte() -> Result<(), fmt::Error> {
    assert_eq!(encode_query_path("")?, "");
    assert_eq!(encode_query_path("a%/+")?, "%61%25%2F%2B");
    assert_eq!(encode_query_path("한/𝄞")?, "%ED%95%9C%2F%F0%9D%84%9E");
    assert_eq!(encode_query_path("e\u{301}/é")?, "%65%CC%81%2F%C3%A9");
    Ok(())
}

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
        let encoded = encode_query_path(path)?;
        let reply = self.read(id, &format!("?path={encoded}")).await?;
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
async fn unicode_data_keeps_byte_identity_versions_replay_and_listing() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("unicode").await?;
    let original = json!({"é": 1, "e\u{301}": 2, "한글": {"값": 3}, "𝄞": 4, "bad/name": 5});
    accepted(&harness.set(&id, "unicode-root", 0, "", &original).await?)?;
    assert_eq!(harness.at(&id, "é").await?["value"], 1);
    assert_eq!(harness.at(&id, "e\u{301}").await?["value"], 2);
    assert_eq!(harness.at(&id, "한글/값").await?["value"], 3);
    let page = harness.members(&id, "?limit=100").await?;
    assert_eq!(
        listed(&page)?,
        (strings(&["e\u{301}", "é", "한글", "𝄞"]), None)
    );
    let first = harness.members(&id, "?limit=1&at_data_version=1").await?;
    let (_, token) = listed(&first)?;
    let token = token.ok_or("Unicode continuation token")?;
    let continuation = harness
        .members(
            &id,
            &format!("?limit=3&at_data_version=1&page_token={token}"),
        )
        .await?;
    assert_eq!(listed(&continuation)?, (strings(&["é", "한글", "𝄞"]), None));
    assert_eq!(
        harness.at(&id, "").await?["data_version"],
        1,
        "listing never writes"
    );
    assert_eq!(
        harness.at(&id, "").await?["value"],
        original,
        "unaddressable members survive"
    );
    accepted(
        &harness
            .set(&id, "ascii-stable", 1, "ascii/path", &json!(7))
            .await?,
    )?;
    accepted(
        &harness
            .set(&id, "unicode-head", 2, "한글/값", &json!(8))
            .await?,
    )?;
    let replay = harness
        .set(&id, "ascii-stable", 1, "ascii/path", &json!(7))
        .await?
        .json()?;
    assert_eq!(replay["replayed"], true);
    assert_eq!(
        replay["data_version"], 2,
        "historical receipt survives a newer head"
    );
    let mismatch = harness
        .set(&id, "unicode-head", 2, "e\u{301}", &json!(8))
        .await?;
    assert!(
        mismatch.is(StatusCode::UNPROCESSABLE_ENTITY, "request_key_reused"),
        "{}",
        mismatch.text
    );
    let noop = harness
        .set(&id, "unicode-noop", 3, "한글/값", &json!(8))
        .await?
        .json()?;
    assert_eq!(noop["changed"], false);
    assert_eq!(noop["data_version"], 3);
    accepted(
        &harness
            .update(&id, "unicode-update", 3, "한글", &json!({"추가": 9}))
            .await?,
    )?;
    assert_eq!(harness.at(&id, "한글/추가").await?["value"], 9);
    accepted(&harness.remove(&id, "unicode-delete", 4, "é").await?)?;
    assert_eq!(harness.at(&id, "é").await?["exists"], false);
    assert_eq!(harness.at(&id, "e\u{301}").await?["value"], 2);
    let removed = harness
        .members(&id, &format!("?limit=100&page_token={token}"))
        .await?;
    assert_eq!(listed(&removed)?.0, strings(&["한글", "𝄞"]));
    Ok(())
}

#[tokio::test]
async fn data_queries_reject_malformed_bytes_and_decode_only_once() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("strict-query").await?;
    accepted(
        &harness
            .set(
                &id,
                "strict-root",
                0,
                "",
                &json!({"a": {"b": 1}, "한": {"글": 2}}),
            )
            .await?,
    )?;
    for query in [
        "?path=%",
        "?path=%2",
        "?path=%GG",
        "?path=%FF",
        "?path=%C0%AF",
        "?path=%ED%A0%80",
        "?path=%F4%90%80%80",
        "?path=%E2%28%A1",
        "?path=a&path=b",
        "?unknown=%FF",
    ] {
        for reply in [
            harness.read(&id, query).await?,
            harness.members(&id, query).await?,
        ] {
            assert!(
                reply.is(StatusCode::BAD_REQUEST, "invalid_request"),
                "{query}: {}",
                reply.text
            );
            assert!(
                !reply.text.contains(query),
                "query contents must stay confidential"
            );
        }
    }
    for query in ["?path=a%252Fb", "?path=a+b", "?path=a%2Bb"] {
        assert!(
            harness
                .read(&id, query)
                .await?
                .is(StatusCode::UNPROCESSABLE_ENTITY, "invalid_request"),
            "{query}"
        );
        assert!(
            harness
                .members(&id, query)
                .await?
                .is(StatusCode::UNPROCESSABLE_ENTITY, "invalid_request"),
            "{query}"
        );
    }
    assert_eq!(harness.read(&id, "?path=a%2Fb").await?.json()?["value"], 1);
    assert_eq!(
        harness
            .read(&id, "?path=%ED%95%9C%2F%EA%B8%80")
            .await?
            .json()?["value"],
        2
    );
    assert!(
        harness
            .set(&id, "no-json-decode", 1, "a%2Fb", &json!(8))
            .await?
            .is(StatusCode::UNPROCESSABLE_ENTITY, "invalid_request")
    );
    Ok(())
}

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
        "bad\u{200d}name",
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
        "?path=bad%E2%80%8Dname",
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

/// `i64::MIN` once overflowed `abs` on the request task; every number outside
/// +-(2^53 - 1) is refused in whatever spelling, and the bounds are accepted.
#[tokio::test]
async fn numbers_outside_the_exact_range_are_refused_in_every_spelling() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    let uri = format!("/v1/artifacts/{id}/data");
    let send = |method: &'static str, field: &'static str, number: String, key: String| {
        let (harness, uri) = (&harness, uri.clone());
        async move {
            harness
                .send_text(
                    method,
                    &uri,
                    Some(OWNER),
                    Some(&key),
                    Some(format!(
                        r#"{{"path":"n","base_data_version":0,"{field}":{number}}}"#
                    )),
                )
                .await
        }
    };
    let refused = [
        "-9223372036854775808",
        "9223372036854775807",
        "-9223372036854775809",
        "18446744073709551615",
        "18446744073709551616",
        "9007199254740992",
        "-9007199254740992",
        "9007199254740993.0",
        "9.007199254740993e15",
        "1e16",
        "-1e16",
        "1e20",
        "-1.5e300",
        "1e999",
    ];
    for (index, number) in refused.into_iter().enumerate() {
        let wrapped = [
            ("PUT", "value", number.to_owned()),
            ("PUT", "value", format!("[{number}]")),
            ("PATCH", "patch", format!(r#"{{"x":{number}}}"#)),
        ];
        for (call, (method, field, text)) in wrapped.into_iter().enumerate() {
            let reply = send(method, field, text, format!("num-{index}-{call}")).await?;
            assert!(
                reply.is(StatusCode::UNPROCESSABLE_ENTITY, "invalid_request"),
                "{method} {number}: {} {}",
                reply.status,
                reply.text
            );
        }
    }
    assert_eq!(
        harness.at(&id, "").await?["data_version"],
        0,
        "nothing written"
    );

    // The bounds are accepted, as integers or as whole floats; 1e999 is not,
    // while a fraction too small for a double reads as 0.
    for (index, (number, stored)) in [
        ("9007199254740991", "9007199254740991"),
        ("-9007199254740991", "-9007199254740991"),
        ("9007199254740991.0", "9007199254740991"),
        ("1.5e3", "1500"),
        ("0.1e-400", "0"),
    ]
    .into_iter()
    .enumerate()
    {
        let uri = format!("/v1/artifacts/{id}/data");
        let reply = harness
            .send_text(
                "PUT",
                &uri,
                Some(OWNER),
                Some(&format!("num-ok-{index}")),
                Some(format!(
                    r#"{{"path":"n","base_data_version":{index},"value":{number}}}"#
                )),
            )
            .await?;
        assert_eq!(reply.status, StatusCode::OK, "{number}: {}", reply.text);
        let read = harness.read(&id, "?path=n").await?;
        assert!(
            read.text.contains(&format!(r#""value":{stored}"#)),
            "{number}: {}",
            read.text
        );
    }
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
        // Text that is not JSON inside the body is a malformed body (400), as
        // `invalid_request` is over MCP for a value given as text.
        (
            "malformed value",
            r#"{"path":"a","base_data_version":0,"value":[1,}"#.to_owned(),
        ),
        (
            "unterminated value",
            r#"{"path":"a","base_data_version":0,"value":{"a":"#.to_owned(),
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
async fn a_delete_whose_body_was_stripped_is_refused_and_deletes_nothing() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    let uri = format!("/v1/artifacts/{id}/data");
    accepted(
        &harness
            .set(&id, "stripped-set", 0, "keep", &json!(1))
            .await?,
    )?;
    // A proxy that strips the body of a DELETE leaves a request with no body,
    // with or without its content type: refused as a malformed body, and
    // nothing is deleted.
    let kept = harness.at(&id, "").await?["data_version"].clone();
    for content_type_and_empty_body in [None, Some(String::new())] {
        let reply = harness
            .send_text(
                "DELETE",
                &uri,
                Some(OWNER),
                Some("stripped-1"),
                content_type_and_empty_body,
            )
            .await?;
        assert!(
            reply.is(StatusCode::BAD_REQUEST, "invalid_request"),
            "{}",
            reply.text
        );
    }
    assert_eq!(harness.at(&id, "").await?["data_version"], kept);
    assert_eq!(
        harness.at(&id, "keep").await?["value"],
        1,
        "nothing was deleted"
    );
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

// --- listing the members of an object (decision 0040, rules 7-11) -------------

impl Harness {
    /// Lists members; `query` is sent as written, so a test can pass
    /// percent-encoded text, unknown parameters, or repeated ones.
    async fn members(&self, id: &str, query: &str) -> Result<Reply, Box<dyn std::error::Error>> {
        self.owner(
            "GET",
            &format!("/v1/artifacts/{id}/data/members{query}"),
            None,
            None,
        )
        .await
    }

    /// Writes `value` at the root, counting on `base` being the version.
    async fn fill(&self, id: &str, key: &str, base: u64, value: &Value) -> TestResult {
        accepted(&self.set(id, key, base, "", value).await?)?;
        Ok(())
    }
}

/// The names of a page that must have succeeded, and its token.
fn listed(reply: &Reply) -> Result<(Vec<String>, Option<String>), Box<dyn std::error::Error>> {
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let body = reply.json()?;
    let names = body["names"]
        .as_array()
        .ok_or("names")?
        .iter()
        .map(|name| name.as_str().map(str::to_owned).ok_or("a name"))
        .collect::<Result<Vec<_>, _>>()?;
    let token = match body.get("next_page_token") {
        None => None,
        Some(token) => Some(token.as_str().ok_or("a token is text")?.to_owned()),
    };
    Ok((names, token))
}

fn strings(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

#[tokio::test]
async fn names_are_ordered_by_utf8_bytes_not_case_or_locale() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    harness
        .fill(
            &id,
            "order-1",
            0,
            &json!({ "a": 1, "_x": 1, "B": 1, "0": 1, "-y": 1 }),
        )
        .await?;
    let reply = harness.members(&id, "").await?;
    assert_eq!(
        listed(&reply)?,
        (strings(&["-y", "0", "B", "_x", "a"]), None)
    );
    assert_eq!(
        reply.json()?,
        json!({
            "artifact_id": id, "path": "", "data_version": 1, "exists": true,
            "names": ["-y", "0", "B", "_x", "a"]
        }),
        "no token member when nothing follows, and no values"
    );
    Ok(())
}

#[tokio::test]
async fn pages_follow_each_other_and_a_full_last_page_has_no_token() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    let seven: Value = (1..=7).map(|n| (format!("n{n}"), json!(n))).collect();
    harness.fill(&id, "seven-1", 0, &seven).await?;
    let mut token = None;
    let mut pages = Vec::new();
    loop {
        let query = match &token {
            Some(token) => format!("?limit=3&page_token={token}"),
            None => "?limit=3".to_owned(),
        };
        let (names, next) = listed(&harness.members(&id, &query).await?)?;
        pages.push(names);
        token = next;
        if token.is_none() {
            break;
        }
    }
    assert_eq!(
        pages,
        vec![
            strings(&["n1", "n2", "n3"]),
            strings(&["n4", "n5", "n6"]),
            strings(&["n7"])
        ]
    );
    // Six names: the second page is full, and nothing follows it.
    harness
        .fill(
            &id,
            "six-1",
            1,
            &json!({ "a": 1, "b": 1, "c": 1, "d": 1, "e": 1, "f": 1 }),
        )
        .await?;
    let (first, token) = listed(&harness.members(&id, "?limit=3").await?)?;
    assert_eq!(first, strings(&["a", "b", "c"]));
    let token = token.ok_or("a token after the first page")?;
    let second = harness
        .members(&id, &format!("?limit=3&page_token={token}"))
        .await?;
    assert_eq!(listed(&second)?, (strings(&["d", "e", "f"]), None));
    assert!(
        second.json()?.get("next_page_token").is_none(),
        "{}",
        second.text
    );
    Ok(())
}

#[tokio::test]
async fn names_outside_the_path_grammar_are_skipped_and_never_counted() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    let long = "y".repeat(65);
    harness
        .fill(
            &id,
            "skip-1",
            0,
            &json!({ "a": 1, "a b": 1, "b": 1, "\u{fe0f}": 1, long: 1 }),
        )
        .await?;
    // `GET data` still returns the skipped names.
    let read = harness.at(&id, "").await?;
    assert!(read["value"].get("a b").is_some(), "{read}");
    assert_eq!(
        listed(&harness.members(&id, "?limit=2").await?)?,
        (strings(&["a", "b"]), None),
        "two listable names fill the page; the names after `b` are not listable"
    );
    let (first, token) = listed(&harness.members(&id, "?limit=1").await?)?;
    assert_eq!(first, strings(&["a"]));
    let token = token.ok_or("`b` follows `a`, past the skipped `a b`")?;
    assert_eq!(
        listed(
            &harness
                .members(&id, &format!("?limit=1&page_token={token}"))
                .await?
        )?,
        (strings(&["b"]), None),
        "paging passes the skipped name, and nothing listable follows `b`"
    );
    // Only skipped names: an empty page, no token.
    harness
        .fill(&id, "skip-2", 1, &json!({ "a b": 1, "\u{fe0f}": 1 }))
        .await?;
    assert_eq!(
        listed(&harness.members(&id, "").await?)?,
        (Vec::new(), None)
    );
    Ok(())
}

#[tokio::test]
async fn the_default_page_is_fifty_names_and_a_token() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    let sixty: Value = (0..60).map(|n| (format!("n{n:02}"), json!(n))).collect();
    harness.fill(&id, "sixty-1", 0, &sixty).await?;
    let (names, token) = listed(&harness.members(&id, "").await?)?;
    assert_eq!(names.len(), 50);
    assert_eq!(names.last().map(String::as_str), Some("n49"));
    let token = token.ok_or("a token: ten names follow")?;
    let (rest, token) = listed(
        &harness
            .members(&id, &format!("?page_token={token}"))
            .await?,
    )?;
    assert_eq!((rest.len(), token), (10, None));
    // The largest page.
    let (hundred, _) = listed(&harness.members(&id, "?limit=100").await?)?;
    assert_eq!(hundred.len(), 60);
    Ok(())
}

#[tokio::test]
async fn a_path_lists_the_object_there_and_a_missing_one_does_not_exist() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    harness
        .fill(
            &id,
            "paths-1",
            0,
            &json!({ "o": { "k": 1, "j": { "x": 1 } }, "s": "text", "l": [1], "n": null, "e": {} }),
        )
        .await?;
    let at = |path: &str| format!("?path={}", path.replace('/', "%2F"));
    let listed_at = async |path: &str| -> Result<Value, Box<dyn std::error::Error>> {
        let reply = harness.members(&id, &at(path)).await?;
        assert_eq!(reply.status, StatusCode::OK, "{path}: {}", reply.text);
        Ok(reply.json()?)
    };
    assert_eq!(
        listed_at("o").await?,
        json!({ "artifact_id": id, "path": "o", "data_version": 1, "exists": true,
                "names": ["j", "k"] })
    );
    assert_eq!(listed_at("o/j").await?["names"], json!(["x"]));
    assert_eq!(listed_at("e").await?["names"], json!([]));
    assert_eq!(
        listed_at("e").await?["exists"],
        true,
        "an empty object exists"
    );
    for missing in ["nothing", "nothing/deeper", "s/deeper", "l/0", "n/x"] {
        assert_eq!(
            listed_at(missing).await?,
            json!({ "artifact_id": id, "path": missing, "data_version": 1,
                    "exists": false, "names": [] }),
            "{missing}"
        );
    }
    for blocked in ["s", "l", "n"] {
        let reply = harness.members(&id, &at(blocked)).await?;
        assert!(
            reply.is(StatusCode::CONFLICT, "data_path_conflict"),
            "{blocked}: {}",
            reply.text
        );
        // The listing changes nothing, so its message must not describe a write.
        assert_eq!(
            reply.json()?["error"]["message"],
            "the value at this path is not an object, so it has no members",
            "{blocked}"
        );
    }
    // A write through the same value keeps its own message, and `GET data`
    // reads a path through it as nothing.
    let write = harness
        .set(&id, "paths-2", 1, "s/deeper", &json!(1))
        .await?;
    assert!(
        write.is(StatusCode::CONFLICT, "data_path_conflict"),
        "{}",
        write.text
    );
    assert_eq!(
        write.json()?["error"]["message"],
        "the path crosses a value that is not an object, which a write cannot make room in"
    );
    assert_eq!(harness.at(&id, "s/deeper").await?["exists"], false);
    // An empty `path` is the root, as for `GET data`.
    assert_eq!(listed_at("").await?["path"], "");
    let explicit = harness.members(&id, "?path=").await?;
    assert_eq!(listed(&explicit)?.0, strings(&["e", "l", "n", "o", "s"]));
    Ok(())
}

#[tokio::test]
async fn a_path_that_breaks_the_grammar_is_refused_without_being_echoed() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    for path in [
        "a%2F%2Fb",
        "%2Fa",
        "a%2F",
        "a%20b",
        "%EF%B8%8F",
        ".",
        "a%2F..%2Fb",
    ] {
        let reply = harness.members(&id, &format!("?path={path}")).await?;
        assert!(
            reply.is(StatusCode::UNPROCESSABLE_ENTITY, "invalid_request"),
            "{path}: {}",
            reply.text
        );
    }
    let deep = vec!["a"; 17].join("%2F");
    let reply = harness.members(&id, &format!("?path={deep}")).await?;
    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "17 segments"
    );
    let reply = harness.members(&id, "?path=a%20b").await?;
    assert!(!reply.text.contains("a b"), "{}", reply.text);
    Ok(())
}

#[tokio::test]
async fn at_data_version_names_the_version_the_page_is_read_at() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    // A space nobody wrote is at 0.
    let unwritten = harness.members(&id, "?at_data_version=0").await?;
    assert_eq!(listed(&unwritten)?, (Vec::new(), None));
    assert_eq!(unwritten.json()?["data_version"], 0);
    let stale = harness.members(&id, "?at_data_version=1").await?;
    assert!(
        stale.is(StatusCode::CONFLICT, "data_conflict"),
        "{}",
        stale.text
    );
    assert_eq!(stale.json()?["error"]["current_data_version"], 0);

    harness
        .fill(&id, "ver-1", 0, &json!({ "a": 1, "b": 1, "c": 1 }))
        .await?;
    let (first, token) = listed(&harness.members(&id, "?limit=1&at_data_version=1").await?)?;
    assert_eq!(first, strings(&["a"]));
    let token = token.ok_or("a token")?;
    let next = |version: u64| format!("?limit=1&page_token={token}&at_data_version={version}");
    assert_eq!(
        listed(&harness.members(&id, &next(1)).await?)?.0,
        strings(&["b"]),
        "equal"
    );
    // Newer: the reader names a version the space has not reached.
    let newer = harness.members(&id, &next(2)).await?;
    assert!(
        newer.is(StatusCode::CONFLICT, "data_conflict"),
        "{}",
        newer.text
    );
    assert_eq!(newer.json()?["error"]["current_data_version"], 1);
    // Older: a write moved the space on; the earlier token with the earlier
    // version is a conflict that names where the space is now.
    accepted(&harness.set(&id, "ver-2", 1, "d", &json!(1)).await?)?;
    let older = harness.members(&id, &next(1)).await?;
    assert!(
        older.is(StatusCode::CONFLICT, "data_conflict"),
        "{}",
        older.text
    );
    assert_eq!(older.json()?["error"]["current_data_version"], 2);
    // Without it the page is read at the version the space has now.
    let without = harness
        .members(&id, &format!("?limit=1&page_token={token}"))
        .await?;
    assert_eq!(listed(&without)?.0, strings(&["b"]));
    assert_eq!(without.json()?["data_version"], 2);
    Ok(())
}

#[tokio::test]
async fn a_query_that_breaks_the_rules_is_refused() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    let token = {
        harness
            .fill(&id, "q-1", 0, &json!({ "a": 1, "b": 1 }))
            .await?;
        listed(&harness.members(&id, "?limit=1").await?)?
            .1
            .ok_or("a token")?
    };
    for query in [
        "?limit=0".to_owned(),
        "?limit=101".to_owned(),
        "?limit=05".to_owned(),
        "?limit=%2B5".to_owned(),
        "?limit=-1".to_owned(),
        "?limit=".to_owned(),
        "?limit=1.0".to_owned(),
        "?limit=1e1".to_owned(),
        "?limit=%205".to_owned(),
        "?limit=five".to_owned(),
        "?limit=99999999999999999999".to_owned(),
        "?at_data_version=05".to_owned(),
        "?at_data_version=%2B1".to_owned(),
        "?at_data_version=-1".to_owned(),
        "?at_data_version=".to_owned(),
        "?at_data_version=9007199254740992".to_owned(),
        "?at_data_version=99999999999999999999".to_owned(),
        "?sort=name".to_owned(),
        "?prefix=a".to_owned(),
        "?Limit=5".to_owned(),
        "?limit=1&limit=1".to_owned(),
        "?path=&path=".to_owned(),
        format!("?page_token={token}&page_token={token}"),
        "?at_data_version=1&at_data_version=1".to_owned(),
    ] {
        let reply = harness.members(&id, &query).await?;
        assert!(
            reply.is(StatusCode::BAD_REQUEST, "invalid_request"),
            "{query}: {} {}",
            reply.status,
            reply.text
        );
    }
    // The bounds themselves, and the largest version, are accepted (the
    // version is stale, which is a conflict and not a malformed query).
    for query in ["?limit=1", "?limit=100", "?at_data_version=1"] {
        let reply = harness.members(&id, query).await?;
        assert_eq!(reply.status, StatusCode::OK, "{query}: {}", reply.text);
    }
    let largest = harness
        .members(&id, "?at_data_version=9007199254740991")
        .await?;
    assert!(
        largest.is(StatusCode::CONFLICT, "data_conflict"),
        "{}",
        largest.text
    );
    let zero = harness.members(&id, "?at_data_version=0").await?;
    assert!(
        zero.is(StatusCode::CONFLICT, "data_conflict"),
        "0 is a version: {}",
        zero.text
    );
    Ok(())
}

/// The bytes of a page token as the server issued it.
fn token_bytes(token: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    Ok(URL_SAFE_NO_PAD.decode(token)?)
}

#[tokio::test]
async fn a_token_is_a_tag_and_the_last_name_in_clear() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    // The deepest path (16 segments of 64 bytes) with a 64-byte name.
    let segments: Vec<String> = (0..16).map(|n| format!("{n:x}").repeat(64)).collect();
    let deepest = segments.join("/");
    let last = "y".repeat(64);
    let other = "x".repeat(64);
    accepted(
        &harness
            .set(
                &id,
                "deep-1",
                0,
                &deepest,
                &json!({ last.clone(): 1, other.clone(): 1, "z": 1 }),
            )
            .await?,
    )?;
    let query = format!("?limit=2&path={}", deepest.replace('/', "%2F"));
    let reply = harness.members(&id, &query).await?;
    let (names, token) = listed(&reply)?;
    assert_eq!(names, vec![other.clone(), last.clone()]);
    let token = token.ok_or("`z` follows the 64-byte names")?;
    assert_eq!(token.len(), 107, "80 bytes in base64 without padding");
    let bytes = token_bytes(&token)?;
    assert_eq!(bytes.len(), 80);
    assert_eq!(
        &bytes[16..],
        last.as_bytes(),
        "the name in clear after the tag"
    );
    // The token continues the listing after its name.
    let after = harness
        .members(&id, &format!("{query}&page_token={token}"))
        .await?;
    assert_eq!(listed(&after)?, (strings(&["z"]), None));
    // The tag depends on the artifact and the path, and not on the name.
    let (_, shorter) = listed(
        &harness
            .members(
                &id,
                &format!("?limit=1&path={}", deepest.replace('/', "%2F")),
            )
            .await?,
    )?;
    let shorter = token_bytes(&shorter.ok_or("a token")?)?;
    assert_eq!(&shorter[16..], other.as_bytes());
    assert_eq!(
        shorter[..16],
        bytes[..16],
        "one tag for one artifact and path"
    );
    // The same names under the root and under another artifact: other tags.
    let root_token = {
        harness
            .fill(&id, "deep-2", 1, &json!({ "a": 1, "b": 1 }))
            .await?;
        token_bytes(
            &listed(&harness.members(&id, "?limit=1").await?)?
                .1
                .ok_or("a token")?,
        )?
    };
    assert_eq!(
        root_token.len(),
        17,
        "a one-byte name makes the shortest token"
    );
    assert_ne!(root_token[..16], bytes[..16], "other path");
    let second = harness.artifact("B").await?;
    harness
        .fill(&second, "deep-3", 0, &json!({ "a": 1, "b": 1 }))
        .await?;
    let foreign = token_bytes(
        &listed(&harness.members(&second, "?limit=1").await?)?
            .1
            .ok_or("a token")?,
    )?;
    assert_ne!(foreign[..16], root_token[..16], "other artifact, same path");
    Ok(())
}

#[tokio::test]
async fn a_page_token_that_was_not_issued_for_the_request_is_refused() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    let second = harness.artifact("B").await?;
    let space = json!({ "a": 1, "b": 1, "c": 1, "d": { "a": 1, "b": 1 } });
    harness.fill(&id, "tok-1", 0, &space).await?;
    harness.fill(&second, "tok-2", 0, &space).await?;
    let issued = |artifact: &str, path: &str| {
        let query = format!("?limit=1&path={}", path.replace('/', "%2F"));
        let harness = &harness;
        let artifact = artifact.to_owned();
        async move {
            listed(&harness.members(&artifact, &query).await?)?
                .1
                .ok_or_else(|| Box::<dyn std::error::Error>::from("a token"))
        }
    };
    let root_token = issued(&id, "").await?;
    let bytes = token_bytes(&root_token)?;
    let valid_tag = &bytes[..16];
    let with_name = |name: &[u8]| URL_SAFE_NO_PAD.encode([valid_tag, name].concat());
    let refusals = [
        ("empty", String::new()),
        ("not base64", "!!!!".to_owned()),
        ("standard alphabet", "+/+/+/+/+/+/+/+/+/+/+/+/".to_owned()),
        ("whitespace", format!("{root_token}%20")),
        ("padded", URL_SAFE.encode(&bytes).replace('=', "%3D")),
        ("too short", URL_SAFE_NO_PAD.encode(valid_tag)),
        ("bad name", with_name(b"a b")),
        ("oversized", "A".repeat(2_000)),
        ("108 characters", with_name(&[b'a'; 65])),
        ("default ignorable name", with_name("\u{fe0f}".as_bytes())),
        ("another path", issued(&id, "d").await?),
        ("another artifact", issued(&second, "").await?),
        ("root token under a path", root_token.clone()),
    ];
    for (case, token) in refusals {
        let path = if case == "root token under a path" {
            "?path=d&"
        } else {
            "?"
        };
        let reply = harness
            .members(&id, &format!("{path}page_token={token}"))
            .await?;
        assert!(
            reply.is(StatusCode::BAD_REQUEST, "invalid_request"),
            "{case}: {} {}",
            reply.status,
            reply.text
        );
        assert!(
            !reply.text.contains(&token) || token.is_empty(),
            "{case}: echoed"
        );
    }
    assert_eq!(with_name(&[b'a'; 65]).len(), 108, "the 108-character case");
    // The same token on its own artifact and path still works.
    let ok = harness
        .members(&id, &format!("?limit=1&page_token={root_token}"))
        .await?;
    assert_eq!(listed(&ok)?.0, strings(&["b"]));
    Ok(())
}

#[tokio::test]
async fn checks_run_in_the_order_of_the_rule() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    let unknown = "art_00000000000000000000000000000000";
    harness
        .fill(&id, "order-1", 0, &json!({ "s": "text", "a": 1, "b": 1 }))
        .await?;
    let token = listed(&harness.members(&id, "?limit=1").await?)?
        .1
        .ok_or("a token")?;
    let bad_token = "page_token=!!!!";
    let cases: [(&str, String, String, StatusCode, &str); 8] = [
        (
            "identifier before query",
            "art_nonsense".to_owned(),
            "?limit=0".to_owned(),
            StatusCode::NOT_FOUND,
            "not_found",
        ),
        (
            "query before path",
            id.clone(),
            "?limit=0&path=a%20b".to_owned(),
            StatusCode::BAD_REQUEST,
            "invalid_request",
        ),
        (
            "path before token",
            id.clone(),
            format!("?path=a%20b&{bad_token}"),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
        ),
        (
            "token before access",
            unknown.to_owned(),
            format!("?{bad_token}"),
            StatusCode::BAD_REQUEST,
            "invalid_request",
        ),
        (
            "foreign token before access",
            unknown.to_owned(),
            format!("?page_token={token}"),
            StatusCode::BAD_REQUEST,
            "invalid_request",
        ),
        (
            "access before the version",
            unknown.to_owned(),
            "?at_data_version=99".to_owned(),
            StatusCode::NOT_FOUND,
            "not_found",
        ),
        (
            "the version before the value at the path",
            id.clone(),
            "?path=s&at_data_version=99".to_owned(),
            StatusCode::CONFLICT,
            "data_conflict",
        ),
        (
            "the value at the path last",
            id.clone(),
            "?path=s&at_data_version=1".to_owned(),
            StatusCode::CONFLICT,
            "data_path_conflict",
        ),
    ];
    for (case, artifact, query, status, code) in cases {
        let reply = harness.members(&artifact, &query).await?;
        assert!(
            reply.is(status, code),
            "{case}: {} {}",
            reply.status,
            reply.text
        );
    }
    Ok(())
}

#[tokio::test]
async fn listing_needs_the_token_and_works_with_an_agent_token() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    harness.fill(&id, "auth-1", 0, &json!({ "a": 1 })).await?;
    let path = format!("/v1/artifacts/{id}/data/members");
    let wrong = "f".repeat(64);
    for token in [None, Some(wrong.as_str())] {
        let reply = harness.send("GET", &path, token, None, None).await?;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{token:?}");
    }
    let agent = harness.agent_token().await?;
    let reply = harness.send("GET", &path, Some(&agent), None, None).await?;
    assert_eq!(listed(&reply)?.0, strings(&["a"]));
    // Reading only: the route accepts no write.
    let post = harness
        .send(
            "POST",
            &path,
            Some(OWNER),
            Some("auth-key-1"),
            Some(&json!({})),
        )
        .await?;
    assert_eq!(post.status, StatusCode::METHOD_NOT_ALLOWED);
    Ok(())
}

#[tokio::test]
async fn the_members_of_an_artifact_in_the_trash_or_purged_are_not_found() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    harness
        .fill(&id, "trash-m1", 0, &json!({ "a": 1, "b": 1 }))
        .await?;
    let token = listed(&harness.members(&id, "?limit=1").await?)?
        .1
        .ok_or("a token")?;
    harness.trash(&id, "trash-m2", 1, true).await?;
    let gone = |reply: &Reply| reply.is(StatusCode::NOT_FOUND, "not_found");
    assert!(gone(&harness.members(&id, "").await?));
    assert!(
        gone(
            &harness
                .members(&id, &format!("?page_token={token}"))
                .await?
        ),
        "a token is a position and not a permission"
    );
    harness.trash(&id, "trash-m3", 2, false).await?;
    assert_eq!(
        listed(
            &harness
                .members(&id, &format!("?page_token={token}"))
                .await?
        )?
        .0,
        strings(&["b"])
    );
    harness.trash(&id, "trash-m4", 3, true).await?;
    let purged = harness
        .owner(
            "POST",
            &format!("/v1/artifacts/{id}/purge"),
            Some("purge-m1"),
            Some(&json!({ "base_metadata_revision": 4 })),
        )
        .await?;
    assert_eq!(purged.status, StatusCode::OK, "{}", purged.text);
    assert!(gone(&harness.members(&id, "").await?));
    Ok(())
}

/// Rule 19 of decision 0040 bounds a page at 7,991 bytes. The largest page
/// that exists: 100 names of 64 bytes at the deepest path, a token because
/// more follow, and the largest version a space can be at.
#[tokio::test]
async fn the_largest_page_is_within_the_published_bound() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("A").await?;
    let deepest = (0..16)
        .map(|_| "𝄞".repeat(16))
        .collect::<Vec<_>>()
        .join("/");
    let members: Value = (0..101)
        .map(|n| (format!("{n:03}{}_", "한".repeat(20)), json!(n)))
        .collect();
    accepted(&harness.set(&id, "big-1", 0, &deepest, &members).await?)?;
    // The version is raised in the file, because 2^53 - 1 writes do not fit
    // in a test.
    let connection = rusqlite::Connection::open(harness.directory.join("db.sqlite3"))?;
    let changed = connection.execute("UPDATE app_data SET data_version = 9007199254740991", [])?;
    assert_eq!(changed, 1);
    let encoded = encode_query_path(&deepest)?;
    assert_eq!(encoded.len(), 3_117);
    let reply = harness
        .members(&id, &format!("?limit=100&path={encoded}"))
        .await?;
    let (names, token) = listed(&reply)?;
    assert_eq!(names.len(), 100);
    assert!(names.iter().all(|name| name.len() == 64));
    assert_eq!(token.map(|token| token.len()), Some(107));
    assert_eq!(reply.json()?["data_version"], 9_007_199_254_740_991_u64);
    // The parts of the bound: 6,699 bytes of quoted names with commas, a
    // quoted path of 1,041, and a quoted token of 109.
    assert_eq!(reply.text.len(), 7_987, "{}", reply.text.len());
    assert!(reply.text.len() <= 7_991);
    Ok(())
}

/// The page size the schema publishes is the one the core enforces (decision
/// 0040, rule 8); the contract crate cannot read the core's constant, so the
/// generated schema is compared with it here.
#[test]
fn the_published_page_size_bounds_are_the_cores() -> TestResult {
    use artifree_core::members::MemberLimit;
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contracts/generated/api-v1.schema.json"
    ))?;
    let schema: Value = serde_json::from_str(&text)?;
    let limit = &schema["$defs"]["ReadDataMembersQuery"]["properties"]["limit"];
    assert_eq!(limit["minimum"], 1);
    assert_eq!(limit["maximum"], MemberLimit::MAX.get());
    assert!(MemberLimit::try_from(1).is_ok());
    assert!(MemberLimit::try_from(u64::try_from(MemberLimit::MAX.get())? + 1).is_err());
    Ok(())
}

#[tokio::test]
async fn outbox_binding_get_and_head_validate_input_and_authentication() -> TestResult {
    let harness = Harness::new()?;
    for (method, suffix, body, token, expected) in [
        ("GET", "", "", Some(OWNER), StatusCode::OK),
        ("HEAD", "", "", Some(OWNER), StatusCode::OK),
        ("GET", "?", "", Some(OWNER), StatusCode::BAD_REQUEST),
        (
            "GET",
            "?unexpected=1",
            "",
            Some(OWNER),
            StatusCode::BAD_REQUEST,
        ),
        ("HEAD", "?", "", Some(OWNER), StatusCode::BAD_REQUEST),
        ("GET", "", "x", Some(OWNER), StatusCode::BAD_REQUEST),
        ("HEAD", "", "x", Some(OWNER), StatusCode::BAD_REQUEST),
        ("GET", "", "", None, StatusCode::UNAUTHORIZED),
        ("HEAD", "?", "x", None, StatusCode::UNAUTHORIZED),
    ] {
        let mut request = Request::builder()
            .method(method)
            .uri(format!("/v1/data-outbox/binding{suffix}"));
        if let Some(token) = token {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let response = harness
            .api
            .clone()
            .oneshot(request.body(Body::from(body))?)
            .await?;
        assert_eq!(response.status(), expected, "{method} {suffix} {body}");
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let bytes = to_bytes(response.into_body(), 4096).await?;
        if method == "HEAD" {
            assert!(bytes.is_empty(), "HEAD has no body");
        } else if expected == StatusCode::OK {
            let binding: Value = serde_json::from_slice(&bytes)?;
            assert_eq!(binding.as_object().ok_or("binding object")?.len(), 3);
            assert_eq!(binding["actor_id"], "local");
            assert_eq!(binding["actor_generation"], "1");
            let epoch = binding["storage_epoch"].as_str().ok_or("epoch")?;
            assert_eq!(epoch.len(), 32);
            assert!(
                epoch
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            );
        } else {
            let error: Value = serde_json::from_slice(&bytes)?;
            assert_eq!(
                error["error"]["code"],
                if expected == StatusCode::UNAUTHORIZED {
                    "unauthenticated"
                } else {
                    "invalid_request"
                }
            );
        }
    }
    Ok(())
}

async fn fenced(
    harness: &Harness,
    method: &str,
    id: &str,
    key: &str,
    body: &Value,
    headers: &[(&str, &str)],
) -> Result<Reply, Box<dyn std::error::Error>> {
    raw_data_request(harness, method, id, Some(key), &body.to_string(), headers).await
}

async fn raw_data_request(
    harness: &Harness,
    method: &str,
    id: &str,
    key: Option<&str>,
    body: &str,
    headers: &[(&str, &str)],
) -> Result<Reply, Box<dyn std::error::Error>> {
    let mut request = Request::builder()
        .method(method)
        .uri(format!("/v1/artifacts/{id}/data"))
        .header(header::AUTHORIZATION, format!("Bearer {OWNER}"))
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(key) = key {
        request = request.header("idempotency-key", key);
    }
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = harness
        .api
        .clone()
        .oneshot(request.body(Body::from(body.to_owned()))?)
        .await?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 4096).await?;
    Ok(Reply {
        status,
        text: String::from_utf8(bytes.to_vec())?,
    })
}

#[tokio::test]
async fn outbox_fence_is_checked_before_replay_and_refusal_leaves_key_free() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("outbox").await?;
    let binding = harness
        .owner("GET", "/v1/data-outbox/binding", None, None)
        .await?
        .json()?;
    let epoch = binding["storage_epoch"].as_str().ok_or("epoch")?;
    let good = [
        ("artifree-outbox-actor", "local"),
        ("artifree-outbox-epoch", epoch),
        ("artifree-outbox-actor-generation", "1"),
    ];
    let mut stale = good;
    stale[2].1 = "2";
    let body = json!({"path":"item", "base_data_version":0, "value":1});
    let denied = fenced(&harness, "PUT", &id, "outbox-write-1", &body, &stale).await?;
    assert_eq!(denied.status, StatusCode::CONFLICT, "{}", denied.text);
    assert_eq!(denied.json()?["error"]["code"], "outbox_binding_changed");
    let committed = fenced(&harness, "PUT", &id, "outbox-write-1", &body, &good).await?;
    assert_eq!(committed.status, StatusCode::OK, "{}", committed.text);
    assert_eq!(committed.json()?["data_version"], 1);
    assert_eq!(committed.json()?["replayed"], false);
    let denied = fenced(&harness, "PUT", &id, "outbox-write-1", &body, &stale).await?;
    assert_eq!(denied.status, StatusCode::CONFLICT);
    assert_eq!(denied.json()?["error"]["code"], "outbox_binding_changed");
    let different = json!({"path":"item", "base_data_version":0, "value":2});
    let denied = fenced(&harness, "PUT", &id, "outbox-write-1", &different, &stale).await?;
    assert_eq!(denied.status, StatusCode::CONFLICT);
    assert_eq!(
        denied.code(),
        "outbox_binding_changed",
        "fence wins over key reuse"
    );
    let reused = fenced(&harness, "PUT", &id, "outbox-write-1", &different, &good).await?;
    assert_eq!(reused.code(), "request_key_reused");
    let replay = fenced(&harness, "PUT", &id, "outbox-write-1", &body, &good).await?;
    assert_eq!(replay.json()?["replayed"], true);
    assert_eq!(harness.at(&id, "item").await?["value"], 1);
    let missing = format!("art_{}", "0".repeat(32));
    for target in [&missing, &id] {
        if target == &id {
            let removed = harness
                .owner(
                    "PATCH",
                    &format!("/v1/artifacts/{id}"),
                    Some("trash-outbox"),
                    Some(&json!({"base_metadata_revision":1,"deleted":true})),
                )
                .await?;
            assert_eq!(removed.status, StatusCode::OK, "{}", removed.text);
        }
        let denied = fenced(&harness, "PUT", target, "outbox-write-1", &body, &stale).await?;
        assert_eq!(denied.status, StatusCode::NOT_FOUND, "{}", denied.text);
        assert_eq!(denied.json()?["error"]["code"], "not_found");
    }
    Ok(())
}

#[tokio::test]
async fn outbox_header_grammar_is_all_or_none_and_rejects_duplicates() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("syntax").await?;
    let body = json!({"path":"item","base_data_version":0,"value":1});
    let epoch = "0123456789abcdef0123456789abcdef";
    let good = [
        ("artifree-outbox-actor", "local"),
        ("artifree-outbox-epoch", epoch),
        ("artifree-outbox-actor-generation", "1"),
    ];
    let mut cases = vec![good[..1].to_vec(), good[..2].to_vec()];
    for (index, bad) in [
        (0, "owner"),
        (0, "u-ABC"),
        (1, "ABCDEF0123456789abcdef0123456789"),
        (1, "short"),
        (2, "0"),
        (2, "01"),
        (2, "+1"),
        (2, "-1"),
        (2, "9223372036854775808"),
    ] {
        let mut headers = good.to_vec();
        headers[index].1 = bad;
        cases.push(headers);
    }
    for duplicate in good {
        let mut headers = good.to_vec();
        headers.push(duplicate);
        cases.push(headers);
    }
    for (index, headers) in cases.iter().enumerate() {
        let rejected = fenced(
            &harness,
            "PUT",
            &id,
            &format!("outbox-syntax-{index}"),
            &body,
            headers,
        )
        .await?;
        assert_eq!(
            rejected.status,
            StatusCode::BAD_REQUEST,
            "{headers:?}: {}",
            rejected.text
        );
        assert_eq!(rejected.json()?["error"]["code"], "invalid_request");
    }
    assert_eq!(harness.at(&id, "").await?["data_version"], 0);
    Ok(())
}

#[tokio::test]
async fn outbox_guards_patch_and_delete_without_changing_direct_write_semantics() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("verbs").await?;
    let binding = harness
        .owner("GET", "/v1/data-outbox/binding", None, None)
        .await?
        .json()?;
    let epoch = binding["storage_epoch"].as_str().ok_or("epoch")?;
    let headers = [
        ("artifree-outbox-actor", "local"),
        ("artifree-outbox-epoch", epoch),
        ("artifree-outbox-actor-generation", "1"),
    ];
    for (index, method, body) in [
        (
            0,
            "PATCH",
            json!({"path":"", "base_data_version":0,"patch":{"item":1}}),
        ),
        (1, "DELETE", json!({"path":"item", "base_data_version":1})),
    ] {
        let mut stale = headers;
        stale[2].1 = "9223372036854775807";
        let name = format!("outbox-verbs-{index}");
        let denied = fenced(&harness, method, &id, &name, &body, &stale).await?;
        assert_eq!(denied.status, StatusCode::CONFLICT, "{}", denied.text);
        assert_eq!(denied.code(), "outbox_binding_changed");
        let written = fenced(&harness, method, &id, &name, &body, &headers).await?;
        assert_eq!(written.status, StatusCode::OK, "{}", written.text);
        assert_eq!(written.json()?["data_version"], index + 1);
        let replay = fenced(&harness, method, &id, &name, &body, &[]).await?;
        assert_eq!(replay.status, StatusCode::OK, "{}", replay.text);
        assert_eq!(replay.json()?["replayed"], true);
    }
    assert_eq!(harness.at(&id, "").await?["value"], json!({}));
    Ok(())
}

#[tokio::test]
async fn outbox_preflight_allows_the_three_fence_headers() -> TestResult {
    let harness = Harness::new()?;
    let response = harness
        .api
        .clone()
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/v1/data-outbox/binding")
                .header(header::ORIGIN, "http://127.0.0.1:5173")
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let allowed = response.headers()[header::ACCESS_CONTROL_ALLOW_HEADERS]
        .to_str()?
        .to_ascii_lowercase();
    for name in [
        "artifree-outbox-actor",
        "artifree-outbox-epoch",
        "artifree-outbox-actor-generation",
    ] {
        assert!(
            allowed.split(',').any(|value| value.trim() == name),
            "missing {name}: {allowed}"
        );
    }
    Ok(())
}

fn invalid_fences() -> [Vec<(&'static str, &'static str)>; 2] {
    [
        vec![("artifree-outbox-actor", "local")],
        vec![
            ("artifree-outbox-actor", "local"),
            ("artifree-outbox-epoch", "0123456789abcdef0123456789abcdef"),
            ("artifree-outbox-actor-generation", "01"),
        ],
    ]
}

#[tokio::test]
async fn outbox_ordinary_body_limit_wins_over_partial_or_malformed_fence() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("body-limit").await?;
    let oversized = " ".repeat(300_000);
    for method in ["PUT", "PATCH", "DELETE"] {
        let direct = raw_data_request(
            &harness,
            method,
            &id,
            Some("outbox-limit-key"),
            &oversized,
            &[],
        )
        .await?;
        assert_eq!(
            direct.status,
            StatusCode::PAYLOAD_TOO_LARGE,
            "{method}: {}",
            direct.text
        );
        assert_eq!(direct.code(), "payload_too_large");
        for headers in invalid_fences() {
            let rejected = raw_data_request(
                &harness,
                method,
                &id,
                Some("outbox-limit-key"),
                &oversized,
                &headers,
            )
            .await?;
            assert_eq!(
                rejected.status,
                StatusCode::PAYLOAD_TOO_LARGE,
                "{method}: {}",
                rejected.text
            );
            assert_eq!(
                rejected.json()?,
                direct.json()?,
                "ordinary body-limit response must be preserved"
            );
        }
    }
    assert_eq!(harness.at(&id, "").await?["data_version"], 0);
    Ok(())
}

#[tokio::test]
async fn outbox_ordinary_path_body_and_key_errors_win_over_invalid_fence() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("validation-order").await?;
    for method in ["PUT", "PATCH", "DELETE"] {
        for (target, key, body, status, code) in [
            (
                id.as_str(),
                Some("outbox-validation"),
                "{",
                StatusCode::BAD_REQUEST,
                "invalid_request",
            ),
            (
                "bad-id",
                Some("outbox-validation"),
                "{",
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                "%FF",
                Some("outbox-validation"),
                "{",
                StatusCode::BAD_REQUEST,
                "invalid_request",
            ),
            (
                id.as_str(),
                None,
                "{",
                StatusCode::BAD_REQUEST,
                "invalid_request",
            ),
            (
                id.as_str(),
                Some("short"),
                "{",
                StatusCode::BAD_REQUEST,
                "invalid_request",
            ),
        ] {
            let direct = raw_data_request(&harness, method, target, key, body, &[]).await?;
            assert_eq!(direct.status, status, "{method} {target}: {}", direct.text);
            assert_eq!(direct.code(), code);
            for headers in invalid_fences() {
                let rejected =
                    raw_data_request(&harness, method, target, key, body, &headers).await?;
                assert_eq!(
                    rejected.status, status,
                    "{method} {target}: {}",
                    rejected.text
                );
                assert_eq!(
                    rejected.json()?,
                    direct.json()?,
                    "ordinary input error must be preserved"
                );
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn outbox_validated_data_path_and_value_win_over_invalid_fence() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("data-validation-order").await?;
    for (method, invalid_path) in [
        (
            "PUT",
            json!({"path":"a//b","base_data_version":0,"value":1}),
        ),
        (
            "PATCH",
            json!({"path":"a//b","base_data_version":0,"patch":{}}),
        ),
        ("DELETE", json!({"path":"a//b","base_data_version":0})),
    ] {
        let body = invalid_path.to_string();
        let direct =
            raw_data_request(&harness, method, &id, Some("outbox-validation"), &body, &[]).await?;
        assert_eq!(
            direct.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            direct.text
        );
        assert_eq!(direct.code(), "invalid_request");
        for headers in invalid_fences() {
            let rejected = raw_data_request(
                &harness,
                method,
                &id,
                Some("outbox-validation"),
                &body,
                &headers,
            )
            .await?;
            assert_eq!(
                rejected.status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "{method}: {}",
                rejected.text
            );
            assert_eq!(rejected.json()?, direct.json()?);
        }
    }
    for method in ["PUT", "PATCH"] {
        let field = if method == "PUT" { "value" } else { "patch" };
        let mut body = json!({"path":"a","base_data_version":0});
        body[field] = json!("x".repeat(265_000));
        let body = body.to_string();
        let direct =
            raw_data_request(&harness, method, &id, Some("outbox-validation"), &body, &[]).await?;
        assert_eq!(
            direct.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            direct.text
        );
        assert_eq!(direct.code(), "data_too_large");
        for headers in invalid_fences() {
            let rejected = raw_data_request(
                &harness,
                method,
                &id,
                Some("outbox-validation"),
                &body,
                &headers,
            )
            .await?;
            assert_eq!(
                rejected.status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "{method}: {}",
                rejected.text
            );
            assert_eq!(rejected.json()?, direct.json()?);
        }
    }
    assert_eq!(harness.at(&id, "").await?["data_version"], 0);
    Ok(())
}
