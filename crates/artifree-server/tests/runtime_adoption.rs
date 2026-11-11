//! Runtime view adoption (decision 0054) over HTTP, in process, through the
//! real routers, application core, and `SQLite` store. Each test plays the
//! workspace page: it holds the account token, the connection's session, and
//! the frame's secrets, and asks for each adoption the way the page does.
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use artifree_providers::storage::SqliteStore;
use artifree_providers::tokens::SqliteTokenStore;
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

/// Frame write identities must be 8 to 36 letters, digits, `-` or `_`.
const SAVE_ONE: &str = "save-0001";
const SAVE_TWO: &str = "save-0002";
const RETRY: &str = "retry-0001";
const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const CONTENT_ORIGIN: &str = "http://127.0.0.1:7411";
/// The one view identifier the page uses; every test has its own server.
const VIEW: &str = "0123456789abcdef0123456789abcdef";
/// The SDK pin that this server supports.
const SDK: &str = "artifree.app.v1";
const EDITING: [&str; 3] = ["source.read", "source.download", "source.write"];
const READING: [&str; 1] = ["source.read"];
const READ_WRITE: [&str; 2] = ["source.read", "source.write"];
/// The entry document. Its marker opts the page in to the SDK.
const ENTRY: &str =
    "<!doctype html><html lang=\"en\"><head><!-- artifree-runtime --></head><body>x</body></html>";
/// The bytes of `notes.txt` in the first revision.
const DRAFT: &str = "first draft\n";
const SAVED: &str = "<h1>saved by a frame</h1>\n";
const SAVED_AGAIN: &str = "<h1>saved again</h1>\n";
/// Creation keys that stay distinct across the artifacts of one process.
static NEXT_KEY: AtomicU32 = AtomicU32::new(1);

fn plentiful() -> RequestLimits {
    RequestLimits {
        burst: 100_000,
        per_minute: 6_000_000,
        concurrent: 64,
    }
}

/// A server in process over a fresh database file, with its API router and
/// its content router.
struct Harness {
    directory: PathBuf,
    api: Router,
    content: Router,
}

impl Harness {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        Self::with_ttl(Duration::from_secs(60))
    }

    /// A server whose previews and runtime views last `ttl`.
    fn with_ttl(ttl: Duration) -> Result<Self, Box<dyn std::error::Error>> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!(
            "artifree-runtime-adoption-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory)?;
        let database = directory.join("db.sqlite3");
        let store = SqliteStore::open(&database)
            .map_err(|error| format!("cannot open the store: {error}"))?;
        let token_store = SqliteTokenStore::open(&database)
            .map_err(|error| format!("cannot open the token store: {error}"))?;
        let token = AccessToken::parse(TOKEN).ok_or("the fixture token is invalid")?;
        let settings = Settings {
            content_origin: CONTENT_ORIGIN.into(),
            preview_ttl: ttl,
            frame_ancestors: "http://127.0.0.1:*".into(),
            limits: RequestLimits::default(),
            frame_limits: plentiful(),
            change_streams: changes::DEFAULT_STREAMS_PER_ACTOR,
            ..Settings::default()
        };
        let state = AppState::new(store, token_store, token, settings)
            .map_err(|error| format!("cannot build the state: {error}"))?;
        Ok(Self {
            directory,
            api: http::router(state.clone()),
            content: preview::router(state),
        })
    }

    async fn send(&self, request: Request<Body>) -> Result<Reply, Box<dyn std::error::Error>> {
        Reply::capture(self.api.clone().oneshot(request).await?).await
    }

    /// A request to a preview URL of this server.
    async fn content(&self, url: &str) -> Result<Reply, Box<dyn std::error::Error>> {
        let path = url
            .strip_prefix(CONTENT_ORIGIN)
            .ok_or("the preview URL uses another origin")?;
        let request = Request::get(path).body(Body::empty())?;
        Reply::capture(self.content.clone().oneshot(request).await?).await
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// A response, read to its end.
struct Reply {
    status: StatusCode,
    body: Vec<u8>,
}

impl Reply {
    async fn capture(
        response: axum::response::Response,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await?.to_vec();
        Ok(Self { status, body })
    }

    fn json(&self) -> Result<Value, serde_json::Error> {
        serde_json::from_slice(&self.body)
    }

    /// The error code of an answer, for a message that names the failure.
    fn code(&self) -> String {
        match self.json() {
            Ok(body) => body["error"]["code"]
                .as_str()
                .unwrap_or("no error code")
                .to_owned(),
            Err(_) => format!("not JSON: {:?}", String::from_utf8_lossy(&self.body)),
        }
    }
}

fn authorized(builder: axum::http::request::Builder) -> axum::http::request::Builder {
    builder.header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
}

fn get(path: &str) -> Result<Request<Body>, axum::http::Error> {
    authorized(Request::get(path)).body(Body::empty())
}

fn post(path: &str, body: &Value) -> Result<Request<Body>, axum::http::Error> {
    authorized(Request::post(path))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
}

fn post_keyed(path: &str, key: &str, body: &Value) -> Result<Request<Body>, axum::http::Error> {
    authorized(Request::post(path))
        .header(header::CONTENT_TYPE, "application/json")
        .header("idempotency-key", key)
        .body(Body::from(body.to_string()))
}

fn grant_request(
    session: &str,
    id: &str,
    revision: u64,
    head: u64,
    expected_version: u64,
    capabilities: &[&str],
) -> Result<Request<Body>, axum::http::Error> {
    authorized(Request::put(format!(
        "/v1/runtime-sessions/{session}/artifacts/{id}/revisions/{revision}/grant"
    )))
    .header(header::CONTENT_TYPE, "application/json")
    .body(Body::from(
        json!({"head_revision": head, "expected_version": expected_version, "capabilities": capabilities})
            .to_string(),
    ))
}

fn close_request(generation: u64) -> Result<Request<Body>, axum::http::Error> {
    authorized(Request::delete(format!(
        "/v1/runtime-views/{VIEW}/{generation}"
    )))
    .body(Body::empty())
}

/// The manifest of a revision: its SDK pin, its declared editing set, and its
/// lifecycle when one is named (an absent lifecycle means restart).
fn manifest(sdk: &str, lifecycle: Option<&str>) -> String {
    let value = match lifecycle {
        Some(lifecycle) => json!({"sdk": sdk, "capabilities": EDITING, "lifecycle": lifecycle}),
        None => json!({"sdk": sdk, "capabilities": EDITING}),
    };
    value.to_string()
}

fn text(view: &Value, field: &str) -> Result<String, Box<dyn std::error::Error>> {
    let value = view[field]
        .as_str()
        .ok_or(format!("the answer has no text field {field}"))?;
    Ok(value.to_owned())
}

fn number(view: &Value, field: &str) -> Result<u64, Box<dyn std::error::Error>> {
    let value = view[field]
        .as_u64()
        .ok_or(format!("the answer has no number field {field}"))?;
    Ok(value)
}

/// The capabilities an answer grants, sorted by name.
fn granted_of(view: &Value) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut names = Vec::new();
    for name in view["granted"]
        .as_array()
        .ok_or("the answer's grants are not a list")?
    {
        names.push(name.as_str().ok_or("a grant is not text")?.to_owned());
    }
    names.sort();
    Ok(names)
}

