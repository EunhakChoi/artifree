//! Rooms and presence over HTTP (decision 0034), exercised in process through
//! the real router, runtime connections, application core, and `SQLite`
//! store. Time is a clock the test moves: no test sleeps.
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use artifree_contracts::api::{RoomEventReceipt, RoomSnapshot};
use artifree_providers::storage::SqliteStore;
use artifree_providers::tokens::SqliteTokenStore;
use artifree_server::auth::AccessToken;
use artifree_server::http::{self, AppState, Settings};
use artifree_server::limits::RequestLimits;
use artifree_server::rooms::RoomClock;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
static NEXT_ARTIFACT: AtomicU32 = AtomicU32::new(1);

fn plentiful() -> RequestLimits {
    RequestLimits {
        burst: 100_000,
        per_minute: 6_000_000,
        concurrent: 64,
    }
}

/// A clock the test moves.
#[derive(Debug, Default)]
struct Clock(AtomicU64);

impl Clock {
    fn advance(&self, millis: u64) {
        self.0.fetch_add(millis, Ordering::SeqCst);
    }
}

impl RoomClock for Clock {
    fn elapsed(&self) -> Duration {
        Duration::from_millis(self.0.load(Ordering::SeqCst))
    }
}

struct Harness {
    directory: PathBuf,
    owns_directory: bool,
    api: Router,
    clock: Arc<Clock>,
}

impl Harness {
    fn new() -> Self {
        Self::with_room_limits(plentiful())
    }

    fn with_room_limits(room_limits: RequestLimits) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!(
            "artifree-rooms-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory)
            .unwrap_or_else(|error| panic!("cannot create test directory: {error}"));
        Self::at(directory, true, room_limits)
    }

    /// A server process over `directory`: a second one is a restart.
    fn at(directory: PathBuf, owns_directory: bool, room_limits: RequestLimits) -> Self {
        let store = SqliteStore::open(&directory.join("db.sqlite3"))
            .unwrap_or_else(|error| panic!("cannot open store: {error}"));
        let token_store = SqliteTokenStore::open(&directory.join("db.sqlite3"))
            .unwrap_or_else(|error| panic!("cannot open token store: {error}"));
        let token = AccessToken::parse(TOKEN).unwrap_or_else(|| panic!("fixture token is invalid"));
        let clock = Arc::new(Clock::default());
        let state = AppState::new(
            store,
            token_store,
            token,
            Settings {
                limits: plentiful(),
                room_limits,
                room_clock: clock.clone(),
                ..Settings::default()
            },
        )
        .unwrap_or_else(|error| panic!("cannot build state: {error}"));
        Self {
            directory,
            owns_directory,
            api: http::router(state),
            clock,
        }
    }

    async fn send(&self, request: Request<Body>) -> Result<Reply, Box<dyn std::error::Error>> {
        Reply::from(self.api.clone().oneshot(request).await?).await
    }

