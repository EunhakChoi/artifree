//! A frame's source and download capabilities (decision 0032), exercised in
//! process through the real routers, the artifact service, and `SQLite`.
//!
//! Each rejection test also checks that nothing was committed, because a
//! refusal that still wrote a revision would be the worst failure.
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use artifree_providers::storage::SqliteStore;
use artifree_server::auth::AccessToken;
use artifree_server::changes;
use artifree_server::http::{self, AppState, Settings};
use artifree_server::limits::RequestLimits;
use artifree_server::preview;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::{Value, json};
use tower::ServiceExt;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const CONTENT_ORIGIN: &str = "http://127.0.0.1:7411";
static VIEWS: AtomicU32 = AtomicU32::new(1);
const ALL: [&str; 3] = ["source.read", "source.write", "source.download"];
const ENTRY: &str = "<!doctype html><html lang=\"en\"><head><!-- artifree-runtime --></head><body>app</body></html>";

struct Harness {
    directory: PathBuf,
    remove: bool,
    api: Router,
    content: Router,
}

impl Harness {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        Self::with_limits(RequestLimits::default())
    }

    /// Limits high enough that a test of something else never meets them.
    fn unlimited() -> Result<Self, Box<dyn std::error::Error>> {
        Self::with_limits(RequestLimits {
            burst: 100_000,
            per_minute: 6_000_000,
            concurrent: 64,
        })
    }

    fn with_limits(limits: RequestLimits) -> Result<Self, Box<dyn std::error::Error>> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!(
            "artifree-source-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory)?;
        Self::open(directory, true, limits)
    }

    fn open(
        directory: PathBuf,
        remove: bool,
        limits: RequestLimits,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let store = SqliteStore::open(&directory.join("db.sqlite3"))?;
        let token_store =
            artifree_providers::tokens::SqliteTokenStore::open(&directory.join("db.sqlite3"))?;
        let token = AccessToken::parse(TOKEN).ok_or("fixture token is invalid")?;
        let state = AppState::new(
            store,
            token_store,
            token,
            Settings {
                content_origin: CONTENT_ORIGIN.into(),
                preview_ttl: Duration::from_secs(60),
                frame_ancestors: "http://127.0.0.1:*".into(),
                limits,
                change_streams: changes::DEFAULT_STREAMS_PER_ACTOR,
                ..Settings::default()
            },
        )?;
        Ok(Self {
            directory,
            remove,
            api: http::router(state.clone()),
            content: preview::router(state),
        })
    }

    /// A new server process over the same data: every connection, grant, and
    /// view of the old one is gone.
    fn restarted(&self) -> Result<Self, Box<dyn std::error::Error>> {
        Self::open(self.directory.clone(), false, RequestLimits::default())
    }

    async fn send(&self, request: Request<Body>) -> Result<Reply, Box<dyn std::error::Error>> {
        Reply::from(self.api.clone().oneshot(request).await?).await
    }

    async fn content(&self, url: &str) -> Result<Reply, Box<dyn std::error::Error>> {
        let path = url
            .strip_prefix(CONTENT_ORIGIN)
            .ok_or("preview URL uses another origin")?;
        let request = Request::get(path).body(Body::empty())?;
        Reply::from(self.content.clone().oneshot(request).await?).await
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if self.remove {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }
}

struct Reply {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}

impl Reply {
    async fn from(response: axum::response::Response) -> Result<Self, Box<dyn std::error::Error>> {
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), usize::MAX).await?.to_vec();
        Ok(Self {
            status,
            headers,
            body,
        })
    }

    fn json(&self) -> Result<Value, serde_json::Error> {
        serde_json::from_slice(&self.body)
    }

    fn header(&self, name: header::HeaderName) -> &str {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
    }

    fn code(&self) -> String {
        self.json()
            .ok()
            .and_then(|body| body["error"]["code"].as_str().map(str::to_owned))
            .unwrap_or_else(|| {
                format!("<no JSON error: {:?}>", String::from_utf8_lossy(&self.body))
            })
    }
}

fn authorized(builder: axum::http::request::Builder) -> axum::http::request::Builder {
    builder.header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
}

fn post_raw(
    path: &str,
    key: Option<&str>,
    content_type: &str,
    body: Vec<u8>,
) -> Result<Request<Body>, axum::http::Error> {
    let mut builder = authorized(Request::post(path)).header(header::CONTENT_TYPE, content_type);
    if let Some(key) = key {
        builder = builder.header("idempotency-key", key);
    }
    builder.body(Body::from(body))
}

fn post(path: &str, key: Option<&str>, body: &Value) -> Result<Request<Body>, axum::http::Error> {
    post_raw(path, key, "application/json", body.to_string().into_bytes())
}

fn get(path: &str) -> Result<Request<Body>, axum::http::Error> {
    authorized(Request::get(path)).body(Body::empty())
}

fn b64(bytes: &[u8]) -> String {
    BASE64.encode(bytes)
}

/// Non-ASCII text with CRLF, an emoji, and no trailing newline.
fn text_bytes() -> Vec<u8> {
    "héllo wörld — 日本語 🙂\r\nsecond line".as_bytes().to_vec()
}

/// Every byte value, so the file is neither UTF-8 nor text.
fn binary_bytes() -> Vec<u8> {
    (0..=255_u8).cycle().take(3000).collect()
}

/// Larger than one read answer holds.
fn big_bytes() -> Vec<u8> {
    (0..(2 * 1024 * 1024 + 12_345_usize))
        .map(|index| u8::try_from((index * 31 + index / 7) % 251).unwrap_or(0))
        .collect()
}

fn upload(path: &str, bytes: &[u8]) -> Value {
    json!({"path": path, "encoding": "base64", "content": b64(bytes)})
}

/// An artifact whose manifest declares `declared`, with a text, a binary, and
/// a large file. Returns its identifier.
async fn artifact(
    harness: &Harness,
    declared: &[&str],
) -> Result<String, Box<dyn std::error::Error>> {
    artifact_with_key(harness, "create-source-fixture", declared).await
}

/// [`artifact`] under its own creation key, for a second artifact.
async fn artifact_with_key(
    harness: &Harness,
    key: &str,
    declared: &[&str],
) -> Result<String, Box<dyn std::error::Error>> {
    let manifest = json!({"sdk": "artifree.app.v1", "capabilities": declared}).to_string();
    let created = harness
        .send(post(
            "/v1/artifacts",
            Some(key),
            &json!({
                "title": "Source probe",
                "files": [
                    upload("index.html", ENTRY.as_bytes()),
                    upload("artifree-runtime.json", manifest.as_bytes()),
                    upload("notes.txt", &text_bytes()),
                    upload("sprite.png", &binary_bytes()),
                    upload("big.txt", &big_bytes()),
                ],
            }),
        )?)
        .await?;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.code());
    Ok(created.json()?["artifact"]["id"]
        .as_str()
        .ok_or("missing id")?
        .to_owned())
}

async fn session(harness: &Harness) -> Result<String, Box<dyn std::error::Error>> {
    let created = harness
        .send(post("/v1/runtime-sessions", None, &json!({}))?)
        .await?;
    assert_eq!(created.status, StatusCode::OK);
    Ok(created.json()?["session_id"]
        .as_str()
        .ok_or("missing session id")?
        .to_owned())
}

/// Replaces the grant of one revision and head; returns the status and the
/// new version.
async fn grant(
    harness: &Harness,
    session: &str,
    artifact: &str,
    (revision, head): (u64, u64),
    expected: u64,
    capabilities: &[&str],
) -> Result<(StatusCode, u64), Box<dyn std::error::Error>> {
    let request = authorized(Request::put(format!(
        "/v1/runtime-sessions/{session}/artifacts/{artifact}/revisions/{revision}/grant"
    )))
    .header(header::CONTENT_TYPE, "application/json")
    .body(Body::from(
        json!({"head_revision": head, "expected_version": expected, "capabilities": capabilities})
            .to_string(),
    ))?;
    let reply = harness.send(request).await?;
    let version = reply.json().ok().and_then(|body| body["version"].as_u64());
    Ok((reply.status, version.unwrap_or(0)))
}