/// Creates an artifact whose manifest is `manifest_text`, and returns its id.
async fn create_artifact(
    harness: &Harness,
    manifest_text: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let body = json!({
        "title": "Adoption probe",
        "files": [
            {"path": "index.html", "encoding": "utf8", "content": ENTRY},
            {"path": "artifree-runtime.json", "encoding": "utf8", "content": manifest_text},
            {"path": "notes.txt", "encoding": "utf8", "content": DRAFT}
        ]
    });
    let key = format!(
        "create-adoption-{}",
        NEXT_KEY.fetch_add(1, Ordering::Relaxed)
    );
    let reply = harness
        .send(post_keyed("/v1/artifacts", &key, &body)?)
        .await?;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.code());
    let created = reply.json()?;
    text(&created["artifact"], "id")
}

async fn open_session(harness: &Harness) -> Result<String, Box<dyn std::error::Error>> {
    let reply = harness
        .send(post("/v1/runtime-sessions", &json!({}))?)
        .await?;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.code());
    text(&reply.json()?, "session_id")
}

/// The user sets `capabilities` for `revision` at `head`, expecting `expected`
/// as the scope's current version. Returns the new version.
async fn set_grant(
    harness: &Harness,
    session: &str,
    id: &str,
    revision: u64,
    head: u64,
    expected: u64,
    capabilities: &[&str],
) -> Result<u64, Box<dyn std::error::Error>> {
    let reply = harness
        .send(grant_request(
            session,
            id,
            revision,
            head,
            expected,
            capabilities,
        )?)
        .await?;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.code());
    number(&reply.json()?, "version")
}

/// What the page holds for one live frame: the artifact, the connection, the
/// generation and secret that its calls carry, and the revision they address.
#[derive(Clone)]
struct Frame {
    id: String,
    session: String,
    url: String,
    revision: u64,
    generation: u64,
    initialization: String,
    grant_version: u64,
}

impl Frame {
    fn route(&self, tail: &str) -> String {
        format!(
            "/v1/artifacts/{}/revisions/{}/runtime-views/{VIEW}/{}/{tail}",
            self.id, self.revision, self.generation
        )
    }

    /// A call's body: the initialization secret and the call's own fields.
    fn body(&self, fields: Value) -> Value {
        let mut body = serde_json::Map::new();
        body.insert("initialization".to_owned(), json!(self.initialization));
        if let Value::Object(fields) = fields {
            body.extend(fields);
        }
        Value::Object(body)
    }
}

