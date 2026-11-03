//! `GET /v1/artifacts/{id}/data/members` (decision 0040) through the real
//! router, core, and `SQLite` store.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

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
            "artifree-data-members-{}-{}",
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

    async fn send(
        &self,
        method: &str,
        uri: &str,
        token: &str,
        key: Option<&str>,
        body: Option<Value>,
    ) -> Result<(StatusCode, Value), Box<dyn std::error::Error>> {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {token}"));
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
        Ok((
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        ))
    }

    async fn ok(
        &self,
        method: &str,
        uri: &str,
        key: Option<&str>,
        body: Option<Value>,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let (status, value) = self.send(method, uri, OWNER, key, body).await?;
        assert!(status.is_success(), "{method} {uri}: {status} {value}");
        Ok(value)
    }

    async fn artifact(&self, key: &str) -> Result<String, Box<dyn std::error::Error>> {
        let page = json!({ "title": "Members", "files": [
            { "path": "index.html", "encoding": "utf8", "content": "<p>1</p>" }] });
        let made = self
            .ok("POST", "/v1/artifacts", Some(key), Some(page))
            .await?;
        Ok(made["artifact"]["id"].as_str().ok_or("id")?.to_owned())
    }

    /// Sets the root of the artifact's space to `space`.
    async fn fill(&self, id: &str, space: &Value) -> Result<(), Box<dyn std::error::Error>> {
        self.ok(
            "PUT",
            &format!("/v1/artifacts/{id}/data"),
            Some(&format!("fill-{id}")),
            Some(json!({ "path": "", "base_data_version": 0, "value": space })),
        )
        .await?;
        Ok(())
    }

    async fn members(&self, id: &str, query: &str) -> Result<Value, Box<dyn std::error::Error>> {
        self.ok(
            "GET",
            &format!("/v1/artifacts/{id}/data/members{query}"),
            None,
            None,
        )
        .await
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn names(page: &Value) -> Vec<&str> {
    page["members"]
        .as_array()
        .map(|members| members.iter().filter_map(|m| m["name"].as_str()).collect())
        .unwrap_or_default()
}

#[tokio::test]
async fn the_default_page_lists_names_and_echoes_the_normalized_query() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("create-1").await?;
    harness
        .fill(
            &id,
            &json!({ "b": 2, "a": 1, "todos": { "y": true, "x": null } }),
        )
        .await?;
    let page = harness.members(&id, "").await?;
    assert_eq!(
        page,
        json!({
            "artifact_id": id, "path": "", "select": "names", "limit": 50,
            "data_version": 1, "is_collection": true,
            "members": [{ "name": "a" }, { "name": "b" }, { "name": "todos" }]
        })
    );
    let nested = harness
        .members(&id, "?path=todos&select=values&limit=7")
        .await?;
    assert_eq!(
        nested,
        json!({
            "artifact_id": id, "path": "todos", "select": "values", "limit": 7,
            "data_version": 1, "is_collection": true,
            "members": [{ "name": "x", "value": null }, { "name": "y", "value": true }]
        })
    );
    // Source revisions and data versions are separate fields: a page has no
    // source revision, and saving a revision does not move its data version.
    assert!(page.get("revision").is_none() && page.get("metadata_revision").is_none());
    harness
        .ok(
            "POST",
            &format!("/v1/artifacts/{id}/revisions"),
            Some("save-1"),
            Some(json!({ "base_revision": 1, "changes": [
                { "op": "put", "path": "index.html", "encoding": "utf8", "content": "<p>2</p>" }] })),
        )
        .await?;
    assert_eq!(harness.members(&id, "").await?["data_version"], 1);
    Ok(())
}

#[tokio::test]
async fn pages_chain_by_next_after_in_utf8_byte_order() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("create-2").await?;
    // U+10000 is a surrogate pair in UTF-16 and sorts last in UTF-8; U+FF5E
    // is one unit above it in UTF-16 and sorts before it in UTF-8.
    harness
        .fill(
            &id,
            &json!({ "\u{10000}": 1, "\u{ff5e}": 2, "a": 3, "B": 4, "é": 5 }),
        )
        .await?;
    let mut seen: Vec<String> = Vec::new();
    let mut after: Option<String> = None;
    let mut pages = 0;
    loop {
        let query = after.as_ref().map_or_else(
            || "?limit=2".to_owned(),
            |name| format!("?limit=2&after={}", percent(name)),
        );
        let page = harness.members(&id, &query).await?;
        assert_eq!(page["after"].as_str(), after.as_deref(), "{page}");
        seen.extend(names(&page).iter().map(|name| (*name).to_owned()));
        pages += 1;
        match page["next_after"].as_str() {
            Some(next) => {
                assert_eq!(Some(next), names(&page).last().copied());
                after = Some(next.to_owned());
            }
            None => break,
        }
    }
    assert_eq!(pages, 3);
    assert_eq!(seen, ["B", "a", "é", "\u{ff5e}", "\u{10000}"]);
    Ok(())
}

