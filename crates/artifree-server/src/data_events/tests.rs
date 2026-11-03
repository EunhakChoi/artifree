//! Data streams through the real router, core, and store (decision 0040).

use std::error::Error;
use std::future::poll_fn;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, Response, StatusCode, header};
use http_body::Body as _;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::*;
use crate::auth::AccessToken;
use crate::http::{self, Settings};
use artifree_core::artifacts::{ArtifactError, MetadataChange};
use artifree_domain::data::{DataChange, DataValue};
use artifree_domain::identity::{RequestKey, TokenId, UnixMillis};
use artifree_domain::{AppDataVersion, MetadataRevision};
use artifree_providers::storage::SqliteStore;
use artifree_providers::tokens::SqliteTokenStore;

type TestResult = Result<(), Box<dyn Error>>;

const OWNER: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
/// The longest any wait in these tests lasts before it fails.
const PATIENCE: Duration = Duration::from_secs(10);

fn actor(number: u8) -> ActorId {
    format!("u-{number:032x}")
        .parse()
        .unwrap_or_else(|error| panic!("actor {number}: {error}"))
}

fn instant_plus(base: Instant, millis: u64) -> Instant {
    base + Duration::from_millis(millis)
}

fn settings(min: u64, heartbeat: u64, lifetime: u64) -> DataStreamSettings {
    DataStreamSettings {
        per_actor: 2,
        per_process: 3,
        min_interval: Duration::from_millis(min),
        heartbeat: Duration::from_millis(heartbeat),
        lifetime: Duration::from_millis(lifetime),
    }
}

mod pace {
    use super::*;

    #[test]
    fn the_lifetime_wins_over_a_pending_wake_and_a_due_heartbeat() {
        let start = Instant::now();
        let mut pace = Pace::new(settings(10, 100, 1_000), start);
        pace.woke();
        assert_eq!(pace.step(instant_plus(start, 1_000)), Step::End);
        assert_eq!(pace.step(instant_plus(start, 5_000)), Step::End);
        // The same instant one millisecond earlier is not the end.
        assert_eq!(pace.step(instant_plus(start, 999)), Step::Read);
    }

    #[test]
    fn a_wake_inside_the_minimum_interval_waits_for_its_end_only() {
        let start = Instant::now();
        let mut pace = Pace::new(settings(250, 15_000, 600_000), start);
        pace.woke();
        let at = instant_plus(start, 100);
        assert_eq!(pace.step(at), Step::Throttle(instant_plus(start, 250)));
        assert_eq!(pace.step(instant_plus(start, 250)), Step::Read);
    }

    #[test]
    fn a_throttle_wait_never_passes_the_lifetime() {
        let start = Instant::now();
        let mut pace = Pace::new(settings(5_000, 15_000, 1_000), start);
        pace.woke();
        assert_eq!(
            pace.step(instant_plus(start, 10)),
            Step::Throttle(instant_plus(start, 1_000))
        );
    }

    #[test]
    fn an_idle_wait_ends_at_the_heartbeat_or_the_lifetime_whichever_is_first() {
        let start = Instant::now();
        let pace = Pace::new(settings(10, 500, 10_000), start);
        assert_eq!(
            pace.step(instant_plus(start, 20)),
            Step::Idle(instant_plus(start, 500))
        );
        let short = Pace::new(settings(10, 500, 300), start);
        assert_eq!(
            short.step(instant_plus(start, 20)),
            Step::Idle(instant_plus(start, 300))
        );
    }

    #[test]
    fn the_heartbeat_reads_without_a_wake_and_a_read_restarts_the_clock() {
        let start = Instant::now();
        let mut pace = Pace::new(settings(10, 500, 60_000), start);
        assert_eq!(pace.step(instant_plus(start, 500)), Step::Read);
        pace.read(instant_plus(start, 510));
        assert_eq!(
            pace.step(instant_plus(start, 600)),
            Step::Idle(instant_plus(start, 1_010))
        );
        assert_eq!(pace.step(instant_plus(start, 1_010)), Step::Read);
    }

    #[test]
    fn a_read_clears_the_pending_wake() {
        let start = Instant::now();
        let mut pace = Pace::new(settings(10, 5_000, 60_000), start);
        pace.woke();
        assert_eq!(pace.step(instant_plus(start, 20)), Step::Read);
        pace.read(instant_plus(start, 20));
        assert!(matches!(pace.step(instant_plus(start, 40)), Step::Idle(_)));
    }

    #[test]
    fn a_writer_that_never_pauses_cannot_move_the_end() {
        // Every 5 ms a wake and a read: the lifetime still comes at 1000 ms.
        let start = Instant::now();
        let mut pace = Pace::new(settings(5, 15_000, 1_000), start);
        let mut ended = None;
        for millis in (5..=2_000).step_by(5) {
            let now = instant_plus(start, millis);
            pace.woke();
            match pace.step(now) {
                Step::End => {
                    ended = Some(millis);
                    break;
                }
                Step::Read => pace.read(now),
                other => panic!("unexpected {other:?} at {millis} ms"),
            }
        }
        assert_eq!(ended, Some(1_000));
    }
}