/// The frame that an answer describes, on the connection `session`.
fn frame_of(id: &str, session: &str, view: &Value) -> Result<Frame, Box<dyn std::error::Error>> {
    Ok(Frame {
        id: id.to_owned(),
        session: session.to_owned(),
        url: text(view, "url")?,
        revision: number(view, "revision")?,
        generation: number(view, "generation")?,
        initialization: text(view, "initialization")?,
        grant_version: number(view, "grant_version")?,
    })
}

/// The frame addressed to `revision` instead of its own.
fn at(frame: &Frame, revision: u64) -> Frame {
    Frame {
        revision,
        ..frame.clone()
    }
}

/// Asks for a view of `revision` at `generation`. With `from`, the request
/// adopts that generation of the same view.
async fn ask_view(
    harness: &Harness,
    id: &str,
    session: &str,
    revision: u64,
    generation: u64,
    from: Option<u64>,
) -> Result<Reply, Box<dyn std::error::Error>> {
    let body = match from {
        Some(from) => json!({
            "session_id": session,
            "view_id": VIEW,
            "generation": generation,
            "adopt": {"generation": from}
        }),
        None => json!({"session_id": session, "view_id": VIEW, "generation": generation}),
    };
    harness
        .send(post(
            &format!("/v1/artifacts/{id}/revisions/{revision}/runtime-views"),
            &body,
        )?)
        .await
}

/// Issues a view and serves its document, as a frame's first call does.
async fn issue(
    harness: &Harness,
    id: &str,
    session: &str,
    revision: u64,
    generation: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    let reply = ask_view(harness, id, session, revision, generation, None).await?;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.code());
    let view = reply.json()?;
    let served = harness.content(&text(&view, "url")?).await?;
    assert_eq!(
        served.status,
        StatusCode::OK,
        "the document: {}",
        served.code()
    );
    Ok(view)
}

/// A frame of revision 1 that holds `granted` at head 1, with its view issued
/// and its document served.
async fn start(
    harness: &Harness,
    manifest_text: &str,
    granted: &[&str],
) -> Result<Frame, Box<dyn std::error::Error>> {
    let id = create_artifact(harness, manifest_text).await?;
    let session = open_session(harness).await?;
    set_grant(harness, &session, &id, 1, 1, 0, granted).await?;
    let view = issue(harness, &id, &session, 1, 1).await?;
    frame_of(&id, &session, &view)
}

/// The page asks for `frame`'s generation to advance to `generation` on
/// `revision`.
async fn adopt_reply(
    harness: &Harness,
    frame: &Frame,
    revision: u64,
    generation: u64,
) -> Result<Reply, Box<dyn std::error::Error>> {
    ask_view(
        harness,
        &frame.id,
        &frame.session,
        revision,
        generation,
        Some(frame.generation),
    )
    .await
}

/// An accepted adoption: the frame it leaves and the answer. The view keeps its
/// ticket, so its URL stays, and it gets a new initialization secret.
async fn adopt(
    harness: &Harness,
    frame: &Frame,
    revision: u64,
    generation: u64,
) -> Result<(Frame, Value), Box<dyn std::error::Error>> {
    let reply = adopt_reply(harness, frame, revision, generation).await?;
    assert_eq!(
        reply.status,
        StatusCode::CREATED,
        "the adoption to generation {generation}: {}",
        reply.code()
    );
    let view = reply.json()?;
    let next = frame_of(&frame.id, &frame.session, &view)?;
    assert_eq!(
        next.url, frame.url,
        "an adopted view keeps its ticket and URL"
    );
    assert_ne!(
        next.initialization, frame.initialization,
        "an adopted view has a new initialization secret"
    );
    Ok((next, view))
}

/// Asserts a refusal under decision 0054, rule 4: `409` with `invalid_request`.
fn assert_refused(reply: &Reply, case: &str) {
    assert_eq!(
        reply.status,
        StatusCode::CONFLICT,
        "{case}: the refusal must answer 409, got {}",
        reply.code()
    );
    assert_eq!(reply.code(), "invalid_request", "{case}");
}

/// The page asks for an adoption that rule 4 refuses, and the refusal is asserted.
async fn refuse(
    harness: &Harness,
    frame: &Frame,
    revision: u64,
    generation: u64,
    case: &str,
) -> TestResult {
    let reply = adopt_reply(harness, frame, revision, generation).await?;
    assert_refused(&reply, case);
    Ok(())
}

/// The status and error code of a read of `notes.txt` through `frame`, for a
/// before-and-after comparison.
async fn read_answer(
    harness: &Harness,
    frame: &Frame,
) -> Result<(StatusCode, String), Box<dyn std::error::Error>> {
    let reply = read(harness, frame, "notes.txt").await?;
    Ok((reply.status, reply.code()))
}

