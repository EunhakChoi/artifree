//! Runtime and handoff races across the real registry and artifact service.

use std::error::Error;
use std::future::{Future, poll_fn};
use std::path::PathBuf;
use std::sync::mpsc;
use std::task::Poll;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use artifree_contracts::api::{
    ContentEncoding, CreateArtifactRequest, CreateRuntimeViewRequest, FileChange, FileUpload,
    Handoff, RoomHeartbeatRequest, RoomJoinRequest, RuntimeDownloadKind, RuntimeDownloadRequest,
    RuntimeSourceReadRequest, RuntimeSourceWriteRequest, RuntimeView, SaveRevisionRequest,
    SetRuntimeGrantRequest, StageHandoffRequest, UpdateArtifactRequest,
};
use artifree_contracts::mcp::RegisteredHandoffSession;
use artifree_domain::SourceRevision;
use artifree_domain::identity::{ActorId, ArtifactId};
use artifree_ports::user_store::{NewUser, UserStore};
use artifree_providers::storage::SqliteStore;
use artifree_providers::tokens::SqliteTokenStore;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt;

use crate::auth::AccessToken;
use crate::http::{self, AppState, Settings};
use crate::limits::RequestLimits;
use crate::operations::ApiError;
use crate::rooms::{self, RoomNames};
use crate::runtime_source::{self, FrameCall};
use crate::{changes, operations, preview};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Result<Self, Box<dyn Error>> {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path = std::env::temp_dir().join(format!(
            "artifree-runtime-race-{}-{nonce}",
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

fn test_state(directory: &TestDirectory) -> Result<AppState, Box<dyn Error>> {
    test_state_with_ttl(directory, Duration::from_secs(60))
}

fn test_state_with_ttl(
    directory: &TestDirectory,
    preview_ttl: Duration,
) -> Result<AppState, Box<dyn Error>> {
    let store = SqliteStore::open(&directory.0.join("runtime.sqlite3"))?;
    let token_store = SqliteTokenStore::open(&directory.0.join("runtime.sqlite3"))?;
    let token = AccessToken::parse(&"0".repeat(64)).ok_or("invalid fixture token")?;
    Ok(AppState::new(
        store,
        token_store,
        token,
        Settings {
            content_origin: "http://127.0.0.1:7411".into(),
            preview_ttl,
            frame_ancestors: "http://127.0.0.1:*".into(),
            limits: RequestLimits::default(),
            change_streams: changes::DEFAULT_STREAMS_PER_ACTOR,
            ..Settings::default()
        },
    )?)
}

async fn initialized_view(
    state: &AppState,
    actor: &ActorId,
) -> Result<(RuntimeView, String), Box<dyn Error>> {
    let artifact_id = create_runtime_artifact(state, actor).await?;
    let session_id = operations::open_runtime_session(state, actor, None)
        .map_err(|error| error.body.message)?
        .session_id;
    operations::set_runtime_grant(
        state,
        actor.clone(),
        &session_id,
        &artifact_id,
        1,
        SetRuntimeGrantRequest {
            head_revision: 1,
            expected_version: 0,
            capabilities: vec!["metadata.read".into()],
        },
    )
    .await
    .map_err(|error| error.body.message)?;
    let view = operations::create_runtime_view(
        state,
        actor.clone(),
        None,
        &artifact_id,
        1,
        CreateRuntimeViewRequest {
            session_id: session_id.clone(),
            view_id: "a".repeat(32),
            generation: 1,
        },
    )
    .await
    .map_err(|error| error.body.message)?;
    let path = view
        .url
        .strip_prefix("http://127.0.0.1:7411")
        .ok_or("wrong preview origin")?;
    let response = preview::router(state.clone())
        .oneshot(Request::get(path).body(Body::empty())?)
        .await?;
    if response.status() != StatusCode::OK {
        return Err("the runtime fixture did not initialize".into());
    }
    Ok((view, session_id))
}

async fn create_runtime_artifact(
    state: &AppState,
    actor: &ActorId,
) -> Result<String, Box<dyn Error>> {
    let created = operations::create(
        state,
        actor.clone(),
        "runtime-race-key".parse()?,
        CreateArtifactRequest {
            title: "Runtime race".into(),
            files: vec![FileUpload {
                path: "index.html".into(),
                encoding: ContentEncoding::Utf8,
                content: "<!doctype html><html><head><!-- artifree-runtime --></head><body>test</body></html>".into(),
            }],
        },
    ).await.map_err(|error| error.body.message)?;
    Ok(created.artifact.id)
}

async fn issue_and_initialize(
    state: &AppState,
    actor: &ActorId,
    artifact: &str,
    session: &str,
    generation: u64,
) -> Result<RuntimeView, Box<dyn Error>> {
    let view = operations::create_runtime_view(
        state,
        actor.clone(),
        None,
        artifact,
        1,
        CreateRuntimeViewRequest {
            session_id: session.into(),
            view_id: "c".repeat(32),
            generation,
        },
    )
    .await
    .map_err(|error| error.body.message)?;
    let path = view
        .url
        .strip_prefix("http://127.0.0.1:7411")
        .ok_or("wrong origin")?;
    let response = preview::router(state.clone())
        .oneshot(Request::get(path).body(Body::empty())?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    Ok(view)
}

async fn read_runtime(
    state: &AppState,
    actor: &ActorId,
    view: &RuntimeView,
) -> Result<(), StatusCode> {
    operations::runtime_metadata(
        state,
        actor.clone(),
        &view.artifact_id,
        view.revision,
        &view.view_id,
        view.generation,
        &view.initialization,
    )
    .await
    .map(|_| ())
    .map_err(|error| error.status)
}

async fn change_grant(
    state: &AppState,
    actor: &ActorId,
    session: &str,
    view: &RuntimeView,
    allow: bool,
) -> Result<u64, StatusCode> {
    operations::set_runtime_grant(
        state,
        actor.clone(),
        session,
        &view.artifact_id,
        view.revision,
        SetRuntimeGrantRequest {
            head_revision: view.head_revision,
            expected_version: view.grant_version,
            capabilities: if allow {
                vec!["metadata.read".into()]
            } else {
                vec![]
            },
        },
    )
    .await
    .map(|grant| grant.version)
    .map_err(|error| error.status)
}

async fn save_fixture_file(
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
        format!("runtime-manifest-{base}").parse()?,
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

async fn manifest_view(
    state: &AppState,
    actor: &ActorId,
    artifact: &str,
    session: &str,
    revision: u64,
) -> Result<RuntimeView, StatusCode> {
    operations::create_runtime_view(
        state,
        actor.clone(),
        None,
        artifact,
        revision,
        CreateRuntimeViewRequest {
            session_id: session.into(),
            view_id: "e".repeat(32),
            generation: revision,
        },
    )
    .await
    .map_err(|error| error.status)
}

#[tokio::test]
async fn manifest_snapshot_is_enforced_at_the_real_grant_boundary() -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let actor: ActorId = "local".parse()?;
    let artifact = create_runtime_artifact(&state, &actor).await?;
    let session = operations::open_runtime_session(&state, &actor, None)
        .map_err(|error| error.body.message)?
        .session_id;
    save_fixture_file(
        &state,
        &actor,
        &artifact,
        1,
        "artifree-runtime.json",
        r#"{"sdk":"artifree.app.v1","capabilities":["metadata.read","future.write"]}"#,
    )
    .await?;
    let declared = manifest_view(&state, &actor, &artifact, &session, 2)
        .await
        .map_err(|status| format!("view: {status}"))?;
    assert_eq!(declared.declared, ["future.write", "metadata.read"]);
    assert_eq!(
        declared.supported,
        [
            "metadata.read",
            "source.download",
            "source.read",
            "source.write"
        ]
    );
    let request = SetRuntimeGrantRequest {
        head_revision: 2,
        expected_version: 0,
        capabilities: vec!["future.write".into()],
    };
    assert!(
        matches!(operations::set_runtime_grant(&state, actor.clone(), &session, &artifact, 2, request).await, Err(error) if error.status == StatusCode::UNPROCESSABLE_ENTITY)
    );
    change_grant(&state, &actor, &session, &declared, true)
        .await
        .map_err(|status| format!("grant: {status}"))?;
    // Omitting the file from a revision change preserves the existing declaration,
    // but the new source head does not inherit the earlier grant.
    save_fixture_file(&state, &actor, &artifact, 2, "note.txt", "new").await?;
    let inherited = manifest_view(&state, &actor, &artifact, &session, 3)
        .await
        .map_err(|status| format!("view: {status}"))?;
    assert_eq!(inherited.declared, declared.declared);
    assert!(inherited.granted.is_empty());
    save_fixture_file(
        &state,
        &actor,
        &artifact,
        3,
        "artifree-runtime.json",
        r#"{"sdk":"artifree.app.v1"}"#,
    )
    .await?;
    let omitted = manifest_view(&state, &actor, &artifact, &session, 4)
        .await
        .map_err(|status| format!("view: {status}"))?;
    assert!(omitted.declared.is_empty());
    assert_eq!(
        omitted.supported,
        [
            "metadata.read",
            "source.download",
            "source.read",
            "source.write"
        ]
    );
    assert_eq!(
        change_grant(&state, &actor, &session, &omitted, true).await,
        Err(StatusCode::UNPROCESSABLE_ENTITY)
    );
    save_fixture_file(
        &state,
        &actor,
        &artifact,
        4,
        "artifree-runtime.json",
        r#"{"sdk":"wrong","capabilities":["metadata.read"]}"#,
    )
    .await?;
    assert!(matches!(
        manifest_view(&state, &actor, &artifact, &session, 5).await,
        Err(StatusCode::UNPROCESSABLE_ENTITY)
    ));
    Ok(())
}

#[tokio::test]
async fn consent_revoke_regrant_revision_and_logout_gate_real_metadata()
-> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let actor: ActorId = "local".parse()?;
    let artifact = create_runtime_artifact(&state, &actor).await?;
    let session = operations::open_runtime_session(&state, &actor, None)
        .map_err(|error| error.body.message)?
        .session_id;
    let before = issue_and_initialize(&state, &actor, &artifact, &session, 1).await?;
    assert_eq!(before.declared, ["metadata.read"]);
    assert!(before.granted.is_empty());
    assert_eq!(
        read_runtime(&state, &actor, &before).await,
        Err(StatusCode::NOT_FOUND)
    );
    let granted = change_grant(&state, &actor, &session, &before, true)
        .await
        .map_err(|status| format!("grant failed: {status}"))?;
    assert_eq!(
        read_runtime(&state, &actor, &before).await,
        Err(StatusCode::NOT_FOUND)
    );
    let first = issue_and_initialize(&state, &actor, &artifact, &session, 2).await?;
    assert_eq!(first.grant_version, granted);
    assert_eq!(first.granted, ["metadata.read"]);
    assert_eq!(read_runtime(&state, &actor, &first).await, Ok(()));
    let revoked = change_grant(&state, &actor, &session, &first, false)
        .await
        .map_err(|status| format!("revoke failed: {status}"))?;
    assert_eq!(
        read_runtime(&state, &actor, &first).await,
        Err(StatusCode::NOT_FOUND)
    );
    assert_eq!(
        change_grant(&state, &actor, &session, &first, true).await,
        Err(StatusCode::CONFLICT)
    );
    let cleared = issue_and_initialize(&state, &actor, &artifact, &session, 3).await?;
    assert_eq!(cleared.grant_version, revoked);
    change_grant(&state, &actor, &session, &cleared, true)
        .await
        .map_err(|status| format!("regrant failed: {status}"))?;
    assert_eq!(
        read_runtime(&state, &actor, &first).await,
        Err(StatusCode::NOT_FOUND)
    );
    let regranted = issue_and_initialize(&state, &actor, &artifact, &session, 4).await?;
    assert_eq!(read_runtime(&state, &actor, &regranted).await, Ok(()));
    operations::save(
        &state,
        actor.clone(),
        "new-source-key".parse()?,
        &artifact,
        SaveRevisionRequest {
            base_revision: 1,
            changes: vec![FileChange::Put {
                path: "note.txt".into(),
                encoding: ContentEncoding::Utf8,
                content: "new".into(),
            }],
        },
    )
    .await
    .map_err(|error| error.body.message)?;
    assert_eq!(
        read_runtime(&state, &actor, &regranted).await,
        Err(StatusCode::NOT_FOUND)
    );
    assert_eq!(
        change_grant(&state, &actor, &session, &regranted, false).await,
        Err(StatusCode::CONFLICT)
    );
    let replaced = issue_and_initialize(&state, &actor, &artifact, &session, 5).await?;
    assert!(replaced.granted.is_empty());
    change_grant(&state, &actor, &session, &replaced, true)
        .await
        .map_err(|status| format!("new grant failed: {status}"))?;
    let current = issue_and_initialize(&state, &actor, &artifact, &session, 6).await?;
    assert_eq!(read_runtime(&state, &actor, &current).await, Ok(()));
    operations::close_runtime_session(&state, &actor, &session)
        .map_err(|error| error.body.message)?;
    assert_eq!(
        read_runtime(&state, &actor, &current).await,
        Err(StatusCode::NOT_FOUND)
    );
    assert_eq!(
        change_grant(&state, &actor, &session, &current, true).await,
        Err(StatusCode::NOT_FOUND)
    );
    Ok(())
}

#[test]
fn revocation_while_metadata_waits_rejects_its_late_result() -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let (view, session) = initialized_view(&state, &actor).await?;
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, waiting) = mpsc::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            let _sent = started.send(());
            let _released = waiting.recv();
        });
        ready.await?;
        let mut request = Box::pin(operations::runtime_metadata(
            &state,
            actor.clone(),
            &view.artifact_id,
            view.revision,
            &view.view_id,
            view.generation,
            &view.initialization,
        ));
        assert!(poll_fn(|context| Poll::Ready(request.as_mut().poll(context).is_pending())).await);
        // The read is held in the service, but the real grant registry can revoke it.
        let session_handle = state
            .runtime_sessions()
            .get(&actor, &session)
            .map_err(|error| format!("{error:?}"))?;
        let scope = crate::runtime_sessions::GrantScope::new(
            view.artifact_id.parse()?,
            SourceRevision(1),
            SourceRevision(1),
        );
        session_handle
            .set_grant(
                scope,
                crate::runtime_policy::CapabilitySet::default(),
                view.grant_version,
            )
            .map_err(|error| format!("{error:?}"))?;
        release.send(())?;
        assert!(matches!(request.await, Err(error) if error.status == StatusCode::NOT_FOUND));
        blocker.await?;
        Ok::<(), Box<dyn Error>>(())
    })?;
    Ok(())
}

