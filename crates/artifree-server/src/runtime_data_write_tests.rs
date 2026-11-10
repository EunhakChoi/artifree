//! Runtime data writes that race a grant change or a source save (decision
//! 0053, rules 8 to 10).
//!
//! A write is judged when its frame call starts, again on the blocking task
//! that commits it just before the store is asked, and once more after the
//! commit. These tests hold the write on that blocking task, so the race lands
//! where the test wants it, once for each of the three writes. A grant withdrawn
//! while the write waited, or a source save that moved the head before the
//! commit, leaves the space, its version and its request keys as they were. A
//! withdrawal after the commit withholds only the receipt, and the commit
//! stands.

use std::error::Error;
use std::future::{Future, poll_fn};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::task::Poll;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use artifree_contracts::api::{
    ContentEncoding, CreateArtifactRequest, CreateRuntimeViewRequest, DataWriteResponse, ErrorCode,
    FileChange, FileUpload, JsonValue, RuntimeDataDeleteRequest, RuntimeDataSetRequest,
    RuntimeDataUpdateRequest, RuntimeView, SaveRevisionRequest, SetDataRequest,
    SetRuntimeGrantRequest,
};
use artifree_core::artifacts::ArtifactService;
use artifree_domain::SourceRevision;
use artifree_domain::identity::{ActorId, StateGeneration};
use artifree_providers::storage::SqliteStore;
use artifree_providers::tokens::SqliteTokenStore;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crate::auth::AccessToken;
use crate::http::{AppState, Settings};
use crate::limits::RequestLimits;
use crate::operations::{self, ApiError};
use crate::runtime_data;
use crate::runtime_source::FrameCall;
use crate::{changes, preview};

/// The path that every write in these tests changes.
const PATH: &str = "doc";
/// The value the owner stores at [`PATH`] before each race, in canonical JSON
/// text, so that the text a read returns is the text that was stored.
const SEED: &str = r#"{"a":1}"#;
/// The value that `set` stores at [`PATH`].
const SET_VALUE: &str = r#"{"a":2}"#;
/// The merge patch that `update` applies at [`PATH`].
const PATCH: &str = r#"{"b":2}"#;
/// The value at [`PATH`] once [`PATCH`] is merged into [`SEED`].
const PATCHED: &str = r#"{"a":1,"b":2}"#;

/// A fresh directory for one test's `SQLite` files, removed when the test ends.
struct TestDirectory(PathBuf);

/// Keeps the directories of tests that start in the same nanosecond apart.
static DIRECTORIES: AtomicU64 = AtomicU64::new(0);

impl TestDirectory {
    fn new() -> Result<Self, Box<dyn Error>> {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let count = DIRECTORIES.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "artifree-data-write-{}-{nonce}-{count}",
            std::process::id()
        ));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.0);
    }
}

/// The application state over one `SQLite` file in `directory`.
fn test_state(directory: &TestDirectory) -> Result<AppState, Box<dyn Error>> {
    let store = SqliteStore::open(&directory.0.join("data.sqlite3"))?;
    let token_store = SqliteTokenStore::open(&directory.0.join("data.sqlite3"))?;
    let token = AccessToken::parse(&"0".repeat(64)).ok_or("invalid fixture token")?;
    Ok(AppState::new(
        store,
        token_store,
        token,
        Settings {
            content_origin: "http://127.0.0.1:7411".into(),
            preview_ttl: Duration::from_secs(60),
            frame_ancestors: "http://127.0.0.1:*".into(),
            limits: RequestLimits::default(),
            change_streams: changes::DEFAULT_STREAMS_PER_ACTOR,
            ..Settings::default()
        },
    )?)
}

/// Saves one file of `artifact` at revision `base`, as the owner.
async fn save(
    state: &AppState,
    actor: &ActorId,
    artifact: &str,
    base: u64,
    path: &str,
    content: &str,
) -> Result<(), Box<dyn Error>> {
    operations::save(
        state,
        actor.clone(),
        format!("data-write-save-{base}").parse()?,
        artifact,
        SaveRevisionRequest {
            base_revision: base,
            changes: vec![FileChange::Put {
                path: path.into(),
                encoding: ContentEncoding::Utf8,
                content: content.into(),
            }],
        },
    )
    .await
    .map_err(|error| error.body.message)?;
    Ok(())
}

