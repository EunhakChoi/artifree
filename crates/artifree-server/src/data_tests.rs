//! Data subscriptions across the real hub, state and router (decision 0038):
//! the registry only ever holds windows of artifacts the actor may use, a
//! write never fails because of its window, and a writer never parks a
//! blocking thread on a gate that a reader holds.

use std::error::Error;
use std::future::poll_fn;
use std::path::PathBuf;
use std::pin::Pin;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use artifree_contracts::api::{
    ContentEncoding, CreateArtifactRequest, FileUpload, JsonValue, PurgeArtifactRequest,
    SetDataRequest, UpdateArtifactRequest,
};
use artifree_domain::AppDataVersion;
use artifree_domain::data::{DataChange, DataPath, DataValue};
use artifree_domain::identity::{ActorId, ArtifactId};
use artifree_providers::storage::SqliteStore;
use artifree_providers::tokens::SqliteTokenStore;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body::Body as _;
use tower::ServiceExt;

use crate::auth::AccessToken;
use crate::data_hub::{self, MAX_WINDOWS, MAX_WINDOWS_PER_ACTOR, Replay};
use crate::http::{self, AppState, Settings};
use crate::operations;

type TestResult = Result<(), Box<dyn Error>>;

const OWNER_TOKEN: &str = "0000000000000000000000000000000000000000000000000000000000000000";

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Result<Self, Box<dyn Error>> {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path =
            std::env::temp_dir().join(format!("artifree-data-hub-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.0);
    }
}

fn test_state(directory: &TestDirectory) -> Result<AppState, Box<dyn Error>> {
    let database = directory.0.join("data.sqlite3");
    Ok(AppState::new(
        SqliteStore::open(&database)?,
        SqliteTokenStore::open(&database)?,
        AccessToken::parse(OWNER_TOKEN).ok_or("invalid fixture token")?,
        Settings {
            content_origin: "http://127.0.0.1:7411".into(),
            ..Settings::default()
        },
    )?)
}

async fn create_artifact(state: &AppState, actor: &ActorId) -> Result<ArtifactId, Box<dyn Error>> {
    let created = operations::create(
        state,
        actor.clone(),
        "data-hub-create".parse()?,
        CreateArtifactRequest {
            title: "Data".into(),
            files: vec![FileUpload {
                path: "index.html".into(),
                encoding: ContentEncoding::Utf8,
                content: "<p>x</p>".into(),
            }],
        },
    )
    .await
    .map_err(|error| error.body.message)?;
    Ok(created.artifact.id.parse()?)
}

fn set_request(path: &str, base: u64, value: &str) -> Result<SetDataRequest, Box<dyn Error>> {
    Ok(SetDataRequest {
        path: path.into(),
        base_data_version: base,
        value: JsonValue::from_text(value.into()).ok_or("fixture value is not JSON")?,
    })
}

async fn send(
    state: &AppState,
    method: &str,
    path: &str,
    key: Option<&str>,
    body: Option<&str>,
) -> Result<StatusCode, Box<dyn Error>> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {OWNER_TOKEN}"));
    if let Some(key) = key {
        builder = builder.header("idempotency-key", key);
    }
    let request = match body {
        Some(body) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_owned()))?,
        None => builder.body(Body::empty())?,
    };
    let response = http::router(state.clone()).oneshot(request).await?;
    Ok(response.status())
}

fn other_actor(number: u8) -> ActorId {
    ActorId::user([number; 16])
}