#[derive(Clone)]
struct Frame {
    artifact: String,
    revision: u64,
    view: String,
    generation: u64,
    initialization: String,
    session: String,
    grant_version: u64,
    granted: Vec<String>,
}

impl Frame {
    fn path(&self, tail: &str) -> String {
        format!(
            "/v1/artifacts/{}/revisions/{}/runtime-views/{}/{}/{tail}",
            self.artifact, self.revision, self.view, self.generation
        )
    }

    fn body(&self, extra: &Value) -> Value {
        let mut body = json!({"initialization": self.initialization});
        if let (Some(target), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
            for (name, value) in extra {
                target.insert(name.clone(), value.clone());
            }
        }
        body
    }

    async fn call(
        &self,
        harness: &Harness,
        tail: &str,
        key: Option<&str>,
        extra: &Value,
    ) -> Result<Reply, Box<dyn std::error::Error>> {
        harness
            .send(post(&self.path(tail), key, &self.body(extra))?)
            .await
    }

    async fn list(&self, harness: &Harness) -> Result<Reply, Box<dyn std::error::Error>> {
        self.call(harness, "source", None, &json!({})).await
    }

    async fn read(
        &self,
        harness: &Harness,
        path: &str,
        range: (Option<u64>, Option<u64>),
    ) -> Result<Reply, Box<dyn std::error::Error>> {
        let mut extra = json!({"path": path});
        if let Some(offset) = range.0 {
            extra["offset"] = json!(offset);
        }
        if let Some(length) = range.1 {
            extra["length"] = json!(length);
        }
        self.call(harness, "source/read", None, &extra).await
    }

    async fn save(
        &self,
        harness: &Harness,
        key: &str,
        changes: &Value,
    ) -> Result<Reply, Box<dyn std::error::Error>> {
        self.call(
            harness,
            "source/save",
            Some(key),
            &json!({"changes": changes}),
        )
        .await
    }

    async fn download(
        &self,
        harness: &Harness,
        path: Option<&str>,
    ) -> Result<Reply, Box<dyn std::error::Error>> {
        let extra = path.map_or_else(|| json!({}), |path| json!({"path": path}));
        self.call(harness, "download", None, &extra).await
    }
}

/// Grants `capabilities` on a fresh connection, issues generation 1 of a view
/// of `revision` and has the frame's document fetch its initialization.
async fn frame(
    harness: &Harness,
    artifact: &str,
    revision: u64,
    capabilities: &[&str],
) -> Result<Frame, Box<dyn std::error::Error>> {
    let session = session(harness).await?;
    let head = current(harness, artifact).await?;
    let (status, version) = grant(
        harness,
        &session,
        artifact,
        (revision, head),
        0,
        capabilities,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "grant {capabilities:?}");
    let view_id = format!("{:032x}", VIEWS.fetch_add(1, Ordering::Relaxed));
    view(harness, &session, artifact, revision, &view_id, 1, version).await
}

async fn view(
    harness: &Harness,
    session: &str,
    artifact: &str,
    revision: u64,
    view_id: &str,
    generation: u64,
    expected_version: u64,
) -> Result<Frame, Box<dyn std::error::Error>> {
    let created = harness
        .send(post(
            &format!("/v1/artifacts/{artifact}/revisions/{revision}/runtime-views"),
            None,
            &json!({"session_id": session, "view_id": view_id, "generation": generation}),
        )?)
        .await?;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.code());
    let body = created.json()?;
    assert_eq!(body["grant_version"], json!(expected_version));
    let url = body["url"].as_str().ok_or("missing URL")?;
    assert_eq!(harness.content(url).await?.status, StatusCode::OK);
    Ok(Frame {
        artifact: artifact.to_owned(),
        revision,
        view: view_id.to_owned(),
        generation,
        initialization: body["initialization"]
            .as_str()
            .ok_or("missing initialization")?
            .to_owned(),
        session: session.to_owned(),
        grant_version: body["grant_version"].as_u64().ok_or("missing version")?,
        granted: serde_json::from_value(body["granted"].clone())?,
    })
}

async fn current(harness: &Harness, artifact: &str) -> Result<u64, Box<dyn std::error::Error>> {
    let reply = harness
        .send(get(&format!("/v1/artifacts/{artifact}"))?)
        .await?;
    reply.json()?["current_revision"]
        .as_u64()
        .ok_or_else(|| "missing revision".into())
}

/// The numbers of all revisions the owner sees.
async fn revisions(
    harness: &Harness,
    artifact: &str,
) -> Result<Vec<u64>, Box<dyn std::error::Error>> {
    let reply = harness
        .send(get(&format!("/v1/artifacts/{artifact}/revisions"))?)
        .await?;
    let mut numbers: Vec<u64> = reply.json()?["revisions"]
        .as_array()
        .ok_or("missing revisions")?
        .iter()
        .filter_map(|revision| revision["revision"].as_u64())
        .collect();
    // The history lists the newest first.
    numbers.sort_unstable();
    Ok(numbers)
}

/// The stored bytes of a file, as the owner's own read returns them.
async fn stored(
    harness: &Harness,
    artifact: &str,
    revision: u64,
    path: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let reply = harness
        .send(get(&format!(
            "/v1/artifacts/{artifact}/revisions/{revision}/files/{path}"
        ))?)
        .await?;
    assert_eq!(reply.status, StatusCode::OK, "owner read of {path}");
    Ok(reply.body)
}

/// The owner commits `notes.txt` as the next revision on top of `base`.
async fn owner_edit(
    harness: &Harness,
    artifact: &str,
    base: u64,
    key: &str,
    content: &str,
) -> TestResult {
    let reply = harness
        .send(post(
            &format!("/v1/artifacts/{artifact}/revisions"),
            Some(key),
            &json!({
                "base_revision": base,
                "changes": [{"op": "put", "path": "notes.txt", "encoding": "utf8", "content": content}],
            }),
        )?)
        .await?;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.code());
    Ok(())
}

fn put(path: &str, bytes: &[u8]) -> Value {
    json!({"op": "put", "path": path, "encoding": "base64", "content": b64(bytes)})
}

#[tokio::test]
async fn reading_needs_its_own_grant_and_returns_the_stored_bytes() -> TestResult {
    let harness = Harness::new()?;
    let id = artifact(&harness, &["metadata.read", "source.read", "source.write"]).await?;
    // Write and metadata access do not make a frame a reader.
    let writer = frame(&harness, &id, 1, &["metadata.read", "source.write"]).await?;
    assert_eq!(writer.granted, ["metadata.read", "source.write"]);
    for reply in [
        writer.list(&harness).await?,
        writer.read(&harness, "notes.txt", (None, None)).await?,
    ] {
        assert_eq!(reply.status, StatusCode::FORBIDDEN);
        assert_eq!(reply.code(), "forbidden");
    }
    let reader = frame(&harness, &id, 1, &["source.read"]).await?;
    let listed = reader.list(&harness).await?;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.code());
    let listing = listed.json()?;
    assert_eq!(listing["artifact_id"], json!(id));
    assert_eq!(listing["revision"], json!(1));
    // The listing is the revision's own, with its sizes and digests.
    let owner = harness
        .send(get(&format!("/v1/artifacts/{id}/revisions/1"))?)
        .await?
        .json()?;
    assert_eq!(listing["files"], owner["files"]);
    for path in ["notes.txt", "sprite.png"] {
        let reply = reader.read(&harness, path, (None, None)).await?;
        assert_eq!(reply.status, StatusCode::OK);
        let part = reply.json()?;
        assert_eq!(part["next_offset"], Value::Null);
        assert_eq!(part["revision"], json!(1));
        let bytes = BASE64.decode(part["content"].as_str().ok_or("content")?)?;
        assert_eq!(bytes, stored(&harness, &id, 1, path).await?, "{path}");
        let listed = listing["files"]
            .as_array()
            .and_then(|files| files.iter().find(|file| file["path"] == json!(path)))
            .ok_or("listed")?;
        assert_eq!(part["sha256"], listed["sha256"]);
        assert_eq!(part["size"], listed["size"]);
    }
    assert!(!String::from_utf8_lossy(&listed.body).contains(TOKEN));
    Ok(())
}