/// A revision-2 view declaring both source capabilities, granted both and
/// initialized, on an artifact that has `note.txt`.
async fn source_view(
    state: &AppState,
    actor: &ActorId,
) -> Result<(RuntimeView, String), Box<dyn Error>> {
    let artifact = create_runtime_artifact(state, actor).await?;
    save_fixture_file(
        state,
        actor,
        &artifact,
        1,
        "artifree-runtime.json",
        r#"{"sdk":"artifree.app.v1","capabilities":["source.read","source.download","source.write"]}"#,
    )
    .await?;
    save_fixture_file(state, actor, &artifact, 2, "note.txt", "stored bytes").await?;
    let session = operations::open_runtime_session(state, actor, None)
        .map_err(|error| error.body.message)?
        .session_id;
    operations::set_runtime_grant(
        state,
        actor.clone(),
        &session,
        &artifact,
        3,
        SetRuntimeGrantRequest {
            head_revision: 3,
            expected_version: 0,
            capabilities: vec![
                "source.download".into(),
                "source.read".into(),
                "source.write".into(),
            ],
        },
    )
    .await
    .map_err(|error| error.body.message)?;
    let view = operations::create_runtime_view(
        state,
        actor.clone(),
        None,
        &artifact,
        3,
        CreateRuntimeViewRequest {
            session_id: session.clone(),
            view_id: "f".repeat(32),
            generation: 1,
        },
    )
    .await
    .map_err(|error| error.body.message)?;
    assert_eq!(
        view.granted,
        ["source.download", "source.read", "source.write"]
    );
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

/// Occupies the only blocking thread until the returned sender is used, so
/// that the service call behind a request waits where the test wants it.
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

async fn pends<T>(request: &mut std::pin::Pin<Box<impl Future<Output = T>>>) -> bool {
    poll_fn(|context| Poll::Ready(request.as_mut().poll(context).is_pending())).await
}

/// Drives `request` to the point where its bytes were read but not yet
/// returned, revokes the grant there, and lets it finish.
async fn revoke_between_the_read_and_the_reply<T>(
    state: &AppState,
    actor: &ActorId,
    session: &str,
    view: &RuntimeView,
    mut request: std::pin::Pin<Box<impl Future<Output = Result<T, operations::ApiError>>>>,
) -> Result<Result<T, operations::ApiError>, Box<dyn Error>> {
    // The first check waits for its artifact lookup behind the blocker.
    let (release, blocker) = hold_blocking_thread().await?;
    assert!(pends(&mut request).await);
    release.send(())?;
    blocker.await?;
    // The lookup ran first in line. Hold the thread again before the request
    // continues, so that its read of the bytes queues behind the blocker.
    tokio::task::spawn_blocking(|| ()).await?;
    let (release, blocker) = hold_blocking_thread().await?;
    assert!(pends(&mut request).await, "the read must be waiting now");
    let handle = state
        .runtime_sessions()
        .get(actor, session)
        .map_err(|error| format!("{error:?}"))?;
    let scope = crate::runtime_sessions::GrantScope::new(
        view.artifact_id.parse()?,
        SourceRevision(3),
        SourceRevision(3),
    );
    handle
        .set_grant(
            scope,
            crate::runtime_policy::CapabilitySet::default(),
            view.grant_version,
        )
        .map_err(|error| format!("{error:?}"))?;
    release.send(())?;
    blocker.await?;
    Ok(request.await)
}

#[test]
fn a_revocation_after_the_bytes_were_read_still_releases_none_of_them() -> Result<(), Box<dyn Error>>
{
    for operation in ["list", "read", "offer", "download"] {
        let directory = TestDirectory::new()?;
        let state = test_state(&directory)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()?;
        runtime.block_on(async {
            let actor: ActorId = "local".parse()?;
            let (view, session) = source_view(&state, &actor).await?;
            let call = FrameCall {
                actor: &actor,
                artifact: &view.artifact_id,
                revision: view.revision,
                view: &view.view_id,
                generation: view.generation,
                initialization: &view.initialization,
            };
            let file = RuntimeDownloadRequest {
                initialization: view.initialization.clone(),
                kind: RuntimeDownloadKind::File,
                path: Some("note.txt".into()),
            };
            let read = RuntimeSourceReadRequest {
                initialization: view.initialization.clone(),
                path: "note.txt".into(),
                offset: 0,
                length: 5,
            };
            let status = match operation {
                "list" => {
                    let waiting = Box::pin(runtime_source::list(&state, &call));
                    revoke_between_the_read_and_the_reply(&state, &actor, &session, &view, waiting)
                        .await?
                        .map(|_| ())
                }
                "read" => {
                    let waiting = Box::pin(runtime_source::read(&state, &call, &read));
                    revoke_between_the_read_and_the_reply(&state, &actor, &session, &view, waiting)
                        .await?
                        .map(|_| ())
                }
                "offer" => {
                    let waiting = Box::pin(runtime_source::offer(&state, &call, &file));
                    revoke_between_the_read_and_the_reply(&state, &actor, &session, &view, waiting)
                        .await?
                        .map(|_| ())
                }
                _ => {
                    let waiting = Box::pin(runtime_source::download(&state, &call, &file));
                    revoke_between_the_read_and_the_reply(&state, &actor, &session, &view, waiting)
                        .await?
                        .map(|_| ())
                }
            };
            assert!(
                matches!(status, Err(ref error) if error.status == StatusCode::FORBIDDEN),
                "{operation}: {status:?}"
            );
            Ok::<(), Box<dyn Error>>(())
        })?;
    }
    Ok(())
}

/// The write of `note.txt` that the tests below send.
fn rewrite(view: &RuntimeView) -> RuntimeSourceWriteRequest {
    RuntimeSourceWriteRequest {
        initialization: view.initialization.clone(),
        retry_id: "retry-0001".into(),
        path: "note.txt".into(),
        content: "cmV3cml0dGVu".into(),
    }
}

#[test]
fn a_revocation_during_a_write_withholds_the_receipt_not_the_revision() -> Result<(), Box<dyn Error>>
{
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let (view, session) = source_view(&state, &actor).await?;
        let call = FrameCall {
            actor: &actor,
            artifact: &view.artifact_id,
            revision: view.revision,
            view: &view.view_id,
            generation: view.generation,
            initialization: &view.initialization,
        };
        let request = rewrite(&view);
        let waiting = Box::pin(runtime_source::write(&state, &call, &request));
        let outcome =
            revoke_between_the_read_and_the_reply(&state, &actor, &session, &view, waiting).await?;
        assert!(
            matches!(outcome, Err(ref error) if error.status == StatusCode::FORBIDDEN),
            "the frame is told nothing of an outcome the grant no longer covers: {:?}",
            outcome.map(|receipt| receipt.revision)
        );
        // The commit happened before the revoke was seen, and a revoke does
        // not undo it: the owner finds the revision in the history.
        let id: ArtifactId = view.artifact_id.parse()?;
        let record = state
            .run({
                let actor = actor.clone();
                move |artifacts| artifacts.artifact(&actor, &id)
            })
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(record.current_revision, SourceRevision(4));
        // With the grant gone the replay is refused too.
        let replay = runtime_source::write(&state, &call, &request).await;
        assert!(matches!(replay, Err(ref error) if error.status == StatusCode::FORBIDDEN));
        Ok::<(), Box<dyn Error>>(())
    })
}