/// Refuses an adoption, and asserts that the old view answers a read as it did
/// before: a refusal changes nothing (decision 0054, rule 4).
async fn refuse_keeping_answer(
    harness: &Harness,
    frame: &Frame,
    revision: u64,
    generation: u64,
    case: &str,
) -> TestResult {
    let before = read_answer(harness, frame).await?;
    refuse(harness, frame, revision, generation, case).await?;
    let after = read_answer(harness, frame).await?;
    assert_eq!(
        after, before,
        "{case}: the old view must answer as it did before"
    );
    Ok(())
}

/// The frame reads `path` at its own revision and generation.
async fn read(
    harness: &Harness,
    frame: &Frame,
    path: &str,
) -> Result<Reply, Box<dyn std::error::Error>> {
    let body = frame.body(json!({"path": path, "offset": 0, "length": 8192}));
    harness
        .send(post(&frame.route("source/read"), &body)?)
        .await
}

/// The whole of `path` as the frame reads it. The file must fit one part.
async fn read_bytes(
    harness: &Harness,
    frame: &Frame,
    path: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let reply = read(harness, frame, path).await?;
    assert_eq!(reply.status, StatusCode::OK, "{path}: {}", reply.code());
    let part = reply.json()?;
    Ok(BASE64.decode(text(&part, "content")?)?)
}

/// The frame saves `content` at `path` under the retry identity `retry`.
async fn save(
    harness: &Harness,
    frame: &Frame,
    retry: &str,
    path: &str,
    content: &str,
) -> Result<Reply, Box<dyn std::error::Error>> {
    let body = frame.body(json!({
        "retry_id": retry,
        "path": path,
        "content": BASE64.encode(content),
    }));
    harness
        .send(post(&frame.route("source/write"), &body)?)
        .await
}

/// The owner adds `path` at `base`, which moves the artifact's head.
async fn owner_adds(harness: &Harness, id: &str, base: u64, path: &str) -> TestResult {
    let body = json!({
        "base_revision": base,
        "edits": [{
            "op": "put",
            "path": path,
            "encoding": "utf8",
            "content": "an owner's note\n",
            "expect": "absent"
        }]
    });
    let key = format!("owner-edit-{id}-{base}-{path}");
    let reply = harness
        .send(post_keyed(
            &format!("/v1/artifacts/{id}/edits"),
            &key,
            &body,
        )?)
        .await?;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.code());
    Ok(())
}

/// How many revisions the owner sees for the artifact `id`.
async fn revisions(harness: &Harness, id: &str) -> Result<usize, Box<dyn std::error::Error>> {
    let reply = harness
        .send(get(&format!("/v1/artifacts/{id}/revisions"))?)
        .await?;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.code());
    let listed = reply.json()?;
    Ok(listed["revisions"]
        .as_array()
        .ok_or("the revisions are not a list")?
        .len())
}

/// The owner's copy of `path` at `revision`.
async fn stored(
    harness: &Harness,
    id: &str,
    revision: u64,
    path: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let reply = harness
        .send(get(&format!(
            "/v1/artifacts/{id}/revisions/{revision}/files/{path}"
        ))?)
        .await?;
    assert_eq!(
        reply.status,
        StatusCode::OK,
        "{path} at revision {revision}: {}",
        reply.code()
    );
    Ok(reply.body)
}

/// Two causes: the user changes the permissions, then the frame saves. The
/// new generation holds exactly the grant the user just set. After the save it
/// holds nothing, because the head is new (decision 0054, rule 6).
async fn permission_and_save(
    harness: &Harness,
    first: &Frame,
) -> Result<Frame, Box<dyn std::error::Error>> {
    let version = set_grant(
        harness,
        &first.session,
        &first.id,
        1,
        1,
        first.grant_version,
        &READ_WRITE,
    )
    .await?;
    let bound = read(harness, first, "notes.txt").await?;
    assert_eq!(
        bound.status,
        StatusCode::FORBIDDEN,
        "a grant change does not reach a bound view: {}",
        bound.code()
    );
    let (second, view) = adopt(harness, first, 1, 2).await?;
    assert_eq!(
        granted_of(&view)?,
        ["source.read", "source.write"],
        "the adopted view holds the grant the user set"
    );
    assert_eq!(
        view["grant_version"].as_u64(),
        Some(version),
        "the adopted view is bound to the new version"
    );
    let retired = read(harness, first, "notes.txt").await?;
    assert_eq!(
        retired.status,
        StatusCode::NOT_FOUND,
        "the old generation is refused after an adoption: {}",
        retired.code()
    );
    let kept = read(harness, &second, "notes.txt").await?;
    assert_eq!(
        kept.status,
        StatusCode::OK,
        "the adopted generation reads: {}",
        kept.code()
    );
    let saved = save(harness, &second, SAVE_ONE, "notes.txt", SAVED).await?;
    assert_eq!(
        saved.status,
        StatusCode::OK,
        "the frame saves: {}",
        saved.code()
    );
    assert_eq!(saved.json()?["revision"], 2);
    let (third, view) = adopt(harness, &second, 2, 3).await?;
    assert!(
        granted_of(&view)?.is_empty(),
        "a new head holds no grant (decision 0054, rule 6)"
    );
    assert_eq!(view["grant_version"].as_u64(), Some(0));
    let unpermitted = read(harness, &third, "notes.txt").await?;
    assert_eq!(
        unpermitted.status,
        StatusCode::FORBIDDEN,
        "the adopted save holds no grant: {}",
        unpermitted.code()
    );
    Ok(third)
}