/// An artifact whose manifest declares `data.write`. Its head is revision 2, the
/// one after the manifest. Returns its identifier.
async fn declared_artifact(state: &AppState, actor: &ActorId) -> Result<String, Box<dyn Error>> {
    let created = operations::create(
        state,
        actor.clone(),
        "data-write-artifact".parse()?,
        CreateArtifactRequest {
            title: "Data write".into(),
            files: vec![FileUpload {
                path: "index.html".into(),
                encoding: ContentEncoding::Utf8,
                content: "<!doctype html><html><head><!-- artifree-runtime --></head><body>test</body></html>".into(),
            }],
        },
    )
    .await
    .map_err(|error| error.body.message)?;
    let artifact = created.artifact.id;
    save(
        state,
        actor,
        &artifact,
        1,
        "artifree-runtime.json",
        r#"{"sdk":"artifree.app.v1","capabilities":["data.write"]}"#,
    )
    .await?;
    Ok(artifact)
}

/// Stores [`SEED`] at [`PATH`] as the owner, and returns the data version that
/// the write left the space at.
async fn owner_seed(
    state: &AppState,
    actor: &ActorId,
    artifact: &str,
) -> Result<u64, Box<dyn Error>> {
    let written = operations::set_data(
        state,
        actor.clone(),
        "data-write-seed".parse()?,
        artifact,
        SetDataRequest {
            path: PATH.into(),
            base_data_version: 0,
            value: JsonValue::from_text(SEED.into()).ok_or("invalid fixture value")?,
        },
        || Ok(None),
    )
    .await
    .map_err(|error| error.body.message)?;
    Ok(written.data_version)
}

/// Opens a connection on `artifact`, grants it `data.write` and creates a view
/// pinned to revision 2. The view's bootstrap is served, as the frame would serve
/// it, so the view is live. Returns the view and the connection's session
/// identifier.
async fn granted_view(
    state: &AppState,
    actor: &ActorId,
    artifact: &str,
) -> Result<(RuntimeView, String), Box<dyn Error>> {
    let session = operations::open_runtime_session(state, actor, None)
        .map_err(|error| error.body.message)?
        .session_id;
    operations::set_runtime_grant(
        state,
        actor.clone(),
        &session,
        artifact,
        2,
        SetRuntimeGrantRequest {
            head_revision: 2,
            expected_version: 0,
            capabilities: vec!["data.write".into()],
        },
    )
    .await
    .map_err(|error| error.body.message)?;
    let view = operations::create_runtime_view(
        state,
        actor.clone(),
        None,
        StateGeneration::FIRST,
        artifact,
        2,
        CreateRuntimeViewRequest {
            session_id: session.clone(),
            view_id: "a".repeat(32),
            generation: 1,
        },
    )
    .await
    .map_err(|error| error.body.message)?;
    assert_eq!(view.granted, ["data.write"]);
    let path = view
        .url
        .strip_prefix("http://127.0.0.1:7411")
        .ok_or("wrong preview origin")?;
    let response = preview::router(state.clone())
        .oneshot(Request::get(path).body(Body::empty())?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    Ok((view, session))
}

/// The frame call that `view` makes on its own connection.
fn frame_call<'a>(actor: &'a ActorId, view: &'a RuntimeView) -> FrameCall<'a> {
    FrameCall {
        actor,
        artifact: &view.artifact_id,
        revision: view.revision,
        view: &view.view_id,
        generation: view.generation,
        initialization: &view.initialization,
    }
}

/// The three frame writes that the races drive through the runtime data calls.
#[derive(Debug, Clone, Copy)]
enum Kind {
    /// `data.set` replaces the value at [`PATH`] with [`SET_VALUE`].
    Set,
    /// `data.update` merges [`PATCH`] into the value at [`PATH`].
    Update,
    /// `data.remove` deletes the value at [`PATH`].
    Remove,
}

impl Kind {
    /// Every kind, so that each race runs once for each write.
    const ALL: [Self; 3] = [Self::Set, Self::Update, Self::Remove];

    /// The value at [`PATH`] once this write commits over [`SEED`], as canonical
    /// JSON text, or `None` where the removal leaves nothing.
    fn committed(self) -> Option<&'static str> {
        match self {
            Self::Set => Some(SET_VALUE),
            Self::Update => Some(PATCHED),
            Self::Remove => None,
        }
    }
}