/// A writer parked on a blocking thread while it waits for the gate would
/// starve the reader that holds the gate and waits for a blocking thread.
#[test]
fn writers_wait_for_the_gate_without_holding_a_blocking_thread() -> TestResult {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        // Fewer threads than writers, so parked writers could take them all.
        .max_blocking_threads(2)
        .build()?;
    runtime.block_on(async {
        let actor = ActorId::owner();
        let id = create_artifact(&state, &actor).await?;
        let space = state
            .data_hub()
            .space(&actor, &id)
            .map_err(|error| error.to_string())?;
        // A reader in the middle of its read: it holds the gate.
        let reader_gate = space.gate_owned().await;
        let mut writers = Vec::new();
        for number in 0..6 {
            let (state, actor, id) = (state.clone(), actor.clone(), id.to_string());
            writers.push(tokio::spawn(async move {
                let request = set_request(&format!("w{number}"), 0, "1")
                    .map_err(|error| error.to_string())?;
                let key = format!("gate-writer-{number}")
                    .parse()
                    .map_err(|_| "bad key".to_owned())?;
                operations::set_data(&state, actor, key, &id, request)
                    .await
                    .map(|reply| reply.data_version)
                    .map_err(|error| error.body.message)
            }));
        }
        // Let every writer reach the gate.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let (reader, id_read) = (actor.clone(), id.clone());
        let read = tokio::time::timeout(
            Duration::from_secs(3),
            state.run(move |artifacts| artifacts.check_data_access(&reader, &id_read)),
        )
        .await;
        // Release the gate before judging, so that a failure ends the test
        // instead of leaving parked threads behind.
        drop(reader_gate);
        let mut committed = 0;
        for writer in writers {
            let outcome = tokio::time::timeout(Duration::from_secs(30), writer).await??;
            committed += usize::from(outcome.is_ok());
        }
        assert!(
            matches!(read, Ok(Ok(()))),
            "the reader that holds the gate got no blocking thread: {read:?}"
        );
        // Every writer prepared against version 0: one wins, the rest conflict.
        assert_eq!(committed, 1);
        Ok::<(), Box<dyn Error>>(())
    })
}

/// A request for an artifact that does not exist makes no window and evicts
/// none, however full the registry is.
#[test]
fn requests_for_missing_artifacts_make_and_evict_no_window() -> TestResult {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    runtime.block_on(async {
        let owner = ActorId::owner();
        let real = create_artifact(&state, &owner).await?;
        // The owner's own window is the oldest of a registry at its bound.
        let hub = state.data_hub();
        drop(
            hub.space(&owner, &real)
                .map_err(|error| error.to_string())?,
        );
        let mut filler = 0_u8;
        'fill: for who in 1..=16 {
            for number in 0..32 {
                if hub.len() == MAX_WINDOWS {
                    break 'fill;
                }
                drop(
                    hub.space(&other_actor(who), &ArtifactId::from_random([number; 16]))
                        .map_err(|error| error.to_string())?,
                );
                filler += 1;
            }
        }
        assert_eq!(hub.len(), MAX_WINDOWS, "{filler} filler windows");
        for number in 0..40_u8 {
            let missing = ArtifactId::from_random([100 + number; 16]);
            let write = send(
                &state,
                "PUT",
                &format!("/v1/artifacts/{missing}/data"),
                Some(&format!("missing-write-{number}")),
                Some(r#"{"path":"a","base_data_version":0,"value":1}"#),
            )
            .await?;
            let stream = send(
                &state,
                "GET",
                &format!("/v1/artifacts/{missing}/data/events"),
                None,
                None,
            )
            .await?;
            assert_eq!(
                (write, stream),
                (StatusCode::NOT_FOUND, StatusCode::NOT_FOUND)
            );
        }
        assert_eq!(
            hub.len(),
            MAX_WINDOWS,
            "a request for nothing made a window"
        );
        assert!(
            hub.contains(&owner, &real),
            "a request for nothing evicted a real window"
        );
        Ok(())
    })
}