    async fn artifact(&self) -> Result<String, Box<dyn std::error::Error>> {
        let key = format!(
            "room-create-{}",
            NEXT_ARTIFACT.fetch_add(1, Ordering::Relaxed)
        );
        let reply = self
            .send(
                authorized(Request::post("/v1/artifacts"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("idempotency-key", key)
                    .body(Body::from(
                        json!({"title": "Room", "files": [
                            {"path": "index.html", "encoding": "utf8",
                             "content": "<!doctype html><title>room</title>"}
                        ]})
                        .to_string(),
                    ))?,
            )
            .await?;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.message());
        Ok(reply.json()?["artifact"]["id"]
            .as_str()
            .ok_or("missing artifact id")?
            .to_owned())
    }

    /// A new tab: a runtime connection the server issued.
    async fn tab(&self) -> Result<String, Box<dyn std::error::Error>> {
        let created = self
            .send(authorized(Request::post("/v1/runtime-sessions")).body(Body::empty())?)
            .await?;
        assert_eq!(created.status, StatusCode::OK, "{}", created.code());
        Ok(created.json()?["session_id"]
            .as_str()
            .ok_or("missing session id")?
            .to_owned())
    }

    async fn join(
        &self,
        tab: &str,
        artifact: &str,
        slot: &str,
    ) -> Result<Reply, Box<dyn std::error::Error>> {
        self.join_with(tab, artifact, json!({ "slot": slot })).await
    }

    async fn join_with(
        &self,
        tab: &str,
        artifact: &str,
        body: Value,
    ) -> Result<Reply, Box<dyn std::error::Error>> {
        self.send(post(&room(tab, artifact), &body)?).await
    }

    async fn beat(
        &self,
        tab: &str,
        artifact: &str,
        who: &RoomSnapshot,
        after: u64,
    ) -> Result<Reply, Box<dyn std::error::Error>> {
        self.send(post(
            &format!(
                "{}/heartbeat",
                member(tab, artifact, who.participant, who.epoch)
            ),
            &json!({ "after": after }),
        )?)
        .await
    }

    async fn emit(
        &self,
        tab: &str,
        artifact: &str,
        who: &RoomSnapshot,
        data: &str,
    ) -> Result<Reply, Box<dyn std::error::Error>> {
        self.send(post(
            &format!(
                "{}/events",
                member(tab, artifact, who.participant, who.epoch)
            ),
            &json!({ "kind": "cursor", "data": data }),
        )?)
        .await
    }

    async fn leave(
        &self,
        tab: &str,
        artifact: &str,
        who: &RoomSnapshot,
    ) -> Result<Reply, Box<dyn std::error::Error>> {
        self.send(post(
            &format!(
                "{}/leave",
                member(tab, artifact, who.participant, who.epoch)
            ),
            &json!({}),
        )?)
        .await
    }

    /// Joins and returns the snapshot of a join that must succeed.
    async fn enter(
        &self,
        tab: &str,
        artifact: &str,
        slot: &str,
    ) -> Result<RoomSnapshot, Box<dyn std::error::Error>> {
        let reply = self.join(tab, artifact, slot).await?;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.code());
        reply.snapshot()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if self.owns_directory {
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

    fn code(&self) -> String {
        self.json()
            .ok()
            .and_then(|body| body["error"]["code"].as_str().map(str::to_owned))
            .unwrap_or_else(|| {
                format!("<no JSON error: {:?}>", String::from_utf8_lossy(&self.body))
            })
    }

    fn message(&self) -> String {
        self.json()
            .ok()
            .and_then(|body| body["error"]["message"].as_str().map(str::to_owned))
            .unwrap_or_default()
    }

    /// The typed body, which refuses any field the contract does not have.
    fn snapshot(&self) -> Result<RoomSnapshot, Box<dyn std::error::Error>> {
        Ok(serde_json::from_slice(&self.body)?)
    }

    fn header(&self, name: header::HeaderName) -> &str {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
    }
}

fn authorized(builder: axum::http::request::Builder) -> axum::http::request::Builder {
    builder.header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
}

fn post(path: &str, body: &Value) -> Result<Request<Body>, axum::http::Error> {
    authorized(Request::post(path))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
}

fn room(tab: &str, artifact: &str) -> String {
    format!("/v1/runtime-sessions/{tab}/artifacts/{artifact}/room/participants")
}

fn member(tab: &str, artifact: &str, participant: u64, epoch: u64) -> String {
    format!("{}/{participant}/{epoch}", room(tab, artifact))
}

/// `(participant, is_me, same_tab)` of every member, in the listed order.
fn flags(view: &RoomSnapshot) -> Vec<(u64, bool, bool)> {
    view.members
        .iter()
        .map(|member| (member.participant, member.is_me, member.same_tab))
        .collect()
}

#[tokio::test]
async fn a_tab_joins_sees_itself_and_leaves() -> TestResult {
    let harness = Harness::new();
    let artifact = harness.artifact().await?;
    let tab = harness.tab().await?;
    let me = harness.enter(&tab, &artifact, "page").await?;
    assert!(me.participant >= 1 && me.epoch >= 1);
    assert_eq!(me.lease_ms, 15_000);
    assert_eq!(me.head, 0);
    assert!(me.events.is_empty() && !me.missed);
    assert_eq!(flags(&me), [(me.participant, true, true)]);
    assert_eq!(me.members[0].actor, "local");
    assert_eq!(me.members[0].state, "");

    let left = harness.leave(&tab, &artifact, &me).await?;
    assert_eq!(left.status, StatusCode::NO_CONTENT);
    let again = harness.leave(&tab, &artifact, &me).await?;
    assert_eq!(
        (again.status, again.code().as_str()),
        (StatusCode::NOT_FOUND, "not_found")
    );
    // The room is empty now: a new participant sees only itself.
    let next = harness.enter(&tab, &artifact, "page").await?;
    assert_eq!(next.members.len(), 1);
    assert_ne!(next.participant, me.participant);
    Ok(())
}

#[tokio::test]
async fn is_me_and_same_tab_differ_for_a_second_participant_of_a_tab_and_for_another_tab()
-> TestResult {
    let harness = Harness::new();
    let artifact = harness.artifact().await?;
    let (one, two) = (harness.tab().await?, harness.tab().await?);
    let page = harness.enter(&one, &artifact, "page").await?;
    let frame = harness.enter(&one, &artifact, "frame").await?;
    let other = harness.enter(&two, &artifact, "page").await?;
    let ids = [page.participant, frame.participant, other.participant];
    assert!(ids[0] < ids[1] && ids[1] < ids[2]);

    // (viewer's tab, viewer) -> (is_me, same_tab) of the page, frame, other.
    let cases = [
        (&one, &page, [(true, true), (false, true), (false, false)]),
        (&one, &frame, [(false, true), (true, true), (false, false)]),
        (&two, &other, [(false, false), (false, false), (true, true)]),
    ];
    for (tab, viewer, expected) in cases {
        let view = harness.beat(tab, &artifact, viewer, 0).await?.snapshot()?;
        let seen: Vec<_> = view.members.iter().map(|m| (m.is_me, m.same_tab)).collect();
        assert_eq!(seen, expected, "seen by participant {}", viewer.participant);
        let listed: Vec<_> = view.members.iter().map(|m| m.participant).collect();
        assert_eq!(listed, ids);
        assert_eq!(
            view.participant, viewer.participant,
            "the viewer is named by the server"
        );
    }
    // The same slot in another tab is another participant, and the tabs are
    // told apart by number.
    assert_ne!(page.tab, other.tab);
    assert_eq!(page.tab, frame.tab);
    Ok(())
}

#[tokio::test]
async fn a_reconnecting_tab_keeps_its_participant_and_the_old_connection_is_refused() -> TestResult
{
    let harness = Harness::new();
    let artifact = harness.artifact().await?;
    let tab = harness.tab().await?;
    let first = harness.enter(&tab, &artifact, "page").await?;
    harness.clock.advance(5_000);
    let again = harness.enter(&tab, &artifact, "page").await?;
    assert_eq!(again.participant, first.participant, "the same participant");
    assert!(again.epoch > first.epoch);
    assert_eq!(
        flags(&again),
        [(first.participant, true, true)],
        "no duplicate"
    );

    let stale = [
        harness.beat(&tab, &artifact, &first, 0).await?,
        harness.emit(&tab, &artifact, &first, "x").await?,
        harness.leave(&tab, &artifact, &first).await?,
    ];
    for reply in &stale {
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::NOT_FOUND, "not_found")
        );
    }
    // The refused calls changed nothing: the new connection still acts.
    let view = harness.beat(&tab, &artifact, &again, 0).await?.snapshot()?;
    assert_eq!(view.members.len(), 1);
    // The lease restarted at the rejoin, 5 s in.
    harness.clock.advance(14_999);
    assert_eq!(
        harness.beat(&tab, &artifact, &again, 0).await?.status,
        StatusCode::OK
    );
    Ok(())
}