mod places {
    use super::*;

    #[test]
    fn places_are_counted_per_actor_and_per_process_and_freed_on_drop() -> TestResult {
        let streams = DataStreams::new(settings(10, 100, 1_000));
        let (one, two) = (actor(1), actor(2));
        let first = streams.admit(&one).map_err(|r| format!("{r:?}"))?;
        let second = streams.admit(&one).map_err(|r| format!("{r:?}"))?;
        assert_eq!(streams.admit(&one).err(), Some(Refusal::Actor));
        let third = streams.admit(&two).map_err(|r| format!("{r:?}"))?;
        assert_eq!(streams.admit(&two).err(), Some(Refusal::Process));
        assert_eq!(streams.open_streams(), (3, 2));
        drop(first);
        assert!(streams.admit(&one).is_ok(), "a freed place is reusable");
        drop((second, third));
        Ok(())
    }

    #[test]
    fn the_table_is_empty_when_every_stream_ended() -> TestResult {
        let streams = DataStreams::new(settings(10, 100, 1_000));
        let places: Vec<Place> = (1..=3)
            .map(|n| streams.admit(&actor(n)).map_err(|r| format!("{r:?}")))
            .collect::<Result<_, _>>()?;
        assert_eq!(streams.open_streams(), (3, 3));
        drop(places);
        assert_eq!(
            streams.open_streams(),
            (0, 0),
            "an actor entry outlived its last stream"
        );
        Ok(())
    }

    #[test]
    fn the_precheck_records_nothing_and_refuses_at_the_bound() -> TestResult {
        let streams = DataStreams::new(settings(10, 100, 1_000));
        let one = actor(1);
        for _ in 0..10 {
            streams.precheck(&one).map_err(|r| format!("{r:?}"))?;
        }
        assert_eq!(streams.open_streams(), (0, 0));
        let _held = [streams.admit(&one), streams.admit(&one)];
        assert_eq!(streams.precheck(&one), Err(Refusal::Actor));
        Ok(())
    }

    #[test]
    fn notifying_needs_no_subscriber_and_does_not_wait() {
        let streams = DataStreams::new(settings(10, 100, 1_000));
        for _ in 0..1_000 {
            streams.notify();
        }
        assert_eq!(*streams.writes.subscribe().borrow(), 1_000);
    }

