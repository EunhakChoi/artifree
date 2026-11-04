//! `GET /v1/artifacts/{id}/data/members` (decision 0040) through the real
//! router, core, and `SQLite` store.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use artifree_contracts::api::CreateTokenResponse;
use artifree_providers::storage::SqliteStore;
use artifree_providers::tokens::SqliteTokenStore;
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

struct Reply {
    status: StatusCode,
    text: String,
}

impl Reply {
    fn json(&self) -> Result<Value, serde_json::Error> {
        serde_json::from_str(&self.text)
    }

    /// The body of an answer that must have succeeded.
    fn page(&self) -> Result<Value, Box<dyn std::error::Error>> {
        assert_eq!(self.status, StatusCode::OK, "{}", self.text);
        Ok(self.json()?)
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.directory);
    }
}

impl Harness {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!(
            "artifree-data-members-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _removed = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory)?;
        let database = directory.join("db.sqlite3");
        let state = AppState::new(
            SqliteStore::open(&database)?,
            SqliteTokenStore::open(&database)?,
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

    async fn owner(
        &self,
        method: &str,
        path: &str,
        key: Option<&str>,
        body: Option<&Value>,
    ) -> Result<Reply, Box<dyn std::error::Error>> {
        self.send(method, path, Some(OWNER), key, body).await
    }

    async fn artifact(&self) -> Result<String, Box<dyn std::error::Error>> {
        let created = self
            .owner(
                "POST",
                "/v1/artifacts",
                Some("create-members"),
                Some(&json!({
                    "title": "Members",
                    "files": [{ "path": "index.html", "encoding": "utf8", "content": "x" }]
                })),
            )
            .await?;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
        Ok(created.json()?["artifact"]["id"]
            .as_str()
            .ok_or("no artifact id")?
            .to_owned())
    }

    /// Replaces the whole space with `object`; returns the new version.
    async fn fill(
        &self,
        id: &str,
        base: u64,
        object: &Value,
    ) -> Result<u64, Box<dyn std::error::Error>> {
        self.set(id, base, "", object).await
    }

    /// Sets the value at `path`; returns the new version.
    async fn set(
        &self,
        id: &str,
        base: u64,
        path: &str,
        value: &Value,
    ) -> Result<u64, Box<dyn std::error::Error>> {
        let reply = self
            .owner(
                "PUT",
                &format!("/v1/artifacts/{id}/data"),
                Some(&format!("set-{id}-{base}-{path}")),
                Some(&json!({ "path": path, "base_data_version": base, "value": value })),
            )
            .await?;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
        Ok(reply.json()?["data_version"].as_u64().ok_or("no version")?)
    }

    async fn members(&self, id: &str, query: &str) -> Result<Reply, Box<dyn std::error::Error>> {
        self.owner(
            "GET",
            &format!("/v1/artifacts/{id}/data/members{query}"),
            None,
            None,
        )
        .await
    }
}

fn names(page: &Value) -> Vec<String> {
    page["members"]
        .as_array()
        .map(|members| {
            members
                .iter()
                .filter_map(|member| member["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn the_page_states_the_query_it_applied_and_the_version_it_read() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact().await?;
    let version = harness
        .fill(
            &id,
            0,
            &json!({ "todos": { "b": 2, "a": { "x": null }, "c": [1] } }),
        )
        .await?;

    let defaults = harness.members(&id, "?path=todos").await?.page()?;
    assert_eq!(
        defaults,
        json!({
            "artifact_id": id, "path": "todos", "select": "names", "limit": 50,
            "data_version": version, "exists": true, "collection": true,
            "members": [{ "name": "a" }, { "name": "b" }, { "name": "c" }]
        })
    );

    let values = harness
        .members(&id, "?path=todos&select=values&limit=2&after=a")
        .await?
        .page()?;
    assert_eq!(
        values,
        json!({
            "artifact_id": id, "path": "todos", "select": "values", "limit": 2,
            "after": "a", "data_version": version, "exists": true, "collection": true,
            "members": [{ "name": "b", "value": 2 }, { "name": "c", "value": [1] }]
        })
    );

    // The root is the empty path, with or without the parameter.
    for query in ["", "?path="] {
        let root = harness.members(&id, query).await?.page()?;
        assert_eq!(root["path"], "");
        assert_eq!(names(&root), ["todos"]);
    }
    Ok(())
}

#[tokio::test]
async fn a_stored_null_is_a_value_and_a_large_value_is_reported_not_sent() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact().await?;
    let exact = "x".repeat(32 * 1024 - 2);
    let over = "x".repeat(32 * 1024 - 1);
    harness
        .fill(&id, 0, &json!({ "a": exact, "b": over, "c": null }))
        .await?;

    let page = harness.members(&id, "?select=values").await?.page()?;
    let members = page["members"].as_array().ok_or("no members")?;
    assert_eq!(members[0], json!({ "name": "a", "value": exact }));
    assert_eq!(members[1], json!({ "name": "b", "too_large": true }));
    assert_eq!(members[2], json!({ "name": "c", "value": null }));
    // Each member has exactly one of the two, and names has neither.
    for member in members {
        assert!(
            member.get("value").is_some() != member.get("too_large").is_some(),
            "{member}"
        );
    }
    let names_only = harness.members(&id, "?select=names").await?.page()?;
    for member in names_only["members"].as_array().ok_or("no members")? {
        assert_eq!(
            member.as_object().map(serde_json::Map::len),
            Some(1),
            "{member}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn members_are_ordered_by_utf8_bytes_and_pages_chain_without_gaps() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact().await?;
    // U+FF5E (EF BD 9E) sorts before U+10000 (F0 90 80 80) by bytes and after
    // it by UTF-16 code units.
    harness
        .fill(
            &id,
            0,
            &json!({ "a": 1, "B": 2, "\u{ff5e}": 3, "\u{10000}": 4, "\u{e9}": 5 }),
        )
        .await?;
    let page = harness.members(&id, "").await?.page()?;
    assert_eq!(names(&page), ["B", "a", "\u{e9}", "\u{ff5e}", "\u{10000}"]);

    // 120 members in pages of 100 and 20, by the cursor each page returns.
    let big: serde_json::Map<String, Value> =
        (0..120).map(|n| (format!("m{n:03}"), json!(n))).collect();
    harness.fill(&id, 1, &Value::Object(big)).await?;
    let first = harness.members(&id, "?limit=100").await?.page()?;
    assert_eq!(names(&first).len(), 100);
    assert_eq!(first["next_after"], "m099");
    let second = harness
        .members(&id, "?limit=100&after=m099")
        .await?
        .page()?;
    assert_eq!(
        names(&second),
        (100..120).map(|n| format!("m{n:03}")).collect::<Vec<_>>()
    );
    assert!(second.get("next_after").is_none());
    Ok(())
}

#[tokio::test]
async fn a_page_of_values_stops_at_its_byte_bound() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact().await?;
    let big = "x".repeat(30_000);
    // One request holds at most 256 KiB, so the ten values come one by one.
    let mut version = 0;
    for n in 0..10 {
        version = harness
            .set(&id, version, &format!("m{n}"), &json!(big))
            .await?;
    }
    let first = harness
        .members(&id, "?select=values&limit=100")
        .await?
        .page()?;
    assert_eq!(names(&first).len(), 8);
    assert_eq!(first["next_after"], "m7");
    assert!(first.to_string().len() < 300 * 1024);
    let second = harness
        .members(&id, "?select=values&limit=100&after=m7")
        .await?
        .page()?;
    assert_eq!(names(&second), ["m8", "m9"]);
    Ok(())
}

#[tokio::test]
async fn a_path_that_is_not_an_object_has_no_members() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact().await?;
    harness
        .fill(&id, 0, &json!({ "text": "x", "list": [1, 2], "o": {} }))
        .await?;
    for (path, exists, collection) in [
        ("text", true, false),
        ("list", true, false),
        ("missing", false, false),
        ("text/deeper", false, false),
        ("o", true, true),
    ] {
        let page = harness
            .members(&id, &format!("?path={}", path.replace('/', "%2F")))
            .await?
            .page()?;
        assert_eq!(page["exists"], exists, "{path}");
        assert_eq!(page["collection"], collection, "{path}");
        assert_eq!(page["members"], json!([]), "{path}");
        assert!(page.get("next_after").is_none(), "{path}");
    }
    Ok(())
}

#[tokio::test]
async fn a_query_that_breaks_a_rule_is_refused_before_anything_is_read() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact().await?;
    let long_name = "n".repeat(257);
    let long_segment = "s".repeat(65);
    let many = vec!["a"; 17].join("%2F");
    let unprocessable = StatusCode::UNPROCESSABLE_ENTITY;
    let bad = StatusCode::BAD_REQUEST;
    let cases: Vec<(String, StatusCode)> = vec![
        ("?limit=0".into(), unprocessable),
        ("?limit=101".into(), unprocessable),
        ("?after=".into(), unprocessable),
        (format!("?after={long_name}"), unprocessable),
        ("?path=a%2F%2Fb".into(), unprocessable),
        ("?path=%2Fa".into(), unprocessable),
        (format!("?path={long_segment}"), unprocessable),
        (format!("?path={many}"), unprocessable),
        ("?limit=abc".into(), bad),
        ("?limit=-1".into(), bad),
        ("?limit=1.5".into(), bad),
        ("?select=keys".into(), bad),
        ("?unknown=1".into(), bad),
        ("?limit=1&limit=2".into(), bad),
        ("?path=a&path=b".into(), bad),
    ];
    for (query, status) in &cases {
        let reply = harness.members(&id, query).await?;
        assert_eq!(reply.status, *status, "{query}: {}", reply.text);
        assert_eq!(reply.json()?["error"]["code"], "invalid_request", "{query}");
    }
    // The edges are accepted.
    let edges = [
        "?limit=1".to_owned(),
        "?limit=100".to_owned(),
        format!("?after={}", "n".repeat(256)),
        format!("?path={}", vec!["a"; 16].join("%2F")),
    ];
    for query in &edges {
        let reply = harness.members(&id, query).await?;
        assert_eq!(reply.status, StatusCode::OK, "{query}: {}", reply.text);
    }
    Ok(())
}

#[tokio::test]
async fn the_data_version_is_not_a_source_revision() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact().await?;
    let version = harness.fill(&id, 0, &json!({ "a": 1 })).await?;
    let saved = harness
        .owner(
            "POST",
            &format!("/v1/artifacts/{id}/revisions"),
            Some("save-members"),
            Some(&json!({
                "base_revision": 1,
                "changes": [{ "op": "put", "path": "index.html", "encoding": "utf8",
                              "content": "y" }]
            })),
        )
        .await?;
    assert_eq!(saved.status, StatusCode::CREATED, "{}", saved.text);
    let page = harness.members(&id, "").await?.page()?;
    assert_eq!(page["data_version"], version);
    let keys: Vec<&String> = page.as_object().ok_or("no object")?.keys().collect();
    assert!(
        keys.iter().all(|key| !key.contains("revision")),
        "a source or metadata revision in the page: {keys:?}"
    );
    Ok(())
}

#[tokio::test]
async fn access_follows_reads_for_every_credential_and_for_the_trash() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact().await?;
    harness.fill(&id, 0, &json!({ "a": 1 })).await?;
    let uri = format!("/v1/artifacts/{id}/data/members");
    for token in [None, Some("f".repeat(64))] {
        let reply = harness
            .send("GET", &uri, token.as_deref(), None, None)
            .await?;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    }
    let agent = harness
        .owner(
            "POST",
            "/v1/tokens",
            None,
            Some(&json!({ "name": "Agent" })),
        )
        .await?;
    assert_eq!(agent.status, StatusCode::CREATED, "{}", agent.text);
    let secret = serde_json::from_str::<CreateTokenResponse>(&agent.text)?.secret;
    let listed = harness.send("GET", &uri, Some(&secret), None, None).await?;
    assert_eq!(names(&listed.page()?), ["a"]);

    let absent = format!("/v1/artifacts/art_{}/data/members", "0".repeat(32));
    let missing = harness.owner("GET", &absent, None, None).await?;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    let trashed = harness
        .owner(
            "PATCH",
            &format!("/v1/artifacts/{id}"),
            Some("trash-members"),
            Some(&json!({ "base_metadata_revision": 1, "deleted": true })),
        )
        .await?;
    assert_eq!(trashed.status, StatusCode::OK, "{}", trashed.text);
    let hidden = harness.owner("GET", &uri, None, None).await?;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND);
    assert_eq!(
        hidden.text.replace(&id, "ID"),
        missing
            .text
            .replace(&format!("art_{}", "0".repeat(32)), "ID"),
        "a trashed artifact answers otherwise than a missing one"
    );
    Ok(())
}