#[tokio::test]
async fn a_lease_ends_without_heartbeats_and_a_rejoin_makes_a_new_participant() -> TestResult {
    let harness = Harness::new();
    let artifact = harness.artifact().await?;
    let (one, two) = (harness.tab().await?, harness.tab().await?);
    let quiet = harness.enter(&one, &artifact, "page").await?;
    let steady = harness.enter(&two, &artifact, "page").await?;

    harness.clock.advance(14_999);
    let seen = harness
        .beat(&two, &artifact, &steady, 0)
        .await?
        .snapshot()?;
    assert_eq!(seen.members.len(), 2, "the lease still holds at 14.999 s");
    harness.clock.advance(1);
    let seen = harness
        .beat(&two, &artifact, &steady, 0)
        .await?
        .snapshot()?;
    assert_eq!(
        flags(&seen),
        [(steady.participant, true, true)],
        "gone at 15 s"
    );

    let late = harness.beat(&one, &artifact, &quiet, 0).await?;
    assert_eq!(
        (late.status, late.code().as_str()),
        (StatusCode::NOT_FOUND, "not_found")
    );
    let back = harness.enter(&one, &artifact, "page").await?;
    assert_ne!(
        back.participant, quiet.participant,
        "an expired participant stays gone"
    );
    assert_eq!(back.members.len(), 2);
    Ok(())
}

#[tokio::test]
async fn an_event_reaches_those_present_and_is_never_replayed_to_a_late_joiner() -> TestResult {
    let harness = Harness::new();
    let artifact = harness.artifact().await?;
    let (one, two, three) = (
        harness.tab().await?,
        harness.tab().await?,
        harness.tab().await?,
    );
    let sender = harness.enter(&one, &artifact, "page").await?;
    let reader = harness.enter(&two, &artifact, "page").await?;

    let sent = harness.emit(&one, &artifact, &sender, "before").await?;
    assert_eq!(sent.status, StatusCode::OK, "{}", sent.code());
    let receipt: RoomEventReceipt = serde_json::from_slice(&sent.body)?;
    assert_eq!(receipt.seq, 1);

    let view = harness
        .beat(&two, &artifact, &reader, 0)
        .await?
        .snapshot()?;
    assert_eq!(view.events.len(), 1);
    let event = &view.events[0];
    assert_eq!(
        (
            event.seq,
            event.participant,
            event.kind.as_str(),
            event.data.as_str()
        ),
        (1, sender.participant, "cursor", "before")
    );
    assert_eq!((event.is_me, event.same_tab), (false, false));
    let echoed = harness
        .beat(&one, &artifact, &sender, 0)
        .await?
        .snapshot()?;
    assert_eq!(
        (echoed.events[0].is_me, echoed.events[0].same_tab),
        (true, true)
    );

    // A third tab joins afterwards. Its join returns no event, and asking
    // to read from the beginning does not return the earlier one either.
    let late = harness.enter(&three, &artifact, "page").await?;
    assert!(late.events.is_empty());
    assert_eq!(late.head, 1);
    let asked = harness
        .beat(&three, &artifact, &late, 0)
        .await?
        .snapshot()?;
    assert!(
        asked.events.is_empty() && !asked.missed,
        "{:?}",
        asked.events
    );
    harness.emit(&one, &artifact, &sender, "after").await?;
    let next = harness
        .beat(&three, &artifact, &late, 0)
        .await?
        .snapshot()?;
    let texts: Vec<_> = next
        .events
        .iter()
        .map(|event| event.data.as_str())
        .collect();
    assert_eq!(texts, ["after"]);
    // Reading from the head returns only what is newer.
    let caught_up = harness
        .beat(&three, &artifact, &late, next.head)
        .await?
        .snapshot()?;
    assert!(caught_up.events.is_empty());
    Ok(())
}