/// Two causes: another writer moves the head, then the lease renews. The new
/// generation keeps the pinned revision and holds no grant at the new head.
/// Once the user allows reading there, a lease renewal keeps that grant.
async fn head_and_lease(harness: &Harness, third: &Frame) -> TestResult {
    owner_adds(harness, &third.id, 2, "owner-note.txt").await?;
    let (fourth, view) = adopt(harness, third, 2, 4).await?;
    assert_eq!(
        view["head_revision"].as_u64(),
        Some(3),
        "the head is the other writer's revision"
    );
    assert_eq!(
        view["revision"].as_u64(),
        Some(2),
        "the view keeps the revision it pins"
    );
    assert!(granted_of(&view)?.is_empty(), "a new head holds no grant");
    let version = set_grant(harness, &fourth.session, &fourth.id, 2, 3, 0, &READING).await?;
    let (fifth, view) = adopt(harness, &fourth, 2, 5).await?;
    assert_eq!(granted_of(&view)?, ["source.read"]);
    assert_eq!(view["grant_version"].as_u64(), Some(version));
    let (sixth, view) = adopt(harness, &fifth, 2, 6).await?;
    assert_eq!(
        granted_of(&view)?,
        ["source.read"],
        "a lease renewal keeps the grant"
    );
    assert_eq!(
        view["grant_version"].as_u64(),
        Some(version),
        "a lease renewal keeps the version"
    );
    let bytes = read_bytes(harness, &sixth, "notes.txt").await?;
    assert_eq!(bytes, SAVED.as_bytes(), "the pinned revision's bytes");
    let retired = read(harness, &fifth, "notes.txt").await?;
    assert_eq!(
        retired.status,
        StatusCode::NOT_FOUND,
        "a superseded generation is refused: {}",
        retired.code()
    );
    Ok(())
}

/// Every cause adopts the live frame: the user changes its permissions, the
/// frame saves, another writer moves the head, and the lease renews
/// (decision 0054, rule 8).
#[tokio::test]
async fn each_cause_adopts_the_live_frame() -> TestResult {
    let harness = Harness::new()?;
    let first = start(&harness, &manifest(SDK, Some("adopt")), &EDITING).await?;
    let third = permission_and_save(&harness, &first).await?;
    head_and_lease(&harness, &third).await
}

/// A saved adoption pins the saved revision and keeps the document, which
/// stays the document revision's. The page's second save works once the user
/// allows editing again and the next generation holds it (rules 5 to 7).
#[tokio::test]
async fn a_saved_adoption_keeps_the_document_and_a_second_save_works() -> TestResult {
    let harness = Harness::new()?;
    let first = start(&harness, &manifest(SDK, Some("adopt")), &EDITING).await?;
    let saved = save(&harness, &first, SAVE_ONE, "notes.txt", SAVED).await?;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.code());
    let (second, view) = adopt(&harness, &first, 2, 2).await?;
    assert!(
        granted_of(&view)?.is_empty(),
        "the saved revision's head holds no grant"
    );

    // The document is served once, and its files keep the document revision.
    let again = harness.content(&first.url).await?;
    assert_eq!(
        again.status,
        StatusCode::NOT_FOUND,
        "the document is not served twice: {}",
        again.code()
    );
    let file = harness.content(&format!("{}notes.txt", first.url)).await?;
    assert_eq!(file.status, StatusCode::OK, "{}", file.code());
    assert_eq!(
        file.body,
        DRAFT.as_bytes(),
        "the document keeps revision 1's bytes"
    );

    // The calls follow the pinned revision: revision 2 is judged, revision 1 is not.
    let pinned = read(&harness, &at(&second, 2), "notes.txt").await?;
    assert_eq!(
        pinned.status,
        StatusCode::FORBIDDEN,
        "revision 2 is the pinned one: {}",
        pinned.code()
    );
    let document_revision = read(&harness, &at(&second, 1), "notes.txt").await?;
    assert_eq!(
        document_revision.status,
        StatusCode::NOT_FOUND,
        "revision 1 is no longer pinned: {}",
        document_revision.code()
    );

    // The user allows editing again. The bound view does not take the grant up,
    // and the generation adopted after it holds it.
    let version = set_grant(&harness, &second.session, &second.id, 2, 2, 0, &EDITING).await?;
    let stale = read(&harness, &second, "notes.txt").await?;
    assert_eq!(
        stale.status,
        StatusCode::FORBIDDEN,
        "a bound view does not take up a later grant: {}",
        stale.code()
    );
    let (third, view) = adopt(&harness, &second, 2, 3).await?;
    assert_eq!(
        granted_of(&view)?,
        ["source.download", "source.read", "source.write"]
    );
    assert_eq!(view["grant_version"].as_u64(), Some(version));

    let second_save = save(&harness, &third, SAVE_TWO, "notes.txt", SAVED_AGAIN).await?;
    assert_eq!(
        second_save.status,
        StatusCode::OK,
        "the second save: {}",
        second_save.code()
    );
    assert_eq!(second_save.json()?["revision"], 3);
    assert_eq!(
        stored(&harness, &first.id, 3, "notes.txt").await?,
        SAVED_AGAIN.as_bytes()
    );
    assert_eq!(
        stored(&harness, &first.id, 2, "notes.txt").await?,
        SAVED.as_bytes()
    );
    let file = harness.content(&format!("{}notes.txt", first.url)).await?;
    assert_eq!(
        file.body,
        DRAFT.as_bytes(),
        "the document still shows revision 1"
    );
    Ok(())
}