/// A window that cannot take its event never fails the write that committed.
#[test]
fn a_write_succeeds_when_its_window_is_poisoned_and_streams_are_woken() -> TestResult {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    runtime.block_on(async {
        let actor = ActorId::owner();
        let id = create_artifact(&state, &actor).await?;
        let held = state
            .data_hub()
            .space(&actor, &id)
            .map_err(|error| error.to_string())?;
        let woken = held.watch();
        let poisoner = std::sync::Arc::clone(&held);
        let died = std::thread::spawn(move || {
            let _ = poisoner.with_window(|_| panic!("poisons the window on purpose"));
        })
        .join();
        assert!(died.is_err(), "the fixture did not poison the window");
        assert!(held.with_window(|_| ()).is_err());

        let reply = operations::set_data(
            &state,
            actor.clone(),
            "poisoned-write".parse()?,
            &id.to_string(),
            set_request("a", 0, "1")?,
        )
        .await
        .map_err(|error| error.body.message)?;
        assert_eq!((reply.data_version, reply.changed), (1, true));
        // The stored value is there.
        let read = state
            .run({
                let (actor, id) = (actor.clone(), id.clone());
                move |artifacts| artifacts.data(&actor, &id, &DataPath::root())
            })
            .await?;
        assert_eq!(read.version, AppDataVersion(1));
        // The poisoned window left the hub, so the next stream starts a new
        // one, and the streams that held it were woken to find it unusable.
        assert!(!state.data_hub().contains(&actor, &id));
        assert!(woken.has_changed()?, "streams on the window were not woken");
        let fresh = state
            .data_hub()
            .space(&actor, &id)
            .map_err(|error| error.to_string())?;
        assert!(
            fresh
                .with_window(|window| window.after(AppDataVersion(0)))
                .is_ok()
        );
        Ok(())
    })
}

/// A merge whose result cannot be read back leaves no event, so the window
/// forgets what it held: streams behind it are reset, not left believing they
/// saw everything.
#[test]
fn a_failed_read_back_forgets_the_window_and_wakes_its_streams() -> TestResult {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    runtime.block_on(async {
        let actor = ActorId::owner();
        let id = create_artifact(&state, &actor).await?;
        operations::set_data(
            &state,
            actor.clone(),
            "readback-first".parse()?,
            &id.to_string(),
            set_request("a", 0, "1")?,
        )
        .await
        .map_err(|error| error.body.message)?;
        let space = state
            .data_hub()
            .space(&actor, &id)
            .map_err(|error| error.to_string())?;
        let covered = |space: &data_hub::Space| {
            space.with_window(|window| {
                matches!(window.after(AppDataVersion(0)), Replay::Edits { .. })
            })
        };
        assert_eq!(
            covered(&space),
            Ok(true),
            "the first write was not recorded"
        );
        let woken = space.watch();
        // The artifact leaves for the trash before the merge's result is read.
        operations::update(
            &state,
            actor.clone(),
            "readback-trash".parse()?,
            &id.to_string(),
            UpdateArtifactRequest {
                base_metadata_revision: 1,
                title: None,
                pinned: None,
                deleted: Some(true),
            },
        )
        .await
        .map_err(|error| error.body.message)?;
        let change = DataChange::Merge {
            path: "a".parse()?,
            patch: DataValue::parse(r#"{"b":1}"#)?,
        };
        let (hub_state, held, who, which) = (
            state.clone(),
            std::sync::Arc::clone(&space),
            actor.clone(),
            id.clone(),
        );
        state
            .run(move |artifacts| {
                data_hub::record_after_commit(
                    hub_state.data_hub(),
                    &held,
                    artifacts,
                    (&who, &which),
                    &change,
                    AppDataVersion(2),
                );
                Ok(())
            })
            .await?;
        assert_eq!(
            covered(&space),
            Ok(false),
            "the window kept a stale history"
        );
        assert!(woken.has_changed()?, "streams were not woken to notice");
        Ok(())
    })
}

/// Opens a data stream of the owner and returns its body.
async fn open_stream(state: &AppState, id: &ArtifactId) -> Result<Body, Box<dyn Error>> {
    let request = Request::builder()
        .method("GET")
        .uri(format!("/v1/artifacts/{id}/data/events"))
        .header(header::AUTHORIZATION, format!("Bearer {OWNER_TOKEN}"))
        .body(Body::empty())?;
    let response = http::router(state.clone()).oneshot(request).await?;
    assert_eq!(response.status(), StatusCode::OK);
    Ok(response.into_body())
}

/// Reads the stream until `needle` has arrived, or fails after `within`.
async fn read_until(
    body: &mut Body,
    needle: &str,
    within: Duration,
) -> Result<String, Box<dyn Error>> {
    let mut seen = String::new();
    let outcome = tokio::time::timeout(within, async {
        while !seen.contains(needle) {
            match poll_fn(|cx| Pin::new(&mut *body).poll_frame(cx)).await {
                Some(Ok(frame)) => {
                    if let Ok(data) = frame.into_data() {
                        seen.push_str(&String::from_utf8_lossy(&data));
                    }
                }
                Some(Err(error)) => return Err(error.to_string()),
                None => return Err("the stream ended".to_owned()),
            }
        }
        Ok(())
    })
    .await;
    match outcome {
        Ok(Ok(())) => Ok(seen),
        Ok(Err(reason)) => Err(format!("{reason} before {needle}: {seen}").into()),
        Err(_) => Err(format!("no {needle} within {within:?}: {seen}").into()),
    }
}

fn runtime() -> Result<tokio::runtime::Runtime, Box<dyn Error>> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?)
}