#[test]
fn another_actor_cannot_write_through_the_owners_view() -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let owner: ActorId = "local".parse()?;
        let other: ActorId = "other".parse()?;
        let (view, _session) = source_view(&state, &owner).await?;
        let call = FrameCall {
            actor: &other,
            artifact: &view.artifact_id,
            revision: view.revision,
            view: &view.view_id,
            generation: view.generation,
            initialization: &view.initialization,
        };
        let refused = runtime_source::write(&state, &call, &rewrite(&view)).await;
        assert!(
            matches!(refused, Err(ref error) if error.status == StatusCode::NOT_FOUND),
            "{:?}",
            refused.map(|receipt| receipt.revision)
        );
        // Nothing was saved: the owner's write at the same call succeeds as
        // the very next revision.
        let owner_call = FrameCall {
            actor: &owner,
            ..call
        };
        let saved = runtime_source::write(&state, &owner_call, &rewrite(&view)).await;
        assert_eq!(saved.map(|receipt| receipt.revision).ok(), Some(4));
        Ok::<(), Box<dyn Error>>(())
    })
}

#[test]
fn only_a_few_downloads_per_actor_are_built_at_once_and_each_frees_its_slot()
-> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let (view, _session) = source_view(&state, &actor).await?;
        let call = FrameCall {
            actor: &actor,
            artifact: &view.artifact_id,
            revision: view.revision,
            view: &view.view_id,
            generation: view.generation,
            initialization: &view.initialization,
        };
        let archive = RuntimeDownloadRequest {
            initialization: view.initialization.clone(),
            kind: RuntimeDownloadKind::Archive,
            path: None,
        };
        let file = RuntimeDownloadRequest {
            initialization: view.initialization.clone(),
            kind: RuntimeDownloadKind::File,
            path: Some("note.txt".into()),
        };
        // Hold every slot the actor may use, as builds in progress would.
        let mut held: Vec<_> = (0..runtime_source::MAX_DOWNLOAD_BUILDS)
            .map(|_| state.download_builds().take(&actor))
            .collect::<Result<_, _>>()
            .map_err(|refusal| refusal.to_string())?;
        for request in [&archive, &file] {
            let refused = runtime_source::download(&state, &call, request).await;
            assert!(
                matches!(&refused, Err(error)
                    if error.status == StatusCode::TOO_MANY_REQUESTS
                        && error.body.code == artifree_contracts::api::ErrorCode::RateLimited),
                "{:?}",
                refused.map(|_| ())
            );
        }
        // The refusal says when to retry, as every rate limit does.
        let refusal = runtime_source::download(&state, &call, &archive)
            .await
            .err()
            .ok_or("a download over the cap must be refused")?;
        let response = axum::response::IntoResponse::into_response(refusal);
        assert_eq!(
            response
                .headers()
                .get(header::RETRY_AFTER)
                .map(axum::http::HeaderValue::as_bytes),
            Some(b"1".as_slice())
        );
        // Only downloads are held back, and only for this actor.
        let listed = runtime_source::list(&state, &call).await;
        assert_eq!(listed.map(|list| list.revision).ok(), Some(view.revision));
        let offered = runtime_source::offer(&state, &call, &file).await;
        assert_eq!(
            offered.map(|offer| offer.file_name).ok().as_deref(),
            Some("note.txt")
        );
        let other: ActorId = "other".parse()?;
        assert!(state.download_builds().take(&other).is_ok());
        // One slot freed: a download is built, and its slot is freed when it
        // returns, so the next one is built too.
        held.pop();
        let zipped = runtime_source::download(&state, &call, &archive).await;
        assert_eq!(
            zipped.map(|built| built.bytes.starts_with(b"PK")).ok(),
            Some(true),
            "an archive is a zip"
        );
        let stored = runtime_source::download(&state, &call, &file).await;
        assert_eq!(
            stored.map(|built| built.bytes).ok().as_deref(),
            Some(b"stored bytes".as_slice())
        );
        // A download that fails also frees its slot.
        let missing = RuntimeDownloadRequest {
            path: Some("absent.txt".into()),
            ..file.clone()
        };
        for _ in 0..4 {
            let failed = runtime_source::download(&state, &call, &missing).await;
            assert!(
                matches!(&failed, Err(error) if error.status == StatusCode::NOT_FOUND),
                "{:?}",
                failed.map(|_| ())
            );
        }
        drop(held);
        Ok::<(), Box<dyn Error>>(())
    })
}