#[tokio::test]
async fn a_resumed_join_delivers_what_came_while_its_participant_was_away_or_says_it_missed()
-> TestResult {
    let harness = Harness::new();
    let artifact = harness.artifact().await?;
    let (sending, reading) = (harness.tab().await?, harness.tab().await?);
    let sender = harness.enter(&sending, &artifact, "page").await?;
    let reader = harness.enter(&reading, &artifact, "page").await?;
    harness.emit(&sending, &artifact, &sender, "read").await?;
    let seen = harness
        .beat(&reading, &artifact, &reader, 0)
        .await?
        .snapshot()?;
    assert_eq!(seen.events.len(), 1);
    // The reader says it read event 1, goes away, and two events arrive.
    harness
        .beat(&reading, &artifact, &reader, 1)
        .await?
        .snapshot()?;
    harness.clock.advance(1_000);
    harness.emit(&sending, &artifact, &sender, "away-1").await?;
    harness.emit(&sending, &artifact, &sender, "away-2").await?;
    harness.clock.advance(1_000);
    let resumed = harness.enter(&reading, &artifact, "page").await?;
    assert_eq!(resumed.participant, reader.participant);
    let data: Vec<_> = resumed.events.iter().map(|e| e.data.as_str()).collect();
    assert_eq!(data, ["away-1", "away-2"]);
    assert_eq!((resumed.head, resumed.missed), (3, false));

    // While another participant is away more than the room keeps, the
    // resuming join says so instead of passing silently.
    let away = harness.tab().await?;
    let gone = harness.enter(&away, &artifact, "page").await?;
    let senders = [
        harness.tab().await?,
        harness.tab().await?,
        harness.tab().await?,
    ];
    let mut talking = Vec::new();
    for tab in &senders {
        for slot in ["a", "b", "c", "d"] {
            talking.push((tab, harness.enter(tab, &artifact, slot).await?));
        }
    }
    // Twelve senders, a burst of eight each, twice: 192 events, 128 kept.
    for _ in 0..2 {
        for (tab, who) in &talking {
            for _ in 0..8 {
                let sent = harness.emit(tab, &artifact, who, "e").await?;
                assert_eq!(sent.status, StatusCode::OK, "{}", sent.code());
            }
        }
        harness.clock.advance(5_000);
    }
    let back = harness.enter(&away, &artifact, "page").await?;
    assert_eq!(back.participant, gone.participant);
    assert_eq!(back.events.len(), 128);
    assert_eq!(back.events[0].seq, 3 + 192 - 127);
    assert!(back.missed, "the oldest events were dropped before it read");
    Ok(())
}

#[tokio::test]
async fn events_are_not_stored_and_a_restart_empties_every_room() -> TestResult {
    let harness = Harness::new();
    let artifact = harness.artifact().await?;
    let tab = harness.tab().await?;
    let me = harness.enter(&tab, &artifact, "page").await?;
    let marker = "ephemeral-marker-4c1f9b7d";
    let sent = harness.emit(&tab, &artifact, &me, marker).await?;
    assert_eq!(sent.status, StatusCode::OK);
    let sentinel = harness
        .send(post(
            &format!(
                "/v1/runtime-sessions/{tab}/artifacts/{artifact}/room/participants/{}/{}/heartbeat",
                me.participant, me.epoch
            ),
            &json!({"after": 0, "state": "state-marker-8e2a6d03"}),
        )?)
        .await?;
    assert_eq!(sentinel.status, StatusCode::OK);

    // Nothing of the room is in the database or its log.
    for file in ["db.sqlite3", "db.sqlite3-wal"] {
        let bytes = std::fs::read(harness.directory.join(file)).unwrap_or_default();
        for needle in [marker, "state-marker-8e2a6d03"] {
            assert!(
                !bytes
                    .windows(needle.len())
                    .any(|window| window == needle.as_bytes()),
                "{file} holds {needle}"
            );
        }
    }

    // A second process over the same files is a restart: the connection and
    // the room are both gone, and a new tab finds an empty room.
    let restarted = Harness::at(harness.directory.clone(), false, plentiful());
    let old = restarted.beat(&tab, &artifact, &me, 0).await?;
    assert_eq!(
        (old.status, old.code().as_str()),
        (StatusCode::NOT_FOUND, "runtime_session_expired")
    );
    let fresh = restarted.tab().await?;
    let view = restarted.enter(&fresh, &artifact, "page").await?;
    assert_eq!((view.members.len(), view.head), (1, 0));
    Ok(())
}