/// A purge empties the windows that streams still hold, so their values do
/// not stay in memory outside the registry's bounds, and wakes those streams
/// so that they end.
#[test]
fn a_purge_empties_the_windows_that_streams_still_hold() -> TestResult {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    runtime()?.block_on(async {
        let actor = ActorId::owner();
        let id = create_artifact(&state, &actor).await?;
        let held = state
            .data_hub()
            .space(&actor, &id)
            .map_err(|error| error.to_string())?;
        let woken = held.watch();
        let mut body = open_stream(&state, &id).await?;
        read_until(&mut body, "event: snapshot", Duration::from_secs(5)).await?;
        let blob = format!("\"{}\"", "x".repeat(100 * 1024));
        operations::set_data(
            &state,
            actor.clone(),
            "purge-held-write".parse()?,
            &id.to_string(),
            set_request("blob", 0, &blob)?,
        )
        .await
        .map_err(|error| error.body.message)?;
        assert!(
            held.retained_bytes() > 100 * 1024,
            "the write was not retained"
        );
        operations::update(
            &state,
            actor.clone(),
            "purge-held-trash".parse()?,
            &id.to_string(),
            UpdateArtifactRequest {
                base_metadata_revision: 1,
                title: None,
                pinned: None,
                deleted: Some(true),
            },
        )
        .await
        .map_err(|error| error.body.message)?;
        // Only the purge is judged: leave what the trash did out of it.
        let _ = woken.has_changed();
        let before = held.retained_bytes();
        operations::purge(
            &state,
            actor.clone(),
            "purge-held-purge".parse()?,
            &id.to_string(),
            PurgeArtifactRequest {
                base_metadata_revision: 2,
            },
        )
        .await
        .map_err(|error| error.body.message)?;
        assert!(before > 0);
        assert_eq!(held.retained_bytes(), 0, "a purged value stayed in memory");
        assert!(
            held.with_window(|_| ()).is_err(),
            "a stream could still use the window of a purged artifact"
        );
        assert!(!state.data_hub().contains(&actor, &id));
        assert!(woken.has_changed()?, "the streams were not woken to end");
        let ended = read_until(&mut body, "never sent", Duration::from_secs(5)).await;
        assert!(
            ended.is_err_and(|error| error.to_string().contains("the stream ended")),
            "the stream did not end after the purge"
        );
        Ok(())
    })
}

/// With the hub full the write still succeeds and makes no window.
#[test]
fn a_write_with_a_full_hub_commits_and_makes_no_window() -> TestResult {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    runtime()?.block_on(async {
        let actor = ActorId::owner();
        let id = create_artifact(&state, &actor).await?;
        let hub = state.data_hub();
        let mut held = Vec::new();
        for who in 1..=u8::try_from(MAX_WINDOWS / MAX_WINDOWS_PER_ACTOR)? {
            for number in 0..u8::try_from(MAX_WINDOWS_PER_ACTOR)? {
                held.push(
                    hub.space(&other_actor(who), &ArtifactId::from_random([number; 16]))
                        .map_err(|error| error.to_string())?,
                );
            }
        }
        assert_eq!(hub.len(), MAX_WINDOWS);
        let reply = operations::set_data(
            &state,
            actor.clone(),
            "full-hub-write".parse()?,
            &id.to_string(),
            set_request("a", 0, "1")?,
        )
        .await
        .map_err(|error| error.body.message)?;
        assert_eq!((reply.data_version, reply.changed), (1, true));
        assert_eq!(hub.len(), MAX_WINDOWS);
        assert!(!hub.contains(&actor, &id));
        Ok(())
    })
}