#[test]
fn logout_while_handoff_source_waits_rejects_its_late_delivery() -> Result<(), Box<dyn Error>> {
    handoff_source_recheck(true)
}

#[test]
fn acknowledgement_while_handoff_source_waits_stops_its_replay() -> Result<(), Box<dyn Error>> {
    handoff_source_recheck(false)
}

async fn queued_handoff(
    state: &AppState,
    actor: &ActorId,
) -> Result<(String, RegisteredHandoffSession, Handoff), Box<dyn Error>> {
    let artifact = create_runtime_artifact(state, actor).await?;
    let revision = operations::revision(state, actor.clone(), &artifact, 1)
        .await
        .map_err(|error| error.body.message)?;
    let workspace = operations::open_runtime_session(state, actor, None)
        .map_err(|error| error.body.message)?
        .session_id;
    let agent = operations::register_handoff_session(state, actor, "Editor")
        .map_err(|error| error.body.message)?;
    let staged = operations::stage_handoff(
        state,
        actor.clone(),
        StageHandoffRequest {
            workspace_session_id: workspace.clone(),
            agent_session_id: agent.session_id.clone(),
            artifact_id: artifact,
            revision: 1,
            path: "index.html".into(),
            file_sha256: revision
                .files
                .first()
                .ok_or("missing source")?
                .sha256
                .clone(),
            start: 0,
            end: 4,
            intent: "Edit".into(),
        },
    )
    .await
    .map_err(|error| error.body.message)?;
    operations::send_handoff(state, actor.clone(), &workspace, &staged.id)
        .await
        .map_err(|error| error.body.message)?;
    Ok((workspace, agent, staged))
}

async fn staged_handoff(
    state: &AppState,
    actor: &ActorId,
) -> Result<(String, RegisteredHandoffSession, Handoff), Box<dyn Error>> {
    let (workspace, agent, existing) = queued_handoff(state, actor).await?;
    state
        .handoffs()
        .cancel(actor, &workspace, &existing.id)
        .map_err(|error| format!("{error:?}"))?;
    let connection = state
        .runtime_sessions()
        .get(actor, &workspace)
        .map_err(|error| format!("{error:?}"))?;
    let staged = state
        .handoffs()
        .stage(
            actor,
            &workspace,
            &connection,
            &agent.session_id,
            existing.selection,
            "Edit".into(),
        )
        .map_err(|error| format!("{error:?}"))?;
    Ok((workspace, agent, staged))
}

#[test]
fn first_send_storage_failure_keeps_stage_retryable() -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let (workspace, _agent, staged) = staged_handoff(&state, &actor).await?;
        let database = directory.0.join("runtime.sqlite3");
        let backup = directory.0.join("runtime-backup.sqlite3");
        std::fs::rename(&database, &backup)?;
        std::fs::create_dir(&database)?;
        let failure = operations::send_handoff(&state, actor.clone(), &workspace, &staged.id).await;
        std::fs::remove_dir(&database)?;
        std::fs::rename(&backup, &database)?;
        assert!(matches!(failure, Err(error) if error.status == StatusCode::SERVICE_UNAVAILABLE));
        assert_eq!(
            operations::get_handoff(&state, &actor, &workspace, &staged.id)
                .map_err(|error| error.body.message)?
                .state,
            artifree_contracts::api::HandoffState::Staged
        );
        assert!(matches!(
            operations::send_handoff(&state, actor, &workspace, &staged.id).await,
            Ok(handoff) if handoff.state == artifree_contracts::api::HandoffState::Queued
        ));
        Ok::<(), Box<dyn Error>>(())
    })
}

#[test]
fn temporary_storage_failure_keeps_an_offer_retryable() -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let (workspace, agent, queued) = queued_handoff(&state, &actor).await?;
        let database = directory.0.join("runtime.sqlite3");
        let backup = directory.0.join("runtime-backup.sqlite3");
        // Each storage operation opens the database anew. A directory at its
        // path makes the real read unavailable until the file is restored.
        std::fs::rename(&database, &backup)?;
        std::fs::create_dir(&database)?;
        let failure = operations::poll_handoffs(
            &state,
            actor.clone(),
            &agent.session_id,
            &agent.session_secret,
        )
        .await;
        std::fs::remove_dir(&database)?;
        std::fs::rename(&backup, &database)?;
        assert!(matches!(failure, Err(error) if error.status == StatusCode::SERVICE_UNAVAILABLE));
        let status = operations::get_handoff(&state, &actor, &workspace, &queued.id)
            .map_err(|error| error.body.message)?;
        assert_eq!(status.state, artifree_contracts::api::HandoffState::Offered);
        let retried =
            operations::poll_handoffs(&state, actor, &agent.session_id, &agent.session_secret)
                .await
                .map_err(|error| error.body.message)?;
        assert_eq!(retried.deliveries.len(), 1);
        assert_eq!(retried.deliveries[0].id, queued.id);
        Ok::<(), Box<dyn Error>>(())
    })
}

async fn queued_second_handoff(
    state: &AppState,
    actor: &ActorId,
    workspace: &str,
    agent: &RegisteredHandoffSession,
) -> Result<Handoff, Box<dyn Error>> {
    let next_artifact = operations::create(
        state,
        actor.clone(),
        "next-handoff".parse()?,
        CreateArtifactRequest {
            title: "Another source".into(),
            files: vec![FileUpload {
                path: "index.html".into(),
                encoding: ContentEncoding::Utf8,
                content: "<p>next</p>".into(),
            }],
        },
    )
    .await
    .map_err(|error| error.body.message)?;
    let next = operations::stage_handoff(
        state,
        actor.clone(),
        StageHandoffRequest {
            workspace_session_id: workspace.to_owned(),
            agent_session_id: agent.session_id.clone(),
            artifact_id: next_artifact.artifact.id,
            revision: 1,
            path: "index.html".into(),
            file_sha256: next_artifact
                .revision
                .files
                .first()
                .ok_or("missing source")?
                .sha256
                .clone(),
            start: 0,
            end: 3,
            intent: "Edit".into(),
        },
    )
    .await
    .map_err(|error| error.body.message)?;
    operations::send_handoff(state, actor.clone(), workspace, &next.id)
        .await
        .map_err(|error| error.body.message)?;
    Ok(next)
}