/// After another writer moves the head past the frame's own save, the
/// adoption onto that save still works, also after a head adoption came
/// first. A revision that the view neither pins nor wrote is refused.
#[tokio::test]
async fn adoption_onto_the_own_save_after_another_writer() -> TestResult {
    let harness = Harness::new()?;
    let first = start(&harness, &manifest(SDK, Some("adopt")), &EDITING).await?;
    let saved = save(&harness, &first, SAVE_ONE, "notes.txt", SAVED).await?;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.code());
    owner_adds(&harness, &first.id, 2, "owner-note.txt").await?;

    // A head adoption comes first. It keeps the pinned revision, and with it
    // the record of the save.
    let (second, view) = adopt(&harness, &first, 1, 2).await?;
    assert_eq!(view["revision"].as_u64(), Some(1));
    assert_eq!(view["head_revision"].as_u64(), Some(3));

    // Then the adoption onto the frame's own save, which the other writer's
    // revision has passed.
    let (third, view) = adopt(&harness, &second, 2, 3).await?;
    assert_eq!(
        view["revision"].as_u64(),
        Some(2),
        "the view pins its own save"
    );
    assert_eq!(view["head_revision"].as_u64(), Some(3));

    // The other writer's revision was never written by this view.
    refuse(&harness, &third, 3, 4, "the revision of another writer").await?;
    let unchanged = read(&harness, &third, "notes.txt").await?;
    assert_eq!(
        unchanged.status,
        StatusCode::FORBIDDEN,
        "the refused view is as it was (no grant at the new head): {}",
        unchanged.code()
    );
    Ok(())
}

async fn closed_generation_is_refused() -> TestResult {
    let harness = Harness::new()?;
    let first = start(&harness, &manifest(SDK, Some("adopt")), &EDITING).await?;
    let closed = harness.send(close_request(1)?).await?;
    assert_eq!(closed.status, StatusCode::NO_CONTENT, "{}", closed.code());
    refuse(&harness, &first, 1, 2, "a closed generation").await?;
    let retired = read(&harness, &first, "notes.txt").await?;
    assert_eq!(
        retired.status,
        StatusCode::NOT_FOUND,
        "a closed view answers nothing: {}",
        retired.code()
    );
    Ok(())
}

async fn unserved_document_is_refused() -> TestResult {
    let harness = Harness::new()?;
    let id = create_artifact(&harness, &manifest(SDK, Some("adopt"))).await?;
    let session = open_session(&harness).await?;
    set_grant(&harness, &session, &id, 1, 1, 0, &EDITING).await?;
    let reply = ask_view(&harness, &id, &session, 1, 1, None).await?;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.code());
    let frame = frame_of(&id, &session, &reply.json()?)?;
    refuse(
        &harness,
        &frame,
        1,
        2,
        "a generation whose document was not served",
    )
    .await?;

    // The refusal served nothing, so the document is still to be served once.
    let served = harness.content(&frame.url).await?;
    assert_eq!(
        served.status,
        StatusCode::OK,
        "the refused view's document is unchanged: {}",
        served.code()
    );
    let answered = read(&harness, &frame, "notes.txt").await?;
    assert_eq!(
        answered.status,
        StatusCode::OK,
        "the refused view still answers: {}",
        answered.code()
    );
    Ok(())
}

async fn generation_that_is_not_live_is_refused() -> TestResult {
    let harness = Harness::new()?;
    let first = start(&harness, &manifest(SDK, Some("adopt")), &EDITING).await?;
    // The live view is generation 1, so generation 2 is not live. The request
    // is otherwise well formed: 2 < 3, and 3 is above the fence of 1.
    let reply = ask_view(&harness, &first.id, &first.session, 1, 3, Some(2)).await?;
    assert_refused(&reply, "a generation that is not live");
    let unchanged = read(&harness, &first, "notes.txt").await?;
    assert_eq!(
        unchanged.status,
        StatusCode::OK,
        "the live view is as it was: {}",
        unchanged.code()
    );
    // The refusal raised no fence, so the correct adoption still succeeds.
    adopt(&harness, &first, 1, 2).await?;
    Ok(())
}

