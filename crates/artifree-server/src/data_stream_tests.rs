//! Data streams (decision 0040) through the real router, core, and `SQLite`
//! store. Reads are counted and held at gates the stream itself provides, so
//! no test waits for a time to pass to learn that something did not happen.

use std::error::Error;
use std::future::poll_fn;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use artifree_contracts::api::ReadDataQuery;
use artifree_core::artifacts::MetadataChange;
use artifree_domain::MetadataRevision;
use artifree_domain::identity::{ActorId, ArtifactId, RequestKey, StateGeneration, TokenId};
use artifree_ports::token_store::{NewTokenRecord, TokenStore};
use artifree_ports::user_store::{NewUser, UserStore};
use artifree_providers::storage::SqliteStore;
use artifree_providers::tokens::{SqliteTokenStore, hash_token_secret};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Request, Response, StatusCode, header};
use axum::response::IntoResponse;
use http_body::Body as _;
use serde_json::{Value, json};
use tokio::time::timeout;
use tower::ServiceExt;

use crate::auth::{AccessToken, AuthContext};
use crate::data_streams::seams::Gate;
use crate::data_streams::{self, DataStreamSettings, DataStreams, Step, next_step};
use crate::http::{self, AppState, Settings};
use crate::limits::RequestLimits;

type TestResult = Result<(), Box<dyn Error>>;
type SendError = Box<dyn Error + Send + Sync>;

/// A bound for every wait for something that must happen.
const WAIT: Duration = Duration::from_secs(10);

const OWNER: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    text: String,
}

impl Reply {
    fn json(&self) -> Result<Value, Box<dyn Error>> {
        Ok(serde_json::from_str(&self.text)?)
    }

    fn retry_after(&self) -> Option<&[u8]> {
        self.headers
            .get(header::RETRY_AFTER)
            .map(axum::http::HeaderValue::as_bytes)
    }
}

struct Harness {
    directory: PathBuf,
    state: AppState,
    api: Router,
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.directory);
    }
}

/// Short times, for tests that wait for the stream itself.
fn quick() -> DataStreamSettings {
    DataStreamSettings {
        min_interval: Duration::from_millis(5),
        heartbeat: Duration::from_secs(3600),
        lifetime: Duration::from_secs(3600),
        ..DataStreamSettings::default()
    }
}

fn events_uri(id: &str, path: &str) -> String {
    format!("/v1/artifacts/{id}/data/events?path={path}")
}

fn request(
    method: &str,
    uri: &str,
    token: &str,
    key: Option<&str>,
    body: Option<&Value>,
) -> Result<Request<Body>, Box<dyn Error>> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    if let Some(key) = key {
        builder = builder.header("idempotency-key", format!("test-key-{key}"));
    }
    Ok(match body {
        Some(body) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))?,
        None => builder.body(Body::empty())?,
    })
}