#[tokio::test]
async fn another_tab_cannot_act_as_a_participant_it_does_not_hold() -> TestResult {
    let harness = Harness::new();
    let artifact = harness.artifact().await?;
    let (mine, theirs) = (harness.tab().await?, harness.tab().await?);
    let owner = harness.enter(&mine, &artifact, "page").await?;
    let other = harness.enter(&theirs, &artifact, "page").await?;
    // The second tab names the first tab's participant and epoch exactly.
    let attempts = [
        harness.beat(&theirs, &artifact, &owner, 0).await?,
        harness.emit(&theirs, &artifact, &owner, "x").await?,
        harness.leave(&theirs, &artifact, &owner).await?,
    ];
    for reply in &attempts {
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::NOT_FOUND, "not_found")
        );
    }
    let view = harness
        .beat(&mine, &artifact, &owner, 0)
        .await?
        .snapshot()?;
    assert_eq!(view.members.len(), 2, "nothing was removed or sent");
    assert!(view.events.is_empty());
    let own = harness
        .beat(&theirs, &artifact, &other, 0)
        .await?
        .snapshot()?;
    assert!(own.events.is_empty());
    Ok(())
}

#[tokio::test]
async fn closing_a_connection_removes_its_participants_at_once() -> TestResult {
    let harness = Harness::new();
    let artifact = harness.artifact().await?;
    let (closing, staying) = (harness.tab().await?, harness.tab().await?);
    harness.enter(&closing, &artifact, "page").await?;
    harness.enter(&closing, &artifact, "frame").await?;
    let kept = harness.enter(&staying, &artifact, "page").await?;
    let closed = harness
        .send(
            authorized(Request::delete(format!("/v1/runtime-sessions/{closing}")))
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(closed.status, StatusCode::NO_CONTENT);
    let view = harness
        .beat(&staying, &artifact, &kept, 0)
        .await?
        .snapshot()?;
    assert_eq!(flags(&view), [(kept.participant, true, true)]);
    // The closed connection cannot join or act again.
    let joined = harness.join(&closing, &artifact, "page").await?;
    assert_eq!(
        (joined.status, joined.code().as_str()),
        (StatusCode::NOT_FOUND, "runtime_session_expired")
    );
    Ok(())
}

#[tokio::test]
async fn a_trashed_artifact_ends_room_calls_but_a_tab_can_still_leave() -> TestResult {
    let harness = Harness::new();
    let artifact = harness.artifact().await?;
    let tab = harness.tab().await?;
    let me = harness.enter(&tab, &artifact, "page").await?;
    let trashed = harness
        .send(
            authorized(Request::patch(format!("/v1/artifacts/{artifact}")))
                .header(header::CONTENT_TYPE, "application/json")
                .header("idempotency-key", "room-trash-1")
                .body(Body::from(
                    json!({"base_metadata_revision": 1, "deleted": true}).to_string(),
                ))?,
        )
        .await?;
    assert_eq!(trashed.status, StatusCode::OK, "{}", trashed.code());
    let calls = [
        harness.join(&tab, &artifact, "page").await?,
        harness.beat(&tab, &artifact, &me, 0).await?,
        harness.emit(&tab, &artifact, &me, "x").await?,
    ];
    for reply in &calls {
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::NOT_FOUND, "not_found")
        );
    }
    assert_eq!(
        harness.leave(&tab, &artifact, &me).await?.status,
        StatusCode::NO_CONTENT
    );
    Ok(())
}

#[tokio::test]
async fn room_calls_need_the_token_a_live_connection_and_a_known_artifact() -> TestResult {
    let harness = Harness::new();
    let artifact = harness.artifact().await?;
    let tab = harness.tab().await?;
    let me = harness.enter(&tab, &artifact, "page").await?;
    let base = member(&tab, &artifact, me.participant, me.epoch);
    // Without the token every route answers 401 before anything else.
    for request in [
        Request::post(room(&tab, &artifact)),
        Request::post(format!("{base}/heartbeat")),
        Request::post(format!("{base}/events")),
        Request::post(format!("{base}/leave")),
    ] {
        let reply = harness
            .send(
                request
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))?,
            )
            .await?;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{}", reply.code());
    }
    // A connection the server never issued, and one that is malformed.
    let unknown = harness.join(&"0".repeat(32), &artifact, "page").await?;
    assert_eq!(
        (unknown.status, unknown.code().as_str()),
        (StatusCode::NOT_FOUND, "runtime_session_expired")
    );
    let malformed = harness.join("not-a-connection", &artifact, "page").await?;
    assert_eq!(malformed.status, StatusCode::BAD_REQUEST);
    // An artifact that does not exist.
    let missing = harness
        .join(&tab, &format!("art_{}", "0".repeat(32)), "page")
        .await?;
    assert_eq!(
        (missing.status, missing.code().as_str()),
        (StatusCode::NOT_FOUND, "not_found")
    );
    let invalid = harness.join(&tab, "nonsense", "page").await?;
    assert_eq!(invalid.status, StatusCode::NOT_FOUND);
    // Unsupported methods.
    for request in [
        Request::get(room(&tab, &artifact)),
        Request::get(format!("{base}/heartbeat")),
        Request::put(format!("{base}/events")),
        Request::delete(format!("{base}/leave")),
    ] {
        let reply = harness
            .send(authorized(request).body(Body::empty())?)
            .await?;
        assert_eq!(reply.status, StatusCode::METHOD_NOT_ALLOWED);
        assert!(!reply.header(header::ALLOW).is_empty());
    }
    Ok(())
}

