//! Multi-user identity (decision 0036), exercised in process through the real
//! router, MCP endpoint, application core, and `SQLite` store.
//!
//! Two users, Alice and Bob, and the owner profile each hold their own
//! credentials. The tests prove that one of them cannot read, list, write,
//! count, or tell apart from nothing what another holds, that only the owner
//! manages users, and that a disabled user loses every way in.
//!
//! The isolation tables walk every artifact-scoped route and tool. A test
//! reads the route declarations of the server and fails when a route is in
//! neither a table nor the short list of routes that are not artifact-scoped,
//! so a new route cannot ship without an isolation case.

use std::collections::BTreeSet;
use std::future::poll_fn;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use artifree_contracts::api::{
    CreateTokenResponse, RotateTokenResponse, TokenList, UserInfo, UserList,
};
use artifree_providers::storage::SqliteStore;
use artifree_providers::tokens::SqliteTokenStore;
use artifree_server::auth::AccessToken;
use artifree_server::http::{self, AppState, Settings};
use artifree_server::limits::RequestLimits;
use artifree_server::{changes, preview};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use http_body::Body as _;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tower::ServiceExt;

type TestError = Box<dyn std::error::Error>;
type TestResult = Result<(), TestError>;

const OWNER: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const CONTENT_ORIGIN: &str = "http://127.0.0.1:7411";
const PROTOCOL: &str = "2025-11-25";
const ENTRY: &str =
    "<!doctype html><html lang=\"en\"><head><!-- artifree-runtime --></head><body>x</body></html>";
const CAPABILITIES: [&str; 4] = [
    "metadata.read",
    "source.read",
    "source.download",
    "source.write",
];

struct Harness {
    directory: PathBuf,
    api: Router,
    content: Router,
}

impl Harness {
    fn new() -> Self {
        Self::with(|_| {})
    }

    fn with(adjust: impl FnOnce(&mut Settings)) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!(
            "artifree-users-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory)
            .unwrap_or_else(|error| panic!("cannot create test directory: {error}"));
        let database = directory.join("db.sqlite3");
        let mut settings = Settings {
            content_origin: CONTENT_ORIGIN.into(),
            preview_ttl: Duration::from_secs(60),
            frame_ancestors: "http://127.0.0.1:*".into(),
            limits: RequestLimits {
                burst: 100_000,
                per_minute: 6_000_000,
                concurrent: 64,
            },
            change_streams: changes::DEFAULT_STREAMS_PER_ACTOR,
            ..Settings::default()
        };
        adjust(&mut settings);
        let state = AppState::new(
            SqliteStore::open(&database)
                .unwrap_or_else(|error| panic!("cannot open store: {error}")),
            SqliteTokenStore::open(&database)
                .unwrap_or_else(|error| panic!("cannot open token store: {error}")),
            AccessToken::parse(OWNER).unwrap_or_else(|| panic!("fixture token is invalid")),
            settings,
        )
        .unwrap_or_else(|error| panic!("cannot build state: {error}"));
        Self {
            directory,
            // The real server records the socket peer; with none, /mcp refuses.
            api: http::router(state.clone()).layer(axum::Extension(ConnectInfo(SocketAddr::from(
                ([127, 0, 0, 1], 40_000),
            )))),
            content: preview::router(state),
        }
    }