#[tokio::test]
async fn reads_are_bounded_and_the_parts_join_into_the_file() -> TestResult {
    let harness = Harness::new()?;
    let id = artifact(&harness, &["source.read"]).await?;
    let reader = frame(&harness, &id, 1, &["source.read"]).await?;
    let expected = stored(&harness, &id, 1, "big.txt").await?;
    let mut joined = Vec::new();
    let mut offset = 0;
    let mut answers = 0;
    loop {
        // A request for far more than one answer holds is cut at 1 MiB.
        let reply = reader
            .read(&harness, "big.txt", (Some(offset), Some(u64::MAX)))
            .await?;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.code());
        let part = reply.json()?;
        let bytes = BASE64.decode(part["content"].as_str().ok_or("content")?)?;
        assert!(bytes.len() <= 1024 * 1024);
        assert_eq!(part["offset"], json!(offset));
        assert_eq!(part["size"], json!(expected.len()));
        joined.extend_from_slice(&bytes);
        answers += 1;
        match part["next_offset"].as_u64() {
            Some(next) => offset = next,
            None => break,
        }
    }
    assert_eq!(answers, 3);
    assert_eq!(joined, expected);
    // A short range is exact; one past the end is refused; none selects nothing.
    let middle = reader
        .read(&harness, "big.txt", (Some(10), Some(5)))
        .await?;
    let slice = middle.json()?;
    assert_eq!(
        BASE64.decode(slice["content"].as_str().ok_or("content")?)?,
        expected[10..15]
    );
    assert_eq!(slice["next_offset"], json!(15));
    let past = reader
        .read(&harness, "big.txt", (Some(expected.len() as u64 + 1), None))
        .await?;
    assert_eq!(past.status, StatusCode::RANGE_NOT_SATISFIABLE);
    let empty = reader.read(&harness, "big.txt", (Some(0), Some(0))).await?;
    assert_eq!(empty.status, StatusCode::BAD_REQUEST);
    for (path, status) in [
        ("missing.txt", StatusCode::NOT_FOUND),
        ("../index.html", StatusCode::UNPROCESSABLE_ENTITY),
        (".hidden.txt", StatusCode::UNPROCESSABLE_ENTITY),
        ("no-extension", StatusCode::UNPROCESSABLE_ENTITY),
        ("naïve.txt", StatusCode::UNPROCESSABLE_ENTITY),
    ] {
        let reply = reader.read(&harness, path, (None, None)).await?;
        assert_eq!(reply.status, status, "{path}");
        // A frame-supplied value is never echoed back.
        assert!(
            !String::from_utf8_lossy(&reply.body).contains(path),
            "{path}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_read_only_frame_cannot_write_and_nothing_is_committed() -> TestResult {
    let harness = Harness::new()?;
    let id = artifact(&harness, &["source.read", "source.write"]).await?;
    // "Viewer": every capability but the edit grant.
    let viewer = frame(&harness, &id, 1, &["source.read"]).await?;
    let reply = viewer
        .save(
            &harness,
            "viewer-save-1",
            &json!([put("notes.txt", b"changed")]),
        )
        .await?;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(reply.code(), "forbidden");
    assert_eq!(revisions(&harness, &id).await?, [1]);
    // The same request from an editor commits, so the refusal was the grant.
    let editor = frame(&harness, &id, 1, &["source.write"]).await?;
    let saved = editor
        .save(
            &harness,
            "editor-save-1",
            &json!([put("notes.txt", b"changed")]),
        )
        .await?;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.code());
    assert_eq!(revisions(&harness, &id).await?, [1, 2]);
    Ok(())
}

#[tokio::test]
async fn a_save_commits_one_revision_with_the_framed_bytes_for_the_owner() -> TestResult {
    let harness = Harness::new()?;
    let id = artifact(&harness, &["source.read", "source.write"]).await?;
    let editor = frame(&harness, &id, 1, &["source.read", "source.write"]).await?;
    let text = "naïve — 🙂\r\n".as_bytes();
    let binary: Vec<u8> = (0..=255_u8).rev().collect();
    let reply = editor
        .save(
            &harness,
            "save-with-bytes-1",
            &json!([
                put("notes.txt", text),
                put("sprite.png", &binary),
                put("added/new.txt", b"new"),
                {"op": "delete", "path": "big.txt"},
            ]),
        )
        .await?;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.code());
    let saved = reply.json()?;
    assert_eq!(saved["revision"], json!(2));
    assert_eq!(saved["replayed"], json!(false));
    assert_eq!(revisions(&harness, &id).await?, [1, 2]);
    // The stored bytes are the framed bytes, and what was not touched carries
    // over unchanged.
    assert_eq!(stored(&harness, &id, 2, "notes.txt").await?, text);
    assert_eq!(stored(&harness, &id, 2, "sprite.png").await?, binary);
    assert_eq!(stored(&harness, &id, 2, "added/new.txt").await?, b"new");
    assert_eq!(
        stored(&harness, &id, 2, "artifree-runtime.json").await?,
        stored(&harness, &id, 1, "artifree-runtime.json").await?
    );
    // Revision 1 is untouched, and the answer lists exactly revision 2.
    assert_eq!(stored(&harness, &id, 1, "notes.txt").await?, text_bytes());
    let owner = harness
        .send(get(&format!("/v1/artifacts/{id}/revisions/2"))?)
        .await?
        .json()?;
    assert_eq!(saved["created_at_ms"], owner["created_at_ms"]);
    // The answer describes what this save wrote and nothing else: a frame that
    // may write but not read is not told the other files, and a deleted file
    // is not listed.
    let written: Vec<Value> = owner["files"]
        .as_array()
        .ok_or("missing files")?
        .iter()
        .filter(|file| {
            ["added/new.txt", "notes.txt", "sprite.png"]
                .contains(&file["path"].as_str().unwrap_or(""))
        })
        .cloned()
        .collect();
    assert_eq!(written.len(), 3);
    assert_eq!(saved["files"], Value::Array(written));
    // The commit is the viewing user's own: the history names the owner's actor
    // and carries nothing that marks the frame.
    let history = harness
        .send(get(&format!("/v1/artifacts/{id}/revisions"))?)
        .await?
        .json()?;
    assert_eq!(
        history["revisions"][1]["created_by"],
        history["revisions"][0]["created_by"]
    );
    assert!(!String::from_utf8_lossy(&reply.body).contains(TOKEN));
    Ok(())
}