#[test]
fn missing_source_does_not_starve_another_offer() -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let (workspace, agent, first) = queued_handoff(&state, &actor).await?;
        let next = queued_second_handoff(&state, &actor, &workspace, &agent).await?;
        operations::update(
            &state,
            actor.clone(),
            "trash-handoff".parse()?,
            &first.selection.artifact_id,
            UpdateArtifactRequest {
                base_metadata_revision: 1,
                title: None,
                pinned: None,
                deleted: Some(true),
            },
        )
        .await
        .map_err(|error| error.body.message)?;
        let failure = operations::poll_handoffs(
            &state,
            actor.clone(),
            &agent.session_id,
            &agent.session_secret,
        )
        .await;
        assert!(matches!(failure, Err(error) if error.status == StatusCode::NOT_FOUND));
        assert_eq!(
            operations::get_handoff(&state, &actor, &workspace, &first.id)
                .map_err(|error| error.body.message)?
                .state,
            artifree_contracts::api::HandoffState::Blocked
        );
        assert!(matches!(
            operations::send_handoff(&state, actor.clone(), &workspace, &first.id).await,
            Ok(handoff) if handoff.state == artifree_contracts::api::HandoffState::Blocked
        ));
        let following = operations::poll_handoffs(
            &state,
            actor.clone(),
            &agent.session_id,
            &agent.session_secret,
        )
        .await
        .map_err(|error| error.body.message)?;
        assert_eq!(following.deliveries.len(), 1);
        assert_eq!(following.deliveries[0].id, next.id);
        assert_eq!(
            operations::acknowledge_handoff(
                &state,
                &actor,
                &agent.session_id,
                &agent.session_secret,
                &first.id
            )
            .map_err(|error| error.body.message)?
            .state,
            artifree_contracts::api::HandoffState::Acknowledged
        );
        operations::close_runtime_session(&state, &actor, &workspace)
            .map_err(|error| error.body.message)?;
        assert!(matches!(
            operations::send_handoff(&state, actor, &workspace, &first.id).await,
            Ok(handoff) if handoff.state == artifree_contracts::api::HandoffState::Acknowledged
        ));
        Ok::<(), Box<dyn Error>>(())
    })
}

#[test]
fn closed_workspace_cannot_send_an_unoffered_item() -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let (workspace, _agent, queued) = queued_handoff(&state, &actor).await?;
        operations::close_runtime_session(&state, &actor, &workspace)
            .map_err(|error| error.body.message)?;
        assert!(matches!(
            operations::send_handoff(&state, actor, &workspace, &queued.id).await,
            Err(error) if error.status == StatusCode::CONFLICT
        ));
        Ok::<(), Box<dyn Error>>(())
    })
}

#[test]
fn created_handoff_location_retrieves_the_same_resource() -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let (workspace, agent, existing) = queued_handoff(&state, &actor).await?;
        let selection = existing.selection;
        let proposal = StageHandoffRequest {
            workspace_session_id: workspace,
            agent_session_id: agent.session_id,
            artifact_id: selection.artifact_id,
            revision: selection.revision,
            path: selection.path,
            file_sha256: selection.file_sha256,
            start: selection.start,
            end: selection.end,
            intent: "Edit".into(),
        };
        let authorization = format!("Bearer {}", "0".repeat(64));
        let app = http::router(state);
        let created = app
            .clone()
            .oneshot(
                Request::post("/v1/handoffs")
                    .header(header::AUTHORIZATION, &authorization)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&proposal)?))?,
            )
            .await?;
        assert_eq!(created.status(), StatusCode::CREATED);
        let location = created
            .headers()
            .get(header::LOCATION)
            .ok_or("missing Location")?
            .to_str()?
            .to_owned();
        let original = axum::body::to_bytes(created.into_body(), usize::MAX).await?;
        let retrieved = app
            .oneshot(
                Request::get(location)
                    .header(header::AUTHORIZATION, authorization)
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(
            retrieved.status(),
            StatusCode::OK,
            "the created Location must be retrievable"
        );
        assert_eq!(
            axum::body::to_bytes(retrieved.into_body(), usize::MAX).await?,
            original
        );
        Ok::<(), Box<dyn Error>>(())
    })
}

#[test]
fn changed_source_digest_blocks_only_that_offer() -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let (workspace, agent, valid) = queued_handoff(&state, &actor).await?;
        state
            .handoffs()
            .cancel(&actor, &workspace, &valid.id)
            .map_err(|error| format!("{error:?}"))?;
        let connection = state
            .runtime_sessions()
            .get(&actor, &workspace)
            .map_err(|error| format!("{error:?}"))?;
        // Model an already queued selection whose pinned digest no longer
        // matches the actual file; the real source read detects the change.
        let mut mismatched_selection = valid.selection.clone();
        mismatched_selection.file_sha256 = "0".repeat(64);
        let old = state
            .handoffs()
            .stage(
                &actor,
                &workspace,
                &connection,
                &agent.session_id,
                mismatched_selection,
                "Edit".into(),
            )
            .map_err(|error| format!("{error:?}"))?;
        state
            .handoffs()
            .send(&actor, &workspace, &old.id)
            .map_err(|error| format!("{error:?}"))?;
        let failure = operations::poll_handoffs(
            &state,
            actor.clone(),
            &agent.session_id,
            &agent.session_secret,
        )
        .await;
        assert!(matches!(failure, Err(error) if error.status == StatusCode::CONFLICT));
        assert_eq!(
            operations::get_handoff(&state, &actor, &workspace, &old.id)
                .map_err(|error| error.body.message)?
                .state,
            artifree_contracts::api::HandoffState::Blocked
        );
        let following = state
            .handoffs()
            .stage(
                &actor,
                &workspace,
                &connection,
                &agent.session_id,
                valid.selection,
                "Edit".into(),
            )
            .map_err(|error| format!("{error:?}"))?;
        state
            .handoffs()
            .send(&actor, &workspace, &following.id)
            .map_err(|error| format!("{error:?}"))?;
        let next = operations::poll_handoffs(
            &state,
            actor.clone(),
            &agent.session_id,
            &agent.session_secret,
        )
        .await
        .map_err(|error| error.body.message)?;
        assert_eq!(next.deliveries.len(), 1);
        assert_eq!(next.deliveries[0].id, following.id);
        assert!(matches!(
            operations::acknowledge_handoff(
                &state,
                &actor,
                &agent.session_id,
                &agent.session_secret,
                &old.id
            ),
            Ok(receipt) if receipt.state == artifree_contracts::api::HandoffState::Acknowledged
        ));
        Ok::<(), Box<dyn Error>>(())
    })
}

#[derive(Clone, Copy)]
enum ConcurrentSendChange {
    Sent,
    Cancelled,
}

fn send_during_failed_source_lookup(change: ConcurrentSendChange) -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let (workspace, _agent, staged) = staged_handoff(&state, &actor).await?;
        operations::update(
            &state,
            actor.clone(),
            "trash-before-send-race".parse()?,
            &staged.selection.artifact_id,
            UpdateArtifactRequest {
                base_metadata_revision: 1,
                title: None,
                pinned: None,
                deleted: Some(true),
            },
        )
        .await
        .map_err(|error| error.body.message)?;
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, waiting) = mpsc::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            let _sent = started.send(());
            let _released = waiting.recv();
        });
        ready.await?;
        let mut request = Box::pin(operations::send_handoff(
            &state,
            actor.clone(),
            &workspace,
            &staged.id,
        ));
        assert!(poll_fn(|context| Poll::Ready(request.as_mut().poll(context).is_pending())).await);
        // Model the other request completing its already validated transition
        // or the user cancelling while this source read is waiting.
        match change {
            ConcurrentSendChange::Sent => state.handoffs().send(&actor, &workspace, &staged.id),
            ConcurrentSendChange::Cancelled => {
                state.handoffs().cancel(&actor, &workspace, &staged.id)
            }
        }
        .map_err(|error| format!("{error:?}"))?;
        release.send(())?;
        let result = request.await;
        match change {
            ConcurrentSendChange::Sent => assert!(
                matches!(result, Ok(handoff) if handoff.state == artifree_contracts::api::HandoffState::Queued),
                "a concurrent committed send must be recoverable despite the late lookup failure"
            ),
            ConcurrentSendChange::Cancelled => assert!(
                matches!(result, Err(error) if error.status == StatusCode::CONFLICT),
                "a concurrent cancel must not be turned into a send"
            ),
        }
        blocker.await?;
        Ok::<(), Box<dyn Error>>(())
    })
}

#[test]
fn concurrent_send_is_returned_after_source_lookup_fails() -> Result<(), Box<dyn Error>> {
    send_during_failed_source_lookup(ConcurrentSendChange::Sent)
}

#[test]
fn concurrent_cancel_wins_after_source_lookup_fails() -> Result<(), Box<dyn Error>> {
    send_during_failed_source_lookup(ConcurrentSendChange::Cancelled)
}