/// The frame's write of `kind` at [`PATH`] under the retry identity `retry`, at
/// data version `base`, on the connection that `view` runs on.
async fn frame_write(
    state: &AppState,
    call: &FrameCall<'_>,
    view: &RuntimeView,
    kind: Kind,
    retry: &str,
    base: u64,
) -> Result<DataWriteResponse, ApiError> {
    let initialization = view.initialization.clone();
    let retry_id = retry.to_owned();
    match kind {
        Kind::Set => {
            runtime_data::set(
                state,
                call,
                &RuntimeDataSetRequest {
                    initialization,
                    retry_id,
                    base_data_version: base,
                    path: PATH.into(),
                    value: SET_VALUE.into(),
                },
            )
            .await
        }
        Kind::Update => {
            runtime_data::update(
                state,
                call,
                &RuntimeDataUpdateRequest {
                    initialization,
                    retry_id,
                    base_data_version: base,
                    path: PATH.into(),
                    patch: PATCH.into(),
                },
            )
            .await
        }
        Kind::Remove => {
            runtime_data::remove(
                state,
                call,
                &RuntimeDataDeleteRequest {
                    initialization,
                    retry_id,
                    base_data_version: base,
                    path: PATH.into(),
                },
            )
            .await
        }
    }
}

/// The owner withdraws the connection's grant: it becomes empty at the next
/// version.
async fn withdraw_grant(
    state: &AppState,
    actor: &ActorId,
    session: &str,
    view: &RuntimeView,
) -> Result<(), Box<dyn Error>> {
    operations::set_runtime_grant(
        state,
        actor.clone(),
        session,
        &view.artifact_id,
        view.revision,
        SetRuntimeGrantRequest {
            head_revision: view.head_revision,
            expected_version: view.grant_version,
            capabilities: vec![],
        },
    )
    .await
    .map_err(|error| error.body.message)?;
    Ok(())
}

/// Occupies the only blocking thread until the returned sender is used, so that
/// a store call queued behind it waits where the test wants it.
async fn hold_blocking_thread()
-> Result<(mpsc::Sender<()>, tokio::task::JoinHandle<()>), Box<dyn Error>> {
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, waiting) = mpsc::channel();
    let blocker = tokio::task::spawn_blocking(move || {
        let _sent = started.send(());
        let _released = waiting.recv();
    });
    ready.await?;
    Ok((release, blocker))
}

/// Polls `request` once and reports whether it is still waiting.
async fn pends<T>(request: &mut Pin<Box<impl Future<Output = T>>>) -> bool {
    poll_fn(|context| Poll::Ready(request.as_mut().poll(context).is_pending())).await
}

/// Returns once every blocking call queued so far has run. The pool has one
/// thread in these tests, so the calls run in the order they were queued.
async fn settle() -> Result<(), Box<dyn Error>> {
    tokio::task::spawn_blocking(|| ()).await?;
    Ok(())
}

/// Drives `write` past its artifact lookup and holds it where its commit waits
/// behind a blocking thread the test holds. The lookup waits behind a first
/// blocking thread, which the test releases once the lookup has run. The caller
/// releases the returned blocker, which lets the commit run.
async fn pause_before_the_commit<T>(
    write: &mut Pin<Box<impl Future<Output = T>>>,
) -> Result<(mpsc::Sender<()>, tokio::task::JoinHandle<()>), Box<dyn Error>> {
    let (release, blocker) = hold_blocking_thread().await?;
    assert!(
        pends(write).await,
        "the artifact lookup must wait behind the blocker"
    );
    release.send(())?;
    blocker.await?;
    settle().await?;
    let (release, blocker) = hold_blocking_thread().await?;
    assert!(
        pends(write).await,
        "the commit must wait behind the blocker"
    );
    Ok((release, blocker))
}