fn percent(text: &str) -> String {
    let mut encoded = String::new();
    for byte in text.bytes() {
        encoded.push('%');
        for nibble in [byte >> 4, byte & 0x0f] {
            encoded.push(
                char::from_digit(u32::from(nibble), 16)
                    .map_or('0', |digit| digit.to_ascii_uppercase()),
            );
        }
    }
    encoded
}

#[tokio::test]
async fn values_that_are_too_large_are_marked_and_pages_stay_within_their_bound() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("create-3").await?;
    let big = "x".repeat(33 * 1024);
    let medium = "m".repeat(30 * 1024);
    harness
        .fill(&id, &json!({ "big": big, "small": 1 }))
        .await?;
    for n in 0..9_u64 {
        harness
            .ok(
                "PUT",
                &format!("/v1/artifacts/{id}/data"),
                Some(&format!("medium-{id}-{n}")),
                Some(
                    json!({ "path": format!("m{n}"), "base_data_version": n + 1, "value": medium }),
                ),
            )
            .await?;
    }
    let page = harness.members(&id, "?select=values&limit=100").await?;
    let members = page["members"].as_array().ok_or("members")?;
    assert_eq!(members[0], json!({ "name": "big", "too_large": true }));
    for member in members {
        let value = member.get("value").is_some();
        let large = member.get("too_large").is_some();
        assert!(
            value ^ large,
            "exactly one of value and too_large: {member}"
        );
    }
    let carried: usize = members
        .iter()
        .filter_map(|member| member["value"].as_str())
        .map(|text| text.len() + 2)
        .sum();
    assert!(
        carried <= 256 * 1024,
        "{carried} bytes of values in one page"
    );
    let next = page["next_after"]
        .as_str()
        .ok_or("the page should end early")?;
    assert!(names(&page).len() < 11);
    // Names do not carry values, so one page lists them all.
    let all = harness.members(&id, "?limit=100").await?;
    assert_eq!(names(&all).len(), 11);
    assert!(all.get("next_after").is_none());
    assert!(all["members"][0].get("too_large").is_none());
    let rest = harness
        .members(
            &id,
            &format!("?select=values&limit=100&after={}", percent(next)),
        )
        .await?;
    assert!(!names(&rest).is_empty());
    Ok(())
}

#[tokio::test]
async fn a_path_without_an_object_is_an_empty_page_not_an_error() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("create-4").await?;
    harness
        .fill(&id, &json!({ "list": [1, 2], "text": "x", "empty": {} }))
        .await?;
    for (path, collection) in [
        ("nothing", false),
        ("list", false),
        ("text", false),
        ("empty", true),
    ] {
        let page = harness
            .members(&id, &format!("?path={path}&select=values"))
            .await?;
        assert_eq!(page["is_collection"], collection, "{path}: {page}");
        assert_eq!(page["members"], json!([]), "{path}");
        assert!(page.get("next_after").is_none());
    }
    // A space nobody wrote is the empty root at version 0.
    let fresh = harness.artifact("create-5").await?;
    let page = harness.members(&fresh, "").await?;
    assert_eq!(
        (page["data_version"].clone(), page["is_collection"].clone()),
        (json!(0), json!(true))
    );
    Ok(())
}