fn send_during_logout(already_offered: bool) -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let (workspace, agent, staged) = staged_handoff(&state, &actor).await?;
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, waiting) = mpsc::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            let _sent = started.send(());
            let _released = waiting.recv();
        });
        ready.await?;
        let mut request = Box::pin(operations::send_handoff(
            &state,
            actor.clone(),
            &workspace,
            &staged.id,
        ));
        assert!(poll_fn(|context| Poll::Ready(request.as_mut().poll(context).is_pending())).await);
        if already_offered {
            state
                .handoffs()
                .send(&actor, &workspace, &staged.id)
                .map_err(|error| format!("{error:?}"))?;
            let offers = state
                .handoffs()
                .poll(&actor, &agent.session_id, &agent.session_secret)
                .map_err(|error| format!("{error:?}"))?;
            assert!(matches!(offers.as_slice(), [offer] if offer.id == staged.id));
        }
        operations::close_runtime_session(&state, &actor, &workspace)
            .map_err(|error| error.body.message)?;
        release.send(())?;
        let result = request.await;
        if already_offered {
            assert!(
                matches!(result, Ok(handoff) if handoff.state == artifree_contracts::api::HandoffState::Offered),
                "a prior offer remains a readable outcome after logout"
            );
        } else {
            assert!(
                matches!(result, Err(error) if error.status == StatusCode::CONFLICT),
                "logout must prevent a stage from being sent after its source check"
            );
        }
        assert!(
            state
                .handoffs()
                .poll(&actor, &agent.session_id, &agent.session_secret)
                .map_err(|error| format!("{error:?}"))?
                .is_empty(),
            "logout must stop later delivery or replay"
        );
        blocker.await?;
        Ok::<(), Box<dyn Error>>(())
    })
}

#[test]
fn prior_offer_is_readable_after_logout_during_source_check() -> Result<(), Box<dyn Error>> {
    send_during_logout(true)
}

#[test]
fn logout_fences_unoffered_send_during_source_check() -> Result<(), Box<dyn Error>> {
    send_during_logout(false)
}

fn handoff_source_recheck(logout: bool) -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let (workspace, agent, staged) = queued_handoff(&state, &actor).await?;
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, waiting) = mpsc::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            let _sent = started.send(());
            let _released = waiting.recv();
        });
        ready.await?;
        let mut request = Box::pin(operations::poll_handoffs(
            &state,
            actor.clone(),
            &agent.session_id,
            &agent.session_secret,
        ));
        assert!(poll_fn(|context| Poll::Ready(request.as_mut().poll(context).is_pending())).await);
        if logout {
            operations::close_runtime_session(&state, &actor, &workspace)
                .map_err(|error| error.body.message)?;
        } else {
            operations::acknowledge_handoff(
                &state,
                &actor,
                &agent.session_id,
                &agent.session_secret,
                &staged.id,
            )
            .map_err(|error| error.body.message)?;
        }
        release.send(())?;
        let result = request.await;
        if logout {
            assert!(
                matches!(result, Err(error) if error.status == StatusCode::NOT_FOUND),
                "logout must fence an offered delivery whose source lookup finishes later"
            );
        } else {
            assert!(
                result
                    .map_err(|error| error.body.message)?
                    .deliveries
                    .is_empty(),
                "an acknowledged item must not be replayed by a delayed poll"
            );
        }
        let receipt = operations::acknowledge_handoff(
            &state,
            &actor,
            &agent.session_id,
            &agent.session_secret,
            &staged.id,
        )
        .map_err(|error| error.body.message)?;
        assert_eq!(
            receipt.state,
            artifree_contracts::api::HandoffState::Acknowledged
        );
        blocker.await?;
        Ok::<(), Box<dyn Error>>(())
    })
}

#[test]
fn logout_fences_pending_grant_and_view_lookups() -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let artifact = create_runtime_artifact(&state, &actor).await?;
        let session = operations::open_runtime_session(&state, &actor, None)
            .map_err(|error| error.body.message)?
            .session_id;
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, waiting) = mpsc::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            let _sent = started.send(());
            let _released = waiting.recv();
        });
        ready.await?;
        let mut view = Box::pin(operations::create_runtime_view(
            &state,
            actor.clone(),
            None,
            &artifact,
            1,
            CreateRuntimeViewRequest {
                session_id: session.clone(),
                view_id: "c".repeat(32),
                generation: 1,
            },
        ));
        let mut grant = Box::pin(operations::set_runtime_grant(
            &state,
            actor.clone(),
            &session,
            &artifact,
            1,
            SetRuntimeGrantRequest {
                head_revision: 1,
                expected_version: 0,
                capabilities: vec!["metadata.read".into()],
            },
        ));
        assert!(poll_fn(|context| Poll::Ready(view.as_mut().poll(context).is_pending())).await);
        assert!(poll_fn(|context| Poll::Ready(grant.as_mut().poll(context).is_pending())).await);
        operations::close_runtime_session(&state, &actor, &session)
            .map_err(|error| error.body.message)?;
        release.send(())?;
        assert!(matches!(view.await, Err(error) if error.status == StatusCode::NOT_FOUND));
        assert!(matches!(grant.await, Err(error) if error.status == StatusCode::NOT_FOUND));
        blocker.await?;
        Ok::<(), Box<dyn Error>>(())
    })?;
    Ok(())
}

#[test]
fn replacement_while_metadata_waits_for_storage_rejects_the_late_result()
-> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    // One blocking worker lets the test hold the actual service read after
    // its first binding check, without sleeps or a mocked storage response.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let (view, _session) = initialized_view(&state, &actor).await?;
        let artifact: ArtifactId = view.artifact_id.parse()?;
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, waiting) = mpsc::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            let _sent = started.send(());
            let _released = waiting.recv();
        });
        ready.await?;
        let mut request = Box::pin(operations::runtime_metadata(
            &state,
            actor.clone(),
            &view.artifact_id,
            view.revision,
            &view.view_id,
            view.generation,
            &view.initialization,
        ));
        let pending = poll_fn(|context| match request.as_mut().poll(context) {
            Poll::Pending => Poll::Ready(true),
            Poll::Ready(_) => Poll::Ready(false),
        })
        .await;
        assert!(
            pending,
            "the service read must be queued before replacement"
        );
        let replacement = state.previews().issue_test_runtime(
            actor,
            artifact,
            SourceRevision(1),
            SourceRevision(1),
            view.view_id.clone(),
            2,
        );
        assert!(replacement.is_ok());
        release.send(())?;
        let result = request.await;
        assert!(
            matches!(result, Err(error) if error.status == StatusCode::NOT_FOUND),
            "a replaced view must not receive the queued metadata result"
        );
        blocker.await?;
        Ok::<(), Box<dyn Error>>(())
    })?;
    drop(runtime);
    drop(state);
    Ok(())
}

#[test]
fn close_while_creation_waits_for_storage_rejects_the_late_view() -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    // An immediately expired fence must still cover an already pending lookup.
    let state = test_state_with_ttl(&directory, Duration::ZERO)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let artifact_id = create_runtime_artifact(&state, &actor).await?;
        let session_id = operations::open_runtime_session(&state, &actor, None)
            .map_err(|error| error.body.message)?
            .session_id;
        let view_id = "a".repeat(32);
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, waiting) = mpsc::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            let _sent = started.send(());
            let _released = waiting.recv();
        });
        ready.await?;
        let mut request = Box::pin(operations::create_runtime_view(
            &state,
            actor.clone(),
            None,
            &artifact_id,
            1,
            CreateRuntimeViewRequest {
                session_id,
                view_id: view_id.clone(),
                generation: 2,
            },
        ));
        let pending = poll_fn(|context| match request.as_mut().poll(context) {
            Poll::Pending => Poll::Ready(true),
            Poll::Ready(_) => Poll::Ready(false),
        })
        .await;
        assert!(pending, "the service lookup must be queued before close");
        operations::close_runtime_view(&state, &actor, &view_id, 2)
            .map_err(|error| error.body.message)?;
        release.send(())?;
        let result = request.await;
        assert!(
            matches!(result, Err(error) if error.status == StatusCode::CONFLICT),
            "a closed generation must not be issued after its lookup finishes"
        );
        blocker.await?;
        Ok::<(), Box<dyn Error>>(())
    })?;
    drop(runtime);
    drop(state);
    Ok(())
}

