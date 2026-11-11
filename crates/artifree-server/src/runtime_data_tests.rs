//! Application data reads of runtime frames, each raced against one action in
//! the window between the store read and the reply (decision 0053, rule 8).
//!
//! A race holds the blocking thread, so the frame's store read waits where the
//! test wants it. The frame must then release no value: a withdrawn grant, a
//! revoked connection or a moved head each turn the reply into a refusal.

use std::error::Error;
use std::future::{Future, poll_fn};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::task::Poll;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use artifree_contracts::api::{
    ContentEncoding, CreateArtifactRequest, CreateRuntimeViewRequest, ErrorCode, FileChange,
    FileUpload, JsonValue, RuntimeDataReadRequest, RuntimeView, SaveRevisionRequest,
    SetDataRequest, SetRuntimeGrantRequest,
};
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

/// The path that the fixture stores its value at.
const PROFILE_PATH: &str = "profile";
/// The value stored at [`PROFILE_PATH`], already in canonical JSON text, so the
/// text that the read returns is the text that was written.
const PROFILE_TEXT: &str = r#"{"name":"Ada"}"#;

/// A fresh directory for one test's `SQLite` files, removed when the test ends.
struct TestDirectory(PathBuf);

/// Keeps the directories of tests that start in the same nanosecond apart.
static DIRECTORIES: AtomicU64 = AtomicU64::new(0);