async fn other_connection_is_refused() -> TestResult {
    let harness = Harness::new()?;
    let first = start(&harness, &manifest(SDK, Some("adopt")), &EDITING).await?;
    let other = open_session(&harness).await?;
    let reply = ask_view(&harness, &first.id, &other, 1, 2, Some(1)).await?;
    assert_refused(&reply, "another connection");
    let unchanged = read(&harness, &first, "notes.txt").await?;
    assert_eq!(
        unchanged.status,
        StatusCode::OK,
        "the view on its own connection is as it was: {}",
        unchanged.code()
    );
    adopt(&harness, &first, 1, 2).await?;
    Ok(())
}

async fn other_artifact_is_refused() -> TestResult {
    let harness = Harness::new()?;
    let first = start(&harness, &manifest(SDK, Some("adopt")), &EDITING).await?;
    let other = create_artifact(&harness, &manifest(SDK, Some("adopt"))).await?;
    let reply = ask_view(&harness, &other, &first.session, 1, 2, Some(1)).await?;
    assert_refused(&reply, "another artifact");
    let unchanged = read(&harness, &first, "notes.txt").await?;
    assert_eq!(
        unchanged.status,
        StatusCode::OK,
        "the view of the first artifact is as it was: {}",
        unchanged.code()
    );
    adopt(&harness, &first, 1, 2).await?;
    Ok(())
}

async fn restart_view_is_refused() -> TestResult {
    let harness = Harness::new()?;
    let first = start(&harness, &manifest(SDK, None), &EDITING).await?;
    refuse(
        &harness,
        &first,
        1,
        2,
        "a view whose manifest does not say adopt",
    )
    .await?;
    let unchanged = read(&harness, &first, "notes.txt").await?;
    assert_eq!(
        unchanged.status,
        StatusCode::OK,
        "the restart view is as it was: {}",
        unchanged.code()
    );
    Ok(())
}

/// The view was issued as restart. The frame later saves a revision whose
/// manifest says adopt, and that revision is refused for the view's own
/// lifecycle.
async fn saved_adopt_manifest_does_not_adopt_a_restart_view() -> TestResult {
    let harness = Harness::new()?;
    let first = start(&harness, &manifest(SDK, None), &EDITING).await?;
    let saved = save(
        &harness,
        &first,
        SAVE_ONE,
        "artifree-runtime.json",
        &manifest(SDK, Some("adopt")),
    )
    .await?;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.code());
    // The frame's own save moved the head, so the view is stale: it answers
    // the same before the refusal and after it.
    refuse_keeping_answer(&harness, &first, 2, 2, "a view issued as restart").await?;
    Ok(())
}

/// The view was issued as adopt. The frame later saves a revision whose
/// manifest says restart, and that revision is refused for its own lifecycle.
async fn saved_restart_manifest_does_not_keep_an_adopt_view() -> TestResult {
    let harness = Harness::new()?;
    let first = start(&harness, &manifest(SDK, Some("adopt")), &EDITING).await?;
    let saved = save(
        &harness,
        &first,
        SAVE_ONE,
        "artifree-runtime.json",
        &manifest(SDK, None),
    )
    .await?;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.code());
    // The frame's own save moved the head, so the view is stale: it answers
    // the same before the refusal and after it.
    refuse_keeping_answer(
        &harness,
        &first,
        2,
        2,
        "a saved revision whose manifest says restart",
    )
    .await?;
    Ok(())
}

/// A saved revision whose SDK pin differs from this server's is not adoptable,
/// and the refusal is `409`, not the `422` that reading the revision gives
/// (decision 0054, rule 16).
async fn changed_sdk_pin_is_refused() -> TestResult {
    let harness = Harness::new()?;
    let first = start(&harness, &manifest(SDK, Some("adopt")), &EDITING).await?;
    let saved = save(
        &harness,
        &first,
        SAVE_ONE,
        "artifree-runtime.json",
        &manifest("artifree.app.v2", Some("adopt")),
    )
    .await?;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.code());
    // The frame's own save moved the head, so the view is stale: it answers
    // the same before the refusal and after it.
    refuse_keeping_answer(&harness, &first, 2, 2, "a changed SDK pin").await?;
    Ok(())
}

async fn unpinned_unwritten_revision_is_refused() -> TestResult {
    let harness = Harness::new()?;
    let first = start(&harness, &manifest(SDK, Some("adopt")), &EDITING).await?;
    owner_adds(&harness, &first.id, 1, "owner-note.txt").await?;
    // The owner's edit moved the head, so the view is stale: it answers the
    // same before the refusal and after it.
    refuse_keeping_answer(
        &harness,
        &first,
        2,
        2,
        "a revision the view neither pins nor wrote",
    )
    .await?;
    Ok(())
}