/// A write that had no window to record in forgets and wakes the window that
/// exists afterwards, so its streams reset at once.
#[test]
fn an_unrecorded_commit_forgets_and_wakes_the_window_that_exists() -> TestResult {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    runtime()?.block_on(async {
        let actor = ActorId::owner();
        let id = create_artifact(&state, &actor).await?;
        let hub = state.data_hub();
        // No window: nothing is made, and no global wake is needed.
        assert!(!data_hub::forget_unrecorded(hub, &actor, &id));
        assert!(!hub.contains(&actor, &id));
        // A stream's window, anchored at version 3 with history to resume from.
        let space = hub.space(&actor, &id).map_err(|error| error.to_string())?;
        space
            .with_window(|window| window.anchor(AppDataVersion(3)))
            .map_err(|_| "the window is poisoned")?;
        let woken = space.watch();
        assert!(!data_hub::forget_unrecorded(hub, &actor, &id));
        let complete = space
            .with_window(|window| matches!(window.after(AppDataVersion(3)), Replay::Edits { .. }))
            .map_err(|_| "the window is poisoned")?;
        assert!(!complete, "the window kept believing it was complete");
        assert!(woken.has_changed()?, "its streams were not woken");
        // A registry that cannot be searched asks for a wake of every stream.
        hub.poison_registry();
        assert!(data_hub::forget_unrecorded(hub, &actor, &id));
        Ok(())
    })
}

/// A write made while the registry is unusable still reaches the streams that
/// were open before it: they compare themselves with storage at once and
/// reset, instead of waiting for the next heartbeat.
#[test]
fn a_stream_open_before_an_unrecorded_write_resets_promptly() -> TestResult {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    runtime()?.block_on(async {
        let actor = ActorId::owner();
        let id = create_artifact(&state, &actor).await?;
        let mut body = open_stream(&state, &id).await?;
        read_until(&mut body, "event: snapshot", Duration::from_secs(5)).await?;
        state.data_hub().poison_registry();
        let reply = operations::set_data(
            &state,
            actor.clone(),
            "unrecorded-write".parse()?,
            &id.to_string(),
            set_request("a", 0, "1")?,
        )
        .await
        .map_err(|error| error.body.message)?;
        assert_eq!(reply.data_version, 1);
        // The heartbeat is fifteen seconds: this is the wake, not the check.
        let seen = read_until(&mut body, "event: reset", Duration::from_secs(5)).await?;
        assert!(seen.contains(r#""data_version":1"#), "{seen}");
        Ok(())
    })
}

/// A registry that a panic poisoned still lets a purge empty the windows it
/// holds: a purged value is not kept because the registry is unusable.
#[test]
fn a_purge_empties_windows_even_when_the_registry_is_poisoned() -> TestResult {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    runtime()?.block_on(async {
        let actor = ActorId::owner();
        let id = create_artifact(&state, &actor).await?;
        let hub = state.data_hub();
        let held = hub.space(&actor, &id).map_err(|error| error.to_string())?;
        let blob = format!("\"{}\"", "x".repeat(10 * 1024));
        operations::set_data(
            &state,
            actor.clone(),
            "poisoned-purge-write".parse()?,
            &id.to_string(),
            set_request("blob", 0, &blob)?,
        )
        .await
        .map_err(|error| error.body.message)?;
        assert!(held.retained_bytes() > 0, "the write was not retained");
        hub.poison_registry();
        hub.forget(&id);
        assert_eq!(held.retained_bytes(), 0, "a purged value stayed in memory");
        assert!(held.with_window(|_| ()).is_err());
        Ok(())
    })
}