    async fn send(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        key: Option<&str>,
        body: Option<String>,
    ) -> Result<Reply, TestError> {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        if let Some(key) = key {
            builder = builder.header("idempotency-key", key);
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

    /// Sends a request that must answer `status`, and returns its body.
    async fn expect<T: DeserializeOwned>(
        &self,
        status: u16,
        method: &str,
        path: &str,
        token: &str,
        key: Option<&str>,
        body: Option<Value>,
    ) -> Result<T, TestError> {
        let reply = self
            .send(
                method,
                path,
                Some(token),
                key,
                body.map(|body| body.to_string()),
            )
            .await?;
        assert_eq!(
            reply.status.as_u16(),
            status,
            "{method} {path}: {}",
            reply.text
        );
        Ok(serde_json::from_str(&reply.text)?)
    }

    /// The status of a request whose body does not matter.
    async fn status(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
    ) -> Result<u16, TestError> {
        Ok(self
            .send(method, path, token, None, None)
            .await?
            .status
            .as_u16())
    }

    async fn serve(&self, url: &str) -> Result<Reply, TestError> {
        let path = url.strip_prefix(CONTENT_ORIGIN).ok_or("another origin")?;
        let response = self
            .content
            .clone()
            .oneshot(Request::get(path).body(Body::empty())?)
            .await?;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await?;
        Ok(Reply {
            status,
            text: String::from_utf8_lossy(&bytes).into_owned(),
        })
    }

    /// A user the owner registers, with one token.
    async fn person(&self, label: &str) -> Result<Person, TestError> {
        let user: UserInfo = self
            .expect(
                201,
                "POST",
                "/v1/users",
                OWNER,
                None,
                Some(json!({ "label": label })),
            )
            .await?;
        let issued = self.issue(Some(&user.id), label).await?;
        Ok(Person {
            id: user.id,
            secret: issued.secret,
            token_id: issued.token.id,
        })
    }

    /// A token the owner issues, to `user` or to the owner profile.
    async fn issue(
        &self,
        user: Option<&str>,
        name: &str,
    ) -> Result<CreateTokenResponse, TestError> {
        let mut body = json!({ "name": name });
        if let Some(user) = user {
            body["user_id"] = json!(user);
        }
        self.expect(201, "POST", "/v1/tokens", OWNER, None, Some(body))
            .await
    }

    async fn rpc(
        &self,
        token: Option<&str>,
        message: &Value,
    ) -> Result<(StatusCode, String), TestError> {
        let mut builder = Request::post("/mcp")
            .header(header::HOST, "127.0.0.1:7410")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header("mcp-protocol-version", PROTOCOL);
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let response = self
            .api
            .clone()
            .oneshot(builder.body(Body::from(message.to_string()))?)
            .await?;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await?;
        Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
    }

    /// Calls a tool and returns the JSON-RPC result.
    async fn tool(&self, token: &str, name: &str, arguments: Value) -> Result<Value, TestError> {
        let (status, text) = self
            .rpc(
                Some(token),
                &json!({
                    "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": { "name": name, "arguments": arguments }
                }),
            )
            .await?;
        assert_eq!(status, StatusCode::OK, "{name}: {text}");
        let reply: Value = serde_json::from_str(&text)?;
        Ok(reply["result"].clone())
    }

    async fn tool_names(&self, token: &str) -> Result<BTreeSet<String>, TestError> {
        let (status, text) = self
            .rpc(
                Some(token),
                &json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
            )
            .await?;
        assert_eq!(status, StatusCode::OK, "{text}");
        let reply: Value = serde_json::from_str(&text)?;
        Ok(reply["result"]["tools"]
            .as_array()
            .ok_or("tools")?
            .iter()
            .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
            .collect())
    }

    async fn open_stream(&self, token: &str) -> Result<Stream, TestError> {
        let response = self
            .api
            .clone()
            .oneshot(
                Request::get("/v1/changes")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let mut stream = Stream {
            body: response.into_body(),
            seen: String::new(),
        };
        stream.until("event: ready").await?;
        Ok(stream)
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

struct Reply {
    status: StatusCode,
    text: String,
}

struct Person {
    id: String,
    secret: String,
    token_id: String,
}

/// An open change stream, read as text.
struct Stream {
    body: Body,
    seen: String,
}

impl Stream {
    /// Reads until `needle` has arrived; fails if the stream ends first.
    async fn until(&mut self, needle: &str) -> TestResult {
        while !self.seen.contains(needle) {
            match tokio::time::timeout(
                Duration::from_secs(5),
                poll_fn(|context| Pin::new(&mut self.body).poll_frame(context)),
            )
            .await
            .map_err(|_| format!("no {needle:?} within 5 s; saw {:?}", self.seen))?
            {
                None => return Err(format!("the stream ended before {needle:?}").into()),
                Some(frame) => {
                    if let Ok(data) = frame?.into_data() {
                        self.seen.push_str(std::str::from_utf8(&data)?);
                    }
                }
            }
        }
        Ok(())
    }

    /// Whether the server ends the stream within a few seconds.
    async fn ended(&mut self) -> Result<bool, TestError> {
        loop {
            let next = tokio::time::timeout(
                Duration::from_secs(5),
                poll_fn(|context| Pin::new(&mut self.body).poll_frame(context)),
            )
            .await;
            match next {
                Err(_) => return Ok(false),
                Ok(None) => return Ok(true),
                Ok(Some(frame)) => {
                    if let Ok(data) = frame?.into_data() {
                        self.seen.push_str(std::str::from_utf8(&data)?);
                    }
                }
            }
        }
    }

    /// Takes in whatever arrives in a moment.
    async fn settle(&mut self) -> TestResult {
        while let Ok(Some(frame)) = tokio::time::timeout(
            Duration::from_millis(300),
            poll_fn(|context| Pin::new(&mut self.body).poll_frame(context)),
        )
        .await
        {
            if let Ok(data) = frame?.into_data() {
                self.seen.push_str(std::str::from_utf8(&data)?);
            }
        }
        Ok(())
    }

    /// The numbers of the `id: feed.number` lines seen.
    fn numbers(&self) -> Vec<u64> {
        self.seen
            .lines()
            .filter_map(|line| line.strip_prefix("id: "))
            .filter_map(|id| id.rsplit_once('.')?.1.parse().ok())
            .collect()
    }
}

// What one user holds ------------------------------------------------------

/// The names of what a user holds, which another user guesses or learns.
#[derive(Clone)]
struct Ids {
    artifact: String,
    trashed: String,
    trashed_base: u64,
    job: String,
    session: String,
    view: String,
    init: String,
    handoff: String,
    agent: String,
    agent_secret: String,
    sha: String,
    participant: u64,
    epoch: u64,
}

impl Ids {
    /// Names with the right spelling that no one holds.
    fn absent() -> Self {
        Self {
            artifact: format!("art_{}", "0".repeat(32)),
            trashed: format!("art_{}", "1".repeat(32)),
            trashed_base: 2,
            job: format!("job_{}", "0".repeat(32)),
            session: "0".repeat(32),
            view: "f".repeat(32),
            init: "0".repeat(64),
            handoff: "0".repeat(32),
            agent: "0".repeat(32),
            agent_secret: "0".repeat(43),
            sha: "0".repeat(64),
            participant: 1,
            epoch: 1,
        }
    }

    fn fill(&self, template: &str) -> String {
        [
            ("artifact", self.artifact.clone()),
            ("trashed", self.trashed.clone()),
            ("trashed_base", self.trashed_base.to_string()),
            ("job", self.job.clone()),
            ("session", self.session.clone()),
            ("view", self.view.clone()),
            ("init", self.init.clone()),
            ("handoff", self.handoff.clone()),
            ("agent", self.agent.clone()),
            ("agent_secret", self.agent_secret.clone()),
            ("sha", self.sha.clone()),
            ("path", "notes.txt".to_owned()),
            ("participant", self.participant.to_string()),
            ("epoch", self.epoch.to_string()),
        ]
        .iter()
        .fold(template.to_owned(), |text, (name, value)| {
            text.replace(&format!("{{{name}}}"), value)
        })
    }

    /// Hides the names in an answer, so two answers can be compared.
    fn scrub(&self, text: &str) -> String {
        [
            &self.artifact,
            &self.trashed,
            &self.job,
            &self.session,
            &self.view,
            &self.init,
            &self.handoff,
            &self.agent,
            &self.agent_secret,
            &self.sha,
        ]
        .iter()
        .fold(text.to_owned(), |text, id| {
            text.replace(id.as_str(), "<name>")
        })
    }
}

/// What a user stores: two revisions, application data, a job, and an
/// artifact in the trash.
struct Stored {
    artifact: String,
    job: String,
    trashed: String,
    trashed_base: u64,
}

async fn stored_things(harness: &Harness, token: &str, tag: &str) -> Result<Stored, TestError> {
    let manifest = json!({ "sdk": "artifree.app.v1", "capabilities": CAPABILITIES }).to_string();
    let created: Value = harness
        .expect(
            201,
            "POST",
            "/v1/artifacts",
            token,
            Some(format!("{tag}-create-0001").as_str()),
            Some(json!({ "title": format!("{tag} plan"), "files": [
                { "path": "index.html", "encoding": "utf8", "content": ENTRY },
                { "path": "artifree-runtime.json", "encoding": "utf8", "content": manifest },
                { "path": "notes.txt", "encoding": "utf8", "content": "first" }
            ]})),
        )
        .await?;
    let artifact = created["artifact"]["id"]
        .as_str()
        .ok_or("artifact id")?
        .to_owned();
    let _: Value = harness
        .expect(
            201,
            "POST",
            &format!("/v1/artifacts/{artifact}/revisions"),
            token,
            Some(format!("{tag}-save-00001").as_str()),
            Some(json!({ "base_revision": 1, "changes": [
                { "op": "put", "path": "notes.txt", "encoding": "utf8", "content": "second" }
            ]})),
        )
        .await?;
    let _: Value = harness
        .expect(
            200,
            "PUT",
            &format!("/v1/artifacts/{artifact}/data"),
            token,
            Some(format!("{tag}-data-00001").as_str()),
            Some(json!({ "path": "notes", "base_data_version": 0,
                         "value": { "private": format!("{tag}-data") } })),
        )
        .await?;
    let job: Value = harness
        .expect(
            201,
            "POST",
            &format!("/v1/artifacts/{artifact}/jobs"),
            token,
            Some(format!("{tag}-job-000001").as_str()),
            Some(json!({ "kind": "verify_revision", "revision": 1 })),
        )
        .await?;
    let trashed: Value = harness
        .expect(
            201,
            "POST",
            "/v1/artifacts",
            token,
            Some(format!("{tag}-create-0002").as_str()),
            Some(json!({ "title": format!("{tag} old plan"), "files": [
                { "path": "index.html", "encoding": "utf8", "content": ENTRY }
            ]})),
        )
        .await?;
    let trashed = trashed["artifact"]["id"]
        .as_str()
        .ok_or("trashed id")?
        .to_owned();
    let binned: Value = harness
        .expect(
            200,
            "PATCH",
            &format!("/v1/artifacts/{trashed}"),
            token,
            Some(format!("{tag}-trash-0001").as_str()),
            Some(json!({ "base_metadata_revision": 1, "deleted": true })),
        )
        .await?;
    let trashed_base = binned["artifact"]["metadata_revision"]
        .as_u64()
        .ok_or("revision")?;
    Ok(Stored {
        job: job["job"]["id"].as_str().ok_or("job id")?.to_owned(),
        artifact,
        trashed,
        trashed_base,
    })
}

/// A runtime connection with a grant, a served frame view, and a room.
struct Connected {
    session: String,
    view: String,
    init: String,
    participant: u64,
    epoch: u64,
}

async fn connected_things(
    harness: &Harness,
    token: &str,
    artifact: &str,
) -> Result<Connected, TestError> {
    let session: Value = harness
        .expect(200, "POST", "/v1/runtime-sessions", token, None, None)
        .await?;
    let session = session["session_id"].as_str().ok_or("session")?.to_owned();
    let _: Value = harness
        .expect(
            200,
            "PUT",
            &format!("/v1/runtime-sessions/{session}/artifacts/{artifact}/revisions/2/grant"),
            token,
            None,
            Some(
                json!({ "head_revision": 2, "expected_version": 0, "capabilities": CAPABILITIES }),
            ),
        )
        .await?;
    let view = "0123456789abcdef0123456789abcdef".to_owned();
    let issued: Value = harness
        .expect(
            201,
            "POST",
            &format!("/v1/artifacts/{artifact}/revisions/2/runtime-views"),
            token,
            None,
            Some(json!({ "session_id": session, "view_id": view, "generation": 1 })),
        )
        .await?;
    let served = harness
        .serve(issued["url"].as_str().ok_or("view url")?)
        .await?;
    assert_eq!(served.status, StatusCode::OK, "{}", served.text);
    let init = issued["initialization"]
        .as_str()
        .ok_or("initialization")?
        .to_owned();
    let room: Value = harness
        .expect(
            200,
            "POST",
            &format!("/v1/runtime-sessions/{session}/artifacts/{artifact}/room/participants"),
            token,
            None,
            Some(json!({ "slot": "main" })),
        )
        .await?;
    Ok(Connected {
        session,
        view,
        init,
        participant: room["participant"].as_u64().ok_or("participant")?,
        epoch: room["epoch"].as_u64().ok_or("epoch")?,
    })
}

/// Everything a user can hold that has a name: two revisions, application
/// data, a job, a trashed artifact, a runtime connection with a grant, a
/// served frame, a room, and a handoff.
async fn fixture(harness: &Harness, who: &Person, tag: &str) -> Result<Ids, TestError> {
    let token = who.secret.as_str();
    let Stored {
        artifact,
        job,
        trashed,
        trashed_base,
    } = stored_things(harness, token, tag).await?;
    let Connected {
        session,
        view,
        init,
        participant,
        epoch,
    } = connected_things(harness, token, &artifact).await?;
    let agent = harness
        .tool(
            token,
            "register_handoff_session",
            json!({ "name": format!("{tag} agent") }),
        )
        .await?;
    let agent_secret = agent["structuredContent"]["session_secret"]
        .as_str()
        .ok_or("agent secret")?
        .to_owned();
    let agent = agent["structuredContent"]["session_id"]
        .as_str()
        .ok_or("agent")?
        .to_owned();
    let revision: Value = harness
        .expect(
            200,
            "GET",
            &format!("/v1/artifacts/{artifact}/revisions/2"),
            token,
            None,
            None,
        )
        .await?;
    let sha = revision["files"]
        .as_array()
        .and_then(|files| files.iter().find(|file| file["path"] == "index.html"))
        .and_then(|file| file["sha256"].as_str())
        .ok_or("index.html digest")?
        .to_owned();
    let handoff: Value = harness
        .expect(
            201,
            "POST",
            "/v1/handoffs",
            token,
            None,
            Some(
                json!({ "workspace_session_id": session, "agent_session_id": agent,
                         "artifact_id": artifact, "revision": 2, "path": "index.html",
                         "file_sha256": sha, "start": 0, "end": 5, "intent": "Edit" }),
            ),
        )
        .await?;
    Ok(Ids {
        job,
        artifact,
        trashed,
        trashed_base,
        session,
        view,
        init,
        handoff: handoff["id"].as_str().ok_or("handoff id")?.to_owned(),
        agent,
        agent_secret,
        sha,
        participant,
        epoch,
    })
}

// The tables ---------------------------------------------------------------

/// What the holder of the names gets when it calls a case.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Holder {
    /// A success.
    Succeeds,
    /// Anything but not-found or a server error: an earlier case may have
    /// changed the state the call is conditional on.
    Reaches,
    /// The caller's own trash (purge): the holder succeeds, a user or the
    /// administrator who does not hold the name is told it does not exist, and
    /// a token issued to the owner profile is an agent token and is refused
    /// (`403`) before the name is read (decisions 0029 and 0036).
    Purges,

    /// Closing is idempotent: the holder gets a success, and anyone else the
    /// same success, which closes nothing of the holder's.
    Closes,
}

/// What a caller of the isolation table holds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Credential {
    /// The bootstrap token.
    Administrator,
    /// A token issued to a user other than the owner profile.
    User,
    /// A token issued to the owner profile: an agent token (decision 0029).
    OwnerAgent,
}

struct Case {
    name: &'static str,
    method: &'static str,
    route: &'static str,
    body: Option<&'static str>,
    holder: Holder,
}

const fn case(
    name: &'static str,
    method: &'static str,
    route: &'static str,
    body: Option<&'static str>,
    holder: Holder,
) -> Case {
    Case {
        name,
        method,
        route,
        body,
        holder,
    }
}

const ROOM_ROUTE: &str = "/v1/runtime-sessions/{session}/artifacts/{artifact}/room/participants";

/// Every artifact-scoped route of the HTTP API, in an order in which the
/// holder's own calls keep working: reads, the frame, the room, then writes.
/// The `route` text is what the route inventory compares with the server.
#[expect(
    clippy::too_many_lines,
    reason = "A table of every route reads best as one list."
)]
fn http_cases() -> Vec<Case> {
    use Holder::{Closes, Purges, Reaches, Succeeds};
    vec![
        case("show", "GET", "/v1/artifacts/{artifact}", None, Succeeds),
        case(
            "revisions",
            "GET",
            "/v1/artifacts/{artifact}/revisions",
            None,
            Succeeds,
        ),
        case(
            "revision",
            "GET",
            "/v1/artifacts/{artifact}/revisions/2",
            None,
            Succeeds,
        ),
        case(
            "file",
            "GET",
            "/v1/artifacts/{artifact}/revisions/2/files/{path}",
            None,
            Succeeds,
        ),
        case(
            "export",
            "GET",
            "/v1/artifacts/{artifact}/revisions/2/export",
            None,
            Succeeds,
        ),
        case(
            "artifact jobs",
            "GET",
            "/v1/artifacts/{artifact}/jobs",
            None,
            Succeeds,
        ),
        case("job", "GET", "/v1/jobs/{job}", None, Succeeds),
        case(
            "data",
            "GET",
            "/v1/artifacts/{artifact}/data",
            None,
            Succeeds,
        ),
        case(
            "data members",
            "GET",
            "/v1/artifacts/{artifact}/data/members",
            None,
            Succeeds,
        ),
        case(
            "agent sessions",
            "GET",
            "/v1/agent-sessions?workspace_session_id={session}",
            None,
            Succeeds,
        ),
        case(
            "handoff",
            "GET",
            "/v1/handoffs/{handoff}?workspace_session_id={session}",
            None,
            Succeeds,
        ),
        case(
            "preview",
            "POST",
            "/v1/artifacts/{artifact}/revisions/2/previews",
            None,
            Succeeds,
        ),
        case(
            "room join",
            "POST",
            ROOM_ROUTE,
            Some(r#"{"slot":"second"}"#),
            Succeeds,
        ),
        case(
            "room heartbeat",
            "POST",
            "/v1/runtime-sessions/{session}/artifacts/{artifact}/room/participants/{participant}/{epoch}/heartbeat",
            Some(r#"{"after":0}"#),
            Succeeds,
        ),
        case(
            "room event",
            "POST",
            "/v1/runtime-sessions/{session}/artifacts/{artifact}/room/participants/{participant}/{epoch}/events",
            Some(r#"{"kind":"cursor","data":"x"}"#),
            Succeeds,
        ),
        case(
            "frame metadata",
            "POST",
            "/v1/artifacts/{artifact}/revisions/2/runtime-views/{view}/1/metadata",
            Some(r#"{"initialization":"{init}"}"#),
            Succeeds,
        ),
        case(
            "frame source list",
            "POST",
            "/v1/artifacts/{artifact}/revisions/2/runtime-views/{view}/1/source/list",
            Some(r#"{"initialization":"{init}"}"#),
            Succeeds,
        ),
        case(
            "frame source read",
            "POST",
            "/v1/artifacts/{artifact}/revisions/2/runtime-views/{view}/1/source/read",
            Some(r#"{"initialization":"{init}","path":"notes.txt","offset":0,"length":16}"#),
            Succeeds,
        ),
        case(
            "frame source offer",
            "POST",
            "/v1/artifacts/{artifact}/revisions/2/runtime-views/{view}/1/source/offer",
            Some(r#"{"initialization":"{init}","kind":"file","path":"index.html"}"#),
            Succeeds,
        ),
        case(
            "frame source download",
            "POST",
            "/v1/artifacts/{artifact}/revisions/2/runtime-views/{view}/1/source/download",
            Some(r#"{"initialization":"{init}","kind":"file","path":"index.html"}"#),
            Succeeds,
        ),
        case(
            "frame source write",
            "POST",
            "/v1/artifacts/{artifact}/revisions/2/runtime-views/{view}/1/source/write",
            Some(
                r#"{"initialization":"{init}","retry_id":"retry-0001","path":"notes.txt","content":"Zm9v"}"#,
            ),
            Reaches,
        ),
        case(
            "runtime view",
            "POST",
            "/v1/artifacts/{artifact}/revisions/2/runtime-views",
            Some(r#"{"session_id":"{session}","view_id":"{view}","generation":2}"#),
            Reaches,
        ),
        case(
            "runtime grant",
            "PUT",
            "/v1/runtime-sessions/{session}/artifacts/{artifact}/revisions/2/grant",
            Some(r#"{"head_revision":2,"expected_version":1,"capabilities":["source.read"]}"#),
            Reaches,
        ),
        case(
            "handoff stage",
            "POST",
            "/v1/handoffs",
            Some(
                r#"{"workspace_session_id":"{session}","agent_session_id":"{agent}","artifact_id":"{artifact}","revision":2,"path":"index.html","file_sha256":"{sha}","start":0,"end":5,"intent":"Edit"}"#,
            ),
            Reaches,
        ),
        case(
            "handoff send",
            "POST",
            "/v1/handoffs/{handoff}/send",
            Some(r#"{"workspace_session_id":"{session}"}"#),
            Reaches,
        ),
        case(
            "handoff cancel",
            "POST",
            "/v1/handoffs/{handoff}/cancel",
            Some(r#"{"workspace_session_id":"{session}"}"#),
            Reaches,
        ),
        case(
            "enqueue job",
            "POST",
            "/v1/artifacts/{artifact}/jobs",
            Some(r#"{"kind":"verify_revision","revision":1}"#),
            Succeeds,
        ),
        case(
            "cancel job",
            "POST",
            "/v1/jobs/{job}/cancel",
            Some("{}"),
            Succeeds,
        ),
        case(
            "set data",
            "PUT",
            "/v1/artifacts/{artifact}/data",
            Some(r#"{"path":"notes","base_data_version":1,"value":{"taken":true}}"#),
            Succeeds,
        ),
        case(
            "update data",
            "PATCH",
            "/v1/artifacts/{artifact}/data",
            Some(r#"{"path":"notes","base_data_version":2,"patch":{"more":1}}"#),
            Succeeds,
        ),
        case(
            "delete data",
            "DELETE",
            "/v1/artifacts/{artifact}/data",
            Some(r#"{"path":"notes","base_data_version":3}"#),
            Succeeds,
        ),
        case(
            "update artifact",
            "PATCH",
            "/v1/artifacts/{artifact}",
            Some(r#"{"base_metadata_revision":1,"title":"Taken"}"#),
            Succeeds,
        ),
        case(
            "save revision",
            "POST",
            "/v1/artifacts/{artifact}/revisions",
            Some(r#"{"base_revision":2,"changes":[{"op":"delete","path":"notes.txt"}]}"#),
            Reaches,
        ),
        case(
            "edit files",
            "POST",
            "/v1/artifacts/{artifact}/edits",
            Some(
                r#"{"base_revision":2,"edits":[{"op":"put","path":"new.txt","encoding":"utf8","content":"x","expect":"absent"}]}"#,
            ),
            Reaches,
        ),
        case(
            "restore",
            "POST",
            "/v1/artifacts/{artifact}/revisions/1/restore",
            Some(r#"{"base_revision":2}"#),
            Reaches,
        ),
        case(
            "duplicate",
            "POST",
            "/v1/artifacts/{artifact}/revisions/1/duplicate",
            Some("{}"),
            Succeeds,
        ),
        case(
            "purge",
            "POST",
            "/v1/artifacts/{trashed}/purge",
            Some(r#"{"base_metadata_revision":{trashed_base}}"#),
            Purges,
        ),
        case(
            "room leave",
            "POST",
            "/v1/runtime-sessions/{session}/artifacts/{artifact}/room/participants/{participant}/{epoch}/leave",
            Some("{}"),
            Reaches,
        ),
        case(
            "close view",
            "DELETE",
            "/v1/runtime-views/{view}/2",
            None,
            Closes,
        ),
        case(
            "close session",
            "DELETE",
            "/v1/runtime-sessions/{session}",
            None,
            Closes,
        ),
    ]
}

/// Routes that name no artifact. Each has its own test: listing and creating
/// are scoped to the caller, the rest are administrative.
const NOT_ARTIFACT_SCOPED: [&str; 12] = [
    "/mcp",
    "/v1/artifacts",
    "/v1/changes",
    "/v1/content/unreferenced",
    "/v1/jobs",
    "/v1/runtime-sessions",
    "/v1/tokens",
    "/v1/tokens/{}",
    "/v1/tokens/{}/rotate",
    "/v1/users",
    "/v1/users/{}/disable",
    "/v1/users/{}/enable",
];

/// Writes a route's parameters, and the numbers the cases put in their
/// place, as `{}`, so names do not matter.
fn normal(route: &str) -> String {
    let path = route.split('?').next().unwrap_or(route);
    path.split('/')
        .map(|part| {
            let number = !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
            if part.starts_with('{') || number {
                "{}"
            } else {
                part
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// The route patterns that the server's modules declare, read from their
/// source text: a literal on a `.route(` line or the line after it, and the
/// two patterns that the frame and room routes are built from.
fn declared_routes() -> BTreeSet<String> {
    let mut routes = BTreeSet::new();
    let mut prefixes = std::collections::BTreeMap::new();
    for source in [
        include_str!("../src/http.rs"),
        include_str!("../src/tokens.rs"),
        include_str!("../src/users.rs"),
    ] {
        let lines: Vec<&str> = source.lines().collect();
        for (number, line) in lines.iter().enumerate() {
            let text = line.trim();
            for name in ["ROOM", "VIEW"] {
                if let Some(rest) = text.strip_prefix(&format!("const {name}: &str = \"")) {
                    prefixes.insert(name, rest.trim_end_matches("\";").to_owned());
                }
            }
            if let (true, Some(prefix)) = (line.contains(".route(ROOM,"), prefixes.get("ROOM")) {
                routes.insert(normal(prefix));
            }
            let declares = line.contains(".route(")
                || number
                    .checked_sub(1)
                    .and_then(|before| lines.get(before))
                    .is_some_and(|before| before.contains(".route("));
            for literal in line.split('"').skip(1).step_by(2) {
                if declares && (literal.starts_with("/v1") || literal.starts_with("/mcp")) {
                    routes.insert(normal(literal));
                }
                for name in ["ROOM", "VIEW"] {
                    if let (Some(tail), Some(prefix)) = (
                        literal.strip_prefix(&format!("{{{name}}}")),
                        prefixes.get(name),
                    ) {
                        let tail = tail.replace("{{", "{").replace("}}", "}");
                        routes.insert(normal(&format!("{prefix}{tail}")));
                    }
                }
            }
        }
    }
    // The MCP path is a constant of its module.
    routes.insert("/mcp".to_owned());
    routes
}

#[test]
fn every_declared_route_is_in_an_isolation_table_or_not_artifact_scoped() {
    let declared = declared_routes();
    let listed: BTreeSet<String> = http_cases()
        .iter()
        .map(|case| normal(case.route))
        .chain(
            NOT_ARTIFACT_SCOPED
                .iter()
                .filter(|route| route.starts_with('/'))
                .map(|route| (*route).to_owned()),
        )
        .collect();
    let unclassified: Vec<&String> = declared.difference(&listed).collect();
    assert!(
        unclassified.is_empty(),
        "routes with no isolation case and not listed as other than artifact-scoped: {unclassified:?}"
    );
    let stale: Vec<&String> = listed.difference(&declared).collect();
    assert!(
        stale.is_empty(),
        "cases for routes the server no longer declares: {stale:?}"
    );
    assert!(
        declared.len() > 40,
        "the route reader found only {} routes: {declared:?}",
        declared.len()
    );
}

/// Others try every case with Alice's names and with names nobody holds.
/// Each answer is the same, and it is the answer for nothing.
async fn others_find_nothing(
    harness: &Harness,
    ids: &Ids,
    others: &[(&str, &str, Credential)],
) -> TestResult {
    let absent = Ids::absent();
    for (who, token, credential) in others {
        for (number, case) in http_cases().iter().enumerate() {
            let mut answers = Vec::new();
            for names in [ids, &absent] {
                let body = case.body.map(|body| names.fill(body));
                let key = format!("{}-case-{number:03}", who.replace(' ', "-"));
                let key = (case.method != "GET").then_some(key.as_str());
                let reply = harness
                    .send(case.method, &names.fill(case.route), Some(token), key, body)
                    .await?;
                answers.push((reply.status, names.scrub(&reply.text)));
            }
            let expected = match case.holder {
                Holder::Purges if *credential == Credential::OwnerAgent => StatusCode::FORBIDDEN,
                Holder::Closes => StatusCode::NO_CONTENT,
                _ => StatusCode::NOT_FOUND,
            };
            assert_eq!(
                answers[0].0, expected,
                "{who}: {} ({} {}): {}",
                case.name, case.method, case.route, answers[0].1
            );
            assert_eq!(
                answers[0], answers[1],
                "{who}: {} {} tells Alice's names from names nobody holds",
                case.method, case.route
            );
        }
    }
    Ok(())
}

/// Nothing the attempts did reached Alice's state.
async fn state_is_untouched(harness: &Harness, alice: &Person, ids: &Ids) -> TestResult {
    let artifact: Value = harness
        .expect(
            200,
            "GET",
            &format!("/v1/artifacts/{}", ids.artifact),
            &alice.secret,
            None,
            None,
        )
        .await?;
    assert_eq!(artifact["current_revision"], 2);
    assert_eq!(artifact["metadata_revision"], 1);
    assert_eq!(artifact["title"], "alice plan");
    let data: Value = harness
        .expect(
            200,
            "GET",
            &format!("/v1/artifacts/{}/data?path=notes", ids.artifact),
            &alice.secret,
            None,
            None,
        )
        .await?;
    assert_eq!(data["data_version"], 1);
    let trash: Value = harness
        .expect(
            200,
            "GET",
            "/v1/artifacts?deleted=true",
            &alice.secret,
            None,
            None,
        )
        .await?;
    assert_eq!(
        trash["artifacts"][0]["id"], ids.trashed,
        "the trash lost its artifact: {trash}"
    );
    let job: Value = harness
        .expect(
            200,
            "GET",
            &format!("/v1/jobs/{}", ids.job),
            &alice.secret,
            None,
            None,
        )
        .await?;
    assert_eq!(job["state"], "queued");

    Ok(())
}

/// Alice, who holds the names, is not told they are missing. The table is in
/// an order in which her own calls keep working.
async fn the_holder_is_not_refused(harness: &Harness, alice: &Person, ids: &Ids) -> TestResult {
    for (number, case) in http_cases().iter().enumerate() {
        let key = format!("alice-case-{number:03}");
        let reply = harness
            .send(
                case.method,
                &ids.fill(case.route),
                Some(&alice.secret),
                (case.method != "GET").then_some(key.as_str()),
                case.body.map(|body| ids.fill(body)),
            )
            .await?;
        match case.holder {
            Holder::Succeeds | Holder::Closes | Holder::Purges => assert!(
                reply.status.is_success(),
                "alice: {} ({} {}): {} {}",
                case.name,
                case.method,
                case.route,
                reply.status,
                reply.text
            ),
            Holder::Reaches => assert!(
                reply.status != StatusCode::NOT_FOUND && !reply.status.is_server_error(),
                "alice: {} ({} {}): {} {}",
                case.name,
                case.method,
                case.route,
                reply.status,
                reply.text
            ),
        }
    }
    Ok(())
}

#[tokio::test]
async fn nobody_else_can_reach_what_a_user_holds_over_http() -> TestResult {
    let harness = Harness::new();
    let alice = harness.person("alice").await?;
    let bob = harness.person("bob").await?;
    let agent = harness.issue(None, "owner agent").await?;
    let ids = fixture(&harness, &alice, "alice").await?;
    others_find_nothing(
        &harness,
        &ids,
        &[
            ("bob", bob.secret.as_str(), Credential::User),
            ("owner", OWNER, Credential::Administrator),
            ("owner agent", agent.secret.as_str(), Credential::OwnerAgent),
        ],
    )
    .await?;
    state_is_untouched(&harness, &alice, &ids).await?;
    the_holder_is_not_refused(&harness, &alice, &ids).await
}

/// Creates an artifact for `token` and moves it to the trash; returns its id
/// and the metadata revision the purge must name.
async fn trashed_artifact(
    harness: &Harness,
    token: &str,
    tag: &str,
) -> Result<(String, u64), TestError> {
    let created: Value = harness
        .expect(
            201,
            "POST",
            "/v1/artifacts",
            token,
            Some(format!("{tag}-create").as_str()),
            Some(json!({ "title": tag, "files": [
                { "path": "index.html", "encoding": "utf8", "content": ENTRY }
            ]})),
        )
        .await?;
    let id = created["artifact"]["id"].as_str().ok_or("id")?.to_owned();
    let binned: Value = harness
        .expect(
            200,
            "PATCH",
            &format!("/v1/artifacts/{id}"),
            token,
            Some(format!("{tag}-trash").as_str()),
            Some(json!({ "base_metadata_revision": 1, "deleted": true })),
        )
        .await?;
    let base = binned["artifact"]["metadata_revision"]
        .as_u64()
        .ok_or("revision")?;
    Ok((id, base))
}

#[tokio::test]
async fn nobody_else_can_purge_what_a_user_trashed() -> TestResult {
    let harness = Harness::new();
    let alice = harness.person("alice").await?;
    let bob = harness.person("bob").await?;
    let (id, base) = trashed_artifact(&harness, &alice.secret, "alice-old").await?;
    let path = format!("/v1/artifacts/{id}/purge");
    let body = json!({ "base_metadata_revision": base });

    // Bob cannot purge Alice's trashed artifact, and the answer is the one for
    // a name nobody holds; nothing of Alice's changed.
    let foreign = harness
        .send(
            "POST",
            &path,
            Some(&bob.secret),
            Some("bob-purge-1"),
            Some(body.to_string()),
        )
        .await?;
    let nothing = format!("/v1/artifacts/art_{}/purge", "2".repeat(32));
    let missing = harness
        .send(
            "POST",
            &nothing,
            Some(&bob.secret),
            Some("bob-purge-2"),
            Some(body.to_string()),
        )
        .await?;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND, "{}", foreign.text);
    assert_eq!(
        foreign.text.replace(&id, "<name>"),
        missing.text.replace(
            &nothing["/v1/artifacts/".len()..nothing.len() - "/purge".len()],
            "<name>"
        ),
        "a foreign trashed artifact answers like a missing one"
    );
    let trash: Value = harness
        .expect(
            200,
            "GET",
            "/v1/artifacts?deleted=true",
            &alice.secret,
            None,
            None,
        )
        .await?;
    assert_eq!(
        trash["artifacts"][0]["id"],
        id.as_str(),
        "still in the trash"
    );

    // The owner profile's administrator does not reach a user's artifact.
    let admin = harness
        .send(
            "POST",
            &path,
            Some(OWNER),
            Some("admin-purge-1"),
            Some(body.to_string()),
        )
        .await?;
    assert_eq!(admin.status, StatusCode::NOT_FOUND, "{}", admin.text);

    Ok(())
}

#[tokio::test]
async fn a_user_purges_their_own_trashed_artifact_under_the_usual_rules() -> TestResult {
    let harness = Harness::new();
    let alice = harness.person("alice").await?;
    let bob = harness.person("bob").await?;
    let (id, base) = trashed_artifact(&harness, &alice.secret, "alice-old").await?;
    let (bobs, bobs_base) = trashed_artifact(&harness, &bob.secret, "bob-old").await?;
    let path = format!("/v1/artifacts/{id}/purge");
    let body = json!({ "base_metadata_revision": base });

    // A stale base is a conflict that removes nothing.
    let stale = harness
        .send(
            "POST",
            &path,
            Some(&alice.secret),
            Some("alice-purge-stale"),
            Some(json!({ "base_metadata_revision": base + 1 }).to_string()),
        )
        .await?;
    assert_eq!(stale.status, StatusCode::CONFLICT, "{}", stale.text);

    // Alice purges her own, and a retry with the same key replays.
    let first: Value = harness
        .expect(
            200,
            "POST",
            &path,
            &alice.secret,
            Some("alice-purge-1"),
            Some(body.clone()),
        )
        .await?;
    assert_eq!(first["replayed"], false);
    let again: Value = harness
        .expect(
            200,
            "POST",
            &path,
            &alice.secret,
            Some("alice-purge-1"),
            Some(body),
        )
        .await?;
    assert_eq!(again["replayed"], true);
    assert_eq!(again["purged_at_ms"], first["purged_at_ms"]);
    assert_eq!(
        harness
            .status("GET", &format!("/v1/artifacts/{id}"), Some(&alice.secret))
            .await?,
        404,
        "a purged artifact is missing"
    );

    // Bob's trashed artifact was not touched by any of it.
    let bobs_trash: Value = harness
        .expect(
            200,
            "GET",
            "/v1/artifacts?deleted=true",
            &bob.secret,
            None,
            None,
        )
        .await?;
    assert_eq!(bobs_trash["artifacts"][0]["id"], bobs.as_str());
    assert_eq!(bobs_trash["artifacts"][0]["metadata_revision"], bobs_base);
    Ok(())
}

#[tokio::test]
async fn a_user_cannot_purge_an_artifact_outside_the_trash_and_cannot_clean_up_content()
-> TestResult {
    let harness = Harness::new();
    let alice = harness.person("alice").await?;
    let live: Value = harness
        .expect(
            201,
            "POST",
            "/v1/artifacts",
            &alice.secret,
            Some("alice-live"),
            Some(json!({ "title": "live", "files": [
                { "path": "index.html", "encoding": "utf8", "content": ENTRY }
            ]})),
        )
        .await?;
    let id = live["artifact"]["id"].as_str().ok_or("id")?;
    let refused = harness
        .send(
            "POST",
            &format!("/v1/artifacts/{id}/purge"),
            Some(&alice.secret),
            Some("alice-purge-live"),
            Some(json!({ "base_metadata_revision": 1 }).to_string()),
        )
        .await?;
    assert_eq!(refused.status, StatusCode::CONFLICT, "{}", refused.text);
    assert!(refused.text.contains("not_in_trash"), "{}", refused.text);

    // Content cleanup is global, so it stays with the administrator.
    assert_eq!(
        harness
            .status("GET", "/v1/content/unreferenced", Some(&alice.secret))
            .await?,
        403
    );
    let apply = harness
        .send(
            "DELETE",
            "/v1/content/unreferenced",
            Some(&alice.secret),
            Some("alice-cleanup"),
            Some(json!({ "base_digest": "0".repeat(64) }).to_string()),
        )
        .await?;
    assert_eq!(apply.status, StatusCode::FORBIDDEN, "{}", apply.text);
    assert_eq!(
        harness
            .status("GET", "/v1/content/unreferenced", Some(OWNER))
            .await?,
        200
    );
    Ok(())
}

/// The tools that name an artifact or a session, with their arguments for the
/// names `ids` holds.
fn tool_cases(ids: &Ids) -> Vec<(&'static str, Value)> {
    let artifact = &ids.artifact;
    vec![
        ("get_artifact", json!({ "artifact_id": artifact })),
        ("list_revisions", json!({ "artifact_id": artifact })),
        (
            "get_revision",
            json!({ "artifact_id": artifact, "revision": 2 }),
        ),
        (
            "read_file",
            json!({ "artifact_id": artifact, "revision": 2, "path": "notes.txt" }),
        ),
        (
            "create_preview",
            json!({ "artifact_id": artifact, "revision": 2 }),
        ),
        (
            "get_data",
            json!({ "artifact_id": artifact, "path": "notes" }),
        ),
        ("list_jobs", json!({ "artifact_id": artifact })),
        ("get_job", json!({ "job_id": ids.job })),
        (
            "enqueue_job",
            json!({ "artifact_id": artifact, "revision": 1, "kind": "verify_revision",
                    "idempotency_key": "tool-job-00001" }),
        ),
        (
            "cancel_job",
            json!({ "job_id": ids.job, "idempotency_key": "tool-cancel-0001" }),
        ),
        (
            "set_data",
            json!({ "artifact_id": artifact, "path": "notes", "base_data_version": 1,
                    "value": "{\"taken\":true}", "idempotency_key": "tool-data-00001" }),
        ),
        (
            "update_data",
            json!({ "artifact_id": artifact, "path": "notes", "base_data_version": 2,
                    "patch": "{\"more\":1}", "idempotency_key": "tool-data-00002" }),
        ),
        (
            "delete_data",
            json!({ "artifact_id": artifact, "path": "notes", "base_data_version": 3,
                    "idempotency_key": "tool-data-00003" }),
        ),
        (
            "update_artifact",
            json!({ "artifact_id": artifact, "base_metadata_revision": 1, "title": "Taken",
                    "idempotency_key": "tool-meta-00001" }),
        ),
        (
            "save_revision",
            json!({ "artifact_id": artifact, "base_revision": 2,
                    "changes": [{ "op": "delete", "path": "notes.txt" }],
                    "idempotency_key": "tool-save-00001" }),
        ),
        (
            "edit_files",
            json!({ "artifact_id": artifact, "base_revision": 2,
                    "edits": [{ "op": "put", "path": "new.txt", "encoding": "utf8",
                                "content": "x", "expect": "absent" }],
                    "idempotency_key": "tool-edit-00001" }),
        ),
        (
            "restore_revision",
            json!({ "artifact_id": artifact, "revision": 1, "base_revision": 2,
                    "idempotency_key": "tool-restore-001" }),
        ),
        (
            "duplicate_artifact",
            json!({ "artifact_id": artifact, "revision": 1, "idempotency_key": "tool-copy-00001" }),
        ),
        (
            "poll_handoffs",
            json!({ "session_id": ids.agent, "session_secret": ids.agent_secret }),
        ),
        (
            "acknowledge_handoff",
            json!({ "session_id": ids.agent, "session_secret": ids.agent_secret,
                    "delivery_id": ids.handoff }),
        ),
    ]
}

/// Tools that name nothing: they act on the caller's own artifacts.
const OWN_TOOLS: [&str; 3] = [
    "list_artifacts",
    "create_artifact",
    "register_handoff_session",
];

#[tokio::test]
async fn nobody_else_can_reach_what_a_user_holds_through_every_tool() -> TestResult {
    let harness = Harness::new();
    let alice = harness.person("alice").await?;
    let bob = harness.person("bob").await?;
    let ids = fixture(&harness, &alice, "alice").await?;
    let absent = Ids::absent();

    // The catalog and the table agree: a new tool needs a case or a place in
    // the list of tools that act on the caller's own artifacts.
    let catalog = harness.tool_names(&alice.secret).await?;
    let tabled: BTreeSet<String> = tool_cases(&ids)
        .into_iter()
        .map(|(name, _)| name.to_owned())
        .chain(OWN_TOOLS.iter().map(|name| (*name).to_owned()))
        .collect();
    assert_eq!(
        catalog, tabled,
        "the tool catalog and the isolation table differ"
    );

    for (who, token) in [("bob", bob.secret.as_str()), ("owner", OWNER)] {
        for ((name, real), (_, none)) in tool_cases(&ids).into_iter().zip(tool_cases(&absent)) {
            let first = harness.tool(token, name, real).await?;
            let second = harness.tool(token, name, none).await?;
            assert_eq!(first["isError"], true, "{who}: {name}: {first}");
            assert_eq!(
                first["structuredContent"]["error"]["code"], "not_found",
                "{who}: {name}: {first}"
            );
            assert_eq!(
                ids.scrub(&first.to_string()),
                absent.scrub(&second.to_string()),
                "{who}: {name} tells Alice's names from names nobody holds"
            );
        }
    }
    for (name, arguments) in tool_cases(&ids) {
        if name == "poll_handoffs" || name == "acknowledge_handoff" {
            continue;
        }
        let result = harness.tool(&alice.secret, name, arguments).await?;
        assert_ne!(
            result["structuredContent"]["error"]["code"], "not_found",
            "alice: {name}: {result}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn listings_counts_and_feeds_hold_only_the_callers_own() -> TestResult {
    let harness = Harness::new();
    let alice = harness.person("alice").await?;
    let bob = harness.person("bob").await?;
    let ids = fixture(&harness, &alice, "alice").await?;

    for (who, token) in [("bob", bob.secret.as_str()), ("owner", OWNER)] {
        let listed: Value = harness
            .expect(200, "GET", "/v1/artifacts", token, None, None)
            .await?;
        assert_eq!(listed["artifacts"], json!([]), "{who}");
        assert!(listed.get("next_page_token").is_none(), "{who}: {listed}");
        let trash: Value = harness
            .expect(200, "GET", "/v1/artifacts?deleted=true", token, None, None)
            .await?;
        assert_eq!(
            trash["artifacts"],
            json!([]),
            "{who} sees a trash that is not its own"
        );
        let jobs: Value = harness
            .expect(200, "GET", "/v1/jobs", token, None, None)
            .await?;
        assert_eq!(jobs["jobs"], json!([]), "{who}");
        let sessions = harness
            .send(
                "GET",
                &format!("/v1/agent-sessions?workspace_session_id={}", ids.session),
                Some(token),
                None,
                None,
            )
            .await?;
        assert_eq!(sessions.status, StatusCode::NOT_FOUND, "{who}");
        let tool = harness.tool(token, "list_artifacts", json!({})).await?;
        assert_eq!(tool["structuredContent"]["artifacts"], json!([]), "{who}");
        let tool = harness.tool(token, "list_jobs", json!({})).await?;
        assert_eq!(tool["structuredContent"]["jobs"], json!([]), "{who}");
    }
    let mine: Value = harness
        .expect(200, "GET", "/v1/artifacts", &alice.secret, None, None)
        .await?;
    assert_eq!(
        mine["artifacts"].as_array().map(Vec::len),
        Some(1),
        "{mine}"
    );
    let trash: Value = harness
        .expect(
            200,
            "GET",
            "/v1/artifacts?deleted=true",
            &alice.secret,
            None,
            None,
        )
        .await?;
    assert_eq!(
        trash["artifacts"].as_array().map(Vec::len),
        Some(1),
        "{trash}"
    );

    Ok(())
}

#[tokio::test]
async fn each_user_has_a_change_feed_of_its_own() -> TestResult {
    let harness = Harness::new();
    let alice = harness.person("alice").await?;
    let bob = harness.person("bob").await?;
    let ids = fixture(&harness, &alice, "alice").await?;

    // Bob hears none of Alice's commits and his first change is number 1,
    // though Alice's feed is several long.
    let mut bob_stream = harness.open_stream(&bob.secret).await?;
    let mut alice_stream = harness.open_stream(&alice.secret).await?;
    let more: Value = harness
        .expect(
            201,
            "POST",
            "/v1/artifacts",
            &alice.secret,
            Some("alice-create-0003"),
            Some(json!({ "title": "More", "files": [
                { "path": "index.html", "encoding": "utf8", "content": ENTRY }] })),
        )
        .await?;
    let more = more["artifact"]["id"].as_str().ok_or("id")?;
    alice_stream.until(more).await?;
    bob_stream.settle().await?;
    assert!(!bob_stream.seen.contains(more), "{}", bob_stream.seen);
    assert!(
        !bob_stream.seen.contains(&ids.artifact),
        "{}",
        bob_stream.seen
    );
    let bobs: Value = harness
        .expect(
            201,
            "POST",
            "/v1/artifacts",
            &bob.secret,
            Some("bob-create-000001"),
            Some(json!({ "title": "Mine", "files": [
                { "path": "index.html", "encoding": "utf8", "content": ENTRY }] })),
        )
        .await?;
    let bobs = bobs["artifact"]["id"].as_str().ok_or("id")?;
    bob_stream.until(bobs).await?;
    assert_eq!(bob_stream.numbers(), vec![0, 1], "{}", bob_stream.seen);
    let hers = alice_stream.numbers();
    assert!(
        hers.len() == 2 && hers[0] > 2 && hers[1] == hers[0] + 1,
        "Alice's own numbering continues from her own commits: {}",
        alice_stream.seen
    );
    alice_stream.settle().await?;
    assert!(!alice_stream.seen.contains(bobs), "{}", alice_stream.seen);
    Ok(())
}

#[tokio::test]
async fn the_same_request_key_is_a_separate_request_for_each_user() -> TestResult {
    let harness = Harness::new();
    let alice = harness.person("alice").await?;
    let bob = harness.person("bob").await?;
    let create = |title: &str| {
        json!({ "title": title, "files": [
            { "path": "index.html", "encoding": "utf8", "content": ENTRY }] })
    };
    let first: Value = harness
        .expect(
            201,
            "POST",
            "/v1/artifacts",
            &alice.secret,
            Some("shared-key-0001"),
            Some(create("Alice")),
        )
        .await?;
    // The same key with another body is no conflict for Bob and no replay of
    // Alice's request.
    let second: Value = harness
        .expect(
            201,
            "POST",
            "/v1/artifacts",
            &bob.secret,
            Some("shared-key-0001"),
            Some(create("Bob")),
        )
        .await?;
    assert_ne!(first["artifact"]["id"], second["artifact"]["id"]);
    assert_eq!(second["replayed"], false, "{second}");
    assert_eq!(second["artifact"]["title"], "Bob");
    // Each user's own repeat is a replay of their own request.
    let again: Value = harness
        .expect(
            201,
            "POST",
            "/v1/artifacts",
            &bob.secret,
            Some("shared-key-0001"),
            Some(create("Bob")),
        )
        .await?;
    assert_eq!(again["replayed"], true);
    assert_eq!(again["artifact"]["id"], second["artifact"]["id"]);
    // And each user's own conflict is theirs alone.
    let reused = harness
        .send(
            "POST",
            "/v1/artifacts",
            Some(&bob.secret),
            Some("shared-key-0001"),
            Some(create("Other").to_string()),
        )
        .await?;
    assert_eq!(
        reused.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        reused.text
    );

    // The same holds for data and jobs on each user's own artifact.
    for (who, person) in [("alice", &alice), ("bob", &bob)] {
        let artifact: Value = harness
            .expect(
                201,
                "POST",
                "/v1/artifacts",
                &person.secret,
                Some("second-key-0001"),
                Some(create(who)),
            )
            .await?;
        let id = artifact["artifact"]["id"].as_str().ok_or("id")?;
        let data: Value = harness
            .expect(
                200,
                "PUT",
                &format!("/v1/artifacts/{id}/data"),
                &person.secret,
                Some("data-key-000001"),
                Some(json!({ "path": "who", "base_data_version": 0, "value": who })),
            )
            .await?;
        assert_eq!(data["replayed"], false, "{who}: {data}");
        let job: Value = harness
            .expect(
                201,
                "POST",
                &format!("/v1/artifacts/{id}/jobs"),
                &person.secret,
                Some("job-key-0000001"),
                Some(json!({ "kind": "verify_revision", "revision": 1 })),
            )
            .await?;
        assert_eq!(job["replayed"], false, "{who}: {job}");
    }
    Ok(())
}

// Who manages users --------------------------------------------------------

#[tokio::test]
async fn the_owner_registers_and_lists_users() -> TestResult {
    let harness = Harness::new();
    let listed: UserList = harness
        .expect(200, "GET", "/v1/users", OWNER, None, None)
        .await?;
    assert_eq!(listed.users.len(), 1);
    assert_eq!(listed.users[0].id, "local");
    assert!(listed.users[0].is_owner);
    assert_eq!(listed.users[0].disabled_at, None);

    let alice: UserInfo = harness
        .expect(
            201,
            "POST",
            "/v1/users",
            OWNER,
            None,
            Some(json!({ "label": "Alice" })),
        )
        .await?;
    assert!(
        alice.id.starts_with("u-") && alice.id.len() == 34,
        "{}",
        alice.id
    );
    assert!(!alice.is_owner);
    assert_eq!(alice.label, "Alice");
    let listed: UserList = harness
        .expect(200, "GET", "/v1/users", OWNER, None, None)
        .await?;
    assert_eq!(
        listed
            .users
            .iter()
            .map(|user| user.id.as_str())
            .collect::<Vec<_>>(),
        ["local", alice.id.as_str()],
        "the owner is first"
    );

    // A label is unique, and bounded; nothing registered by a refused call.
    for (body, status) in [
        (json!({ "label": "Alice" }), 409),
        (json!({ "label": "Local owner" }), 409),
        (json!({ "label": "" }), 400),
        (json!({ "label": " padded" }), 400),
        (json!({ "label": "x".repeat(65) }), 400),
        (json!({ "label": "bell\u{7}" }), 400),
        (json!({ "label": "a", "id": "local" }), 400),
        (json!({}), 400),
    ] {
        let reply = harness
            .send(
                "POST",
                "/v1/users",
                Some(OWNER),
                None,
                Some(body.to_string()),
            )
            .await?;
        assert_eq!(reply.status.as_u16(), status, "{body}: {}", reply.text);
    }
    let listed: UserList = harness
        .expect(200, "GET", "/v1/users", OWNER, None, None)
        .await?;
    assert_eq!(listed.users.len(), 2);
    Ok(())
}

#[tokio::test]
async fn the_number_of_users_is_bounded() -> TestResult {
    let harness = Harness::new();
    // The owner counts as one of 32.
    for number in 1..32 {
        let _: UserInfo = harness
            .expect(
                201,
                "POST",
                "/v1/users",
                OWNER,
                None,
                Some(json!({ "label": format!("user {number}") })),
            )
            .await?;
    }
    let refused = harness
        .send(
            "POST",
            "/v1/users",
            Some(OWNER),
            None,
            Some(json!({ "label": "one more" }).to_string()),
        )
        .await?;
    assert_eq!(refused.status, StatusCode::CONFLICT, "{}", refused.text);
    let listed: UserList = harness
        .expect(200, "GET", "/v1/users", OWNER, None, None)
        .await?;
    assert_eq!(listed.users.len(), 32);
    Ok(())
}

#[tokio::test]
async fn only_the_owner_manages_users_and_tokens_and_authenticates_first() -> TestResult {
    let harness = Harness::new();
    let alice = harness.person("alice").await?;
    let agent = harness.issue(None, "owner agent").await?;
    let routes = [
        ("GET", "/v1/users".to_owned()),
        ("POST", "/v1/users".to_owned()),
        ("POST", format!("/v1/users/{}/disable", alice.id)),
        ("POST", format!("/v1/users/{}/enable", alice.id)),
        ("POST", "/v1/users/local/disable".to_owned()),
        ("GET", "/v1/tokens".to_owned()),
        ("POST", "/v1/tokens".to_owned()),
        ("POST", format!("/v1/tokens/{}/rotate", alice.token_id)),
        ("DELETE", format!("/v1/tokens/{}", alice.token_id)),
        ("GET", "/v1/content/unreferenced".to_owned()),
    ];
    // A malformed body must not be judged before the caller is known.
    let broken = "{ not json".to_owned();
    let wrong = "f".repeat(64);
    for (method, path) in &routes {
        for (who, token) in [
            ("a user", Some(alice.secret.as_str())),
            ("an owner agent", Some(agent.secret.as_str())),
        ] {
            let reply = harness
                .send(
                    method,
                    path,
                    token,
                    Some("manager-key-0001"),
                    Some(broken.clone()),
                )
                .await?;
            assert_eq!(
                reply.status,
                StatusCode::FORBIDDEN,
                "{who}: {method} {path}: {}",
                reply.text
            );
        }
        for token in [None, Some(wrong.as_str()), Some("art_nottoken")] {
            let reply = harness
                .send(
                    method,
                    path,
                    token,
                    Some("manager-key-0001"),
                    Some(broken.clone()),
                )
                .await?;
            assert_eq!(
                reply.status,
                StatusCode::UNAUTHORIZED,
                "{method} {path}: {}",
                reply.text
            );
        }
    }
    // The refusals changed nothing.
    let listed: UserList = harness
        .expect(200, "GET", "/v1/users", OWNER, None, None)
        .await?;
    assert_eq!(listed.users.len(), 2);
    assert_eq!(listed.users[1].disabled_at, None);
    let still = harness
        .status("GET", "/v1/artifacts", Some(&alice.secret))
        .await?;
    assert_eq!(still, 200);
    Ok(())
}

#[tokio::test]
async fn the_owner_profile_cannot_be_disabled_and_unknown_users_are_not_found() -> TestResult {
    let harness = Harness::new();
    let refused = harness
        .send("POST", "/v1/users/local/disable", Some(OWNER), None, None)
        .await?;
    assert_eq!(refused.status, StatusCode::CONFLICT, "{}", refused.text);
    assert_eq!(
        harness.status("GET", "/v1/artifacts", Some(OWNER)).await?,
        200
    );
    for id in [
        "u-00000000000000000000000000000000",
        "someone-else",
        "u-xyz",
        "LOCAL",
    ] {
        for action in ["disable", "enable"] {
            let reply = harness
                .send(
                    "POST",
                    &format!("/v1/users/{id}/{action}"),
                    Some(OWNER),
                    None,
                    None,
                )
                .await?;
            assert_eq!(
                reply.status,
                StatusCode::NOT_FOUND,
                "{action} {id}: {}",
                reply.text
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn a_token_belongs_to_exactly_one_registered_user() -> TestResult {
    let harness = Harness::new();
    let alice = harness.person("alice").await?;

    let owned = harness.issue(None, "owner agent").await?;
    assert_eq!(owned.token.user_id, "local");
    let listed: TokenList = harness
        .expect(200, "GET", "/v1/tokens", OWNER, None, None)
        .await?;
    let users: Vec<(&str, &str)> = listed
        .tokens
        .iter()
        .map(|token| (token.name.as_str(), token.user_id.as_str()))
        .collect();
    assert_eq!(
        users,
        [
            ("Bootstrap Owner", "local"),
            ("alice", alice.id.as_str()),
            ("owner agent", "local")
        ]
    );
    assert!(!owned.token.is_admin);

    // Not to an unregistered user, a spelling that is no user, or a name that
    // only looks like the owner's.
    for user in [
        "u-00000000000000000000000000000000",
        "someone-else",
        "LOCAL",
        "",
    ] {
        let reply = harness
            .send(
                "POST",
                "/v1/tokens",
                Some(OWNER),
                None,
                Some(json!({ "name": "stray", "user_id": user }).to_string()),
            )
            .await?;
        assert_eq!(
            reply.status,
            StatusCode::NOT_FOUND,
            "{user:?}: {}",
            reply.text
        );
    }
    let listed: TokenList = harness
        .expect(200, "GET", "/v1/tokens", OWNER, None, None)
        .await?;
    assert_eq!(listed.tokens.len(), 3, "a refused token was stored");

    Ok(())
}

#[tokio::test]
async fn rotation_keeps_the_user_and_revoking_one_token_leaves_the_others() -> TestResult {
    let harness = Harness::new();
    let alice = harness.person("alice").await?;
    let owned = harness.issue(None, "owner agent").await?;

    // Rotation keeps the user; the old secret stops, the new one acts as the
    // same user.
    let rotated: RotateTokenResponse = harness
        .expect(
            200,
            "POST",
            &format!("/v1/tokens/{}/rotate", alice.token_id),
            OWNER,
            None,
            None,
        )
        .await?;
    assert_eq!(rotated.token.user_id, alice.id);
    assert_eq!(
        harness
            .status("GET", "/v1/artifacts", Some(&alice.secret))
            .await?,
        401
    );
    let created: Value = harness
        .expect(
            201,
            "POST",
            "/v1/artifacts",
            &rotated.secret,
            Some("rotated-key-0001"),
            Some(json!({ "title": "Alice's", "files": [
                { "path": "index.html", "encoding": "utf8", "content": ENTRY }] })),
        )
        .await?;
    let id = created["artifact"]["id"].as_str().ok_or("id")?;
    // The artifact is Alice's: the owner profile and its agent do not see it.
    for token in [OWNER, owned.secret.as_str()] {
        assert_eq!(
            harness
                .status("GET", &format!("/v1/artifacts/{id}"), Some(token))
                .await?,
            404
        );
    }

    // Revoking one token leaves the user's other tokens working.
    let other = harness.issue(Some(&alice.id), "second").await?;
    assert_eq!(
        harness
            .status(
                "DELETE",
                &format!("/v1/tokens/{}", rotated.token.id),
                Some(OWNER)
            )
            .await?,
        204
    );
    assert_eq!(
        harness
            .status("GET", "/v1/artifacts", Some(&rotated.secret))
            .await?,
        401
    );
    let body: Value = harness
        .expect(200, "GET", "/v1/artifacts", &other.secret, None, None)
        .await?;
    assert_eq!(
        body["artifacts"].as_array().map(Vec::len),
        Some(1),
        "{body}"
    );
    Ok(())
}

#[tokio::test]
async fn the_owner_profile_works_as_it_did_with_one_user() -> TestResult {
    let harness = Harness::new();
    let created: Value = harness
        .expect(
            201,
            "POST",
            "/v1/artifacts",
            OWNER,
            Some("owner-create-0001"),
            Some(json!({ "title": "Mine", "files": [
                { "path": "index.html", "encoding": "utf8", "content": ENTRY }] })),
        )
        .await?;
    let id = created["artifact"]["id"].as_str().ok_or("id")?.to_owned();
    let agent = harness.issue(None, "agent").await?;
    // The owner's agent token acts as the owner: it sees what the owner made
    // and what it makes, and the other way round.
    let seen: Value = harness
        .expect(
            200,
            "GET",
            &format!("/v1/artifacts/{id}"),
            &agent.secret,
            None,
            None,
        )
        .await?;
    assert_eq!(seen["title"], "Mine");
    let made: Value = harness
        .expect(
            201,
            "POST",
            "/v1/artifacts",
            &agent.secret,
            Some("agent-create-0001"),
            Some(json!({ "title": "Agent's", "files": [
                { "path": "index.html", "encoding": "utf8", "content": ENTRY }] })),
        )
        .await?;
    let made = made["artifact"]["id"].as_str().ok_or("id")?;
    assert_eq!(
        harness
            .status("GET", &format!("/v1/artifacts/{made}"), Some(OWNER))
            .await?,
        200
    );
    // A user registered later starts empty.
    let alice = harness.person("alice").await?;
    let theirs: Value = harness
        .expect(200, "GET", "/v1/artifacts", &alice.secret, None, None)
        .await?;
    assert_eq!(theirs["artifacts"], json!([]));
    Ok(())
}

#[tokio::test]
async fn no_agent_tool_manages_users_or_tokens() -> TestResult {
    let harness = Harness::new();
    let alice = harness.person("alice").await?;
    for token in [OWNER, alice.secret.as_str()] {
        for name in harness.tool_names(token).await? {
            for word in ["user", "token", "account", "member", "invite"] {
                assert!(!name.contains(word), "tool {name} must not be offered");
            }
        }
        for name in [
            "create_user",
            "list_users",
            "disable_user",
            "create_token",
            "rotate_token",
            "revoke_token",
        ] {
            let (_, text) = harness
                .rpc(
                    Some(token),
                    &json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                             "params": { "name": name, "arguments": { "label": "x", "name": "x" } } }),
                )
                .await?;
            let reply: Value = serde_json::from_str(&text)?;
            let refused = reply.get("error").is_some() || reply["result"]["isError"] == true;
            assert!(refused, "{name} must not be a tool: {reply}");
        }
    }
    let listed: UserList = harness
        .expect(200, "GET", "/v1/users", OWNER, None, None)
        .await?;
    assert_eq!(listed.users.len(), 2, "a tool call registered a user");
    Ok(())
}

// What disabling stops -------------------------------------------------------

/// Disables or enables a user as the owner and returns the answer.
async fn set_disabled(harness: &Harness, user: &str, disable: bool) -> Result<UserInfo, TestError> {
    let action = if disable { "disable" } else { "enable" };
    harness
        .expect(
            200,
            "POST",
            &format!("/v1/users/{user}/{action}"),
            OWNER,
            None,
            None,
        )
        .await
}

/// A preview link of the fixture's artifact, served once to show it works.
async fn live_preview(harness: &Harness, alice: &Person, ids: &Ids) -> Result<String, TestError> {
    let preview: Value = harness
        .expect(
            201,
            "POST",
            &format!("/v1/artifacts/{}/revisions/2/previews", ids.artifact),
            &alice.secret,
            None,
            None,
        )
        .await?;
    let preview = preview["url"].as_str().ok_or("url")?.to_owned();
    assert_eq!(harness.serve(&preview).await?.status, StatusCode::OK);
    Ok(preview)
}

#[tokio::test]
async fn a_disabled_user_loses_every_way_in() -> TestResult {
    let harness = Harness::new();
    let alice = harness.person("alice").await?;
    let bob = harness.person("bob").await?;
    let ids = fixture(&harness, &alice, "alice").await?;
    let second = harness.issue(Some(&alice.id), "second").await?;
    let mut alice_stream = harness.open_stream(&alice.secret).await?;
    let preview = live_preview(&harness, &alice, &ids).await?;

    let disabled = set_disabled(&harness, &alice.id, true).await?;
    let stamp = disabled.disabled_at.clone().ok_or("disabled_at")?;
    let again = set_disabled(&harness, &alice.id, true).await?;
    assert_eq!(again.disabled_at, Some(stamp), "the first time is kept");

    // Every credential of the user is refused, on every surface.
    for token in [&alice.secret, &second.secret] {
        for (method, path) in [
            ("GET", "/v1/artifacts"),
            ("POST", "/v1/runtime-sessions"),
            ("GET", "/v1/changes"),
            ("GET", "/v1/jobs"),
        ] {
            assert_eq!(
                harness.status(method, path, Some(token)).await?,
                401,
                "{method} {path}"
            );
        }
        let (status, _) = harness
            .rpc(
                Some(token),
                &json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
            )
            .await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "MCP");
    }
    // What the user held open is closed: the stream, the preview.
    assert!(alice_stream.ended().await?, "the stream outlived the user");
    assert_eq!(harness.serve(&preview).await?.status, StatusCode::NOT_FOUND);
    // Bob still has his token.
    assert_eq!(
        harness
            .status("GET", "/v1/artifacts", Some(&bob.secret))
            .await?,
        200
    );
    Ok(())
}

#[tokio::test]
async fn disabling_is_not_revoking_and_a_rotated_secret_does_not_get_in() -> TestResult {
    let harness = Harness::new();
    let alice = harness.person("alice").await?;
    let second = harness.issue(Some(&alice.id), "second").await?;
    set_disabled(&harness, &alice.id, true).await?;

    let rotated: RotateTokenResponse = harness
        .expect(
            200,
            "POST",
            &format!("/v1/tokens/{}/rotate", second.token.id),
            OWNER,
            None,
            None,
        )
        .await?;
    assert_eq!(
        harness
            .status("GET", "/v1/artifacts", Some(&rotated.secret))
            .await?,
        401
    );
    // The tokens are listed, not revoked: disabling is not revoking.
    let listed: TokenList = harness
        .expect(200, "GET", "/v1/tokens", OWNER, None, None)
        .await?;
    let alices: Vec<_> = listed
        .tokens
        .iter()
        .filter(|token| token.user_id == alice.id)
        .collect();
    assert_eq!(alices.len(), 2, "{listed:?}");
    assert!(
        alices.iter().all(|token| token.revoked_at.is_none()),
        "{listed:?}"
    );
    Ok(())
}

#[tokio::test]
async fn enabling_restores_only_the_tokens_not_what_was_closed() -> TestResult {
    let harness = Harness::new();
    let alice = harness.person("alice").await?;
    let ids = fixture(&harness, &alice, "alice").await?;
    let second = harness.issue(Some(&alice.id), "second").await?;
    let preview = live_preview(&harness, &alice, &ids).await?;
    set_disabled(&harness, &alice.id, true).await?;
    let rotated: RotateTokenResponse = harness
        .expect(
            200,
            "POST",
            &format!("/v1/tokens/{}/rotate", second.token.id),
            OWNER,
            None,
            None,
        )
        .await?;

    // Enabling lets the tokens in again, with the data intact. What was closed
    // stays closed: the old connection, the view, the preview.
    set_disabled(&harness, &alice.id, false).await?;
    let kept: Value = harness
        .expect(200, "GET", "/v1/artifacts", &alice.secret, None, None)
        .await?;
    assert_eq!(
        kept["artifacts"].as_array().map(Vec::len),
        Some(1),
        "{kept}"
    );
    assert_eq!(
        harness
            .status("GET", "/v1/artifacts", Some(&second.secret))
            .await?,
        401,
        "old secret"
    );
    assert_eq!(
        harness
            .status("GET", "/v1/artifacts", Some(&rotated.secret))
            .await?,
        200
    );
    let rejoin = harness
        .send(
            "POST",
            &format!(
                "/v1/runtime-sessions/{}/artifacts/{}/room/participants",
                ids.session, ids.artifact
            ),
            Some(&alice.secret),
            None,
            Some(json!({ "slot": "main" }).to_string()),
        )
        .await?;
    assert_eq!(
        rejoin.status,
        StatusCode::NOT_FOUND,
        "the connection outlived the disabling: {}",
        rejoin.text
    );
    let metadata = harness
        .send(
            "POST",
            &format!(
                "/v1/artifacts/{}/revisions/2/runtime-views/{}/1/metadata",
                ids.artifact, ids.view
            ),
            Some(&alice.secret),
            None,
            Some(json!({ "initialization": ids.init }).to_string()),
        )
        .await?;
    assert_eq!(metadata.status, StatusCode::NOT_FOUND, "{}", metadata.text);
    assert_eq!(harness.serve(&preview).await?.status, StatusCode::NOT_FOUND);
    let fresh: Value = harness
        .expect(
            200,
            "POST",
            "/v1/runtime-sessions",
            &alice.secret,
            None,
            None,
        )
        .await?;
    assert!(fresh["session_id"].is_string());
    Ok(())
}

#[tokio::test]
async fn a_disabled_users_token_can_be_revoked_and_stays_revoked_after_enabling() -> TestResult {
    let harness = Harness::new();
    let alice = harness.person("alice").await?;
    let stolen = harness.issue(Some(&alice.id), "stolen").await?;
    let kept = harness.issue(Some(&alice.id), "kept").await?;
    set_disabled(&harness, &alice.id, true).await?;

    // The administrator can revoke while the user is disabled, and again.
    for _ in 0..2 {
        assert_eq!(
            harness
                .status(
                    "DELETE",
                    &format!("/v1/tokens/{}", stolen.token.id),
                    Some(OWNER)
                )
                .await?,
            204
        );
    }
    let listed: TokenList = harness
        .expect(200, "GET", "/v1/tokens", OWNER, None, None)
        .await?;
    assert!(
        listed
            .tokens
            .iter()
            .any(|token| token.id == stolen.token.id && token.revoked_at.is_some()),
        "{listed:?}"
    );

    set_disabled(&harness, &alice.id, false).await?;
    assert_eq!(
        harness
            .status("GET", "/v1/artifacts", Some(&stolen.secret))
            .await?,
        401,
        "a token revoked while its user was disabled works again"
    );
    assert_eq!(
        harness
            .status("GET", "/v1/artifacts", Some(&kept.secret))
            .await?,
        200
    );
    // A revoked token cannot be rotated back either.
    assert_eq!(
        harness
            .status(
                "POST",
                &format!("/v1/tokens/{}/rotate", stolen.token.id),
                Some(OWNER)
            )
            .await?,
        409
    );
    Ok(())
}

#[tokio::test]
async fn a_user_at_the_stream_bound_can_open_one_again_after_a_disable_and_enable() -> TestResult {
    let harness = Harness::with(|settings| settings.change_streams = 1);
    let alice = harness.person("alice").await?;
    let mut held = harness.open_stream(&alice.secret).await?;
    assert_eq!(
        harness
            .status("GET", "/v1/changes", Some(&alice.secret))
            .await?,
        429
    );

    set_disabled(&harness, &alice.id, true).await?;
    assert!(held.ended().await?, "the stream outlived the user");
    set_disabled(&harness, &alice.id, false).await?;

    // The slot is freed when the stream's task has finished, a moment after
    // the body ends.
    let mut opened = false;
    for _ in 0..500 {
        // Only the status is read: the body of an open stream never ends.
        let response = harness
            .api
            .clone()
            .oneshot(
                Request::get("/v1/changes")
                    .header(header::AUTHORIZATION, format!("Bearer {}", alice.secret))
                    .body(Body::empty())?,
            )
            .await?;
        if response.status() == StatusCode::OK {
            opened = true;
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(opened, "the ended stream still holds the user's place");
    Ok(())
}

#[tokio::test]
async fn a_user_disabled_and_enabled_again_registers_through_every_route_with_its_new_generation()
-> TestResult {
    // The token authenticates under a generation that is not the first. A
    // handler that confirmed with a generation of its own making (or the
    // first) would refuse the user for good.
    let harness = Harness::new();
    let alice = harness.person("alice").await?;
    let ids = fixture(&harness, &alice, "alice").await?;
    set_disabled(&harness, &alice.id, true).await?;
    set_disabled(&harness, &alice.id, false).await?;

    live_preview(&harness, &alice, &ids).await?;
    // A connection, its grant, a runtime view and a room participant.
    connected_things(&harness, &alice.secret, &ids.artifact).await?;
    let _stream = harness.open_stream(&alice.secret).await?;
    let agent = harness
        .tool(
            &alice.secret,
            "register_handoff_session",
            json!({ "name": "again" }),
        )
        .await?;
    assert!(
        agent["structuredContent"]["session_id"].is_string(),
        "{agent}"
    );
    let preview = harness
        .tool(
            &alice.secret,
            "create_preview",
            json!({ "artifact_id": ids.artifact, "revision": 2 }),
        )
        .await?;
    assert!(preview["structuredContent"]["url"].is_string(), "{preview}");
    Ok(())
}

#[test]
fn request_handlers_open_connections_and_inboxes_only_through_the_confirmed_functions() {
    // A connection and an inbox outlive the request that opens them, so a
    // handler that authenticated before a disable must confirm the user after
    // registering. The registering functions alone do not.
    for (file, source) in [
        ("http.rs", include_str!("../src/http.rs")),
        ("mcp.rs", include_str!("../src/mcp.rs")),
    ] {
        for primitive in [
            "operations::open_runtime_session(",
            "operations::register_handoff_session(",
        ] {
            assert!(
                !source.contains(primitive),
                "{file} registers through {primitive} without confirming the user"
            );
        }
    }
    assert!(include_str!("../src/http.rs").contains("operations::open_confirmed_runtime_session("));
    assert!(
        include_str!("../src/mcp.rs").contains("operations::register_confirmed_handoff_session(")
    );
}

#[test]
fn every_registration_that_outlives_a_request_is_confirmed_against_the_generation() {
    // The registrations that outlive their request: a stream, a preview link,
    // a runtime view, a connection, an inbox. Each confirms the generation
    // its request authenticated under. A new registration must do the same,
    // and this count is where it is noticed.
    let confirmations = |source: &str| source.matches(".ensure_current(").count();
    assert_eq!(confirmations(include_str!("../src/operations.rs")), 4);
    assert_eq!(confirmations(include_str!("../src/changes.rs")), 1);
    for (file, source) in [
        ("http.rs", include_str!("../src/http.rs")),
        ("mcp.rs", include_str!("../src/mcp.rs")),
        ("operations.rs", include_str!("../src/operations.rs")),
        ("changes.rs", include_str!("../src/changes.rs")),
    ] {
        assert!(
            !source.contains("ensure_enabled("),
            "{file} confirms that the user is enabled now, which a disable and an enable undoes"
        );
    }
}

#[tokio::test]
async fn disabling_one_user_leaves_the_others_requests_and_streams_alone() -> TestResult {
    let harness = Harness::new();
    let alice = harness.person("alice").await?;
    let bob = harness.person("bob").await?;
    let mut bob_stream = harness.open_stream(&bob.secret).await?;
    let _: UserInfo = harness
        .expect(
            200,
            "POST",
            &format!("/v1/users/{}/disable", alice.id),
            OWNER,
            None,
            None,
        )
        .await?;
    assert_eq!(
        harness
            .status("GET", "/v1/artifacts", Some(&alice.secret))
            .await?,
        401
    );
    assert_eq!(
        harness
            .status("GET", "/v1/artifacts", Some(&bob.secret))
            .await?,
        200
    );
    bob_stream.settle().await?;
    assert!(
        !bob_stream.ended().await?,
        "Bob's stream was closed by Alice's disabling"
    );
    Ok(())
}

// Budgets are per user -------------------------------------------------------

#[tokio::test]
async fn one_users_request_budget_does_not_use_up_anothers() -> TestResult {
    let harness = Harness::with(|settings| {
        settings.limits = RequestLimits {
            burst: 10,
            per_minute: 60,
            concurrent: 4,
        };
    });
    let alice = harness.person("alice").await?;
    let bob = harness.person("bob").await?;
    let mut refused = false;
    for _ in 0..40 {
        if harness
            .status("GET", "/v1/artifacts", Some(&alice.secret))
            .await?
            == 429
        {
            refused = true;
            break;
        }
    }
    assert!(refused, "Alice was never limited");
    assert_eq!(
        harness
            .status("GET", "/v1/artifacts", Some(&bob.secret))
            .await?,
        200
    );
    assert_eq!(harness.status("GET", "/v1/users", Some(OWNER)).await?, 200);
    Ok(())
}

#[tokio::test]
async fn one_users_open_streams_do_not_use_up_anothers_places() -> TestResult {
    let harness = Harness::with(|settings| settings.change_streams = 1);
    let alice = harness.person("alice").await?;
    let bob = harness.person("bob").await?;
    let _first = harness.open_stream(&alice.secret).await?;
    assert_eq!(
        harness
            .status("GET", "/v1/changes", Some(&alice.secret))
            .await?,
        429
    );
    let _bobs = harness.open_stream(&bob.secret).await?;
    let _owners = harness.open_stream(OWNER).await?;
    Ok(())
}