fn bad<T>() -> Result<T, ApiError> {
    Err(ApiError::invalid("body is malformed"))
}

fn join_request(slot: &str) -> RoomJoinRequest {
    RoomJoinRequest {
        slot: slot.into(),
        state: None,
    }
}

#[test]
fn another_actor_has_no_access_to_the_owners_room_or_participants() -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let owner: ActorId = "local".parse()?;
        let other: ActorId = "other".parse()?;
        let artifact = create_runtime_artifact(&state, &owner).await?;
        let tab = operations::open_runtime_session(&state, &owner, None)
            .map_err(|error| error.body.message)?
            .session_id;
        let theirs = operations::open_runtime_session(&state, &other, None)
            .map_err(|error| error.body.message)?
            .session_id;
        let mine = rooms::join(&state, &owner, &tab, &artifact, Ok(join_request("page")))
            .await
            .map_err(|error| error.body.message)?;

        // The other actor's own connection cannot reach the owner's artifact.
        let joined =
            rooms::join(&state, &other, &theirs, &artifact, Ok(join_request("page"))).await;
        assert!(matches!(&joined, Err(error) if error.status == StatusCode::NOT_FOUND));
        // Nor can it use the owner's connection or participant.
        let (participant, epoch) = (mine.participant.to_string(), mine.epoch.to_string());
        for session in [&tab, &theirs] {
            let names = RoomNames {
                session,
                artifact: &artifact,
                participant: &participant,
                epoch: &epoch,
            };
            let beat = rooms::heartbeat(
                &state,
                &other,
                names,
                Ok(RoomHeartbeatRequest {
                    after: 0,
                    state: None,
                }),
            )
            .await;
            assert!(
                matches!(&beat, Err(error) if error.status == StatusCode::NOT_FOUND),
                "{:?}",
                beat.map(|view| view.participant)
            );
        }
        // The other actor's judgement of a bad body is the judgement of a good
        // one on the owner's live connection: a connection that is not its
        // own tells it nothing about how a body was read.
        let same = |reference: &ApiError, other: &ApiError| {
            (reference.status, reference.body.code) == (other.status, other.body.code)
        };
        let reference = rooms::join(&state, &other, &tab, &artifact, Ok(join_request("page")))
            .await
            .err()
            .ok_or("the owner's connection must refuse another actor")?;
        assert_eq!(reference.status, StatusCode::NOT_FOUND);
        let refused = rooms::join(&state, &other, &tab, &artifact, bad())
            .await
            .err()
            .ok_or("must be refused")?;
        assert!(same(&reference, &refused), "join: {refused:?}");
        let names = RoomNames {
            session: &tab,
            artifact: &artifact,
            participant: &participant,
            epoch: &epoch,
        };
        let refused = rooms::heartbeat(&state, &other, names, bad())
            .await
            .err()
            .ok_or("must be refused")?;
        assert!(same(&reference, &refused), "heartbeat: {refused:?}");
        let refused = rooms::send(&state, &other, names, bad())
            .await
            .err()
            .ok_or("must be refused")?;
        assert!(same(&reference, &refused), "send: {refused:?}");
        // The owner's participant is untouched.
        let names = RoomNames {
            session: &tab,
            artifact: &artifact,
            participant: &participant,
            epoch: &epoch,
        };
        let beat = rooms::heartbeat(
            &state,
            &owner,
            names,
            Ok(RoomHeartbeatRequest {
                after: 0,
                state: None,
            }),
        )
        .await
        .map_err(|error| error.body.message)?;
        assert_eq!(beat.members.len(), 1);
        Ok::<(), Box<dyn Error>>(())
    })
}

#[test]
fn a_join_whose_connection_closes_during_the_artifact_lookup_leaves_no_participant()
-> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let artifact = create_runtime_artifact(&state, &actor).await?;
        let closing = operations::open_runtime_session(&state, &actor, None)
            .map_err(|error| error.body.message)?
            .session_id;
        let staying = operations::open_runtime_session(&state, &actor, None)
            .map_err(|error| error.body.message)?
            .session_id;
        let (release, blocker) = hold_blocking_thread().await?;
        let mut join = Box::pin(rooms::join(
            &state,
            &actor,
            &closing,
            &artifact,
            Ok(join_request("page")),
        ));
        let pending = poll_fn(|context| match join.as_mut().poll(context) {
            Poll::Pending => Poll::Ready(true),
            Poll::Ready(_) => Poll::Ready(false),
        })
        .await;
        assert!(pending, "the artifact lookup must be queued before close");
        operations::close_runtime_session(&state, &actor, &closing)
            .map_err(|error| error.body.message)?;
        release.send(())?;
        let joined = join.await;
        assert!(
            matches!(&joined, Err(error) if error.status == StatusCode::NOT_FOUND),
            "{:?}",
            joined.map(|view| view.participant)
        );
        blocker.await?;
        // The room holds nobody from the closed connection.
        let view = rooms::join(
            &state,
            &actor,
            &staying,
            &artifact,
            Ok(join_request("page")),
        )
        .await
        .map_err(|error| error.body.message)?;
        assert_eq!(view.members.len(), 1);
        Ok::<(), Box<dyn Error>>(())
    })
}

#[test]
fn revoking_the_token_of_a_connection_ends_what_its_participants_can_do()
-> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let token = artifree_domain::identity::TokenId::from_random([9; 16]);
        let artifact = create_runtime_artifact(&state, &actor).await?;
        let tab = operations::open_runtime_session(&state, &actor, Some(token.clone()))
            .map_err(|error| error.body.message)?
            .session_id;
        let mine = rooms::join(&state, &actor, &tab, &artifact, Ok(join_request("page")))
            .await
            .map_err(|error| error.body.message)?;
        state.runtime_sessions().revoke_token(&token);
        let (participant, epoch) = (mine.participant.to_string(), mine.epoch.to_string());
        let names = RoomNames {
            session: &tab,
            artifact: &artifact,
            participant: &participant,
            epoch: &epoch,
        };
        let beat = rooms::heartbeat(
            &state,
            &actor,
            names,
            Ok(RoomHeartbeatRequest {
                after: 0,
                state: None,
            }),
        )
        .await;
        assert!(
            matches!(&beat, Err(error) if error.status == StatusCode::NOT_FOUND
                && error.body.code == artifree_contracts::api::ErrorCode::RuntimeSessionExpired),
            "{:?}",
            beat.map(|view| view.participant)
        );
        let joined = rooms::join(&state, &actor, &tab, &artifact, Ok(join_request("page"))).await;
        assert!(matches!(&joined, Err(error) if error.status == StatusCode::NOT_FOUND));
        Ok::<(), Box<dyn Error>>(())
    })
}

fn registered_user(state: &AppState, seed: u8) -> Result<ActorId, Box<dyn Error>> {
    let id = ActorId::user([seed; 16]);
    state
        .token_store()
        .register(&NewUser {
            id: id.clone(),
            label: format!("user {seed}").parse()?,
            created_at: "2026-01-01T00:00:00Z".into(),
        })
        .map_err(|error| error.to_string())?;
    Ok(id)
}

/// A connection, a room, a preview ticket and an inbox of `actor`.
async fn hold_things(state: &AppState, actor: &ActorId) -> Result<String, Box<dyn Error>> {
    let artifact = create_runtime_artifact(state, actor).await?;
    let session = operations::open_runtime_session(state, actor, None)
        .map_err(|error| error.body.message)?
        .session_id;
    rooms::join(state, actor, &session, &artifact, Ok(join_request("page")))
        .await
        .map_err(|error| error.body.message)?;
    operations::preview(state, actor.clone(), None, &artifact, 1)
        .await
        .map_err(|error| error.body.message)?;
    operations::register_handoff_session(state, actor, "Editor")
        .map_err(|error| error.body.message)?;
    Ok(session)
}