#[tokio::test]
async fn parameters_out_of_range_are_refused_with_their_rule() -> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("create-6").await?;
    let long = "a".repeat(257);
    let two_byte = "%C3%A9".repeat(129);
    let cases: Vec<(String, StatusCode)> = vec![
        ("?limit=0".into(), StatusCode::UNPROCESSABLE_ENTITY),
        ("?limit=101".into(), StatusCode::UNPROCESSABLE_ENTITY),
        ("?limit=-1".into(), StatusCode::BAD_REQUEST),
        ("?limit=ten".into(), StatusCode::BAD_REQUEST),
        ("?limit=".into(), StatusCode::BAD_REQUEST),
        ("?select=keys".into(), StatusCode::BAD_REQUEST),
        ("?after=".into(), StatusCode::UNPROCESSABLE_ENTITY),
        (format!("?after={long}"), StatusCode::UNPROCESSABLE_ENTITY),
        (
            format!("?after={two_byte}"),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        ("?path=/a".into(), StatusCode::UNPROCESSABLE_ENTITY),
        ("?path=a/".into(), StatusCode::UNPROCESSABLE_ENTITY),
        ("?path=a//b".into(), StatusCode::UNPROCESSABLE_ENTITY),
        ("?path=a%20b".into(), StatusCode::UNPROCESSABLE_ENTITY),
        (
            format!("?path={}", "a".repeat(65)),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            format!("?path={}", ["s"; 17].join("/")),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        ("?bogus=1".into(), StatusCode::BAD_REQUEST),
        ("?limit=1&limit=2".into(), StatusCode::BAD_REQUEST),
    ];
    for (query, expected) in cases {
        let (status, body) = harness
            .send(
                "GET",
                &format!("/v1/artifacts/{id}/data/members{query}"),
                OWNER,
                None,
                None,
            )
            .await?;
        assert_eq!(status, expected, "{query}: {body}");
        assert!(body["error"]["message"].is_string(), "{query}: {body}");
    }
    // The bounds themselves are accepted.
    for query in [
        "?limit=1",
        "?limit=100",
        &format!("?after={}", "a".repeat(256)),
        &format!("?path={}", ["s"; 16].join("/")),
    ] {
        harness.members(&id, query).await?;
    }
    Ok(())
}

#[tokio::test]
async fn missing_foreign_and_trashed_artifacts_are_the_same_404_and_a_token_is_required()
-> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("create-7").await?;
    harness.fill(&id, &json!({ "a": 1 })).await?;
    let user = harness
        .ok("POST", "/v1/users", None, Some(json!({ "label": "eve" })))
        .await?;
    let issued = harness
        .ok(
            "POST",
            "/v1/tokens",
            Some("issue-eve-1"),
            Some(json!({ "name": "eve", "user_id": user["id"] })),
        )
        .await?;
    let eve = issued["secret"].as_str().ok_or("secret")?;
    let missing = "art_00000000000000000000000000000000";
    let mut answers = Vec::new();
    for (target, token) in [(id.as_str(), eve), (missing, eve), (missing, OWNER)] {
        let (status, body) = harness
            .send(
                "GET",
                &format!("/v1/artifacts/{target}/data/members"),
                token,
                None,
                None,
            )
            .await?;
        assert_eq!(status, StatusCode::NOT_FOUND);
        answers.push(body);
    }
    assert!(
        answers.windows(2).all(|pair| pair[0] == pair[1]),
        "{answers:?}"
    );
    harness
        .ok(
            "PATCH",
            &format!("/v1/artifacts/{id}"),
            Some("trash-1"),
            Some(json!({ "base_metadata_revision": 1, "deleted": true })),
        )
        .await?;
    let (status, body) = harness
        .send(
            "GET",
            &format!("/v1/artifacts/{id}/data/members"),
            OWNER,
            None,
            None,
        )
        .await?;
    assert_eq!((status, &body), (StatusCode::NOT_FOUND, &answers[0]));
    let response = harness
        .api
        .clone()
        .oneshot(Request::get(format!("/v1/artifacts/{id}/data/members")).body(Body::empty())?)
        .await?;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn an_agent_token_of_the_owner_reads_members_and_a_user_reads_only_its_own_space()
-> TestResult {
    let harness = Harness::new()?;
    let id = harness.artifact("create-8").await?;
    harness.fill(&id, &json!({ "a": 1 })).await?;
    let agent = harness
        .ok(
            "POST",
            "/v1/tokens",
            Some("issue-agent-1"),
            Some(json!({ "name": "agent" })),
        )
        .await?;
    let secret = agent["secret"].as_str().ok_or("secret")?;
    let (status, page) = harness
        .send(
            "GET",
            &format!("/v1/artifacts/{id}/data/members?select=values"),
            secret,
            None,
            None,
        )
        .await?;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["members"], json!([{ "name": "a", "value": 1 }]));
    Ok(())
}