/// Decision 0054, rule 4: every refusal answers `409 invalid_request` and
/// leaves the view it refused as it was. Each case builds its own server.
#[tokio::test]
async fn every_refusal_leaves_the_old_view_as_it_was() -> TestResult {
    closed_generation_is_refused().await?;
    unserved_document_is_refused().await?;
    generation_that_is_not_live_is_refused().await?;
    other_connection_is_refused().await?;
    other_artifact_is_refused().await?;
    restart_view_is_refused().await?;
    saved_adopt_manifest_does_not_adopt_a_restart_view().await?;
    saved_restart_manifest_does_not_keep_an_adopt_view().await?;
    changed_sdk_pin_is_refused().await?;
    unpinned_unwritten_revision_is_refused().await
}

/// An expired generation cannot be adopted, and the expired view answers
/// nothing (decision 0054, rule 4).
#[tokio::test]
async fn an_expired_generation_cannot_be_adopted() -> TestResult {
    let harness = Harness::with_ttl(Duration::from_secs(2))?;
    let first = start(&harness, &manifest(SDK, Some("adopt")), &EDITING).await?;
    // The lease is two seconds from issue; this wait only ever lengthens it.
    tokio::time::sleep(Duration::from_millis(2_200)).await;
    refuse(&harness, &first, 1, 2, "an expired generation").await?;
    let expired = read(&harness, &first, "notes.txt").await?;
    assert_eq!(
        expired.status,
        StatusCode::NOT_FOUND,
        "an expired view answers nothing: {}",
        expired.code()
    );
    Ok(())
}

/// A create or a close for a generation below the adopted one is refused or
/// a no-op, and neither touches the live view (decision 0054, rule 5).
#[tokio::test]
async fn late_create_and_close_below_the_adopted_generation() -> TestResult {
    let harness = Harness::new()?;
    let first = start(&harness, &manifest(SDK, Some("adopt")), &EDITING).await?;
    let (second, _) = adopt(&harness, &first, 1, 2).await?;

    let late = ask_view(&harness, &first.id, &first.session, 1, 1, None).await?;
    assert_refused(&late, "a late create below the adopted generation");

    let closed = harness.send(close_request(1)?).await?;
    assert_eq!(
        closed.status,
        StatusCode::NO_CONTENT,
        "a late close is a no-op: {}",
        closed.code()
    );
    let kept = read(&harness, &second, "notes.txt").await?;
    assert_eq!(
        kept.status,
        StatusCode::OK,
        "the adopted generation survives a late close: {}",
        kept.code()
    );
    let retired = read(&harness, &first, "notes.txt").await?;
    assert_eq!(
        retired.status,
        StatusCode::NOT_FOUND,
        "the generation below is gone: {}",
        retired.code()
    );
    Ok(())
}

/// Rule 7: a repeat of a frame's write is replayed before an adoption, and
/// after one it is a new write: refused without the grant, and stale against
/// the view's base where the first one committed.
#[tokio::test]
async fn a_write_repeated_under_the_new_generation_is_a_new_write() -> TestResult {
    let harness = Harness::new()?;
    let first = start(&harness, &manifest(SDK, Some("adopt")), &EDITING).await?;
    let written = save(&harness, &first, RETRY, "notes.txt", SAVED).await?;
    assert_eq!(written.status, StatusCode::OK, "{}", written.code());

    let replayed = save(&harness, &first, RETRY, "notes.txt", SAVED).await?;
    assert_eq!(
        replayed.status,
        StatusCode::OK,
        "a repeat before an adoption is replayed: {}",
        replayed.code()
    );
    assert_eq!(
        replayed.json()?,
        written.json()?,
        "the first outcome comes back"
    );
    assert_eq!(revisions(&harness, &first.id).await?, 2);

    // The head moved and the view keeps its revision: it holds no grant at
    // the new head.
    let (second, _) = adopt(&harness, &first, 1, 2).await?;
    let denied = save(&harness, &second, RETRY, "notes.txt", SAVED).await?;
    assert_eq!(
        denied.status,
        StatusCode::FORBIDDEN,
        "a repeat after an adoption is a new write, refused without the grant: {}",
        denied.code()
    );
    assert_eq!(revisions(&harness, &first.id).await?, 2);

    // Granted at the new head, the same repeat is judged as a new write: its
    // base is the pinned revision 1, and the head is revision 2.
    let version = set_grant(&harness, &second.session, &second.id, 1, 2, 0, &EDITING).await?;
    let (third, view) = adopt(&harness, &second, 1, 3).await?;
    assert_eq!(view["grant_version"].as_u64(), Some(version));
    let stale = save(&harness, &third, RETRY, "notes.txt", SAVED).await?;
    assert_eq!(
        stale.status,
        StatusCode::CONFLICT,
        "the repeat is stale where the first one committed: {}",
        stale.code()
    );
    assert_eq!(stale.code(), "revision_conflict");
    assert_eq!(stale.json()?["error"]["current_revision"], 2);
    assert_eq!(
        revisions(&harness, &first.id).await?,
        2,
        "nothing was saved"
    );
    Ok(())
}