#[tokio::test]
async fn a_caller_without_a_live_connection_learns_nothing_from_how_its_body_is_judged()
-> TestResult {
    let harness = Harness::new();
    let artifact = harness.artifact().await?;
    let tab = harness.tab().await?;
    let unknown = "0".repeat(32);
    // The same invalid slot: refused for the live tab, not found for the
    // other.
    let bad = json!({ "slot": "has space" });
    let live = harness.join_with(&tab, &artifact, bad.clone()).await?;
    assert_eq!(live.status, StatusCode::BAD_REQUEST);
    let dead = harness.join_with(&unknown, &artifact, bad).await?;
    assert_eq!(
        (dead.status, dead.code().as_str()),
        (StatusCode::NOT_FOUND, "runtime_session_expired")
    );
    Ok(())
}

/// The path of a room call from a tab, an artifact and the caller's participant.
type RoutePath = fn(&str, &str, &RoomSnapshot) -> String;

fn raw(path: &str, body: String) -> Result<Request<Body>, axum::http::Error> {
    authorized(Request::post(path))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
}

#[tokio::test]
async fn a_body_is_judged_after_the_connection_on_every_route_that_has_one() -> TestResult {
    let harness = Harness::new();
    let artifact = harness.artifact().await?;
    let tab = harness.tab().await?;
    let me = harness.enter(&tab, &artifact, "page").await?;
    let unknown = "0".repeat(32);
    // Each route with a body, as `(name, path for a connection, a valid body)`.
    let routes: [(&str, RoutePath, &str); 3] = [
        (
            "join",
            |tab, artifact, _| room(tab, artifact),
            r#"{"slot":"page"}"#,
        ),
        (
            "heartbeat",
            |tab, artifact, me| {
                format!(
                    "{}/heartbeat",
                    member(tab, artifact, me.participant, me.epoch)
                )
            },
            r#"{"after":0}"#,
        ),
        (
            "events",
            |tab, artifact, me| {
                format!("{}/events", member(tab, artifact, me.participant, me.epoch))
            },
            r#"{"kind":"k","data":"d"}"#,
        ),
    ];
    let oversized = format!(r#"{{"data":"{}"}}"#, "d".repeat(20_000));
    for (name, path, valid) in routes {
        // A connection the server never issued answers the same whatever the
        // body is: a valid one, malformed JSON, an unknown field, or one far
        // over the room's limit.
        let dead_path = path(&unknown, &artifact, &me);
        let reference = harness.send(raw(&dead_path, valid.to_owned())?).await?;
        assert_eq!(
            (reference.status, reference.code().as_str()),
            (StatusCode::NOT_FOUND, "runtime_session_expired"),
            "{name}"
        );
        let live_path = path(&tab, &artifact, &me);
        for (what, body, expected) in [
            ("malformed JSON", "{".to_owned(), StatusCode::BAD_REQUEST),
            (
                "an unknown field",
                r#"{"slot":"a","kind":"k","data":"","after":0,"zzz":1}"#.to_owned(),
                StatusCode::BAD_REQUEST,
            ),
            (
                "an oversized body",
                oversized.clone(),
                StatusCode::PAYLOAD_TOO_LARGE,
            ),
        ] {
            let dead = harness.send(raw(&dead_path, body.clone())?).await?;
            assert_eq!(
                (dead.status, dead.code()),
                (reference.status, reference.code()),
                "{name}, {what}: a dead connection must not learn how the body was judged"
            );
            let live = harness.send(raw(&live_path, body)?).await?;
            assert_eq!(live.status, expected, "{name}, {what}: {}", live.message());
        }
    }
    Ok(())
}

#[tokio::test]
async fn route_numbers_are_plain_positive_integers() -> TestResult {
    let harness = Harness::new();
    let artifact = harness.artifact().await?;
    let tab = harness.tab().await?;
    let me = harness.enter(&tab, &artifact, "page").await?;
    for (participant, epoch) in [
        ("01", "1"),
        ("0", "1"),
        ("+1", "1"),
        ("1", "0"),
        ("1", "00"),
        ("1", "1.0"),
        ("9007199254740992", "1"),
        ("1", "99999999999999999999"),
    ] {
        let reply = harness
            .send(post(
                &format!("{}/{participant}/{epoch}/heartbeat", room(&tab, &artifact)),
                &json!({"after": 0}),
            )?)
            .await?;
        assert_eq!(
            reply.status,
            StatusCode::BAD_REQUEST,
            "{participant}/{epoch}: {}",
            reply.code()
        );
    }
    // Well formed but not ours: not found, not invalid.
    let reply = harness
        .send(post(
            &format!(
                "{}/{}/{}/heartbeat",
                room(&tab, &artifact),
                me.participant + 100,
                me.epoch
            ),
            &json!({"after": 0}),
        )?)
        .await?;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn a_position_ahead_of_the_room_or_beyond_a_safe_integer_is_refused() -> TestResult {
    let harness = Harness::new();
    let artifact = harness.artifact().await?;
    let tab = harness.tab().await?;
    let me = harness.enter(&tab, &artifact, "page").await?;
    for after in [1_u64, 9_007_199_254_740_992] {
        let reply = harness.beat(&tab, &artifact, &me, after).await?;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "after {after}");
    }
    harness.emit(&tab, &artifact, &me, "x").await?;
    assert_eq!(
        harness.beat(&tab, &artifact, &me, 1).await?.status,
        StatusCode::OK
    );
    Ok(())
}

#[tokio::test]
async fn sizes_and_labels_are_bounded_in_bytes_and_characters() -> TestResult {
    let harness = Harness::new();
    let artifact = harness.artifact().await?;
    let tab = harness.tab().await?;
    // Slots: 32 characters pass, 33 and unlisted characters do not.
    for (slot, expected) in [
        ("x".repeat(32), StatusCode::OK),
        ("x".repeat(33), StatusCode::BAD_REQUEST),
        (String::new(), StatusCode::BAD_REQUEST),
        ("a b".into(), StatusCode::BAD_REQUEST),
        ("é".into(), StatusCode::BAD_REQUEST),
    ] {
        let reply = harness.join(&tab, &artifact, &slot).await?;
        assert_eq!(reply.status, expected, "slot {slot:?}");
    }
    // State: 256 bytes pass; 257 bytes, or 129 two-byte characters, do not.
    for (state, expected) in [
        ("s".repeat(256), StatusCode::OK),
        ("s".repeat(257), StatusCode::BAD_REQUEST),
        ("é".repeat(128), StatusCode::OK),
        ("é".repeat(129), StatusCode::BAD_REQUEST),
    ] {
        let reply = harness
            .join_with(&tab, &artifact, json!({"slot": "state", "state": state}))
            .await?;
        assert_eq!(reply.status, expected, "state of {} bytes", state.len());
    }
    // Events: 512 bytes of data pass, 513 do not; so do kinds of 32 and 33.
    let me = harness.enter(&tab, &artifact, "page").await?;
    let path = format!(
        "{}/events",
        member(&tab, &artifact, me.participant, me.epoch)
    );
    for (kind, data, expected) in [
        ("k".repeat(32), "d".repeat(512), StatusCode::OK),
        ("k".repeat(33), "d".into(), StatusCode::BAD_REQUEST),
        ("bad kind".into(), "d".into(), StatusCode::BAD_REQUEST),
        ("k".into(), "d".repeat(513), StatusCode::BAD_REQUEST),
        ("k".into(), "é".repeat(257), StatusCode::BAD_REQUEST),
    ] {
        harness.clock.advance(1_000);
        let reply = harness
            .send(post(&path, &json!({"kind": kind, "data": data}))?)
            .await?;
        assert_eq!(
            reply.status,
            expected,
            "kind {kind:?}, {} data bytes",
            data.len()
        );
    }
    // A body far over the room's own limit is refused as too large.
    let huge = harness
        .send(post(
            &path,
            &json!({"kind": "k", "data": "d".repeat(9_000)}),
        )?)
        .await?;
    assert_eq!(
        (huge.status, huge.code().as_str()),
        (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large")
    );
    // Unknown fields are refused.
    let extra = harness
        .send(post(&path, &json!({"kind": "k", "data": "", "to": 1}))?)
        .await?;
    assert_eq!(extra.status, StatusCode::BAD_REQUEST);
    Ok(())
}

#[tokio::test]
async fn a_participant_may_send_a_burst_then_the_sustained_rate() -> TestResult {
    let harness = Harness::new();
    let artifact = harness.artifact().await?;
    let tab = harness.tab().await?;
    let me = harness.enter(&tab, &artifact, "page").await?;
    harness.clock.advance(1_000);
    for count in 1..=8 {
        let reply = harness.emit(&tab, &artifact, &me, "x").await?;
        assert_eq!(reply.status, StatusCode::OK, "event {count}");
    }
    let refused = harness.emit(&tab, &artifact, &me, "x").await?;
    assert_eq!(
        (refused.status, refused.code().as_str()),
        (StatusCode::TOO_MANY_REQUESTS, "rate_limited")
    );
    assert_eq!(refused.header(header::RETRY_AFTER), "1");
    harness.clock.advance(249);
    assert_eq!(
        harness.emit(&tab, &artifact, &me, "x").await?.status,
        StatusCode::TOO_MANY_REQUESTS
    );
    harness.clock.advance(1);
    assert_eq!(
        harness.emit(&tab, &artifact, &me, "x").await?.status,
        StatusCode::OK
    );
    Ok(())
}

#[tokio::test]
async fn one_flooding_participant_does_not_stop_another_from_sending_or_reading() -> TestResult {
    let harness = Harness::new();
    let artifact = harness.artifact().await?;
    let (loud_tab, calm_tab) = (harness.tab().await?, harness.tab().await?);
    let loud = harness.enter(&loud_tab, &artifact, "page").await?;
    let calm = harness.enter(&calm_tab, &artifact, "page").await?;
    harness.clock.advance(1_000);
    let mut accepted = 0;
    for _ in 0..50 {
        if harness.emit(&loud_tab, &artifact, &loud, "x").await?.status == StatusCode::OK {
            accepted += 1;
        }
    }
    assert_eq!(accepted, 8, "the flood is held to its burst");
    // The calm participant still sends, and reads both.
    assert_eq!(
        harness
            .emit(&calm_tab, &artifact, &calm, "mine")
            .await?
            .status,
        StatusCode::OK
    );
    let view = harness
        .beat(&calm_tab, &artifact, &calm, 0)
        .await?
        .snapshot()?;
    assert_eq!(view.events.len(), 9);
    assert!(!view.missed);
    Ok(())
}

#[tokio::test]
async fn participants_per_tab_per_room_and_rooms_are_bounded() -> TestResult {
    let harness = Harness::new();
    let artifact = harness.artifact().await?;
    let tabs = [
        harness.tab().await?,
        harness.tab().await?,
        harness.tab().await?,
        harness.tab().await?,
        harness.tab().await?,
    ];
    // Four participants per tab.
    for slot in ["a", "b", "c", "d"] {
        harness.enter(&tabs[0], &artifact, slot).await?;
    }
    let fifth = harness.join(&tabs[0], &artifact, "e").await?;
    assert_eq!(
        (fifth.status, fifth.code().as_str()),
        (StatusCode::TOO_MANY_REQUESTS, "rate_limited")
    );
    assert!(fifth.message().contains("this tab"), "{}", fifth.message());
    // A resume is not a new participant, so a full tab can still resume.
    assert_eq!(
        harness.join(&tabs[0], &artifact, "a").await?.status,
        StatusCode::OK
    );

    // Sixteen participants per room.
    for tab in &tabs[1..4] {
        for slot in ["a", "b", "c", "d"] {
            harness.enter(tab, &artifact, slot).await?;
        }
    }
    let crowded = harness.join(&tabs[4], &artifact, "a").await?;
    assert_eq!(
        (crowded.status, crowded.code().as_str()),
        (StatusCode::TOO_MANY_REQUESTS, "rate_limited")
    );
    assert!(
        crowded.message().contains("room has"),
        "{}",
        crowded.message()
    );
    let full = harness.enter(&tabs[1], &artifact, "a").await?;
    assert_eq!(full.members.len(), 16, "the refused joins added nothing");

    // Sixty-four rooms: this one plus 63 more fill them.
    for _ in 0..63 {
        let other = harness.artifact().await?;
        harness.enter(&tabs[4], &other, "page").await?;
    }
    let overflow = harness.artifact().await?;
    let refused = harness.join(&tabs[4], &overflow, "page").await?;
    assert_eq!(
        (refused.status, refused.code().as_str()),
        (StatusCode::TOO_MANY_REQUESTS, "rate_limited")
    );
    assert!(refused.message().contains("rooms"), "{}", refused.message());
    // Expired rooms free their places.
    harness.clock.advance(15_000);
    assert_eq!(
        harness.join(&tabs[4], &overflow, "page").await?.status,
        StatusCode::OK
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_joins_never_exceed_the_bounds_of_a_room() -> TestResult {
    let harness = Arc::new(Harness::new());
    let artifact = harness.artifact().await?;
    let mut tabs = Vec::new();
    for _ in 0..10 {
        tabs.push(harness.tab().await?);
    }
    let mut calls = Vec::new();
    for tab in &tabs {
        for slot in ["a", "b", "c", "d"] {
            let (harness, tab, artifact) = (harness.clone(), tab.clone(), artifact.clone());
            calls.push(tokio::spawn(async move {
                harness
                    .join(&tab, &artifact, slot)
                    .await
                    .map(|reply| reply.status)
                    .map_err(|error| error.to_string())
            }));
        }
    }
    let mut joined = 0;
    let mut refused = 0;
    for call in calls {
        match call.await?? {
            StatusCode::OK => joined += 1,
            StatusCode::TOO_MANY_REQUESTS => refused += 1,
            other => return Err(format!("unexpected status {other}").into()),
        }
    }
    assert_eq!((joined, refused), (16, 24));
    Ok(())
}

#[tokio::test]
async fn room_calls_have_their_own_budget_and_the_owners_is_untouched() -> TestResult {
    let harness = Harness::with_room_limits(RequestLimits {
        burst: 3,
        per_minute: 60,
        concurrent: 4,
    });
    let artifact = harness.artifact().await?;
    let tab = harness.tab().await?;
    let me = harness.enter(&tab, &artifact, "page").await?;
    // Two more room calls use the burst; the next is refused with a delay.
    for _ in 0..2 {
        assert_eq!(
            harness.beat(&tab, &artifact, &me, 0).await?.status,
            StatusCode::OK
        );
    }
    let refused = harness.beat(&tab, &artifact, &me, 0).await?;
    assert_eq!(
        (refused.status, refused.code().as_str()),
        (StatusCode::TOO_MANY_REQUESTS, "rate_limited")
    );
    assert!(!refused.header(header::RETRY_AFTER).is_empty());
    // The owner's own requests are not counted against it.
    for _ in 0..10 {
        let listed = harness
            .send(authorized(Request::get("/v1/artifacts")).body(Body::empty())?)
            .await?;
        assert_eq!(listed.status, StatusCode::OK);
    }
    Ok(())
}