impl TestDirectory {
    fn new() -> Result<Self, Box<dyn Error>> {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let count = DIRECTORIES.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "artifree-data-race-{}-{nonce}-{count}",
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
        format!("data-race-save-{base}").parse()?,
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

/// What [`granted_view`] sets up: the view, the connection it runs on, and the
/// data version that the owner's write left the space at.
struct Fixture {
    view: RuntimeView,
    session: String,
    version: u64,
}

/// An artifact whose manifest declares `data.read`, with the owner's value
/// stored at [`PROFILE_PATH`], and a connection granted `data.read` with a view
/// pinned to revision 2, the head after the manifest.
async fn granted_view(state: &AppState, actor: &ActorId) -> Result<Fixture, Box<dyn Error>> {
    let created = operations::create(
        state,
        actor.clone(),
        "data-race-artifact".parse()?,
        CreateArtifactRequest {
            title: "Data race".into(),
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
        r#"{"sdk":"artifree.app.v1","capabilities":["data.read"]}"#,
    )
    .await?;
    let written = operations::set_data(
        state,
        actor.clone(),
        "data-race-write".parse()?,
        &artifact,
        SetDataRequest {
            path: PROFILE_PATH.into(),
            base_data_version: 0,
            value: JsonValue::from_text(PROFILE_TEXT.into()).ok_or("invalid fixture value")?,
        },
        || Ok(None),
    )
    .await
    .map_err(|error| error.body.message)?;
    let session = operations::open_runtime_session(state, actor, None)
        .map_err(|error| error.body.message)?
        .session_id;
    operations::set_runtime_grant(
        state,
        actor.clone(),
        &session,
        &artifact,
        2,
        SetRuntimeGrantRequest {
            head_revision: 2,
            expected_version: 0,
            capabilities: vec!["data.read".into()],
        },
    )
    .await
    .map_err(|error| error.body.message)?;
    let view = operations::create_runtime_view(
        state,
        actor.clone(),
        None,
        StateGeneration::FIRST,
        &artifact,
        2,
        CreateRuntimeViewRequest {
            session_id: session.clone(),
            view_id: "a".repeat(32),
            generation: 1,
            adopt: None,
        },
    )
    .await
    .map_err(|error| error.body.message)?;
    assert_eq!(view.granted, ["data.read"]);
    let path = view
        .url
        .strip_prefix("http://127.0.0.1:7411")
        .ok_or("wrong preview origin")?;
    let response = preview::router(state.clone())
        .oneshot(Request::get(path).body(Body::empty())?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    Ok(Fixture {
        view,
        session,
        version: written.data_version,
    })
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

/// The read of [`PROFILE_PATH`] that `view` makes on its own connection.
fn read_body(view: &RuntimeView) -> RuntimeDataReadRequest {
    RuntimeDataReadRequest {
        initialization: view.initialization.clone(),
        path: PROFILE_PATH.into(),
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

/// Occupies the only blocking thread until the returned sender is used, so
/// that a store call queued behind it waits where the test wants it.
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

/// Drives `request` to the point where its store read has returned and its
/// reply is not sent yet. The first check's lookup and the store read each
/// wait behind a blocking thread the test holds, so the request cannot pass
/// either one until the test lets it.
async fn pause_after_the_store_read<T>(
    request: &mut Pin<Box<impl Future<Output = T>>>,
) -> Result<(), Box<dyn Error>> {
    let (release, blocker) = hold_blocking_thread().await?;
    assert!(
        pends(request).await,
        "the first check must wait for its lookup"
    );
    release.send(())?;
    blocker.await?;
    settle().await?;
    let (release, blocker) = hold_blocking_thread().await?;
    assert!(
        pends(request).await,
        "the store read must wait behind the blocker"
    );
    release.send(())?;
    blocker.await?;
    settle().await?;
    Ok(())
}

/// The refusal the call must give: `status`, and no data with it. A reply
/// that carries data fails here, whatever the data is.
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
fn on_one_blocking_thread<T>(
    scenario: impl Future<Output = Result<T, Box<dyn Error>>>,
) -> Result<T, Box<dyn Error>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(scenario)
}

/// The read that nothing races returns the stored text, at the version that
/// the owner's write left the space at. The refusals in the tests are only
/// meaningful if this holds, because a view that was never granted would
/// refuse them just the same.
async fn assert_unraced_read(
    state: &AppState,
    call: &FrameCall<'_>,
    fixture: &Fixture,
) -> Result<(), Box<dyn Error>> {
    let answer = runtime_data::read(state, call, &read_body(&fixture.view))
        .await
        .map_err(|error| {
            format!(
                "the unraced read was refused: {} {}",
                error.status, error.body.message
            )
        })?;
    assert_eq!(answer.artifact_id, fixture.view.artifact_id);
    assert_eq!(answer.path, PROFILE_PATH);
    assert!(answer.exists, "the stored value must be found");
    assert_eq!(answer.value.as_deref(), Some(PROFILE_TEXT));
    assert_eq!(answer.data_version, fixture.version);
    Ok(())
}

/// One action taken in the window between a read's store read and its reply.
#[derive(Debug, Clone, Copy)]
enum Race {
    /// The owner withdraws the connection's grant of `data.read`.
    Withdrawal,
    /// The actor's connections are revoked.
    Revocation,
    /// The owner saves a revision, which moves the artifact's head.
    HeadMove,
}

/// The refusal that a race must produce.
struct Expected {
    status: StatusCode,
    code: ErrorCode,
    /// The revision that the refusal names as the head, if it names one.
    current_revision: Option<u64>,
}

impl Race {
    /// The refusal that a read must give after this race.
    fn expected(self) -> Expected {
        match self {
            Self::Withdrawal => Expected {
                status: StatusCode::FORBIDDEN,
                code: ErrorCode::Forbidden,
                current_revision: None,
            },
            Self::Revocation => Expected {
                status: StatusCode::NOT_FOUND,
                code: ErrorCode::NotFound,
                current_revision: None,
            },
            Self::HeadMove => Expected {
                status: StatusCode::CONFLICT,
                code: ErrorCode::RevisionConflict,
                current_revision: Some(3),
            },
        }
    }

    /// Makes this change the way the owner or the user would make it.
    async fn apply(
        self,
        state: &AppState,
        actor: &ActorId,
        fixture: &Fixture,
    ) -> Result<(), Box<dyn Error>> {
        match self {
            Self::Withdrawal => withdraw_grant(state, actor, &fixture.session, &fixture.view).await,
            Self::Revocation => {
                state.revoke_actor(actor);
                Ok(())
            }
            Self::HeadMove => {
                save(
                    state,
                    actor,
                    &fixture.view.artifact_id,
                    fixture.view.head_revision,
                    "note.txt",
                    "moved",
                )
                .await
            }
        }
    }
}

/// Makes `race` between the store read and the reply of a granted read, and
/// checks that the read is refused as `race` expects. The unraced read runs
/// first, so a refusal can only come from the race.
fn run_race(race: Race) -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    on_one_blocking_thread(async {
        let actor: ActorId = "local".parse()?;
        let fixture = granted_view(&state, &actor).await?;
        let call = frame_call(&actor, &fixture.view);
        assert_unraced_read(&state, &call, &fixture).await?;
        let body = read_body(&fixture.view);
        let mut read = Box::pin(runtime_data::read(&state, &call, &body));
        pause_after_the_store_read(&mut read).await?;
        race.apply(&state, &actor, &fixture).await?;
        let expected = race.expected();
        let error = refusal(read.await, expected.status)?;
        assert_eq!(
            error.body.code, expected.code,
            "{race:?} must be refused with its error code"
        );
        assert_eq!(
            error.body.current_revision, expected.current_revision,
            "{race:?} must name the revision that the artifact's head is at"
        );
        Ok::<(), Box<dyn Error>>(())
    })
}

#[test]
fn an_unraced_read_returns_the_stored_text() -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    on_one_blocking_thread(async {
        let actor: ActorId = "local".parse()?;
        let fixture = granted_view(&state, &actor).await?;
        let call = frame_call(&actor, &fixture.view);
        assert_unraced_read(&state, &call, &fixture).await?;
        Ok::<(), Box<dyn Error>>(())
    })
}

#[test]
fn a_read_whose_grant_is_withdrawn_after_its_store_read_releases_no_value()
-> Result<(), Box<dyn Error>> {
    run_race(Race::Withdrawal)
}

#[test]
fn a_read_whose_connection_is_revoked_after_its_store_read_releases_no_value()
-> Result<(), Box<dyn Error>> {
    run_race(Race::Revocation)
}

#[test]
fn a_read_whose_head_moves_after_its_store_read_releases_no_value() -> Result<(), Box<dyn Error>> {
    run_race(Race::HeadMove)
}

/// The read bound the layers apply is the size of one data space.
#[test]
fn the_read_bound_is_the_space_limit() {
    assert_eq!(
        usize::try_from(artifree_contracts::api::MAX_RUNTIME_DATA_READ_BYTES).ok(),
        Some(artifree_domain::data::MAX_SPACE_BYTES)
    );
}
