//! Runtime view races across the real registry and artifact service.

use std::error::Error;
use std::future::{Future, poll_fn};
use std::path::PathBuf;
use std::sync::mpsc;
use std::task::Poll;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use artifree_contracts::api::{
    ContentEncoding, CreateArtifactRequest, CreateRuntimeViewRequest, FileChange, FileUpload,
    RuntimeView, SaveRevisionRequest, SetRuntimeGrantRequest, StageHandoffRequest,
};
use artifree_domain::SourceRevision;
use artifree_domain::identity::{ActorId, ArtifactId};
use artifree_providers::storage::SqliteStore;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crate::auth::AccessToken;
use crate::http::{AppState, Settings};
use crate::limits::RequestLimits;
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
    let token = AccessToken::parse(&"0".repeat(64)).ok_or("invalid fixture token")?;
    Ok(AppState::new(
        store,
        token,
        Settings {
            content_origin: "http://127.0.0.1:7411".into(),
            preview_ttl,
            frame_ancestors: "http://127.0.0.1:*".into(),
            limits: RequestLimits::default(),
            change_streams: changes::DEFAULT_STREAMS_PER_ACTOR,
        },
    )?)
}

async fn initialized_view(
    state: &AppState,
    actor: &ActorId,
) -> Result<(RuntimeView, String), Box<dyn Error>> {
    let artifact_id = create_runtime_artifact(state, actor).await?;
    let session_id = operations::open_runtime_session(state, actor)
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
    let session = operations::open_runtime_session(&state, &actor)
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
    assert_eq!(declared.supported, ["metadata.read"]);
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
    assert_eq!(omitted.supported, ["metadata.read"]);
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
    let session = operations::open_runtime_session(&state, &actor)
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
            .set_grant(scope, false, view.grant_version)
            .map_err(|error| format!("{error:?}"))?;
        release.send(())?;
        assert!(matches!(request.await, Err(error) if error.status == StatusCode::NOT_FOUND));
        blocker.await?;
        Ok::<(), Box<dyn Error>>(())
    })?;
    Ok(())
}

#[test]
fn logout_while_handoff_source_waits_rejects_its_late_delivery() -> Result<(), Box<dyn Error>> {
    handoff_source_recheck(true)
}

#[test]
fn acknowledgement_while_handoff_source_waits_stops_its_replay() -> Result<(), Box<dyn Error>> {
    handoff_source_recheck(false)
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
        let artifact = create_runtime_artifact(&state, &actor).await?;
        let revision = operations::revision(&state, actor.clone(), &artifact, 1)
            .await
            .map_err(|error| error.body.message)?;
        let workspace = operations::open_runtime_session(&state, &actor)
            .map_err(|error| error.body.message)?
            .session_id;
        let agent = operations::register_handoff_session(&state, &actor, "Editor")
            .map_err(|error| error.body.message)?;
        let staged = operations::stage_handoff(
            &state,
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
        operations::send_handoff(&state, actor.clone(), &workspace, &staged.id)
            .await
            .map_err(|error| error.body.message)?;
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
        let session = operations::open_runtime_session(&state, &actor)
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
        let replacement = state.previews().issue_runtime(
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
        let session_id = operations::open_runtime_session(&state, &actor)
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