#[tokio::test]
async fn replaying_a_key_returns_the_first_result_without_a_second_revision() -> TestResult {
    let harness = Harness::new()?;
    let id = artifact(&harness, &["source.write"]).await?;
    let editor = frame(&harness, &id, 1, &["source.write"]).await?;
    let changes = json!([put("notes.txt", b"once")]);
    let first = editor.save(&harness, "replayed-key-1", &changes).await?;
    assert_eq!(first.status, StatusCode::OK);
    let second = editor.save(&harness, "replayed-key-1", &changes).await?;
    assert_eq!(second.status, StatusCode::OK);
    assert_eq!(second.json()?["replayed"], json!(true));
    assert_eq!(second.json()?["revision"], first.json()?["revision"]);
    assert_eq!(revisions(&harness, &id).await?, [1, 2]);
    // The same key for other changes is a different request.
    let reused = editor
        .save(
            &harness,
            "replayed-key-1",
            &json!([put("notes.txt", b"other")]),
        )
        .await?;
    assert_eq!(reused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(reused.code(), "request_key_reused");
    assert_eq!(revisions(&harness, &id).await?, [1, 2]);
    // A replay is still a request: it needs the grant, and once the
    // connection ends it is refused rather than answered.
    let closed = harness
        .send(
            authorized(Request::delete(format!(
                "/v1/runtime-sessions/{}",
                editor.session
            )))
            .body(Body::empty())?,
        )
        .await?;
    assert_eq!(closed.status, StatusCode::NO_CONTENT);
    let after = editor.save(&harness, "replayed-key-1", &changes).await?;
    assert_eq!(after.status, StatusCode::FORBIDDEN);
    Ok(())
}

#[tokio::test]
async fn frame_keys_never_collide_with_the_owners_keys() -> TestResult {
    // The second spelling is the textual prefix an earlier design used for
    // frame keys: it must be an ordinary key now, one a frame cannot reach.
    for owner_key in ["shared-key-0001", "frame:shared-key-0001"] {
        let harness = Harness::new()?;
        let id = artifact(&harness, &["source.write"]).await?;
        let editor = frame(&harness, &id, 1, &["source.write"]).await?;
        // The owner uses a key; the frame, pinned to revision 1, uses the same
        // string (without the owner's `frame:` where there is one) for the
        // same changes. Were keys shared, the frame would be told the owner's
        // commit as its own result.
        owner_edit(&harness, &id, 1, owner_key, "owner").await?;
        let reply = editor
            .save(
                &harness,
                "shared-key-0001",
                &json!([{"op": "put", "path": "notes.txt", "encoding": "utf8", "content": "owner"}]),
            )
            .await?;
        assert_eq!(reply.status, StatusCode::CONFLICT, "{owner_key}");
        assert_eq!(reply.code(), "revision_conflict");
        assert_eq!(reply.json()?["error"]["current_revision"], json!(2));
        // Different changes under the same string are not "key reused" either:
        // the owner's key is not visible to the frame, not even as an error.
        let other = editor
            .save(
                &harness,
                "shared-key-0001",
                &json!([{"op": "put", "path": "notes.txt", "encoding": "utf8", "content": "other"}]),
            )
            .await?;
        assert_eq!(other.code(), "revision_conflict", "{owner_key}");
        assert_eq!(revisions(&harness, &id).await?, [1, 2]);
        // A frame's own key still replays for the frame.
        let again = frame(&harness, &id, 2, &["source.write"]).await?;
        let changes = json!([put("notes.txt", b"framed")]);
        let first = again.save(&harness, owner_key, &changes).await?;
        assert_eq!(first.status, StatusCode::OK, "{owner_key}");
        let repeated = again.save(&harness, owner_key, &changes).await?;
        assert_eq!(repeated.json()?["replayed"], json!(true), "{owner_key}");
        assert_eq!(revisions(&harness, &id).await?, [1, 2, 3]);
    }
    Ok(())
}

#[tokio::test]
async fn one_key_on_two_artifacts_is_two_keys_for_their_frames() -> TestResult {
    let harness = Harness::new()?;
    let first = artifact(&harness, &["source.write"]).await?;
    let second = artifact_with_key(&harness, "create-second-fixture", &["source.write"]).await?;
    assert_ne!(first, second);
    let one = frame(&harness, &first, 1, &["source.write"]).await?;
    let two = frame(&harness, &second, 1, &["source.write"]).await?;
    // A preview that hard-codes a key must not be able to use up, replay, or
    // probe the key of an app on another artifact of the same user.
    let key = "shared-key-0001";
    let changes = json!([put("notes.txt", b"on the first")]);
    let saved = one.save(&harness, key, &changes).await?;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.code());
    // The same key with other changes on the other artifact is a new request,
    // not `request_key_reused`, and does not return the first artifact's
    // result.
    let other = json!([put("notes.txt", b"on the second")]);
    let reply = two.save(&harness, key, &other).await?;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.code());
    let body = reply.json()?;
    assert_eq!(body["replayed"], json!(false));
    assert_eq!(
        stored(&harness, &second, 2, "notes.txt").await?,
        b"on the second"
    );
    assert_eq!(
        stored(&harness, &first, 2, "notes.txt").await?,
        b"on the first"
    );
    // Within one artifact the key still replays and still conflicts.
    let repeated = one.save(&harness, key, &changes).await?;
    assert_eq!(repeated.status, StatusCode::OK, "{}", repeated.code());
    assert_eq!(repeated.json()?["replayed"], json!(true));
    let reused = one.save(&harness, key, &other).await?;
    assert_eq!(reused.code(), "request_key_reused");
    Ok(())
}

#[tokio::test]
async fn no_route_outside_a_frame_accepts_a_key_in_the_frames_namespace() -> TestResult {
    let harness = Harness::new()?;
    let id = artifact(&harness, &["source.write"]).await?;
    // The namespace a frame's key is recorded in is spelled with a character
    // no other route's key can contain; every route parses its key the same
    // way, so each refuses it before doing anything.
    let key = "frame/shared-key-0001";
    let save = json!({"base_revision": 1, "changes": [
        {"op": "put", "path": "notes.txt", "encoding": "utf8", "content": "x"}]});
    for (method, path, body) in [
        (
            "POST",
            "/v1/artifacts".to_owned(),
            json!({"title": "t", "files": [
            {"path": "index.html", "encoding": "utf8", "content": "<p>x</p>"}]}),
        ),
        ("POST", format!("/v1/artifacts/{id}/revisions"), save),
        (
            "PUT",
            format!("/v1/artifacts/{id}/data"),
            json!({"path": "a", "base_data_version": 0, "value": 1}),
        ),
        (
            "PATCH",
            format!("/v1/artifacts/{id}"),
            json!({"base_metadata_revision": 1, "title": "other"}),
        ),
        (
            "POST",
            format!("/v1/artifacts/{id}/revisions/1/duplicate"),
            json!({}),
        ),
    ] {
        let request = authorized(Request::builder().method(method).uri(path.as_str()))
            .header(header::CONTENT_TYPE, "application/json")
            .header("idempotency-key", key)
            .body(Body::from(body.to_string()))?;
        let reply = harness.send(request).await?;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{method} {path}");
        assert_eq!(reply.code(), "invalid_request", "{method} {path}");
    }
    assert_eq!(revisions(&harness, &id).await?, [1]);
    Ok(())
}

#[tokio::test]
async fn a_frame_pinned_to_a_revision_that_is_no_longer_the_head_conflicts() -> TestResult {
    let harness = Harness::new()?;
    let id = artifact(&harness, &["source.read", "source.write"]).await?;
    let editor = frame(&harness, &id, 1, &["source.write"]).await?;
    owner_edit(&harness, &id, 1, "owner-moves-head", "owner").await?;
    let reply = editor
        .save(
            &harness,
            "stale-save-1",
            &json!([put("notes.txt", b"frame")]),
        )
        .await?;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert_eq!(reply.code(), "revision_conflict");
    // The frame learns the head it lost to, and nothing was overwritten.
    assert_eq!(reply.json()?["error"]["current_revision"], json!(2));
    assert_eq!(revisions(&harness, &id).await?, [1, 2]);
    assert_eq!(stored(&harness, &id, 2, "notes.txt").await?, b"owner");
    // A view of the old revision is not a view of the head either.
    let old = frame(&harness, &id, 1, &["source.write"]).await?;
    let again = old
        .save(
            &harness,
            "stale-save-2",
            &json!([put("notes.txt", b"again")]),
        )
        .await?;
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert_eq!(revisions(&harness, &id).await?, [1, 2]);
    Ok(())
}

#[tokio::test]
async fn two_frames_saving_from_one_base_have_exactly_one_winner() -> TestResult {
    let harness = Harness::new()?;
    let id = artifact(&harness, &["source.write"]).await?;
    let first = frame(&harness, &id, 1, &["source.write"]).await?;
    let second = view_of(&harness, &id, &first, "b").await?;
    let a = json!([put("notes.txt", b"from first")]);
    let b = json!([put("notes.txt", b"from second")]);
    let (left, right) = tokio::join!(
        first.save(&harness, "race-key-first", &a),
        second.save(&harness, "race-key-second", &b),
    );
    let (left, right) = (left?, right?);
    let mut statuses = [left.status, right.status];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::CONFLICT]);
    assert_eq!(revisions(&harness, &id).await?, [1, 2]);
    let winner = if left.status == StatusCode::OK {
        &b"from first"[..]
    } else {
        &b"from second"[..]
    };
    assert_eq!(stored(&harness, &id, 2, "notes.txt").await?, winner);
    Ok(())
}