#[test]
fn disabling_a_user_closes_what_it_holds_and_nothing_of_anothers() -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let (alice, bob) = (registered_user(&state, 1)?, registered_user(&state, 2)?);
        let alices = hold_things(&state, &alice).await?;
        let bobs = hold_things(&state, &bob).await?;
        assert_eq!(state.previews().live_tickets(&alice), 1);
        assert_eq!(state.rooms().counts().map(|counts| counts.rooms), Some(2));

        state.revoke_actor(&alice);

        // The connection is forgotten, so it no longer counts against the
        // user's bound after the user is enabled again.
        assert!(matches!(
            state.runtime_sessions().get(&alice, &alices),
            Err(crate::runtime_sessions::SessionError::Missing)
        ));
        assert_eq!(state.runtime_sessions().held_by(&alice), 0);
        assert!(state.runtime_sessions().get(&bob, &bobs).is_ok());
        assert_eq!(state.previews().live_tickets(&alice), 0);
        assert_eq!(state.previews().live_tickets(&bob), 1);
        assert_eq!(state.rooms().counts().map(|counts| counts.rooms), Some(1));
        let inbox = |actor: &ActorId| state.handoffs().list(actor).map(|agents| agents.len());
        assert_eq!(inbox(&alice).map_err(|error| format!("{error:?}"))?, 0);
        assert_eq!(inbox(&bob).map_err(|error| format!("{error:?}"))?, 1);
        Ok::<(), Box<dyn Error>>(())
    })
}

#[test]
fn a_request_that_authenticated_before_a_disable_leaves_nothing_registered()
-> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let alice = registered_user(&state, 1)?;
        let artifact = create_runtime_artifact(&state, &alice).await?;
        let session = operations::open_runtime_session(&state, &alice, None)
            .map_err(|error| error.body.message)?
            .session_id;
        operations::set_runtime_grant(
            &state,
            alice.clone(),
            &session,
            &artifact,
            1,
            SetRuntimeGrantRequest {
                head_revision: 1,
                expected_version: 0,
                capabilities: vec!["metadata.read".into()],
            },
        )
        .await
        .map_err(|error| error.body.message)?;
        // The registry changes between the moment the requests below
        // authenticated and the moment they register: the disabling's own
        // cleanup (`revoke_actor`) has not run.
        state
            .token_store()
            .set_disabled(&alice, Some("2026-01-02T00:00:00Z"))
            .map_err(|error| error.to_string())?;

        let view = operations::create_runtime_view(
            &state,
            alice.clone(),
            None,
            &artifact,
            1,
            CreateRuntimeViewRequest {
                session_id: session.clone(),
                view_id: "a".repeat(32),
                generation: 1,
            },
        )
        .await;
        assert!(
            matches!(&view, Err(error) if error.status == StatusCode::UNAUTHORIZED),
            "{:?}",
            view.map(|view| view.url)
        );
        assert_eq!(state.previews().live_tickets(&alice), 0, "a view was kept");
        let preview = operations::preview(&state, alice.clone(), None, &artifact, 1).await;
        assert!(
            matches!(&preview, Err(error) if error.status == StatusCode::UNAUTHORIZED),
            "{:?}",
            preview.map(|ticket| ticket.url)
        );
        assert_eq!(
            state.previews().live_tickets(&alice),
            0,
            "a ticket was kept"
        );
        assert!(
            matches!(
                state.runtime_sessions().get(&alice, &session),
                Err(crate::runtime_sessions::SessionError::Missing)
            ),
            "the late request did not close the connection"
        );
        Ok::<(), Box<dyn Error>>(())
    })
}

#[test]
fn a_connection_or_inbox_registered_after_a_disable_does_not_come_back_with_the_user()
-> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let alice = registered_user(&state, 1)?;
        // The registry changes after the two requests below authenticated,
        // and the disabling's own cleanup (`revoke_actor`) ran before they
        // register: nothing but the confirmation can undo what they leave.
        state
            .token_store()
            .set_disabled(&alice, Some("2026-01-02T00:00:00Z"))
            .map_err(|error| error.to_string())?;

        let connection = operations::open_confirmed_runtime_session(&state, &alice, None).await;
        assert!(
            matches!(&connection, Err(error) if error.status == StatusCode::UNAUTHORIZED),
            "{:?}",
            connection.map(|opened| opened.session_id)
        );
        let inbox = operations::register_confirmed_handoff_session(&state, &alice, "Editor").await;
        assert!(
            matches!(&inbox, Err(error) if error.status == StatusCode::UNAUTHORIZED),
            "{:?}",
            inbox.map(|registered| registered.session_id)
        );

        // Once the user is enabled again nothing of the late requests is there.
        state
            .token_store()
            .set_disabled(&alice, None)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            state.runtime_sessions().held_by(&alice),
            0,
            "a connection outlived the disable"
        );
        let inboxes = state
            .handoffs()
            .list(&alice)
            .map_err(|error| format!("{error:?}"))?;
        assert!(inboxes.is_empty(), "an inbox outlived the disable");
        Ok::<(), Box<dyn Error>>(())
    })
}

#[test]
fn a_join_that_passed_its_lookup_before_a_disable_leaves_no_participant()
-> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(async {
        let alice = registered_user(&state, 1)?;
        let artifact = create_runtime_artifact(&state, &alice).await?;
        let session = operations::open_runtime_session(&state, &alice, None)
            .map_err(|error| error.body.message)?
            .session_id;
        let (release, blocker) = hold_blocking_thread().await?;
        let mut join = Box::pin(rooms::join(
            &state,
            &alice,
            &session,
            &artifact,
            Ok(join_request("page")),
        ));
        let pending = poll_fn(|context| match join.as_mut().poll(context) {
            Poll::Pending => Poll::Ready(true),
            Poll::Ready(_) => Poll::Ready(false),
        })
        .await;
        assert!(
            pending,
            "the artifact lookup must be queued before the disable"
        );
        // The join has its connection and waits for the artifact; the user is
        // disabled with everything the server holds for it, rooms included.
        state
            .token_store()
            .set_disabled(&alice, Some("2026-01-02T00:00:00Z"))
            .map_err(|error| error.to_string())?;
        state.revoke_actor(&alice);
        release.send(())?;
        let joined = join.await;
        assert!(
            matches!(&joined, Err(error) if error.status == StatusCode::NOT_FOUND),
            "{:?}",
            joined.map(|view| view.participant)
        );
        blocker.await?;
        // Nothing of the user is in a room, now or after it is enabled again.
        state
            .token_store()
            .set_disabled(&alice, None)
            .map_err(|error| error.to_string())?;
        let counts = state.rooms().counts();
        assert_eq!(
            counts.map(|counts| (counts.rooms, counts.participants)),
            Some((0, 0)),
            "a participant outlived the disable"
        );
        Ok::<(), Box<dyn Error>>(())
    })
}

#[test]
fn only_a_registered_enabled_user_or_the_owner_passes_the_confirmation()
-> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let alice = registered_user(&state, 1)?;
        assert!(state.ensure_enabled(&ActorId::owner()).await.is_ok());
        assert!(state.ensure_enabled(&alice).await.is_ok());
        let stranger = ActorId::user([9; 16]);
        assert!(matches!(
            state.ensure_enabled(&stranger).await,
            Err(error) if error.status == StatusCode::UNAUTHORIZED
        ));
        state
            .token_store()
            .set_disabled(&alice, Some("2026-01-02T00:00:00Z"))
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            state.ensure_enabled(&alice).await,
            Err(error) if error.status == StatusCode::UNAUTHORIZED
        ));
        Ok::<(), Box<dyn Error>>(())
    })
}

#[test]
fn a_stream_for_a_user_disabled_meanwhile_is_refused_and_one_for_an_enabled_user_opens()
-> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let alice = registered_user(&state, 1)?;
        let caller = || crate::auth::AuthContext {
            actor: alice.clone(),
            token_id: "tok_0123456789abcdef0123456789abcdef".parse().ok(),
            is_admin: false,
        };
        let open = changes::stream(
            axum::extract::State(state.clone()),
            caller(),
            axum::http::HeaderMap::new(),
        )
        .await
        .map_err(|error| error.body.message)?;
        assert_eq!(open.status(), StatusCode::OK);
        drop(open);

        // Disabled in the registry after this request authenticated.
        state
            .token_store()
            .set_disabled(&alice, Some("2026-01-02T00:00:00Z"))
            .map_err(|error| error.to_string())?;
        let refused = changes::stream(
            axum::extract::State(state.clone()),
            caller(),
            axum::http::HeaderMap::new(),
        )
        .await;
        assert!(
            matches!(&refused, Err(error) if error.status == StatusCode::UNAUTHORIZED),
            "{:?}",
            refused.map(|response| response.status())
        );
        Ok::<(), Box<dyn Error>>(())
    })
}