/// The refusal the call must give: `status`, and no data with it. A reply that
/// carries data fails here, whatever the data is.
fn refusal<T>(
    outcome: Result<T, ApiError>,
    status: StatusCode,
) -> Result<ApiError, Box<dyn Error>> {
    match outcome {
        Ok(_) => Err(format!("expected {status} and no data, but the call returned data").into()),
        Err(error) if error.status == status => Ok(error),
        Err(error) => Err(format!(
            "expected {status}, got {} {}",
            error.status, error.body.message
        )
        .into()),
    }
}

/// Runs `scenario` on a current-thread runtime whose blocking pool has one
/// thread, so that store calls run in the order that the test queues them.
fn on_one_blocking_thread(
    scenario: impl Future<Output = Result<(), Box<dyn Error>>>,
) -> Result<(), Box<dyn Error>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(scenario)
}

/// Saves `note.txt` at revision `base` of `artifact` through a second connection
/// to the database file. The owner's store is not used, so the save runs on the
/// test thread while the blocking thread is held.
fn save_on_second_connection(
    directory: &TestDirectory,
    actor: &ActorId,
    artifact: &str,
    base: u64,
) -> Result<(), Box<dyn Error>> {
    let service = ArtifactService::new(SqliteStore::open(&directory.0.join("data.sqlite3"))?);
    let artifact = operations::artifact_id(artifact).map_err(|error| error.body.message)?;
    service.save(
        actor,
        &format!("data-write-race-{base}").parse()?,
        &artifact,
        SourceRevision(base),
        vec![artifree_domain::package::FileChange::Put {
            path: "note.txt".parse()?,
            content: b"moved".to_vec(),
        }],
    )?;
    Ok(())
}

/// The owner's space of data for an artifact, read on a connection of the test's
/// own: its version, the canonical text at [`PATH`] and how many request keys it
/// has recorded. A write that committed anything changes one of them.
#[derive(Debug, PartialEq, Eq)]
struct Space {
    version: u64,
    value: Option<String>,
    requests: i64,
}

/// The space of `artifact`, read as [`Space`] describes it.
fn space(
    directory: &TestDirectory,
    actor: &ActorId,
    artifact: &str,
) -> Result<Space, Box<dyn Error>> {
    let service = ArtifactService::new(SqliteStore::open(&directory.0.join("data.sqlite3"))?);
    let id = operations::artifact_id(artifact).map_err(|error| error.body.message)?;
    let path = operations::data_path("path", PATH).map_err(|error| error.body.message)?;
    let read = service.data(actor, &id, &path)?;
    let connection = rusqlite::Connection::open(directory.0.join("data.sqlite3"))?;
    let requests: i64 =
        connection.query_row("SELECT COUNT(*) FROM data_requests", [], |row| row.get(0))?;
    Ok(Space {
        version: read.version.0,
        value: read.value.map(|value| value.to_canonical()),
        requests,
    })
}

/// What each race starts from: an artifact whose owner's seed is stored, a
/// connection granted `data.write` with its view, and the space as the seed left
/// it.
struct Fixture {
    artifact: String,
    seeded: u64,
    view: RuntimeView,
    session: String,
    before: Space,
}

/// Sets up a [`Fixture`] and checks that the space reads as the seed left it, so
/// that a refusal in a race can only come from the race.
async fn started(
    directory: &TestDirectory,
    state: &AppState,
    actor: &ActorId,
) -> Result<Fixture, Box<dyn Error>> {
    let artifact = declared_artifact(state, actor).await?;
    let seeded = owner_seed(state, actor, &artifact).await?;
    let (view, session) = granted_view(state, actor, &artifact).await?;
    let before = space(directory, actor, &artifact)?;
    assert_eq!(
        before,
        Space {
            version: seeded,
            value: Some(SEED.to_owned()),
            requests: 1,
        },
        "the owner's seed must be stored before any race"
    );
    Ok(Fixture {
        artifact,
        seeded,
        view,
        session,
        before,
    })
}