impl Harness {
    fn new(streams: DataStreamSettings) -> Result<Self, Box<dyn Error>> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!(
            "artifree-data-streams-{}-{}",
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
                limits: RequestLimits {
                    burst: 100_000,
                    per_minute: 6_000_000,
                    concurrent: 64,
                },
                data_streams: streams,
                ..Settings::default()
            },
        )?;
        Ok(Self {
            directory,
            api: http::router(state.clone()),
            state,
        })
    }

    fn streams(&self) -> &DataStreams {
        self.state.data_streams()
    }

    async fn raw(
        &self,
        method: &str,
        uri: &str,
        token: &str,
        key: Option<&str>,
        body: Option<&Value>,
    ) -> Result<Response<Body>, Box<dyn Error>> {
        // A request that is never answered fails the test instead of hanging.
        let answered = timeout(
            WAIT,
            self.api
                .clone()
                .oneshot(request(method, uri, token, key, body)?),
        )
        .await
        .map_err(|_| format!("{method} {uri} was not answered within {WAIT:?}"))?;
        Ok(answered?)
    }

    async fn call(
        &self,
        method: &str,
        uri: &str,
        token: &str,
        key: Option<&str>,
        body: Option<&Value>,
    ) -> Result<Reply, Box<dyn Error>> {
        let response = self.raw(method, uri, token, key, body).await?;
        let status = response.status();
        let headers = response.headers().clone();
        // An answer that turns out to be a stream fails the test, too.
        let bytes = timeout(WAIT, to_bytes(response.into_body(), usize::MAX))
            .await
            .map_err(|_| format!("{method} {uri} did not end within {WAIT:?}"))??;
        Ok(Reply {
            status,
            headers,
            text: String::from_utf8_lossy(&bytes).into_owned(),
        })
    }

    /// An artifact of `token`'s user; returns its identifier.
    async fn artifact(&self, token: &str, name: &str) -> Result<String, Box<dyn Error>> {
        let created = self
            .call(
                "POST",
                "/v1/artifacts",
                token,
                Some(&format!("create-{name}")),
                Some(&json!({
                    "title": name,
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

    /// Sets `value` at `path` on top of `base`; returns the new version.
    async fn put(
        &self,
        token: &str,
        id: &str,
        base: u64,
        path: &str,
        value: &Value,
    ) -> Result<u64, Box<dyn Error>> {
        let reply = self
            .call(
                "PUT",
                &format!("/v1/artifacts/{id}/data"),
                token,
                Some(&format!("put-{id}-{base}-{path}")),
                Some(&json!({ "path": path, "base_data_version": base, "value": value })),
            )
            .await?;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
        reply.json()?["data_version"]
            .as_u64()
            .ok_or_else(|| "no version".into())
    }

    /// Opens a stream; it must be accepted.
    async fn open(&self, token: &str, id: &str, path: &str) -> Result<Stream, Box<dyn Error>> {
        let response = self
            .raw("GET", &events_uri(id, path), token, None, None)
            .await?;
        assert_eq!(response.status(), StatusCode::OK, "the stream was refused");
        Ok(Stream {
            body: response.into_body(),
            text: String::new(),
            ended: false,
        })
    }

    /// Starts an open in a task of its own and returns its status when done.
    fn opening(
        &self,
        token: &str,
        id: &str,
    ) -> tokio::task::JoinHandle<Result<Response<Body>, SendError>> {
        let api = self.api.clone();
        let uri = events_uri(id, "");
        let token = token.to_owned();
        tokio::spawn(async move {
            let request = Request::get(uri)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())?;
            Ok(api.oneshot(request).await?)
        })
    }

    /// A user with a token of its own; returns both.
    fn user(&self, seed: u8) -> Result<(ActorId, String), Box<dyn Error>> {
        let actor = ActorId::user([seed; 16]);
        self.state
            .token_store()
            .register(&NewUser {
                id: actor.clone(),
                label: format!("user {seed}").parse()?,
                created_at: "2026-01-01T00:00:00Z".into(),
            })
            .map_err(|error| error.to_string())?;
        let secret = self.token(&actor, seed)?;
        Ok((actor, secret))
    }

    /// A token of `actor` whose identifier is made from `seed`.
    fn token(&self, actor: &ActorId, seed: u8) -> Result<String, Box<dyn Error>> {
        let secret = AccessToken::generate_agent_token()?;
        self.state
            .token_store()
            .create(&NewTokenRecord {
                id: TokenId::from_random([seed; 16]),
                actor: actor.clone(),
                name: "test".into(),
                secret_hash: hash_token_secret(secret.secret()),
                prefix: secret.prefix(),
                created_at: "2026-01-01T00:00:00Z".into(),
            })
            .map_err(|error| error.to_string())?;
        Ok(secret.secret().to_owned())
    }

    /// Waits until the registry holds `open` places and `registered` entries.
    async fn settled(&self, open: u32, registered: usize) -> TestResult {
        timeout(WAIT, async {
            while self.streams().counts() != Some((open, registered)) {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .map_err(|_| {
            format!(
                "the registry did not reach ({open}, {registered}); it is {:?}",
                self.streams().counts()
            )
        })?;
        Ok(())
    }
}

/// The answer to an open started by [`Harness::opening`].
async fn finish(
    opening: tokio::task::JoinHandle<Result<Response<Body>, SendError>>,
) -> Result<Response<Body>, Box<dyn Error>> {
    let joined = timeout(WAIT, opening)
        .await
        .map_err(|_| "the open did not finish")??;
    joined.map_err(|error| error.to_string().into())
}

/// An open stream, read as server-sent events.
struct Stream {
    body: Body,
    text: String,
    ended: bool,
}

impl Stream {
    /// The next event as its name and parsed data, or `None` when the stream
    /// ended. Comments are skipped.
    async fn next(&mut self) -> Result<Option<(String, Value)>, Box<dyn Error>> {
        // One deadline for the whole wait: the keep-alive comments of a stream
        // that never sends anything must not keep it going.
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            if let Some(end) = self.text.find("\n\n") {
                let block: String = self.text.drain(..end + 2).collect();
                let mut name = None;
                let mut data = String::new();
                for line in block.lines() {
                    if let Some(value) = line.strip_prefix("event: ") {
                        name = Some(value.to_owned());
                    } else if let Some(value) = line.strip_prefix("data: ") {
                        data.push_str(value);
                    }
                }
                if let Some(name) = name {
                    return Ok(Some((name, serde_json::from_str(&data)?)));
                }
                continue;
            }
            if self.ended {
                return Ok(None);
            }
            let frame = tokio::time::timeout_at(
                deadline,
                poll_fn(|context| Pin::new(&mut self.body).poll_frame(context)),
            )
            .await
            .map_err(|_| format!("no event within {WAIT:?}; saw {:?}", self.text))?;
            match frame {
                None => self.ended = true,
                Some(frame) => {
                    if let Ok(data) = frame?.into_data() {
                        self.text.push_str(std::str::from_utf8(&data)?);
                    }
                }
            }
        }
    }

    /// The next event, which must be `name`; returns its data.
    async fn expect(&mut self, name: &str) -> Result<Value, Box<dyn Error>> {
        match self.next().await? {
            Some((got, data)) if got == name => Ok(data),
            other => Err(format!("expected {name}, got {other:?}").into()),
        }
    }

    /// The stream must end without another event.
    async fn expect_end(&mut self) -> TestResult {
        match self.next().await? {
            None => Ok(()),
            Some(event) => Err(format!("an event followed: {event:?}").into()),
        }
    }
}

#[test]
fn the_defaults_are_the_numbers_of_the_decision() {
    let defaults = DataStreamSettings::default();
    assert_eq!(defaults.per_actor, 8);
    assert_eq!(defaults.per_process, 128);
    assert_eq!(defaults.min_interval, Duration::from_millis(250));
    assert_eq!(defaults.heartbeat, Duration::from_secs(15));
    assert_eq!(defaults.lifetime, Duration::from_secs(600));
    assert_eq!(defaults.open_timeout, Duration::from_secs(30));
}

#[test]
fn a_state_refuses_zero_limits_and_times() {
    let base = DataStreamSettings::default();
    for settings in [
        DataStreamSettings {
            per_actor: 0,
            ..base
        },
        DataStreamSettings {
            per_process: 0,
            ..base
        },
        DataStreamSettings {
            min_interval: Duration::ZERO,
            ..base
        },
        DataStreamSettings {
            heartbeat: Duration::ZERO,
            ..base
        },
        DataStreamSettings {
            lifetime: Duration::ZERO,
            ..base
        },
        DataStreamSettings {
            open_timeout: Duration::ZERO,
            ..base
        },
    ] {
        assert!(settings.validate().is_err(), "{settings:?}");
    }
    assert!(base.validate().is_ok());
}

#[test]
fn a_stream_sends_only_versions_that_increase() {
    for (sent, version, too_large, expected) in [
        (None, 0, false, Step::Send),
        (None, 5, true, Step::SendAndEnd),
        (Some(3), 4, false, Step::Send),
        (Some(3), 9, true, Step::SendAndEnd),
        (Some(3), 3, false, Step::Nothing),
        (Some(3), 3, true, Step::Nothing),
        (Some(3), 2, false, Step::End),
        (Some(3), 2, true, Step::End),
    ] {
        assert_eq!(
            next_step(sent, version, too_large),
            expected,
            "sent {sent:?}, read {version}, too large {too_large}"
        );
    }
}

#[tokio::test]
async fn a_snapshot_is_followed_by_full_values_with_increasing_versions() -> TestResult {
    let harness = Harness::new(quick())?;
    let id = harness.artifact(OWNER, "watched").await?;
    let mut stream = harness.open(OWNER, &id, "list").await?;
    assert_eq!(
        stream.expect("snapshot").await?,
        json!({ "artifact_id": id, "path": "list", "data_version": 0, "exists": false })
    );

    assert_eq!(
        harness.put(OWNER, &id, 0, "list", &json!({"a": 1})).await?,
        1
    );
    assert_eq!(
        stream.expect("change").await?,
        json!({ "artifact_id": id, "path": "list", "data_version": 1,
                "exists": true, "value": { "a": 1 } })
    );
    // A write elsewhere in the space moves the version; the path still gets
    // its full value, here the same.
    assert_eq!(harness.put(OWNER, &id, 1, "other", &json!(1)).await?, 2);
    let second = stream.expect("change").await?;
    assert_eq!(second["data_version"], 2);
    assert_eq!(second["value"], json!({ "a": 1 }));
    Ok(())
}

#[tokio::test]
async fn writes_during_a_throttle_wait_cost_one_read_and_later_ones_one_more() -> TestResult {
    // An interval no test waits for: the wait ends when the gate lets it.
    let harness = Harness::new(DataStreamSettings {
        min_interval: Duration::from_secs(3600),
        ..quick()
    })?;
    let id = harness.artifact(OWNER, "burst").await?;
    let gate = Gate::new();
    harness.streams().hold_throttle(Some(Arc::clone(&gate)));
    let mut stream = harness.open(OWNER, &id, "n").await?;
    stream.expect("snapshot").await?;
    assert_eq!(harness.streams().reads(), 1);

    // The first write wakes the stream, which then waits out its interval.
    harness.put(OWNER, &id, 0, "n", &json!(1)).await?;
    timeout(WAIT, gate.arrival()).await?;
    for (base, value) in (1..6_u64).zip(2..) {
        harness.put(OWNER, &id, base, "n", &json!(value)).await?;
    }
    assert_eq!(harness.streams().reads(), 1, "a read began during the wait");

    // One release, one read, the newest state.
    gate.release();
    let change = stream.expect("change").await?;
    assert_eq!(change["data_version"], 6);
    assert_eq!(change["value"], 6);
    assert_eq!(
        harness.streams().reads(),
        2,
        "the burst cost more than one read"
    );

    // The burst left nothing pending. The permit below waits for the next
    // write's interval and for nothing else: exactly one read follows it.
    gate.release();
    harness.put(OWNER, &id, 6, "n", &json!(7)).await?;
    assert_eq!(stream.expect("change").await?["data_version"], 7);
    assert_eq!(
        harness.streams().reads(),
        3,
        "the burst left a wake-up pending"
    );
    Ok(())
}

#[tokio::test]
async fn a_write_after_the_read_began_stays_pending_for_the_next_read() -> TestResult {
    let harness = Harness::new(quick())?;
    let id = harness.artifact(OWNER, "late").await?;
    let mut stream = harness.open(OWNER, &id, "n").await?;
    stream.expect("snapshot").await?;
    let gate = Gate::new();
    harness.streams().hold_reads(Some(Arc::clone(&gate)));

    // The read begins on this write and is held after it looked at storage.
    harness.put(OWNER, &id, 0, "n", &json!(1)).await?;
    timeout(WAIT, gate.arrival()).await?;
    harness.put(OWNER, &id, 1, "n", &json!(2)).await?;
    gate.release();
    assert_eq!(stream.expect("change").await?["data_version"], 1);

    // The second write came after the read began: it is still pending.
    timeout(WAIT, gate.arrival()).await?;
    gate.release();
    assert_eq!(stream.expect("change").await?["data_version"], 2);
    Ok(())
}

#[tokio::test]
async fn a_trashed_artifact_ends_the_stream_through_the_wake_up() -> TestResult {
    // The heartbeat is an hour away: only the wake-up can end it.
    let harness = Harness::new(quick())?;
    let id = harness.artifact(OWNER, "trash").await?;
    let mut stream = harness.open(OWNER, &id, "").await?;
    stream.expect("snapshot").await?;
    let trashed = harness
        .call(
            "PATCH",
            &format!("/v1/artifacts/{id}"),
            OWNER,
            Some("trash-it"),
            Some(&json!({ "base_metadata_revision": 1, "deleted": true })),
        )
        .await?;
    assert_eq!(trashed.status, StatusCode::OK, "{}", trashed.text);
    stream.expect_end().await?;
    harness.settled(0, 0).await
}

#[tokio::test]
async fn a_commit_by_another_process_is_found_at_the_heartbeat() -> TestResult {
    let harness = Harness::new(DataStreamSettings {
        heartbeat: Duration::from_millis(50),
        ..quick()
    })?;
    let id = harness.artifact(OWNER, "elsewhere").await?;
    let mut stream = harness.open(OWNER, &id, "").await?;
    stream.expect("snapshot").await?;
    // Through the core, as another process would commit: no wake-up.
    let artifact: ArtifactId = id.parse()?;
    let key: RequestKey = "elsewhere-trash".parse()?;
    let change = MetadataChange::new(None, None, Some(true)).ok_or("no change")?;
    harness
        .state
        .run(move |artifacts| {
            artifacts.update(
                &ActorId::owner(),
                &key,
                &artifact,
                MetadataRevision(1),
                &change,
            )
        })
        .await
        .map_err(|error| error.to_string())?;
    stream.expect_end().await?;
    harness.settled(0, 0).await
}

#[tokio::test]
async fn a_stream_under_constant_writes_still_ends_at_its_lifetime() -> TestResult {
    let harness = Harness::new(DataStreamSettings {
        lifetime: Duration::from_millis(400),
        ..quick()
    })?;
    let id = harness.artifact(OWNER, "lifetime").await?;
    let mut stream = harness.open(OWNER, &id, "n").await?;
    stream.expect("snapshot").await?;
    timeout(WAIT, async {
        let mut base = 0;
        loop {
            base = harness.put(OWNER, &id, base, "n", &json!(base)).await?;
            // Take whatever arrived; the stream is over when none can.
            match timeout(Duration::from_millis(30), stream.next()).await {
                Ok(Ok(None)) => return Ok::<(), Box<dyn Error>>(()),
                Ok(Ok(Some(_))) | Err(_) => {}
                Ok(Err(error)) => return Err(error),
            }
        }
    })
    .await
    .map_err(|_| "the stream outlived its lifetime under constant writes")??;
    harness.settled(0, 0).await
}

#[tokio::test]
async fn a_throttle_wait_never_outlasts_the_lifetime() -> TestResult {
    let harness = Harness::new(DataStreamSettings {
        min_interval: Duration::from_secs(3600),
        lifetime: Duration::from_millis(300),
        ..quick()
    })?;
    let id = harness.artifact(OWNER, "bounded").await?;
    let mut stream = harness.open(OWNER, &id, "n").await?;
    stream.expect("snapshot").await?;
    // The write puts the stream into a wait of an hour, cut at the lifetime.
    harness.put(OWNER, &id, 0, "n", &json!(1)).await?;
    stream.expect_end().await?;
    harness.settled(0, 0).await?;
    assert_eq!(
        harness.streams().reads(),
        1,
        "a read began after the lifetime was over"
    );
    Ok(())
}

#[tokio::test]
async fn a_signal_and_a_write_that_arrive_together_cost_no_read() -> TestResult {
    let harness = Harness::new(DataStreamSettings {
        min_interval: Duration::from_millis(1),
        ..quick()
    })?;
    let (_, secret) = harness.user(1)?;
    let id = harness.artifact(&secret, "together").await?;
    let mut stream = harness.open(&secret, &id, "n").await?;
    stream.expect("snapshot").await?;
    assert_eq!(harness.streams().reads(), 1);
    // Lets the throttle interval pass, so that a wrongly started read would
    // not wait; the assertions below do not depend on it.
    tokio::time::sleep(Duration::from_millis(10)).await;
    // The producer is idle; the runtime does not run it between these two
    // calls, so it finds the revocation and the wake-up in one poll.
    harness
        .streams()
        .revoke_token(&TokenId::from_random([1; 16]));
    harness.streams().wake();
    stream.expect_end().await?;
    harness.settled(0, 0).await?;
    assert_eq!(
        harness.streams().reads(),
        1,
        "a read began although a signal was visible"
    );
    Ok(())
}

#[tokio::test]
async fn a_value_over_the_bound_is_announced_and_ends_the_stream() -> TestResult {
    let harness = Harness::new(quick())?;
    let id = harness.artifact(OWNER, "big").await?;
    let big = json!("x".repeat(128 * 1024));
    // The first read finds it too large.
    let mut version = harness.put(OWNER, &id, 0, "big", &big).await?;
    let mut stream = harness.open(OWNER, &id, "big").await?;
    assert_eq!(
        stream.expect("too_large").await?,
        json!({ "artifact_id": id, "path": "big", "data_version": version })
    );
    stream.expect_end().await?;

    // One that grows past the bound while it is watched.
    version = harness
        .put(OWNER, &id, version, "grows", &json!("small"))
        .await?;
    let mut stream = harness.open(OWNER, &id, "grows").await?;
    assert_eq!(stream.expect("snapshot").await?["value"], "small");
    version = harness.put(OWNER, &id, version, "grows", &big).await?;
    assert_eq!(stream.expect("too_large").await?["data_version"], version);
    stream.expect_end().await?;

    // A value of exactly the bound (with its quotes) is sent whole.
    let exact = json!("x".repeat(128 * 1024 - 2));
    harness.put(OWNER, &id, version, "exact", &exact).await?;
    let mut stream = harness.open(OWNER, &id, "exact").await?;
    assert_eq!(stream.expect("snapshot").await?["value"], exact);
    Ok(())
}

#[tokio::test]
async fn a_revoked_token_ends_its_stream_before_another_event_is_sent() -> TestResult {
    let harness = Harness::new(quick())?;
    let (alice, secret) = harness.user(1)?;
    let second = harness.token(&alice, 2)?;
    let id = harness.artifact(&secret, "alice").await?;
    let mut stream = harness.open(&secret, &id, "n").await?;
    stream.expect("snapshot").await?;

    let revoked = harness
        .call(
            "DELETE",
            &format!("/v1/tokens/{}", TokenId::from_random([1; 16])),
            OWNER,
            None,
            None,
        )
        .await?;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT, "{}", revoked.text);
    // The user's other token writes; the stream of the revoked one sees none.
    harness.put(&second, &id, 0, "n", &json!(1)).await?;
    stream.expect_end().await?;
    harness.settled(0, 0).await
}

#[tokio::test]
async fn an_event_waiting_for_the_connection_is_dropped_when_a_signal_is_visible() -> TestResult {
    let harness = Harness::new(quick())?;
    let (_, secret) = harness.user(1)?;
    let id = harness.artifact(&secret, "queued").await?;
    // Opened and not read: the snapshot waits in the channel.
    let mut stream = harness.open(&secret, &id, "n").await?;
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    harness
        .streams()
        .revoke_token(&TokenId::from_random([1; 16]));
    stream.expect_end().await
}

#[tokio::test]
async fn a_disabled_user_loses_the_open_streams_and_others_keep_theirs() -> TestResult {
    let harness = Harness::new(quick())?;
    let (alice, secret) = harness.user(1)?;
    let (_, bob_secret) = harness.user(2)?;
    let id = harness.artifact(&secret, "alice").await?;
    let bobs = harness.artifact(&bob_secret, "bob").await?;
    let mut stream = harness.open(&secret, &id, "n").await?;
    let mut other = harness.open(&bob_secret, &bobs, "n").await?;
    stream.expect("snapshot").await?;
    other.expect("snapshot").await?;
    harness
        .state
        .token_store()
        .set_disabled(&alice, Some("2026-01-02T00:00:00Z"))
        .map_err(|error| error.to_string())?;
    harness.state.revoke_actor(&alice);
    stream.expect_end().await?;
    harness.put(&bob_secret, &bobs, 0, "n", &json!(1)).await?;
    assert_eq!(other.expect("change").await?["data_version"], 1);
    Ok(())
}

#[tokio::test]
async fn a_user_disabled_before_the_first_read_gets_no_stream() -> TestResult {
    let harness = Harness::new(quick())?;
    let (alice, secret) = harness.user(1)?;
    let id = harness.artifact(&secret, "alice").await?;
    let gate = Gate::new();
    harness.streams().hold_reads(Some(Arc::clone(&gate)));
    let opening = harness.opening(&secret, &id);
    timeout(WAIT, gate.arrival()).await?;
    harness
        .state
        .token_store()
        .set_disabled(&alice, Some("2026-01-02T00:00:00Z"))
        .map_err(|error| error.to_string())?;
    harness.state.revoke_actor(&alice);
    gate.release();
    let response = finish(opening).await?;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    harness.settled(0, 0).await
}

#[tokio::test]
async fn a_revocation_during_the_first_read_refuses_the_open_and_sends_nothing() -> TestResult {
    let harness = Harness::new(quick())?;
    let (_, secret) = harness.user(1)?;
    let id = harness.artifact(&secret, "held").await?;
    let gate = Gate::new();
    harness.streams().hold_reads(Some(Arc::clone(&gate)));
    let opening = harness.opening(&secret, &id);
    // The request is registered and has read; the signal comes now.
    timeout(WAIT, gate.arrival()).await?;
    assert_eq!(harness.streams().counts(), Some((0, 1)));
    harness
        .streams()
        .revoke_token(&TokenId::from_random([1; 16]));
    gate.release();
    let response = finish(opening).await?;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    harness.settled(0, 0).await
}

#[tokio::test]
async fn shutdown_during_the_first_read_refuses_the_open() -> TestResult {
    let harness = Harness::new(quick())?;
    let id = harness.artifact(OWNER, "closing").await?;
    let gate = Gate::new();
    harness.streams().hold_reads(Some(Arc::clone(&gate)));
    let opening = harness.opening(OWNER, &id);
    timeout(WAIT, gate.arrival()).await?;
    harness.state.close_streams();
    gate.release();
    let response = finish(opening).await?;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    harness.settled(0, 0).await
}

#[tokio::test]
async fn an_event_waiting_for_the_connection_is_dropped_at_shutdown_too() -> TestResult {
    let harness = Harness::new(quick())?;
    let id = harness.artifact(OWNER, "queued-closing").await?;
    // Opened and not read: the snapshot waits in the channel.
    let mut stream = harness.open(OWNER, &id, "n").await?;
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    harness.state.close_streams();
    stream.expect_end().await
}

#[tokio::test]
async fn a_reader_that_never_reads_cannot_hold_a_stream_past_its_lifetime() -> TestResult {
    let harness = Harness::new(DataStreamSettings {
        lifetime: Duration::from_millis(500),
        ..quick()
    })?;
    let id = harness.artifact(OWNER, "stalled").await?;
    // The snapshot fills the channel; the change after it has to wait for a
    // reader that never comes. The body stays alive and unread.
    let _stalled = harness.open(OWNER, &id, "n").await?;
    harness.put(OWNER, &id, 0, "n", &json!(1)).await?;
    harness.settled(0, 0).await
}

#[tokio::test]
async fn a_revocation_ends_a_stream_whose_reader_is_behind() -> TestResult {
    let harness = Harness::new(quick())?;
    let (_, secret) = harness.user(1)?;
    let id = harness.artifact(&secret, "behind").await?;
    // The snapshot fills the channel and nobody reads it; the next event has
    // to wait for room.
    let _stalled = harness.open(&secret, &id, "n").await?;
    harness.put(&secret, &id, 0, "n", &json!(1)).await?;
    timeout(WAIT, async {
        while harness.streams().reads() < 2 {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .map_err(|_| "the write did not make the stream read")?;
    harness
        .streams()
        .revoke_token(&TokenId::from_random([1; 16]));
    harness.settled(0, 0).await
}

/// A stream whose next read is stuck, as one on a blocking pool that makes
/// no progress is. The gate is never released: the read does not finish
/// until the stream is gone. The body stays open and unread.
async fn stalled(harness: &Harness, secret: &str) -> Result<(Stream, Arc<Gate>), Box<dyn Error>> {
    let id = harness.artifact(secret, "stalled-read").await?;
    let mut stream = harness.open(secret, &id, "n").await?;
    stream.expect("snapshot").await?;
    let gate = Gate::new();
    harness.streams().hold_reads(Some(Arc::clone(&gate)));
    harness.put(secret, &id, 0, "n", &json!(1)).await?;
    timeout(WAIT, gate.arrival()).await?;
    assert_eq!(harness.streams().counts(), Some((1, 1)));
    Ok((stream, gate))
}

#[tokio::test]
async fn a_stalled_read_does_not_keep_a_stream_past_its_lifetime() -> TestResult {
    let harness = Harness::new(DataStreamSettings {
        lifetime: Duration::from_millis(700),
        ..quick()
    })?;
    let (_, secret) = harness.user(1)?;
    let (_stream, _gate) = stalled(&harness, &secret).await?;
    harness.settled(0, 0).await
}

#[tokio::test]
async fn a_revocation_ends_a_stream_whose_read_is_stalled() -> TestResult {
    let harness = Harness::new(quick())?;
    let (_, secret) = harness.user(1)?;
    let (_stream, _gate) = stalled(&harness, &secret).await?;
    harness
        .streams()
        .revoke_token(&TokenId::from_random([1; 16]));
    harness.settled(0, 0).await
}

#[tokio::test]
async fn shutdown_ends_a_stream_whose_read_is_stalled() -> TestResult {
    let harness = Harness::new(quick())?;
    let (_, secret) = harness.user(1)?;
    let (_stream, _gate) = stalled(&harness, &secret).await?;
    harness.state.close_streams();
    harness.settled(0, 0).await
}

#[tokio::test]
async fn a_reader_that_leaves_ends_a_stream_whose_read_is_stalled() -> TestResult {
    let harness = Harness::new(quick())?;
    let (_, secret) = harness.user(1)?;
    let (stream, _gate) = stalled(&harness, &secret).await?;
    drop(stream);
    harness.settled(0, 0).await
}

#[tokio::test]
async fn a_first_read_that_never_finishes_is_refused_when_the_open_times_out() -> TestResult {
    let harness = Harness::new(DataStreamSettings {
        open_timeout: Duration::from_millis(300),
        ..quick()
    })?;
    let id = harness.artifact(OWNER, "slow-open").await?;
    let gate = Gate::new();
    harness.streams().hold_reads(Some(Arc::clone(&gate)));
    let opening = harness.opening(OWNER, &id);
    timeout(WAIT, gate.arrival()).await?;
    assert_eq!(harness.streams().counts(), Some((0, 1)));
    // The gate is never released.
    let response = finish(opening).await?;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response.headers().get(header::RETRY_AFTER),
        Some(&header::HeaderValue::from(10_u32))
    );
    harness.settled(0, 0).await
}

#[tokio::test]
async fn a_user_who_cannot_be_confirmed_in_time_is_refused_and_releases_the_registration()
-> TestResult {
    let harness = Harness::new(DataStreamSettings {
        open_timeout: Duration::from_millis(300),
        ..quick()
    })?;
    let (_, secret) = harness.user(1)?;
    let id = harness.artifact(&secret, "unconfirmed").await?;
    let gate = Gate::new();
    harness.streams().hold_opening(Some(Arc::clone(&gate)));
    let opening = harness.opening(&secret, &id);
    timeout(WAIT, gate.arrival()).await?;
    assert_eq!(harness.streams().counts(), Some((0, 1)));
    let response = finish(opening).await?;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    harness.settled(0, 0).await
}

#[tokio::test]
async fn a_revocation_refuses_an_open_whose_first_read_is_stalled() -> TestResult {
    let harness = Harness::new(quick())?;
    let (_, secret) = harness.user(1)?;
    let id = harness.artifact(&secret, "held-open").await?;
    let gate = Gate::new();
    harness.streams().hold_reads(Some(Arc::clone(&gate)));
    let opening = harness.opening(&secret, &id);
    timeout(WAIT, gate.arrival()).await?;
    harness
        .streams()
        .revoke_token(&TokenId::from_random([1; 16]));
    // The gate is never released.
    let response = finish(opening).await?;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    harness.settled(0, 0).await
}

#[tokio::test]
async fn a_request_that_authenticated_before_a_disable_opens_no_stream() -> TestResult {
    let harness = Harness::new(quick())?;
    let (alice, secret) = harness.user(1)?;
    let id = harness.artifact(&secret, "alice").await?;
    // The context is what the token gave before the disable. The disable is
    // committed without the in-process revocation, as if a request that
    // authenticated earlier were only now reaching the handler.
    let auth = AuthContext {
        actor: alice.clone(),
        token_id: Some(TokenId::from_random([1; 16])),
        generation: StateGeneration::FIRST,
        is_admin: false,
    };
    harness
        .state
        .token_store()
        .set_disabled(&alice, Some("2026-01-02T00:00:00Z"))
        .map_err(|error| error.to_string())?;
    let answer = data_streams::events(
        State(harness.state.clone()),
        auth,
        HeaderMap::new(),
        Ok(Path(id)),
        Ok(Query(ReadDataQuery { path: None })),
    )
    .await;
    let status = match answer {
        Ok(response) => response.status(),
        Err(error) => error.into_response().status(),
    };
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    harness.settled(0, 0).await
}

#[tokio::test]
async fn shutdown_ends_open_streams() -> TestResult {
    let harness = Harness::new(quick())?;
    let id = harness.artifact(OWNER, "shutdown").await?;
    let mut stream = harness.open(OWNER, &id, "").await?;
    stream.expect("snapshot").await?;
    harness.state.close_streams();
    stream.expect_end().await?;
    harness.settled(0, 0).await
}

#[tokio::test]
async fn an_actor_has_eight_streams_and_the_ninth_is_refused_without_a_read() -> TestResult {
    let harness = Harness::new(quick())?;
    let id = harness.artifact(OWNER, "eight").await?;
    let mut open = Vec::new();
    for _ in 0..8 {
        let mut stream = harness.open(OWNER, &id, "").await?;
        stream.expect("snapshot").await?;
        open.push(stream);
    }
    let reads = harness.streams().reads();
    let refused = harness
        .call("GET", &events_uri(&id, ""), OWNER, None, None)
        .await?;
    assert_eq!(
        refused.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        refused.text
    );
    assert_eq!(refused.retry_after(), Some(&b"5"[..]));
    assert_eq!(refused.json()?["error"]["code"], "rate_limited");
    assert_eq!(harness.streams().reads(), reads, "a refusal read storage");
    assert_eq!(harness.streams().counts(), Some((8, 8)));

    // Another actor is not held back by the first's streams.
    let (_, secret) = harness.user(1)?;
    let theirs = harness.artifact(&secret, "theirs").await?;
    let mut theirs = harness.open(&secret, &theirs, "").await?;
    theirs.expect("snapshot").await?;

    // A place that is freed can be taken.
    drop(open.pop());
    harness.settled(8, 8).await?;
    harness
        .open(OWNER, &id, "")
        .await?
        .expect("snapshot")
        .await?;
    Ok(())
}

#[tokio::test]
async fn the_process_has_a_bound_and_refuses_with_retry_after_ten() -> TestResult {
    let harness = Harness::new(DataStreamSettings {
        per_process: 3,
        ..quick()
    })?;
    let id = harness.artifact(OWNER, "three").await?;
    let (_, secret) = harness.user(1)?;
    let theirs = harness.artifact(&secret, "theirs").await?;
    let mut open = Vec::new();
    for (token, artifact) in [(OWNER, &id), (secret.as_str(), &theirs), (OWNER, &id)] {
        let mut stream = harness.open(token, artifact, "").await?;
        stream.expect("snapshot").await?;
        open.push(stream);
    }
    let reads = harness.streams().reads();
    let refused = harness
        .call("GET", &events_uri(&theirs, ""), &secret, None, None)
        .await?;
    assert_eq!(
        refused.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{}",
        refused.text
    );
    assert_eq!(refused.retry_after(), Some(&b"10"[..]));
    assert_eq!(harness.streams().reads(), reads, "a refusal read storage");
    Ok(())
}

#[tokio::test]
async fn two_opens_that_both_passed_the_precheck_cannot_both_take_the_last_place() -> TestResult {
    let harness = Harness::new(DataStreamSettings {
        per_actor: 1,
        ..quick()
    })?;
    let id = harness.artifact(OWNER, "race").await?;
    let gate = Gate::new();
    harness.streams().hold_reads(Some(Arc::clone(&gate)));
    let (first, second) = (harness.opening(OWNER, &id), harness.opening(OWNER, &id));
    // Both are registered and have read, and neither has a place yet.
    timeout(WAIT, gate.arrival()).await?;
    timeout(WAIT, gate.arrival()).await?;
    assert_eq!(harness.streams().counts(), Some((0, 2)));
    gate.release();
    gate.release();
    let first = finish(first).await?;
    let second = finish(second).await?;
    let mut statuses = [first.status(), second.status()];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::TOO_MANY_REQUESTS]);
    assert_eq!(harness.streams().counts(), Some((1, 1)));
    Ok(())
}

#[tokio::test]
async fn opens_that_have_not_taken_a_place_are_bounded_too() -> TestResult {
    let harness = Harness::new(DataStreamSettings {
        per_process: 1,
        ..quick()
    })?;
    let id = harness.artifact(OWNER, "opening").await?;
    let gate = Gate::new();
    harness.streams().hold_reads(Some(Arc::clone(&gate)));
    let (first, second) = (harness.opening(OWNER, &id), harness.opening(OWNER, &id));
    timeout(WAIT, gate.arrival()).await?;
    timeout(WAIT, gate.arrival()).await?;
    assert_eq!(harness.streams().counts(), Some((0, 2)));
    let reads = harness.streams().reads();
    let refused = harness
        .call("GET", &events_uri(&id, ""), OWNER, None, None)
        .await?;
    assert_eq!(
        refused.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{}",
        refused.text
    );
    assert_eq!(harness.streams().reads(), reads, "a refusal read storage");
    assert_eq!(harness.streams().counts(), Some((0, 2)));
    gate.release();
    gate.release();
    let (first, second) = (finish(first).await?, finish(second).await?);
    drop((first, second));
    harness.settled(0, 0).await
}

#[tokio::test]
async fn one_actor_cannot_fill_the_registrations_other_actors_need() -> TestResult {
    // Two places an actor, so four registrations an actor, of 256 in all.
    let harness = Harness::new(DataStreamSettings {
        per_actor: 2,
        ..quick()
    })?;
    let (_, hostile) = harness.user(1)?;
    let (_, other) = harness.user(2)?;
    let mine = harness.artifact(&hostile, "mine").await?;
    let theirs = harness.artifact(&other, "theirs").await?;
    let gate = Gate::new();
    harness.streams().hold_reads(Some(Arc::clone(&gate)));
    let mut opening = Vec::new();
    for _ in 0..4 {
        opening.push(harness.opening(&hostile, &mine));
        timeout(WAIT, gate.arrival()).await?;
    }
    assert_eq!(harness.streams().counts(), Some((0, 4)));
    let reads = harness.streams().reads();
    let refused = harness
        .call("GET", &events_uri(&mine, ""), &hostile, None, None)
        .await?;
    assert_eq!(
        refused.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        refused.text
    );
    assert_eq!(harness.streams().reads(), reads, "a refusal read storage");
    assert_eq!(harness.streams().counts(), Some((0, 4)));
    // Another actor is not affected.
    opening.push(harness.opening(&other, &theirs));
    timeout(WAIT, gate.arrival()).await?;
    assert_eq!(harness.streams().counts(), Some((0, 5)));
    for _ in 0..5 {
        gate.release();
    }
    let mut answers = Vec::new();
    for handle in opening {
        answers.push(finish(handle).await?);
    }
    drop(answers);
    harness.settled(0, 0).await
}

#[tokio::test]
async fn a_write_before_the_first_read_is_covered_by_it_and_one_after_the_mark_is_not() -> TestResult
{
    let harness = Harness::new(DataStreamSettings {
        min_interval: Duration::from_millis(1),
        ..quick()
    })?;
    let id = harness.artifact(OWNER, "first").await?;
    let gate = Gate::new();
    harness.streams().hold_opening(Some(Arc::clone(&gate)));
    let opening = harness.opening(OWNER, &id);
    timeout(WAIT, gate.arrival()).await?;
    // The write commits while the request is on its way to the first read.
    let first = harness.put(OWNER, &id, 0, "n", &json!(1)).await?;
    gate.release();
    let response = finish(opening).await?;
    let mut stream = Stream {
        body: response.into_body(),
        text: String::new(),
        ended: false,
    };
    assert_eq!(stream.expect("snapshot").await?["data_version"], first);
    // Lets a wake-up that was wrongly left pending show itself as a read; the
    // assertion does not depend on it.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        harness.streams().reads(),
        1,
        "the first read left a wake-up"
    );
    // A write after the mark is one more read.
    harness.put(OWNER, &id, first, "n", &json!(2)).await?;
    assert_eq!(stream.expect("change").await?["data_version"], 2);
    assert_eq!(harness.streams().reads(), 2);
    Ok(())
}

#[tokio::test]
async fn a_missing_or_foreign_artifact_holds_nothing_and_answers_alike() -> TestResult {
    let harness = Harness::new(DataStreamSettings {
        per_actor: 2,
        ..quick()
    })?;
    let (_, alice) = harness.user(1)?;
    let (_, bob) = harness.user(2)?;
    let theirs = harness.artifact(&alice, "alice").await?;
    let absent = format!("art_{}", "0".repeat(32));
    let mut answers = Vec::new();
    for _ in 0..5 {
        for id in [&theirs, &absent] {
            let reply = harness
                .call("GET", &events_uri(id, ""), &bob, None, None)
                .await?;
            assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.text);
            answers.push(reply.text.replace(id.as_str(), "ID"));
        }
    }
    assert!(
        answers.windows(2).all(|pair| pair[0] == pair[1]),
        "{answers:?}"
    );
    harness.settled(0, 0).await?;
    // Bob still has all his places: the failures took none.
    let own = harness.artifact(&bob, "bob").await?;
    for _ in 0..2 {
        harness
            .open(&bob, &own, "")
            .await?
            .expect("snapshot")
            .await?;
    }
    Ok(())
}

#[tokio::test]
async fn a_reader_that_leaves_frees_its_place_and_registration() -> TestResult {
    let harness = Harness::new(quick())?;
    let id = harness.artifact(OWNER, "leaves").await?;
    let mut stream = harness.open(OWNER, &id, "").await?;
    stream.expect("snapshot").await?;
    assert_eq!(harness.streams().counts(), Some((1, 1)));
    drop(stream);
    harness.settled(0, 0).await
}

#[tokio::test]
async fn a_poisoned_registry_refuses_streams_and_never_fails_a_write() -> TestResult {
    let harness = Harness::new(quick())?;
    let id = harness.artifact(OWNER, "poisoned").await?;
    let mut stream = harness.open(OWNER, &id, "n").await?;
    stream.expect("snapshot").await?;
    harness.streams().poison_registry();
    assert_eq!(harness.put(OWNER, &id, 0, "n", &json!(1)).await?, 1);
    // The open stream still receives the change.
    assert_eq!(stream.expect("change").await?["data_version"], 1);
    let refused = harness
        .call("GET", &events_uri(&id, ""), OWNER, None, None)
        .await?;
    assert_eq!(refused.status, StatusCode::SERVICE_UNAVAILABLE);
    Ok(())
}

#[tokio::test]
async fn writes_are_not_delayed_by_streams_that_nobody_reads() -> TestResult {
    let harness = Harness::new(quick())?;
    let id = harness.artifact(OWNER, "stalled").await?;
    // Opened and never read: each producer ends up waiting to hand an event
    // over.
    let mut streams = Vec::new();
    for _ in 0..4 {
        streams.push(harness.open(OWNER, &id, "n").await?);
    }
    let mut base = 0;
    timeout(WAIT, async {
        for _ in 0..5 {
            base = harness.put(OWNER, &id, base, "n", &json!(base)).await?;
        }
        Ok::<(), Box<dyn Error>>(())
    })
    .await
    .map_err(|_| "a write waited for a stream")??;
    assert_eq!(base, 5);
    Ok(())
}

#[tokio::test]
async fn a_stored_value_that_cannot_be_read_is_a_server_error_and_ends_streams() -> TestResult {
    let harness = Harness::new(quick())?;
    let id = harness.artifact(OWNER, "corrupt").await?;
    harness.put(OWNER, &id, 0, "n", &json!(1)).await?;
    let mut stream = harness.open(OWNER, &id, "n").await?;
    stream.expect("snapshot").await?;

    // A number beyond what a space allows is valid JSON for the database's
    // check and unreadable for the domain.
    let connection = rusqlite::Connection::open(harness.directory.join("db.sqlite3"))?;
    connection.execute(
        "UPDATE app_data SET document = '{\"n\":9007199254740993}', data_version = 2
         WHERE artifact_id = ?1",
        [&id],
    )?;
    harness.streams().wake();
    stream.expect_end().await?;

    for uri in [
        format!("/v1/artifacts/{id}/data"),
        format!("/v1/artifacts/{id}/data/members"),
        events_uri(&id, ""),
    ] {
        let reply = harness.call("GET", &uri, OWNER, None, None).await?;
        assert_eq!(
            reply.status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{uri}: {}",
            reply.text
        );
        assert_eq!(reply.json()?["error"]["code"], "internal", "{uri}");
    }
    harness.settled(0, 0).await
}

#[tokio::test]
async fn a_stream_cannot_be_resumed_and_its_query_is_strict() -> TestResult {
    let harness = Harness::new(quick())?;
    let id = harness.artifact(OWNER, "strict").await?;
    let resume = Request::get(events_uri(&id, ""))
        .header(header::AUTHORIZATION, format!("Bearer {OWNER}"))
        .header("last-event-id", "1")
        .body(Body::empty())?;
    let reply = harness.api.clone().oneshot(resume).await?;
    assert_eq!(reply.status(), StatusCode::BAD_REQUEST);
    let base = format!("/v1/artifacts/{id}/data/events");
    for (uri, status) in [
        (format!("{base}?path=a&path=b"), StatusCode::BAD_REQUEST),
        (format!("{base}?other=1"), StatusCode::BAD_REQUEST),
        (
            format!("{base}?path=a//b"),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            format!("{base}?path={}", "a".repeat(65)),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        let reply = harness.call("GET", &uri, OWNER, None, None).await?;
        assert_eq!(reply.status, status, "{uri}: {}", reply.text);
    }
    harness.settled(0, 0).await
}