    #[test]
    fn settings_outside_their_range_are_refused() {
        let good = settings(10, 100, 1_000);
        assert!(good.validate().is_ok());
        for bad in [
            DataStreamSettings {
                per_actor: 0,
                ..good
            },
            DataStreamSettings {
                per_process: 0,
                ..good
            },
            DataStreamSettings {
                min_interval: Duration::ZERO,
                ..good
            },
            DataStreamSettings {
                heartbeat: Duration::ZERO,
                ..good
            },
            DataStreamSettings {
                lifetime: Duration::from_secs(25 * 3600),
                ..good
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }
}

struct Harness {
    directory: PathBuf,
    state: AppState,
    api: Router,
}

impl Harness {
    fn new(streams: DataStreamSettings) -> Result<Self, Box<dyn Error>> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!(
            "artifree-data-events-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory)?;
        let database = directory.join("db.sqlite3");
        let state = AppState::new(
            SqliteStore::open(&database)?,
            SqliteTokenStore::open(&database)?,
            AccessToken::parse(OWNER).ok_or("fixture token")?,
            Settings {
                content_origin: "http://127.0.0.1:7411".into(),
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

    async fn send(
        &self,
        method: &str,
        uri: &str,
        token: &str,
        key: Option<&str>,
        body: Option<Value>,
    ) -> Result<(StatusCode, HeaderMap, Value), Box<dyn Error>> {
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
        let (status, headers) = (response.status(), response.headers().clone());
        let bytes = to_bytes(response.into_body(), usize::MAX).await?;
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Ok((status, headers, value))
    }

    async fn ok(
        &self,
        method: &str,
        uri: &str,
        token: &str,
        key: Option<&str>,
        body: Option<Value>,
    ) -> Result<Value, Box<dyn Error>> {
        let (status, _, value) = self.send(method, uri, token, key, body).await?;
        assert!(status.is_success(), "{method} {uri}: {status} {value}");
        Ok(value)
    }

    async fn create(&self, token: &str, key: &str) -> Result<String, Box<dyn Error>> {
        let page = json!({
            "title": "Followed",
            "files": [{ "path": "index.html", "encoding": "utf8", "content": "<p>1</p>" }]
        });
        let created = self
            .ok("POST", "/v1/artifacts", token, Some(key), Some(page))
            .await?;
        Ok(created["artifact"]["id"]
            .as_str()
            .ok_or("no artifact id")?
            .to_owned())
    }

    async fn set(
        &self,
        id: &str,
        base: u64,
        path: &str,
        value: &Value,
    ) -> Result<Value, Box<dyn Error>> {
        let body = json!({ "path": path, "base_data_version": base, "value": value });
        self.ok(
            "PUT",
            &format!("/v1/artifacts/{id}/data"),
            OWNER,
            Some(&format!("set-{id}-{base}-{}", path.replace('/', "-"))),
            Some(body),
        )
        .await
    }

    async fn open(
        &self,
        id: &str,
        query: &str,
        token: &str,
    ) -> Result<Response<Body>, Box<dyn Error>> {
        let request = Request::get(format!("/v1/artifacts/{id}/data/events{query}"))
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::ACCEPT, "text/event-stream")
            .body(Body::empty())?;
        Ok(self.api.clone().oneshot(request).await?)
    }

    async fn follow(&self, id: &str, query: &str, token: &str) -> Result<Events, Box<dyn Error>> {
        let response = self.open(id, query, token).await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .map(|v| v.as_bytes().to_vec()),
            Some(b"text/event-stream".to_vec())
        );
        Ok(Events {
            body: response.into_body(),
            buffer: String::new(),
        })
    }

    /// Waits until `condition` holds; fails after [`PATIENCE`].
    async fn eventually(&self, what: &str, condition: impl Fn(&AppState) -> bool) -> TestResult {
        let wait = async {
            while !condition(&self.state) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        tokio::time::timeout(PATIENCE, wait)
            .await
            .map_err(|_| format!("{what}: not true within {PATIENCE:?}").into())
    }

    async fn nothing_held(&self) -> TestResult {
        self.eventually(
            "every stream released its place and registration",
            |state| state.data_streams().open_streams() == (0, 0) && state.feed().registered() == 0,
        )
        .await
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[derive(Debug)]
struct Frame2 {
    name: String,
    data: Value,
    /// Whether the event block had an `id:` line.
    has_id: bool,
}

struct Events {
    body: Body,
    buffer: String,
}

impl Events {
    /// The next event, skipping comments; `None` when the stream ended. Fails
    /// after [`PATIENCE`].
    async fn next(&mut self) -> Result<Option<Frame2>, Box<dyn Error>> {
        loop {
            if let Some(end) = self.buffer.find("\n\n") {
                let block: String = self.buffer.drain(..end + 2).collect();
                let field = |name: &str| {
                    block
                        .lines()
                        .find_map(|line| line.strip_prefix(name)?.strip_prefix(": "))
                        .map(str::to_owned)
                };
                let Some(name) = field("event") else { continue };
                return Ok(Some(Frame2 {
                    name,
                    data: serde_json::from_str(&field("data").ok_or("an event without data")?)?,
                    has_id: block.lines().any(|line| line.starts_with("id:")),
                }));
            }
            let frame = tokio::time::timeout(
                PATIENCE,
                poll_fn(|context| Pin::new(&mut self.body).poll_frame(context)),
            )
            .await
            .map_err(|_| format!("no event within {PATIENCE:?}"))?;
            match frame {
                None => return Ok(None),
                Some(frame) => {
                    if let Ok(data) = frame?.into_data() {
                        self.buffer.push_str(std::str::from_utf8(&data)?);
                    }
                }
            }
        }
    }

    async fn expect(&mut self, name: &str) -> Result<Value, Box<dyn Error>> {
        let event = self.next().await?.ok_or("the stream ended")?;
        assert_eq!(event.name, name, "{event:?}");
        assert!(!event.has_id, "data events carry no id: {event:?}");
        Ok(event.data)
    }

    async fn ends(&mut self) -> TestResult {
        match self.next().await? {
            None => Ok(()),
            Some(event) => Err(format!("the stream continued with {event:?}").into()),
        }
    }
}

fn quick() -> DataStreamSettings {
    DataStreamSettings {
        per_actor: 4,
        per_process: 16,
        min_interval: Duration::from_millis(5),
        heartbeat: Duration::from_secs(60),
        lifetime: Duration::from_secs(60),
    }
}

#[tokio::test]
async fn a_stream_sends_a_snapshot_then_the_full_value_each_time_the_version_moves() -> TestResult {
    let harness = Harness::new(quick())?;
    let id = harness.create(OWNER, "create-1").await?;
    let mut events = harness.follow(&id, "?path=todos", OWNER).await?;
    assert_eq!(
        events.expect("snapshot").await?,
        json!({ "artifact_id": id, "path": "todos", "data_version": 0, "exists": false })
    );

    harness.set(&id, 0, "todos", &json!({ "a": 1 })).await?;
    assert_eq!(
        events.expect("change").await?,
        json!({ "artifact_id": id, "path": "todos", "data_version": 1, "exists": true,
                "value": { "a": 1 } })
    );
    harness.set(&id, 1, "todos/b", &json!(null)).await?;
    assert_eq!(
        events.expect("change").await?,
        json!({ "artifact_id": id, "path": "todos", "data_version": 2, "exists": true,
                "value": { "a": 1, "b": null } })
    );
    // A write elsewhere moves the version, so it is announced too.
    harness.set(&id, 2, "other", &json!(7)).await?;
    let elsewhere = events.expect("change").await?;
    assert_eq!(elsewhere["data_version"], 3);
    assert_eq!(elsewhere["value"], json!({ "a": 1, "b": null }));
    Ok(())
}

#[tokio::test]
async fn the_snapshot_of_an_existing_value_carries_it_and_a_stored_null_is_present() -> TestResult {
    let harness = Harness::new(quick())?;
    let id = harness.create(OWNER, "create-2").await?;
    harness.set(&id, 0, "n", &json!(null)).await?;
    let mut events = harness.follow(&id, "?path=n", OWNER).await?;
    let snapshot = events.expect("snapshot").await?;
    assert_eq!(snapshot["exists"], true);
    assert_eq!(snapshot.get("value"), Some(&Value::Null), "{snapshot}");
    Ok(())
}

#[tokio::test]
async fn a_burst_of_writes_is_coalesced_and_versions_only_increase() -> TestResult {
    let mut timing = quick();
    timing.min_interval = Duration::from_millis(250);
    let harness = Harness::new(timing)?;
    let id = harness.create(OWNER, "create-3").await?;
    let mut events = harness.follow(&id, "?path=count", OWNER).await?;
    events.expect("snapshot").await?;
    let before = harness.state.data_streams().reads();
    let writes: u64 = 30;
    let started = Instant::now();
    for version in 0..writes {
        harness.set(&id, version, "count", &json!(version)).await?;
    }
    let mut versions = Vec::new();
    while versions.last() != Some(&writes) {
        let change = events.expect("change").await?;
        versions.push(change["data_version"].as_u64().ok_or("version")?);
    }
    assert!(
        versions.windows(2).all(|pair| pair[0] < pair[1]),
        "{versions:?}"
    );
    let reads = harness.state.data_streams().reads() - before;
    // One read per 250 ms at most, however long the writes took, and the
    // first read waits for the end of the interval too.
    let allowed = u64::try_from(started.elapsed().as_millis() / 250)? + 2;
    assert!(
        reads <= allowed && reads < writes,
        "{writes} writes caused {reads} reads in {:?}; at most {allowed} are allowed",
        started.elapsed()
    );
    assert!(
        versions.len() as u64 <= reads,
        "a change needs a read that moved the version"
    );
    Ok(())
}

#[tokio::test]
async fn a_value_over_the_event_bound_is_a_too_large_event_and_ends_the_stream() -> TestResult {
    let harness = Harness::new(quick())?;
    let id = harness.create(OWNER, "create-4").await?;
    let big = json!({ "s": "x".repeat(EVENT_VALUE_BYTES) });
    let mut events = harness.follow(&id, "?path=big", OWNER).await?;
    events.expect("snapshot").await?;
    harness.set(&id, 0, "big", &big).await?;
    assert_eq!(
        events.expect("too_large").await?,
        json!({ "artifact_id": id, "path": "big", "data_version": 1,
                "max_bytes": EVENT_VALUE_BYTES })
    );
    events.ends().await?;
    harness.nothing_held().await?;

    // Opening on a value that is already too large starts with the event.
    let mut again = harness.follow(&id, "?path=big", OWNER).await?;
    let first = again.expect("too_large").await?;
    assert_eq!(first["data_version"], 1);
    again.ends().await
}

#[tokio::test]
async fn a_value_exactly_at_the_event_bound_is_still_carried() -> TestResult {
    let harness = Harness::new(quick())?;
    let id = harness.create(OWNER, "create-5").await?;
    // {"s":"..."} is 8 bytes of structure around the text.
    let fits = json!({ "s": "x".repeat(EVENT_VALUE_BYTES - 8) });
    harness.set(&id, 0, "edge", &fits).await?;
    let mut events = harness.follow(&id, "?path=edge", OWNER).await?;
    let snapshot = events.expect("snapshot").await?;
    assert_eq!(snapshot["value"], fits);
    Ok(())
}

#[tokio::test]
async fn requests_that_break_the_rules_are_refused_before_storage_is_read() -> TestResult {
    let harness = Harness::new(quick())?;
    let id = harness.create(OWNER, "create-6").await?;
    let reads = harness.state.data_streams().reads();
    let base = format!("/v1/artifacts/{id}/data/events");
    let cases: [(String, StatusCode); 6] = [
        (
            format!("{base}?path=a//b"),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            format!("{base}?path={}", "a".repeat(65)),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            format!("{base}?path=%C3%A9"),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (format!("{base}?bogus=1"), StatusCode::BAD_REQUEST),
        (format!("{base}?path=a&path=b"), StatusCode::BAD_REQUEST),
        (
            format!("{base}?path={}", ["s"; 17].join("/")),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ];
    for (uri, expected) in cases {
        let (status, _, body) = harness.send("GET", &uri, OWNER, None, None).await?;
        assert_eq!(status, expected, "{uri}: {body}");
    }
    let resume = Request::get(&base)
        .header(header::AUTHORIZATION, format!("Bearer {OWNER}"))
        .header("last-event-id", "5")
        .body(Body::empty())?;
    let response = harness.api.clone().oneshot(resume).await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let none = Request::get(&base).body(Body::empty())?;
    assert_eq!(
        harness.api.clone().oneshot(none).await?.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        harness.state.data_streams().reads(),
        reads,
        "a refused request read storage"
    );
    assert_eq!(harness.state.data_streams().open_streams(), (0, 0));
    Ok(())
}

#[tokio::test]
async fn a_missing_trashed_or_foreign_artifact_is_the_same_404_and_holds_nothing() -> TestResult {
    let harness = Harness::new(quick())?;
    let other = harness
        .ok(
            "POST",
            "/v1/users",
            OWNER,
            None,
            Some(json!({ "label": "bob" })),
        )
        .await?;
    let issued = harness
        .ok(
            "POST",
            "/v1/tokens",
            OWNER,
            Some("issue-bob-1"),
            Some(json!({ "name": "bob", "user_id": other["id"] })),
        )
        .await?;
    let bob = issued["secret"].as_str().ok_or("secret")?.to_owned();
    let id = harness.create(OWNER, "create-7").await?;
    let mut answers = Vec::new();
    for (uri_id, token) in [
        (id.as_str(), bob.as_str()),
        ("art_00000000000000000000000000000000", OWNER),
        ("art_00000000000000000000000000000000", bob.as_str()),
    ] {
        let (status, _, body) = harness
            .send(
                "GET",
                &format!("/v1/artifacts/{uri_id}/data/events?path=a"),
                token,
                None,
                None,
            )
            .await?;
        assert_eq!(status, StatusCode::NOT_FOUND);
        answers.push(body);
        assert_eq!(
            harness.state.data_streams().open_streams(),
            (0, 0),
            "a refused request held a place"
        );
        assert_eq!(harness.state.feed().registered(), 0);
    }
    assert!(
        answers.windows(2).all(|pair| pair[0] == pair[1]),
        "{answers:?}"
    );

    // Trashed.
    harness
        .ok(
            "PATCH",
            &format!("/v1/artifacts/{id}"),
            OWNER,
            Some("trash-1"),
            Some(json!({ "base_metadata_revision": 1, "deleted": true })),
        )
        .await?;
    let (status, _, body) = harness
        .send(
            "GET",
            &format!("/v1/artifacts/{id}/data/events"),
            OWNER,
            None,
            None,
        )
        .await?;
    assert_eq!((status, &body), (StatusCode::NOT_FOUND, &answers[0]));
    Ok(())
}

#[tokio::test]
async fn trashing_ends_open_streams_and_purged_data_cannot_be_followed() -> TestResult {
    let harness = Harness::new(quick())?;
    let id = harness.create(OWNER, "create-8").await?;
    harness.set(&id, 0, "secret", &json!({ "k": "v" })).await?;
    let mut events = harness.follow(&id, "?path=secret", OWNER).await?;
    events.expect("snapshot").await?;
    harness
        .ok(
            "PATCH",
            &format!("/v1/artifacts/{id}"),
            OWNER,
            Some("trash-2"),
            Some(json!({ "base_metadata_revision": 1, "deleted": true })),
        )
        .await?;
    events.ends().await?;
    harness.nothing_held().await?;

    harness
        .ok(
            "PATCH",
            &format!("/v1/artifacts/{id}"),
            OWNER,
            Some("recover-2"),
            Some(json!({ "base_metadata_revision": 2, "deleted": false })),
        )
        .await?;
    let mut again = harness.follow(&id, "?path=secret", OWNER).await?;
    again.expect("snapshot").await?;
    harness
        .ok(
            "PATCH",
            &format!("/v1/artifacts/{id}"),
            OWNER,
            Some("trash-3"),
            Some(json!({ "base_metadata_revision": 3, "deleted": true })),
        )
        .await?;
    again.ends().await?;
    harness
        .ok(
            "POST",
            &format!("/v1/artifacts/{id}/purge"),
            OWNER,
            Some("purge-3"),
            Some(json!({ "base_metadata_revision": 4 })),
        )
        .await?;
    let (status, _, _) = harness
        .send(
            "GET",
            &format!("/v1/artifacts/{id}/data/events?path=secret"),
            OWNER,
            None,
            None,
        )
        .await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    harness.nothing_held().await
}

fn heartbeat_only() -> DataStreamSettings {
    DataStreamSettings {
        per_actor: 4,
        per_process: 16,
        min_interval: Duration::from_millis(5),
        heartbeat: Duration::from_millis(60),
        lifetime: Duration::from_secs(60),
    }
}

#[tokio::test]
async fn a_write_nobody_announced_arrives_at_the_heartbeat() -> TestResult {
    // Another process wrote: no notification came, the heartbeat read finds it.
    let harness = Harness::new(heartbeat_only())?;
    let id = harness.create(OWNER, "create-9").await?;
    let mut events = harness.follow(&id, "?path=x", OWNER).await?;
    events.expect("snapshot").await?;
    let artifact: ArtifactId = id.parse()?;
    let key: RequestKey = "quiet-write-key".parse()?;
    let change = DataChange::Set {
        path: "x".parse()?,
        value: DataValue::parse("5")?,
    };
    harness
        .state
        .run(move |artifacts| {
            artifacts.write_data(
                &ActorId::owner(),
                &key,
                &artifact,
                AppDataVersion(0),
                &change,
                UnixMillis(1),
            )
        })
        .await?;
    let found = events.expect("change").await?;
    assert_eq!(
        (found["data_version"].clone(), found["value"].clone()),
        (json!(1), json!(5))
    );
    Ok(())
}

#[tokio::test]
async fn the_heartbeat_read_ends_a_stream_whose_artifact_went_without_a_notification() -> TestResult
{
    let harness = Harness::new(heartbeat_only())?;
    let id = harness.create(OWNER, "create-10").await?;
    let mut events = harness.follow(&id, "", OWNER).await?;
    events.expect("snapshot").await?;
    let artifact: ArtifactId = id.parse()?;
    let key: RequestKey = "quiet-trash-key".parse()?;
    // Trash through the service: no data notification is sent.
    harness
        .state
        .run(move |artifacts| {
            let change =
                MetadataChange::new(None, None, Some(true)).ok_or(ArtifactError::NotInTrash)?;
            artifacts.update(
                &ActorId::owner(),
                &key,
                &artifact,
                MetadataRevision(1),
                &change,
            )
        })
        .await?;
    events.ends().await?;
    harness.nothing_held().await
}

#[tokio::test]
async fn a_busy_writer_cannot_keep_a_stream_past_its_lifetime() -> TestResult {
    let harness = Harness::new(DataStreamSettings {
        per_actor: 4,
        per_process: 16,
        min_interval: Duration::from_millis(5),
        heartbeat: Duration::from_secs(60),
        lifetime: Duration::from_millis(400),
    })?;
    let id = harness.create(OWNER, "create-11").await?;
    let mut events = harness.follow(&id, "?path=n", OWNER).await?;
    events.expect("snapshot").await?;
    let state = harness.state.clone();
    let writer = tokio::spawn(async move {
        loop {
            state.data_streams().notify();
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });
    let ended = events.ends().await;
    writer.abort();
    ended?;
    harness.nothing_held().await
}

#[tokio::test]
async fn a_writer_is_never_refused_or_delayed_by_a_subscriber() -> TestResult {
    let harness = Harness::new(quick())?;
    let id = harness.create(OWNER, "create-12").await?;
    // Nobody subscribed: the notification has no receiver and must not fail.
    harness.set(&id, 0, "a", &json!(1)).await?;
    // A subscriber that never reads holds its stream; writes still commit.
    let _stalled = harness.follow(&id, "?path=a", OWNER).await?;
    for version in 1..20 {
        tokio::time::timeout(
            PATIENCE,
            harness.set(&id, version, "a", &json!(version + 100)),
        )
        .await
        .map_err(|_| "a write waited for a subscriber")??;
    }
    Ok(())
}

#[tokio::test]
async fn a_stored_space_that_cannot_be_parsed_is_a_server_error_and_ends_streams() -> TestResult {
    let harness = Harness::new(quick())?;
    let id = harness.create(OWNER, "create-13").await?;
    harness.set(&id, 0, "a", &json!(1)).await?;
    let mut events = harness.follow(&id, "?path=a", OWNER).await?;
    events.expect("snapshot").await?;
    // Valid JSON for SQLite, a number the data rules refuse.
    let database = rusqlite::Connection::open(harness.directory.join("db.sqlite3"))?;
    database.execute(
        "UPDATE app_data SET document = '{\"a\":9007199254740993}' WHERE artifact_id = ?1",
        [&id],
    )?;
    harness.state.data_streams().notify();
    events.ends().await?;
    for uri in [
        format!("/v1/artifacts/{id}/data/events?path=a"),
        format!("/v1/artifacts/{id}/data/members"),
        format!("/v1/artifacts/{id}/data?path=a"),
    ] {
        let (status, _, body) = harness.send("GET", &uri, OWNER, None, None).await?;
        assert_ne!(status, StatusCode::OK, "{uri}: a corrupt space was served");
        assert!(status.is_server_error(), "{uri}: {status} {body}");
    }
    harness.nothing_held().await
}

#[tokio::test]
async fn disabling_the_user_and_revoking_the_token_end_the_stream() -> TestResult {
    let harness = Harness::new(quick())?;
    let user = harness
        .ok(
            "POST",
            "/v1/users",
            OWNER,
            None,
            Some(json!({ "label": "carol" })),
        )
        .await?;
    let user_id = user["id"].as_str().ok_or("id")?.to_owned();
    let mut tokens = Vec::new();
    for key in ["issue-carol-1", "issue-carol-2"] {
        let issued = harness
            .ok(
                "POST",
                "/v1/tokens",
                OWNER,
                Some(key),
                Some(json!({ "name": key, "user_id": user_id })),
            )
            .await?;
        tokens.push((
            issued["secret"].as_str().ok_or("secret")?.to_owned(),
            issued["token"]["id"].as_str().ok_or("token id")?.to_owned(),
        ));
    }
    let id = harness.create(&tokens[0].0, "create-14").await?;

    let mut revoked = harness.follow(&id, "", &tokens[0].0).await?;
    revoked.expect("snapshot").await?;
    harness
        .ok(
            "DELETE",
            &format!("/v1/tokens/{}", tokens[0].1),
            OWNER,
            None,
            None,
        )
        .await?;
    revoked.ends().await?;

    let mut disabled = harness.follow(&id, "", &tokens[1].0).await?;
    disabled.expect("snapshot").await?;
    harness
        .ok(
            "POST",
            &format!("/v1/users/{user_id}/disable"),
            OWNER,
            Some("disable-1"),
            None,
        )
        .await?;
    disabled.ends().await?;
    harness.nothing_held().await
}

/// A revocation that runs while a request is between registering and its
/// first read must end the stream it then opens, whichever way it reaches the
/// registration (the token, or the user). The request is held at the point
/// just before the first read. Repeated, because a stream whose revocation is
/// already signalled must never be left to `select!`'s random choice.
#[tokio::test]
async fn a_revocation_during_the_open_ends_the_stream_without_an_event() -> TestResult {
    let harness = Harness::new(quick())?;
    let id = harness.create(OWNER, "create-20").await?;
    let issued = harness
        .ok(
            "POST",
            "/v1/tokens",
            OWNER,
            Some("issue-agent-20"),
            Some(json!({ "name": "agent" })),
        )
        .await?;
    let secret = issued["secret"].as_str().ok_or("secret")?.to_owned();
    let token_id: TokenId = issued["token"]["id"].as_str().ok_or("token id")?.parse()?;
    let streams = harness.state.data_streams();
    for round in 0..8_u64 {
        for by_actor in [false, true] {
            streams.hold_first_reads(true);
            let held_before = streams.held();
            let opening = harness.open(&id, "", &secret);
            let revoke = async {
                harness
                    .eventually("the open request reached the hold", |state| {
                        state.data_streams().held() > held_before
                    })
                    .await?;
                assert_eq!(
                    harness.state.feed().registered(),
                    1,
                    "round {round}: a request held before its first read is not registered for revocation"
                );
                if by_actor {
                    harness.state.revoke_actor(&ActorId::owner());
                } else {
                    harness.state.feed().revoke_token(&token_id);
                }
                streams.hold_first_reads(false);
                Ok::<(), Box<dyn Error>>(())
            };
            let (response, revoked) = tokio::join!(opening, revoke);
            revoked?;
            let response = response?;
            assert_eq!(response.status(), StatusCode::OK);
            let mut events = Events {
                body: response.into_body(),
                buffer: String::new(),
            };
            events
                .ends()
                .await
                .map_err(|error| format!("round {round}, by_actor {by_actor}: {error}"))?;
            harness.nothing_held().await?;
        }
    }
    Ok(())
}

#[tokio::test]
async fn a_request_that_fails_or_is_refused_while_registered_leaves_no_registration() -> TestResult
{
    let harness = Harness::new(DataStreamSettings {
        per_actor: 1,
        ..quick()
    })?;
    let id = harness.create(OWNER, "create-21").await?;
    let issued = harness
        .ok(
            "POST",
            "/v1/tokens",
            OWNER,
            Some("issue-agent-21"),
            Some(json!({ "name": "agent" })),
        )
        .await?;
    let secret = issued["secret"].as_str().ok_or("secret")?.to_owned();
    // A missing artifact: the registration of the open is dropped with the 404.
    let missing = "art_00000000000000000000000000000000";
    let (status, _, _) = harness
        .send(
            "GET",
            &format!("/v1/artifacts/{missing}/data/events"),
            &secret,
            None,
            None,
        )
        .await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(harness.state.feed().registered(), 0);
    assert_eq!(harness.state.data_streams().open_streams(), (0, 0));
    // A refusal at the per-actor limit: the second open of the same actor.
    let mut held = harness.follow(&id, "", &secret).await?;
    held.expect("snapshot").await?;
    let (status, _, _) = harness
        .send(
            "GET",
            &format!("/v1/artifacts/{id}/data/events"),
            &secret,
            None,
            None,
        )
        .await?;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        harness.state.feed().registered(),
        1,
        "only the open stream is registered"
    );
    drop(held);
    harness.nothing_held().await
}

#[tokio::test]
async fn shutdown_ends_streams() -> TestResult {
    let harness = Harness::new(quick())?;
    let id = harness.create(OWNER, "create-15").await?;
    let mut events = harness.follow(&id, "", OWNER).await?;
    events.expect("snapshot").await?;
    harness.state.close_streams();
    events.ends().await?;
    harness.nothing_held().await
}

#[tokio::test]
async fn a_dropped_connection_frees_its_place() -> TestResult {
    let harness = Harness::new(quick())?;
    let id = harness.create(OWNER, "create-16").await?;
    let mut events = harness.follow(&id, "", OWNER).await?;
    events.expect("snapshot").await?;
    drop(events);
    harness.nothing_held().await
}

#[tokio::test]
async fn the_limits_refuse_with_a_delay_and_a_refused_request_reads_nothing() -> TestResult {
    let harness = Harness::new(settings(5, 60_000, 60_000))?;
    let mine = harness.create(OWNER, "create-17").await?;
    let user = harness
        .ok(
            "POST",
            "/v1/users",
            OWNER,
            None,
            Some(json!({ "label": "dave" })),
        )
        .await?;
    let issued = harness
        .ok(
            "POST",
            "/v1/tokens",
            OWNER,
            Some("issue-dave-1"),
            Some(json!({ "name": "dave", "user_id": user["id"] })),
        )
        .await?;
    let dave = issued["secret"].as_str().ok_or("secret")?.to_owned();
    let theirs = harness.create(&dave, "create-18").await?;

    let _first = harness.follow(&mine, "", OWNER).await?;
    let _second = harness.follow(&mine, "?path=a", OWNER).await?;
    let reads = harness.state.data_streams().reads();
    let response = harness.open(&mine, "", OWNER).await?;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        response
            .headers()
            .get(header::RETRY_AFTER)
            .map(axum::http::HeaderValue::as_bytes),
        Some(b"5".as_slice())
    );
    let body: Value = serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await?)?;
    assert_eq!(body["error"]["code"], "rate_limited");
    // The actor is full, so even a request for a missing artifact is refused
    // at the limit without a read.
    let response = harness
        .open("art_00000000000000000000000000000000", "", OWNER)
        .await?;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

    let _third = harness.follow(&theirs, "", &dave).await?;
    assert_eq!(harness.state.data_streams().open_streams(), (3, 2));
    let reads_before = harness.state.data_streams().reads();
    let response = harness.open(&theirs, "", &dave).await?;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response
            .headers()
            .get(header::RETRY_AFTER)
            .map(axum::http::HeaderValue::as_bytes),
        Some(b"5".as_slice())
    );
    let body: Value = serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await?)?;
    assert_eq!(body["error"]["code"], "unavailable");
    assert_eq!(
        harness.state.data_streams().reads(),
        reads_before,
        "a request refused at a limit read storage"
    );
    assert!(
        reads_before > reads,
        "the opened stream's own read was counted"
    );
    assert_eq!(harness.state.data_streams().open_streams(), (3, 2));
    Ok(())
}

#[test]
fn concurrent_opens_on_one_executor_thread_with_one_blocking_thread_do_not_deadlock() -> TestResult
{
    // A lock held across the await of a storage read would block the only
    // executor thread while the read's result waits to be polled. The run is
    // on its own thread so that a deadlock fails the test instead of hanging.
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let outcome: Result<(), String> = (|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .max_blocking_threads(1)
                .build()
                .map_err(|error| error.to_string())?;
            runtime.block_on(async {
                let harness = Harness::new(quick()).map_err(|error| error.to_string())?;
                let id = harness
                    .create(OWNER, "create-19")
                    .await
                    .map_err(|error| error.to_string())?;
                let (a, b, c) = tokio::join!(
                    harness.follow(&id, "?path=a", OWNER),
                    harness.follow(&id, "?path=b", OWNER),
                    harness.follow(&id, "?path=c", OWNER),
                );
                for opened in [a, b, c] {
                    let mut events = opened.map_err(|error| error.to_string())?;
                    events
                        .expect("snapshot")
                        .await
                        .map_err(|error| error.to_string())?;
                }
                Ok(())
            })
        })();
        let _sent = done.send(outcome);
    });
    finished
        .recv_timeout(PATIENCE)
        .map_err(|_| "opening streams deadlocked")??;
    Ok(())
}