/// A write that nothing races commits its value and its receipt at the next
/// version. The refusals in the races are meaningful only if this holds.
fn unraced_write(kind: Kind) -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    on_one_blocking_thread(async {
        let actor: ActorId = "local".parse()?;
        let fixture = started(&directory, &state, &actor).await?;
        let call = frame_call(&actor, &fixture.view);
        let receipt = frame_write(
            &state,
            &call,
            &fixture.view,
            kind,
            "write-control",
            fixture.seeded,
        )
        .await
        .map_err(|error| format!("{kind:?}: {}", error.body.message))?;
        assert_eq!(
            receipt.artifact_id, fixture.artifact,
            "{kind:?}: the receipt must name the artifact"
        );
        assert_eq!(
            receipt.path, PATH,
            "{kind:?}: the receipt must name the path"
        );
        assert_eq!(
            receipt.data_version,
            fixture.seeded + 1,
            "{kind:?}: the write must move the version once"
        );
        assert!(receipt.changed, "{kind:?}: the write must change the space");
        assert!(
            !receipt.replayed,
            "{kind:?}: the write must not be a replay"
        );
        assert_eq!(
            space(&directory, &actor, &fixture.artifact)?,
            Space {
                version: fixture.seeded + 1,
                value: kind.committed().map(str::to_owned),
                requests: fixture.before.requests + 1,
            },
            "{kind:?}: the committed write must hold its value at the next version"
        );
        Ok::<(), Box<dyn Error>>(())
    })
}

/// A grant withdrawn while the write waits for its commit refuses the commit on
/// the blocking task: `403`, and nothing is committed. The withdrawal lands after
/// the write's own checks, so only the check on the blocking task can refuse it.
fn grant_withdrawn_while_queued(kind: Kind) -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    on_one_blocking_thread(async {
        let actor: ActorId = "local".parse()?;
        let fixture = started(&directory, &state, &actor).await?;
        let call = frame_call(&actor, &fixture.view);
        let mut write = Box::pin(frame_write(
            &state,
            &call,
            &fixture.view,
            kind,
            "write-race-1",
            fixture.seeded,
        ));
        let mut withdrawal = Box::pin(withdraw_grant(
            &state,
            &actor,
            &fixture.session,
            &fixture.view,
        ));
        // The write's lookup and then the withdrawal queue behind the first
        // blocker. Once both have run, the write has passed its checks while the
        // grant still stands, and its commit queues behind the second blocker.
        // The withdrawal takes effect before that blocker is released.
        let (release, blocker) = hold_blocking_thread().await?;
        assert!(
            pends(&mut write).await,
            "{kind:?}: the write must wait for its lookup"
        );
        assert!(
            pends(&mut withdrawal).await,
            "{kind:?}: the withdrawal must wait behind the write's lookup"
        );
        release.send(())?;
        blocker.await?;
        settle().await?;
        let (release, blocker) = hold_blocking_thread().await?;
        assert!(
            pends(&mut write).await,
            "{kind:?}: the commit must wait behind the second blocker"
        );
        withdrawal.await?;
        release.send(())?;
        blocker.await?;
        let outcome = write.await;
        assert_eq!(
            space(&directory, &actor, &fixture.artifact)?,
            fixture.before,
            "{kind:?}: a write whose grant was withdrawn must commit nothing"
        );
        let error = refusal(outcome, StatusCode::FORBIDDEN)
            .map_err(|error| format!("{kind:?}: {error}"))?;
        assert_eq!(
            error.body.code,
            ErrorCode::Forbidden,
            "{kind:?}: the refusal must be a denial"
        );
        Ok::<(), Box<dyn Error>>(())
    })
}