/// A second view of the same connection, with its own view id.
async fn view_of(
    harness: &Harness,
    artifact_id: &str,
    like: &Frame,
    digit: &str,
) -> Result<Frame, Box<dyn std::error::Error>> {
    view(
        harness,
        &like.session,
        artifact_id,
        like.revision,
        &digit.repeat(32),
        1,
        like.grant_version,
    )
    .await
}

#[tokio::test]
async fn a_replaced_generation_can_no_longer_read_write_or_download() -> TestResult {
    let harness = Harness::new()?;
    let id = artifact(&harness, &ALL).await?;
    let old = frame(&harness, &id, 1, &ALL).await?;
    assert_eq!(
        old.read(&harness, "notes.txt", (None, None)).await?.status,
        StatusCode::OK
    );
    // The workspace replaces the view with generation 2 of the same view id.
    let new = view(
        &harness,
        &old.session,
        &id,
        1,
        &old.view,
        2,
        old.grant_version,
    )
    .await?;
    for reply in [
        old.list(&harness).await?,
        old.read(&harness, "notes.txt", (None, None)).await?,
        old.save(
            &harness,
            "old-generation-1",
            &json!([put("notes.txt", b"late")]),
        )
        .await?,
        old.download(&harness, Some("notes.txt")).await?,
        old.download(&harness, None).await?,
    ] {
        assert_eq!(reply.status, StatusCode::NOT_FOUND);
    }
    assert_eq!(revisions(&harness, &id).await?, [1]);
    // The replacement works, so the refusal was the generation. A closed
    // generation is refused too.
    assert_eq!(new.list(&harness).await?.status, StatusCode::OK);
    let closed = harness
        .send(
            authorized(Request::delete(format!(
                "/v1/runtime-views/{}/{}",
                new.view, new.generation
            )))
            .body(Body::empty())?,
        )
        .await?;
    assert_eq!(closed.status, StatusCode::NO_CONTENT);
    assert_eq!(new.list(&harness).await?.status, StatusCode::NOT_FOUND);
    assert_eq!(
        new.save(
            &harness,
            "closed-generation-1",
            &json!([put("notes.txt", b"x")])
        )
        .await?
        .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(revisions(&harness, &id).await?, [1]);
    Ok(())
}

#[tokio::test]
async fn revoking_or_ending_the_connection_takes_effect_on_the_next_request() -> TestResult {
    let harness = Harness::new()?;
    let id = artifact(&harness, &ALL).await?;
    let editor = frame(&harness, &id, 1, &ALL).await?;
    assert_eq!(editor.list(&harness).await?.status, StatusCode::OK);
    assert_eq!(
        editor.download(&harness, Some("notes.txt")).await?.status,
        StatusCode::OK
    );
    // The user clears the grant. The frame keeps what it was issued, but its
    // next request of every kind is refused.
    let (status, _) = grant(
        &harness,
        &editor.session,
        &id,
        (1, 1),
        editor.grant_version,
        &[],
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    for reply in [
        editor.list(&harness).await?,
        editor.read(&harness, "notes.txt", (None, None)).await?,
        editor
            .save(
                &harness,
                "revoked-save-1",
                &json!([put("notes.txt", b"late")]),
            )
            .await?,
        editor.download(&harness, Some("notes.txt")).await?,
    ] {
        assert_eq!(reply.status, StatusCode::FORBIDDEN);
    }
    assert_eq!(revisions(&harness, &id).await?, [1]);
    // Dropping only the edit grant leaves reading in a new view, and a view
    // issued before cannot be revived by granting again.
    let next = frame(&harness, &id, 1, &ALL).await?;
    let (status, version) = grant(
        &harness,
        &next.session,
        &id,
        (1, 1),
        next.grant_version,
        &["source.read"],
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(version, next.grant_version);
    assert_eq!(next.list(&harness).await?.status, StatusCode::FORBIDDEN);
    let (status, _) = grant(&harness, &next.session, &id, (1, 1), version, &ALL).await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(next.list(&harness).await?.status, StatusCode::FORBIDDEN);
    // Logout: closing the connection ends every view of it.
    let live = frame(&harness, &id, 1, &ALL).await?;
    assert_eq!(live.list(&harness).await?.status, StatusCode::OK);
    let closed = harness
        .send(
            authorized(Request::delete(format!(
                "/v1/runtime-sessions/{}",
                live.session
            )))
            .body(Body::empty())?,
        )
        .await?;
    assert_eq!(closed.status, StatusCode::NO_CONTENT);
    assert_eq!(live.list(&harness).await?.status, StatusCode::FORBIDDEN);
    assert_eq!(
        live.save(
            &harness,
            "logout-save-0001",
            &json!([put("notes.txt", b"x")])
        )
        .await?
        .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(revisions(&harness, &id).await?, [1]);
    Ok(())
}

#[tokio::test]
async fn a_restart_ends_every_grant_and_view() -> TestResult {
    let harness = Harness::new()?;
    let id = artifact(&harness, &ALL).await?;
    let editor = frame(&harness, &id, 1, &ALL).await?;
    assert_eq!(editor.list(&harness).await?.status, StatusCode::OK);
    let restarted = harness.restarted()?;
    for reply in [
        editor.list(&restarted).await?,
        editor.read(&restarted, "notes.txt", (None, None)).await?,
        editor
            .save(
                &restarted,
                "restart-save-0001",
                &json!([put("notes.txt", b"x")]),
            )
            .await?,
        editor.download(&restarted, None).await?,
    ] {
        assert_eq!(reply.status, StatusCode::NOT_FOUND);
    }
    assert_eq!(revisions(&restarted, &id).await?, [1]);
    Ok(())
}

#[tokio::test]
async fn forged_or_mismatched_bindings_are_not_found() -> TestResult {
    let harness = Harness::new()?;
    let id = artifact(&harness, &ALL).await?;
    let other = artifact_named(&harness, "other-source-probe").await?;
    let editor = frame(&harness, &id, 1, &ALL).await?;
    let changes = json!([put("notes.txt", b"forged")]);
    let mut forged = editor.clone();
    forged.initialization = "0".repeat(64);
    let mut unknown_view = editor.clone();
    unknown_view.view = "c".repeat(32);
    let mut wrong_generation = editor.clone();
    wrong_generation.generation = 7;
    let mut wrong_artifact = editor.clone();
    wrong_artifact.artifact = other;
    let mut wrong_revision = editor.clone();
    wrong_revision.revision = 2;
    let mut malformed_view = editor.clone();
    malformed_view.view = "not-a-view".into();
    for (name, frame) in [
        ("secret", &forged),
        ("view", &unknown_view),
        ("generation", &wrong_generation),
        ("artifact", &wrong_artifact),
        ("revision", &wrong_revision),
    ] {
        for reply in [
            frame.list(&harness).await?,
            frame.save(&harness, "forged-binding-1", &changes).await?,
            frame.download(&harness, None).await?,
        ] {
            assert_eq!(reply.status, StatusCode::NOT_FOUND, "{name}");
            assert!(!String::from_utf8_lossy(&reply.body).contains(&editor.initialization));
        }
    }
    assert_eq!(
        malformed_view.list(&harness).await?.status,
        StatusCode::BAD_REQUEST
    );
    // No credentials at all is a plain 401.
    let anonymous = Request::post(editor.path("source"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(editor.body(&json!({})).to_string()))?;
    assert_eq!(
        harness.send(anonymous).await?.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(revisions(&harness, &id).await?, [1]);
    Ok(())
}

async fn artifact_named(
    harness: &Harness,
    key: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let created = harness
        .send(post(
            "/v1/artifacts",
            Some(key),
            &json!({"title": "Other", "files": [upload("index.html", ENTRY.as_bytes())]}),
        )?)
        .await?;
    Ok(created.json()?["artifact"]["id"]
        .as_str()
        .ok_or("missing id")?
        .to_owned())
}

#[tokio::test]
async fn an_artifact_in_the_trash_is_not_found_to_its_frames() -> TestResult {
    let harness = Harness::new()?;
    let id = artifact(&harness, &ALL).await?;
    let editor = frame(&harness, &id, 1, &ALL).await?;
    let metadata = harness
        .send(get(&format!("/v1/artifacts/{id}"))?)
        .await?
        .json()?["metadata_revision"]
        .as_u64()
        .ok_or("metadata revision")?;
    let trashed = harness
        .send(
            authorized(Request::patch(format!("/v1/artifacts/{id}")))
                .header(header::CONTENT_TYPE, "application/json")
                .header("idempotency-key", "trash-source-probe")
                .body(Body::from(
                    json!({"base_metadata_revision": metadata, "deleted": true}).to_string(),
                ))?,
        )
        .await?;
    assert_eq!(trashed.status, StatusCode::OK, "{}", trashed.code());
    for reply in [
        editor.list(&harness).await?,
        editor.read(&harness, "notes.txt", (None, None)).await?,
        editor
            .save(
                &harness,
                "trashed-save-0001",
                &json!([put("notes.txt", b"x")]),
            )
            .await?,
        editor.download(&harness, Some("notes.txt")).await?,
        editor.download(&harness, None).await?,
    ] {
        assert_eq!(reply.status, StatusCode::NOT_FOUND);
        assert_eq!(reply.code(), "not_found");
    }
    Ok(())
}

#[tokio::test]
async fn file_count_size_and_path_rules_hold_for_frames_and_commit_nothing() -> TestResult {
    let harness = Harness::new()?;
    let id = artifact(&harness, &["source.write"]).await?;
    let editor = frame(&harness, &id, 1, &["source.write"]).await?;
    let one_over = vec![b'x'; 5 * 1024 * 1024 + 1];
    let many: Vec<Value> = (0..513)
        .map(|index| put(&format!("many/{index}.txt"), b"x"))
        .collect();
    let cases: Vec<(&str, Value)> = vec![
        ("file over 5 MiB", json!([put("huge.txt", &one_over)])),
        ("513 changes", Value::Array(many)),
        ("dot-dot", json!([put("../escape.txt", b"x")])),
        ("hidden", json!([put(".env.txt", b"x")])),
        ("backslash", json!([put("a\\b.txt", b"x")])),
        ("non-ASCII path", json!([put("naïve.txt", b"x")])),
        ("reserved", json!([put("con.txt", b"x")])),
        ("no extension", json!([put("README", b"x")])),
        ("nine segments", json!([put("a/b/c/d/e/f/g/h/i.txt", b"x")])),
        (
            "same path twice",
            json!([put("twice.txt", b"x"), put("twice.txt", b"y")]),
        ),
        (
            "delete of a file that is absent",
            json!([{"op": "delete", "path": "absent.txt"}]),
        ),
        (
            "invalid base64",
            json!([{"op": "put", "path": "bad.txt", "encoding": "base64", "content": "***"}]),
        ),
        ("no change at all", json!([])),
    ];
    for (index, (name, changes)) in cases.iter().enumerate() {
        let reply = editor
            .save(&harness, &format!("rule-case-{index:04}"), changes)
            .await?;
        assert!(
            reply.status.is_client_error(),
            "{name}: {} {}",
            reply.status,
            reply.code()
        );
        assert_ne!(reply.status, StatusCode::FORBIDDEN, "{name}");
        assert_eq!(revisions(&harness, &id).await?, [1], "{name}");
    }
    // A package over 25 MiB in total: each file is within its own limit.
    let chunk = vec![b'y'; 5 * 1024 * 1024];
    let wide: Vec<Value> = (0..5)
        .map(|index| put(&format!("wide/{index}.txt"), &chunk))
        .collect();
    let reply = editor
        .save(&harness, "rule-total-0001", &Value::Array(wide))
        .await?;
    assert!(reply.status.is_client_error(), "{}", reply.status);
    assert_eq!(revisions(&harness, &id).await?, [1]);
    Ok(())
}

/// Bodies a hostile frame might send to the four frame routes, built around
/// the view's real secret so that an echo of it would be visible.
fn hostile_bodies(init: &str, marker: &str) -> Vec<(&'static str, Vec<u8>)> {
    let nested = format!("{}1{}", "[".repeat(5000), "]".repeat(5000));
    let long = "x".repeat(300_000);
    vec![
        (
            "string where a list belongs",
            format!("{{\"initialization\":\"{init}\",\"changes\":\"{marker}\"}}").into_bytes(),
        ),
        (
            "unknown member",
            format!("{{\"initialization\":\"{init}\",\"{marker}\":1}}").into_bytes(),
        ),
        (
            "unknown variant",
            format!(
                "{{\"initialization\":\"{init}\",\"changes\":[{{\"op\":\"{marker}\",\"path\":\"a.txt\"}}]}}"
            )
            .into_bytes(),
        ),
        (
            "number where a string belongs",
            format!("{{\"initialization\":\"{init}\",\"path\":123456789}}").into_bytes(),
        ),
        (
            "string where a number belongs",
            format!(
                "{{\"initialization\":\"{init}\",\"path\":\"notes.txt\",\"offset\":\"{marker}\"}}"
            )
            .into_bytes(),
        ),
        ("empty", Vec::new()),
        ("null", b"null".to_vec()),
        ("array", b"[]".to_vec()),
        ("string", b"\"x\"".to_vec()),
        ("empty object", b"{}".to_vec()),
        ("invalid UTF-8", vec![0xff, 0xfe, 0x7b, 0x7d]),
        ("truncated", format!("{{\"initialization\":\"{init}\"").into_bytes()),
        (
            "wrong type",
            br#"{"initialization": 1, "changes": 2}"#.to_vec(),
        ),
        (
            "duplicate field",
            format!("{{\"initialization\":\"{init}\",\"initialization\":\"{init}\",\"changes\":[]}}")
                .into_bytes(),
        ),
        (
            "unknown field",
            format!("{{\"initialization\":\"{init}\",\"changes\":[],\"base_revision\":9}}")
                .into_bytes(),
        ),
        (
            "unknown operation",
            format!("{{\"initialization\":\"{init}\",\"changes\":[{{\"op\":\"rename\",\"path\":\"a.txt\"}}]}}")
                .into_bytes(),
        ),
        (
            "missing content",
            format!("{{\"initialization\":\"{init}\",\"changes\":[{{\"op\":\"put\",\"path\":\"a.txt\"}}]}}")
                .into_bytes(),
        ),
        (
            "deeply nested",
            format!("{{\"initialization\":\"{init}\",\"changes\":{nested}}}").into_bytes(),
        ),
        (
            "long path",
            format!("{{\"initialization\":\"{init}\",\"path\":\"{long}\"}}").into_bytes(),
        ),
        (
            "long secret",
            format!("{{\"initialization\":\"{long}\",\"changes\":[]}}").into_bytes(),
        ),
        (
            "negative range",
            format!("{{\"initialization\":\"{init}\",\"path\":\"notes.txt\",\"offset\":-1}}")
                .into_bytes(),
        ),
        (
            "fractional range",
            format!("{{\"initialization\":\"{init}\",\"path\":\"notes.txt\",\"length\":1.5}}")
                .into_bytes(),
        ),
        (
            "huge number",
            format!("{{\"initialization\":\"{init}\",\"path\":\"notes.txt\",\"offset\":1e400}}")
                .into_bytes(),
        ),
    ]
}

#[tokio::test]
async fn hostile_messages_are_refused_without_panicking_or_committing() -> TestResult {
    let harness = Harness::unlimited()?;
    let id = artifact(&harness, &ALL).await?;
    let editor = frame(&harness, &id, 1, &ALL).await?;
    let init = editor.initialization.clone();
    let marker = "frame-supplied-marker-7431";
    // serde's own messages quote the value that did not fit; none of these
    // may come back.
    let bodies = hostile_bodies(&init, marker);
    for tail in ["source", "source/read", "source/save", "download"] {
        for (name, body) in &bodies {
            let reply = harness
                .send(post_raw(
                    &editor.path(tail),
                    Some("hostile-message-1"),
                    "application/json",
                    body.clone(),
                )?)
                .await?;
            assert!(
                reply.status.is_client_error(),
                "{tail} {name}: {}",
                reply.status
            );
            let text = String::from_utf8_lossy(&reply.body);
            assert!(!text.contains(&init), "{tail} {name} echoed the secret");
            assert!(!text.contains(marker), "{tail} {name} echoed the body");
            assert!(!text.contains("123456789"), "{tail} {name} echoed the body");
        }
        let reply = harness
            .send(post_raw(
                &editor.path(tail),
                Some("hostile-message-1"),
                "text/plain",
                b"{}".to_vec(),
            )?)
            .await?;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{tail}");
    }
    // A save without its key is not a save.
    let keyless = editor
        .call(
            &harness,
            "source/save",
            None,
            &json!({"changes": [put("notes.txt", b"x")]}),
        )
        .await?;
    assert_eq!(keyless.status, StatusCode::BAD_REQUEST);
    for key in [
        "short",
        "has space in it",
        "slash/slash/slash",
        &"k".repeat(129),
    ] {
        let reply = editor
            .save(&harness, key, &json!([put("notes.txt", b"x")]))
            .await?;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{key}");
    }
    assert_eq!(revisions(&harness, &id).await?, [1]);
    // The frame is intact after all of that.
    assert_eq!(editor.list(&harness).await?.status, StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn a_frame_that_floods_cannot_spend_the_owners_budget_or_block_its_revocation() -> TestResult
{
    // Frames get half of each limit; the owner's own requests keep theirs.
    let harness = Harness::with_limits(RequestLimits {
        burst: 40,
        per_minute: 1,
        concurrent: 16,
    })?;
    let id = artifact(&harness, &ALL).await?;
    let flooder = frame(&harness, &id, 1, &ALL).await?;
    let mut admitted = 0;
    let mut limited = None;
    for _ in 0..60 {
        let reply = flooder.list(&harness).await?;
        if reply.status == StatusCode::TOO_MANY_REQUESTS {
            limited = Some(reply);
            break;
        }
        assert_eq!(reply.status, StatusCode::OK);
        admitted += 1;
    }
    assert_eq!(admitted, 20, "half of the burst of 40");
    let limited = limited.ok_or("the flood was never limited")?;
    assert_eq!(limited.code(), "rate_limited");
    assert!(!limited.header(header::RETRY_AFTER).is_empty());
    // Every kind of frame request is limited alike.
    for reply in [
        flooder.read(&harness, "notes.txt", (None, None)).await?,
        flooder.download(&harness, None).await?,
        flooder
            .save(
                &harness,
                "limited-save-0001",
                &json!([put("notes.txt", b"x")]),
            )
            .await?,
        flooder.call(&harness, "metadata", None, &json!({})).await?,
    ] {
        assert_eq!(reply.status, StatusCode::TOO_MANY_REQUESTS);
    }
    // The owner's own request is admitted, and so is the grant change that
    // revokes the flooder.
    assert_eq!(
        harness
            .send(get(&format!("/v1/artifacts/{id}"))?)
            .await?
            .status,
        StatusCode::OK
    );
    let (status, _) = grant(
        &harness,
        &flooder.session,
        &id,
        (1, 1),
        flooder.grant_version,
        &[],
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "revoking must stay available");
    assert_eq!(revisions(&harness, &id).await?, [1]);
    Ok(())
}

#[tokio::test]
async fn the_owners_own_requests_are_limited_by_the_owners_budget() -> TestResult {
    let harness = Harness::with_limits(RequestLimits {
        burst: 3,
        per_minute: 1,
        concurrent: 16,
    })?;
    let mut statuses = Vec::new();
    for _ in 0..5 {
        statuses.push(harness.send(get("/v1/artifacts")?).await?.status);
    }
    assert_eq!(&statuses[..3], &[StatusCode::OK; 3]);
    assert_eq!(statuses[3], StatusCode::TOO_MANY_REQUESTS);
    Ok(())
}

#[tokio::test]
async fn a_download_is_the_stored_bytes_as_an_attachment_of_a_closed_type() -> TestResult {
    let harness = Harness::new()?;
    let id = artifact(&harness, &ALL).await?;
    let viewer = frame(&harness, &id, 1, &["source.read"]).await?;
    // Reading is not downloading.
    assert_eq!(
        viewer.download(&harness, Some("notes.txt")).await?.status,
        StatusCode::FORBIDDEN
    );
    let downloader = frame(&harness, &id, 1, &["source.download"]).await?;
    for (path, name) in [
        ("notes.txt", "notes.txt"),
        ("sprite.png", "sprite.png"),
        ("index.html", "index.html"),
        ("artifree-runtime.json", "artifree-runtime.json"),
    ] {
        let reply = downloader.download(&harness, Some(path)).await?;
        assert_eq!(reply.status, StatusCode::OK, "{path}");
        assert_eq!(reply.body, stored(&harness, &id, 1, path).await?, "{path}");
        // A browser saves it and never renders it, whatever the file is.
        assert_eq!(
            reply.header(header::CONTENT_TYPE),
            "application/octet-stream",
            "{path}"
        );
        assert_eq!(
            reply.header(header::CONTENT_DISPOSITION),
            format!("attachment; filename=\"{name}\"")
        );
        assert_eq!(reply.header(header::X_CONTENT_TYPE_OPTIONS), "nosniff");
        assert!(
            reply
                .header(header::CONTENT_SECURITY_POLICY)
                .contains("sandbox")
        );
    }
    // A nested file is saved under its own name only.
    let nested = frame(&harness, &id, 1, &["source.write"]).await?;
    assert_eq!(
        nested
            .save(
                &harness,
                "nested-download-1",
                &json!([put("deep/dir/a_b-c.1.txt", b"deep")])
            )
            .await?
            .status,
        StatusCode::OK
    );
    let head = frame(&harness, &id, 2, &["source.download"]).await?;
    let reply = head
        .download(&harness, Some("deep/dir/a_b-c.1.txt"))
        .await?;
    assert_eq!(reply.body, b"deep");
    assert_eq!(
        reply.header(header::CONTENT_DISPOSITION),
        "attachment; filename=\"a_b-c.1.txt\""
    );
    for (path, status) in [
        ("missing.txt", StatusCode::NOT_FOUND),
        ("../notes.txt", StatusCode::UNPROCESSABLE_ENTITY),
        ("a\"b.txt", StatusCode::UNPROCESSABLE_ENTITY),
    ] {
        let reply = head.download(&harness, Some(path)).await?;
        assert_eq!(reply.status, status, "{path}");
        assert!(!String::from_utf8_lossy(&reply.body).contains(path));
    }
    Ok(())
}

/// Every name a download can be saved under, which the client accepts.
fn is_a_download_name(name: &str) -> bool {
    (1..=128).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

#[tokio::test]
async fn the_longest_last_segment_is_saved_under_a_name_the_client_accepts() -> TestResult {
    let harness = Harness::new()?;
    let id = artifact(&harness, &["source.write", "source.download"]).await?;
    // A package path allows 240 bytes in all but at most 100 in a segment,
    // and no segment starts with a dot: the longest name a download can have
    // is 100, well inside the 128 the client accepts. The deepest path of
    // 240 bytes has a last segment of 100 as well.
    let long_segment = format!("{}.txt", "a".repeat(96));
    let deep = format!("{}/{}/{long_segment}", "d".repeat(99), "e".repeat(30));
    let cases = [
        (
            format!("{}.txt", "b".repeat(95)),
            format!("{}.txt", "b".repeat(95)),
        ),
        (long_segment.clone(), long_segment.clone()),
        (deep, long_segment),
    ];
    let writer = frame(&harness, &id, 1, &["source.write"]).await?;
    let changes: Vec<Value> = cases
        .iter()
        .map(|(path, _)| put(path, path.as_bytes()))
        .collect();
    let saved = writer
        .save(&harness, "long-names-save-1", &json!(changes))
        .await?;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.code());
    let downloader = frame(&harness, &id, 2, &["source.download"]).await?;
    for (path, name) in &cases {
        let reply = downloader.download(&harness, Some(path)).await?;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.code());
        assert_eq!(reply.body, path.as_bytes());
        assert!(is_a_download_name(name), "{name}");
        assert_eq!(
            reply.header(header::CONTENT_DISPOSITION),
            format!("attachment; filename=\"{name}\""),
            "{path}"
        );
    }
    // One byte more in a segment is not a package path, so no download can
    // ask for a longer name; nor can a segment start with a dot.
    let writer = frame(&harness, &id, 2, &["source.write"]).await?;
    for (key, path) in [
        ("long-names-save-2", format!("{}.txt", "f".repeat(97))),
        ("long-names-save-3", format!(".{}.txt", "g".repeat(10))),
    ] {
        let refused = writer
            .save(&harness, key, &json!([put(&path, b"x")]))
            .await?;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY, "{path}");
    }
    Ok(())
}

#[tokio::test]
async fn the_archive_is_the_export_of_the_pinned_revision_not_the_head() -> TestResult {
    let harness = Harness::new()?;
    let id = artifact(&harness, &["source.download"]).await?;
    owner_edit(&harness, &id, 1, "owner-moves-head", "newer").await?;
    // A view of revision 1 opened while the head is 2 (the user is looking at
    // an old revision): its grant belongs to the head 2, and its bytes to 1.
    let downloader = frame(&harness, &id, 1, &["source.download"]).await?;
    let reply = downloader.download(&harness, None).await?;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.code());
    let export = harness
        .send(get(&format!("/v1/artifacts/{id}/revisions/1/export"))?)
        .await?;
    assert_eq!(export.status, StatusCode::OK);
    assert_eq!(reply.body, export.body);
    assert_eq!(reply.header(header::CONTENT_TYPE), "application/zip");
    assert_eq!(
        reply.header(header::CONTENT_DISPOSITION),
        format!("attachment; filename=\"{id}-r1.zip\"")
    );
    let head = harness
        .send(get(&format!("/v1/artifacts/{id}/revisions/2/export"))?)
        .await?;
    assert_ne!(reply.body, head.body);
    // A file is pinned in the same way.
    let file = downloader.download(&harness, Some("notes.txt")).await?;
    assert_eq!(file.body, text_bytes());
    Ok(())
}

#[tokio::test]
async fn a_view_whose_head_has_moved_reads_and_downloads_nothing_until_a_new_consent() -> TestResult
{
    let harness = Harness::new()?;
    let id = artifact(&harness, &ALL).await?;
    let old = frame(&harness, &id, 1, &ALL).await?;
    assert_eq!(old.list(&harness).await?.status, StatusCode::OK);
    let changes = json!([put("notes.txt", b"before")]);
    let first = old.save(&harness, "head-moves-save-1", &changes).await?;
    assert_eq!(first.status, StatusCode::OK, "{}", first.code());
    // The head is now 2. Everything the old view could read or download is
    // refused with a code of its own, and nothing is disclosed.
    for reply in [
        old.list(&harness).await?,
        old.read(&harness, "notes.txt", (None, None)).await?,
        old.download(&harness, Some("notes.txt")).await?,
        old.download(&harness, None).await?,
    ] {
        assert_eq!(reply.status, StatusCode::CONFLICT);
        assert_eq!(reply.code(), "stale_view");
        let text = String::from_utf8_lossy(&reply.body);
        assert!(!text.contains("notes.txt") && !text.contains(&old.initialization));
    }
    // A repeated save is not a read: it still returns what it committed.
    let replay = old.save(&harness, "head-moves-save-1", &changes).await?;
    assert_eq!(replay.status, StatusCode::OK, "{}", replay.code());
    assert_eq!(replay.json()?["replayed"], json!(true));
    assert_eq!(replay.json()?["revision"], json!(2));
    // A new save from the stale base is still a conflict, as before.
    let stale = old
        .save(
            &harness,
            "head-moves-save-2",
            &json!([put("notes.txt", b"late")]),
        )
        .await?;
    assert_eq!(stale.status, StatusCode::CONFLICT);
    assert_eq!(stale.code(), "revision_conflict");
    assert_eq!(stale.json()?["error"]["current_revision"], json!(2));
    // The user consents again on the new head and a new view works.
    let (status, version) = grant(&harness, &old.session, &id, (2, 2), 0, &ALL).await?;
    assert_eq!(status, StatusCode::OK);
    let view_id = format!("{:032x}", VIEWS.fetch_add(1, Ordering::Relaxed));
    let fresh = view(&harness, &old.session, &id, 2, &view_id, 1, version).await?;
    let read = fresh.read(&harness, "notes.txt", (None, None)).await?;
    assert_eq!(read.status, StatusCode::OK, "{}", read.code());
    assert_eq!(
        BASE64.decode(read.json()?["content"].as_str().ok_or("content")?)?,
        b"before"
    );
    assert_eq!(fresh.download(&harness, None).await?.status, StatusCode::OK);
    // The old view stays refused: its grant was for another head.
    assert_eq!(old.list(&harness).await?.code(), "stale_view");
    Ok(())
}

#[tokio::test]
async fn a_head_moved_by_the_owner_stales_the_view_but_not_the_owners_own_reads() -> TestResult {
    let harness = Harness::new()?;
    let id = artifact(&harness, &["source.read"]).await?;
    let viewer = frame(&harness, &id, 1, &["source.read"]).await?;
    assert_eq!(
        viewer
            .read(&harness, "notes.txt", (None, None))
            .await?
            .status,
        StatusCode::OK
    );
    owner_edit(&harness, &id, 1, "owner-moves-head-2", "newer").await?;
    let read = viewer.read(&harness, "notes.txt", (None, None)).await?;
    assert_eq!(read.status, StatusCode::CONFLICT);
    assert_eq!(read.code(), "stale_view");
    // The owner's own read of the same revision is unaffected.
    assert_eq!(stored(&harness, &id, 1, "notes.txt").await?, text_bytes());
    Ok(())
}

#[tokio::test]
async fn a_source_capability_must_be_declared_by_the_pinned_revision() -> TestResult {
    let harness = Harness::new()?;
    let id = artifact(&harness, &["source.read"]).await?;
    let session = session(&harness).await?;
    // Declared read only: write and download cannot be granted at all.
    for capabilities in [
        vec!["source.write"],
        vec!["source.read", "source.write"],
        vec!["source.download"],
        vec!["source.read", "source.read"],
        vec!["source.upload"],
    ] {
        let (status, _) = grant(&harness, &session, &id, (1, 1), 0, &capabilities).await?;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{capabilities:?}");
    }
    let (status, _) = grant(&harness, &session, &id, (1, 1), 0, &["source.read"]).await?;
    assert_eq!(status, StatusCode::OK);
    // A marker without any manifest declares metadata only (decision 0020).
    let legacy = harness
        .send(post(
            "/v1/artifacts",
            Some("legacy-source-probe"),
            &json!({"title": "Legacy", "files": [upload("index.html", ENTRY.as_bytes())]}),
        )?)
        .await?
        .json()?["artifact"]["id"]
        .as_str()
        .ok_or("id")?
        .to_owned();
    let (status, _) = grant(&harness, &session, &legacy, (1, 1), 0, &["source.read"]).await?;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    Ok(())
}