/// A source save that moves the head after a write's checks, and before its
/// commit, refuses the commit at the store: `409 revision_conflict` with the new
/// head, and nothing is written.
fn source_moves_before_commit(kind: Kind) -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    on_one_blocking_thread(async {
        let actor: ActorId = "local".parse()?;
        let fixture = started(&directory, &state, &actor).await?;
        let call = frame_call(&actor, &fixture.view);
        let mut write = Box::pin(frame_write(
            &state,
            &call,
            &fixture.view,
            kind,
            "write-race-2",
            fixture.seeded,
        ));
        let (release, blocker) = pause_before_the_commit(&mut write).await?;
        // The source moves through a second connection while the commit waits.
        save_on_second_connection(&directory, &actor, &fixture.artifact, 2)?;
        release.send(())?;
        blocker.await?;
        let outcome = write.await;
        assert_eq!(
            space(&directory, &actor, &fixture.artifact)?,
            fixture.before,
            "{kind:?}: a write refused by the source fence must commit nothing"
        );
        let error =
            refusal(outcome, StatusCode::CONFLICT).map_err(|error| format!("{kind:?}: {error}"))?;
        assert_eq!(
            error.body.code,
            ErrorCode::RevisionConflict,
            "{kind:?}: the refusal must be a revision conflict"
        );
        assert_eq!(
            error.body.current_revision,
            Some(3),
            "{kind:?}: the source head must have moved to revision 3"
        );
        Ok::<(), Box<dyn Error>>(())
    })
}

/// A grant withdrawn after a write's commit withholds only the receipt: `403`.
/// The commit is not undone, so the space keeps the write's value and its version
/// moves once.
fn grant_withdrawn_after_commit(kind: Kind) -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    on_one_blocking_thread(async {
        let actor: ActorId = "local".parse()?;
        let fixture = started(&directory, &state, &actor).await?;
        let call = frame_call(&actor, &fixture.view);
        let mut write = Box::pin(frame_write(
            &state,
            &call,
            &fixture.view,
            kind,
            "write-race-3",
            fixture.seeded,
        ));
        let (release, blocker) = pause_before_the_commit(&mut write).await?;
        // The commit runs first, while the grant stands. The withdrawal queues
        // behind it and lands before the write's receipt is built.
        let mut withdrawal = Box::pin(withdraw_grant(
            &state,
            &actor,
            &fixture.session,
            &fixture.view,
        ));
        assert!(
            pends(&mut withdrawal).await,
            "{kind:?}: the withdrawal must wait behind the commit"
        );
        release.send(())?;
        blocker.await?;
        withdrawal.await?;
        let outcome = write.await;
        assert_eq!(
            space(&directory, &actor, &fixture.artifact)?,
            Space {
                version: fixture.seeded + 1,
                value: kind.committed().map(str::to_owned),
                requests: fixture.before.requests + 1,
            },
            "{kind:?}: the commit must stand: the version moves once and the value is stored"
        );
        let error = refusal(outcome, StatusCode::FORBIDDEN)
            .map_err(|error| format!("{kind:?}: {error}"))?;
        assert_eq!(
            error.body.code,
            ErrorCode::Forbidden,
            "{kind:?}: the refusal must be a denial"
        );
        Ok::<(), Box<dyn Error>>(())
    })
}

#[test]
fn an_unraced_write_commits_its_value_at_the_next_version() -> Result<(), Box<dyn Error>> {
    for kind in Kind::ALL {
        unraced_write(kind)?;
    }
    Ok(())
}

#[test]
fn a_queued_write_whose_grant_is_withdrawn_commits_nothing() -> Result<(), Box<dyn Error>> {
    for kind in Kind::ALL {
        grant_withdrawn_while_queued(kind)?;
    }
    Ok(())
}

#[test]
fn a_queued_write_whose_source_moves_before_its_commit_commits_nothing()
-> Result<(), Box<dyn Error>> {
    for kind in Kind::ALL {
        source_moves_before_commit(kind)?;
    }
    Ok(())
}

#[test]
fn a_write_whose_grant_is_withdrawn_after_its_commit_withholds_only_the_receipt()
-> Result<(), Box<dyn Error>> {
    for kind in Kind::ALL {
        grant_withdrawn_after_commit(kind)?;
    }
    Ok(())
}
