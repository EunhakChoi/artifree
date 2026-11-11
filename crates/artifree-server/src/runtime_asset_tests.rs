//! Asset reads and listings of runtime frames, each raced against one action
//! in the window between the store read and the reply (decision 0045, rule 12).
//!
//! A race holds the blocking thread, so the frame's store calls wait where the
//! test wants them. The frame must then release neither bytes nor a listing:
//! a deleted asset, a withdrawn grant, a revoked connection or a moved head
//! each turn the reply into a refusal.

use std::error::Error;
use std::future::{Future, poll_fn};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::task::Poll;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use artifree_contracts::api::{
    ContentEncoding, CreateArtifactRequest, CreateRuntimeViewRequest, DeleteAssetRequest,
    ErrorCode, FileChange, FileUpload, RuntimeAssetReadRequest, RuntimeView, SaveRevisionRequest,
    SetRuntimeGrantRequest, UploadAssetRequest,
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
use crate::runtime_assets;
use crate::runtime_source::FrameCall;
use crate::{changes, preview};

/// A 1x1 PNG with a text chunk and an unknown ancillary chunk, stored as sent.
const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAACnRFWHRDb21tZW50AGhpoqJYZgAAAAJwclZ0enrhbIZhAAAADUlEQVR4nGNgaPjPAAADgwGAJwGhsAAAAABJRU5ErkJggg==";
/// SHA-256 of the 106 decoded bytes of [`PNG`], computed outside the server.
const PNG_SHA256: &str = "3e2841980d3e451678e92cae6fdb667d3d4f3cdec9795fccf19005a3f688fbf7";

/// A fresh directory for one test's `SQLite` files, removed when the test ends.
struct TestDirectory(PathBuf);

/// Keeps the directories of tests that start in the same nanosecond apart.
static DIRECTORIES: AtomicU64 = AtomicU64::new(0);

impl TestDirectory {
    fn new() -> Result<Self, Box<dyn Error>> {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let count = DIRECTORIES.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "artifree-asset-race-{}-{nonce}-{count}",
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
    let store = SqliteStore::open(&directory.0.join("assets.sqlite3"))?;
    let token_store = SqliteTokenStore::open(&directory.0.join("assets.sqlite3"))?;
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
        format!("asset-race-save-{base}").parse()?,
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

/// An artifact whose manifest declares `assets.read`, holding one stored PNG,
/// with a connection granted that capability and a view pinned to revision 2,
/// the head after the manifest. Returns the view, the connection and the
/// asset's identifier.
async fn granted_view(
    state: &AppState,
    actor: &ActorId,
) -> Result<(RuntimeView, String, String), Box<dyn Error>> {
    let created = operations::create(
        state,
        actor.clone(),
        "asset-race-artifact".parse()?,
        CreateArtifactRequest {
            title: "Asset race".into(),
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
        r#"{"sdk":"artifree.app.v1","capabilities":["assets.read"]}"#,
    )
    .await?;
    let uploaded = operations::upload_asset(
        state,
        actor.clone(),
        "asset-race-upload".parse()?,
        &artifact,
        UploadAssetRequest {
            base_asset_version: 0,
            content_type: "image/png".into(),
            content_base64: PNG.into(),
        },
    )
    .await
    .map_err(|error| error.body.message)?;
    let asset = uploaded.asset.id;
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
            capabilities: vec!["assets.read".into()],
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
    assert_eq!(view.granted, ["assets.read"]);
    let path = view
        .url
        .strip_prefix("http://127.0.0.1:7411")
        .ok_or("wrong preview origin")?;
    let response = preview::router(state.clone())
        .oneshot(Request::get(path).body(Body::empty())?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    Ok((view, session, asset))
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

/// The first 1,024 bytes of `asset`, asked for through `view`.
fn read_request(view: &RuntimeView, asset: &str) -> RuntimeAssetReadRequest {
    RuntimeAssetReadRequest {
        initialization: view.initialization.clone(),
        asset_id: asset.into(),
        offset: 0,
        length: 1024,
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

/// Drives `request` past its final check and holds it at its association
/// lookup, its last store read. The lookup waits behind a blocking thread the
/// test holds, and the caller releases it with the returned sender. Anything
/// queued behind the lookup runs after the lookup has read the store.
async fn pause_at_the_association_lookup<T>(
    request: &mut Pin<Box<impl Future<Output = T>>>,
) -> Result<(mpsc::Sender<()>, tokio::task::JoinHandle<()>), Box<dyn Error>> {
    pause_after_the_store_read(request).await?;
    let (release, blocker) = hold_blocking_thread().await?;
    assert!(
        pends(request).await,
        "the final check's lookup must wait behind the blocker"
    );
    release.send(())?;
    blocker.await?;
    settle().await?;
    let (release, blocker) = hold_blocking_thread().await?;
    assert!(
        pends(request).await,
        "the association lookup must wait behind the blocker"
    );
    Ok((release, blocker))
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

/// The read that nothing races returns the stored bytes. The refusals in the
/// tests are only meaningful if this holds, because a view that was never
/// granted would refuse them just the same.
async fn assert_unraced_read(
    state: &AppState,
    call: &FrameCall<'_>,
    view: &RuntimeView,
    asset: &str,
) -> Result<(), Box<dyn Error>> {
    let part = runtime_assets::read(state, call, &read_request(view, asset))
        .await
        .map_err(|error| {
            format!(
                "the unraced read was refused: {} {}",
                error.status, error.body.message
            )
        })?;
    assert_eq!(part.content, PNG);
    assert_eq!(part.sha256, PNG_SHA256);
    assert_eq!(part.size, 106);
    assert_eq!(part.next_offset, None);
    Ok(())
}

/// The listing that nothing races names the one asset, at asset version 1.
async fn assert_unraced_listing(
    state: &AppState,
    call: &FrameCall<'_>,
    asset: &str,
) -> Result<(), Box<dyn Error>> {
    let listing = runtime_assets::list(state, call).await.map_err(|error| {
        format!(
            "the unraced listing was refused: {} {}",
            error.status, error.body.message
        )
    })?;
    assert_eq!(listing.asset_version, 1);
    let ids: Vec<&str> = listing
        .assets
        .iter()
        .map(|entry| entry.id.as_str())
        .collect();
    assert_eq!(ids, [asset]);
    Ok(())
}

#[test]
fn a_read_pending_on_a_deleted_asset_releases_no_bytes() -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let (view, _session, asset) = granted_view(&state, &actor).await?;
        let call = frame_call(&actor, &view);
        assert_unraced_read(&state, &call, &view, &asset).await?;
        let read = read_request(&view, &asset);
        let mut request = Box::pin(runtime_assets::read(&state, &call, &read));
        // The bytes are read here; the association check has not run yet.
        pause_after_the_store_read(&mut request).await?;
        let deleted = operations::delete_asset(
            &state,
            actor.clone(),
            "asset-race-delete".parse()?,
            &view.artifact_id,
            &asset,
            DeleteAssetRequest {
                base_asset_version: 1,
            },
        )
        .await
        .map_err(|error| error.body.message)?;
        assert!(deleted.deleted, "the fixture must delete the asset");
        let error = refusal(request.await, StatusCode::NOT_FOUND)?;
        assert_eq!(error.body.message, "asset not found");
        Ok::<(), Box<dyn Error>>(())
    })
}

#[test]
fn a_read_whose_grant_is_withdrawn_before_the_reply_releases_no_bytes() -> Result<(), Box<dyn Error>>
{
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let (view, session, asset) = granted_view(&state, &actor).await?;
        let call = frame_call(&actor, &view);
        assert_unraced_read(&state, &call, &view, &asset).await?;
        let read = read_request(&view, &asset);
        let mut request = Box::pin(runtime_assets::read(&state, &call, &read));
        pause_after_the_store_read(&mut request).await?;
        withdraw_grant(&state, &actor, &session, &view).await?;
        refusal(request.await, StatusCode::FORBIDDEN)?;
        Ok::<(), Box<dyn Error>>(())
    })
}

#[test]
fn a_read_whose_connection_is_revoked_before_the_reply_releases_no_bytes()
-> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let (view, _session, asset) = granted_view(&state, &actor).await?;
        let call = frame_call(&actor, &view);
        assert_unraced_read(&state, &call, &view, &asset).await?;
        let read = read_request(&view, &asset);
        let mut request = Box::pin(runtime_assets::read(&state, &call, &read));
        pause_after_the_store_read(&mut request).await?;
        state.revoke_actor(&actor);
        refusal(request.await, StatusCode::NOT_FOUND)?;
        Ok::<(), Box<dyn Error>>(())
    })
}

#[test]
fn a_read_whose_head_moves_before_the_reply_releases_no_bytes() -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let (view, _session, asset) = granted_view(&state, &actor).await?;
        let call = frame_call(&actor, &view);
        assert_unraced_read(&state, &call, &view, &asset).await?;
        let read = read_request(&view, &asset);
        let mut request = Box::pin(runtime_assets::read(&state, &call, &read));
        pause_after_the_store_read(&mut request).await?;
        save(&state, &actor, &view.artifact_id, 2, "note.txt", "moved").await?;
        let error = refusal(request.await, StatusCode::CONFLICT)?;
        assert_eq!(error.body.code, ErrorCode::RevisionConflict);
        assert_eq!(
            error.body.current_revision,
            Some(3),
            "the head must have moved to revision 3"
        );
        Ok::<(), Box<dyn Error>>(())
    })
}

#[test]
fn a_listing_whose_grant_is_withdrawn_before_the_reply_releases_no_listing()
-> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let (view, session, asset) = granted_view(&state, &actor).await?;
        let call = frame_call(&actor, &view);
        assert_unraced_listing(&state, &call, &asset).await?;
        let mut request = Box::pin(runtime_assets::list(&state, &call));
        pause_after_the_store_read(&mut request).await?;
        withdraw_grant(&state, &actor, &session, &view).await?;
        refusal(request.await, StatusCode::FORBIDDEN)?;
        Ok::<(), Box<dyn Error>>(())
    })
}

#[test]
fn a_read_whose_asset_is_deleted_during_the_final_check_releases_no_bytes()
-> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let (view, _session, asset) = granted_view(&state, &actor).await?;
        let call = frame_call(&actor, &view);
        assert_unraced_read(&state, &call, &view, &asset).await?;
        let read = read_request(&view, &asset);
        let mut request = Box::pin(runtime_assets::read(&state, &call, &read));
        // The bytes are read here; the final check has not run yet.
        pause_after_the_store_read(&mut request).await?;
        // The final check's artifact lookup waits behind a blocking thread the
        // test holds. The deletion waits behind that lookup, so it commits after
        // the lookup and before the association lookup runs.
        let (release, blocker) = hold_blocking_thread().await?;
        assert!(
            pends(&mut request).await,
            "the final check's lookup must wait behind the blocker"
        );
        let mut deletion = Box::pin(operations::delete_asset(
            &state,
            actor.clone(),
            "asset-race-delete".parse()?,
            &view.artifact_id,
            &asset,
            DeleteAssetRequest {
                base_asset_version: 1,
            },
        ));
        assert!(
            pends(&mut deletion).await,
            "the deletion must wait behind the final check's lookup"
        );
        release.send(())?;
        blocker.await?;
        let deleted = deletion.await.map_err(|error| error.body.message)?;
        assert!(deleted.deleted, "the fixture must delete the asset");
        let error = refusal(request.await, StatusCode::NOT_FOUND)?;
        assert_eq!(error.body.message, "asset not found");
        Ok::<(), Box<dyn Error>>(())
    })
}

#[test]
fn a_read_whose_grant_is_withdrawn_during_the_association_lookup_releases_no_bytes()
-> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let (view, session, asset) = granted_view(&state, &actor).await?;
        let call = frame_call(&actor, &view);
        assert_unraced_read(&state, &call, &view, &asset).await?;
        let read = read_request(&view, &asset);
        let mut request = Box::pin(runtime_assets::read(&state, &call, &read));
        let (release, blocker) = pause_at_the_association_lookup(&mut request).await?;
        // The withdrawal waits behind the lookup, so the lookup still finds the
        // asset and only the synchronous check can refuse the read.
        let mut withdrawal = Box::pin(withdraw_grant(&state, &actor, &session, &view));
        assert!(
            pends(&mut withdrawal).await,
            "the withdrawal must wait behind the association lookup"
        );
        release.send(())?;
        blocker.await?;
        withdrawal.await?;
        refusal(request.await, StatusCode::FORBIDDEN)?;
        Ok::<(), Box<dyn Error>>(())
    })
}

#[test]
fn a_read_whose_connection_is_revoked_during_the_association_lookup_releases_no_bytes()
-> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let (view, _session, asset) = granted_view(&state, &actor).await?;
        let call = frame_call(&actor, &view);
        assert_unraced_read(&state, &call, &view, &asset).await?;
        let read = read_request(&view, &asset);
        let mut request = Box::pin(runtime_assets::read(&state, &call, &read));
        let (release, blocker) = pause_at_the_association_lookup(&mut request).await?;
        // The revocation lands while the lookup waits. The lookup does not check
        // the connection, so only the synchronous check can refuse the read.
        state.revoke_actor(&actor);
        release.send(())?;
        blocker.await?;
        refusal(request.await, StatusCode::NOT_FOUND)?;
        Ok::<(), Box<dyn Error>>(())
    })
}

#[test]
fn a_read_whose_head_moves_during_the_association_lookup_still_returns_its_bytes()
-> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let (view, _session, asset) = granted_view(&state, &actor).await?;
        let call = frame_call(&actor, &view);
        assert_unraced_read(&state, &call, &view, &asset).await?;
        let read = read_request(&view, &asset);
        let mut request = Box::pin(runtime_assets::read(&state, &call, &read));
        let (release, blocker) = pause_at_the_association_lookup(&mut request).await?;
        // The save waits behind the lookup and commits after it. Assets do not
        // depend on the source head, so the synchronous check does not refuse.
        let mut saving = Box::pin(save(
            &state,
            &actor,
            &view.artifact_id,
            2,
            "note.txt",
            "moved",
        ));
        assert!(
            pends(&mut saving).await,
            "the save must wait behind the association lookup"
        );
        release.send(())?;
        blocker.await?;
        saving.await?;
        let part = request.await.map_err(|error| {
            format!(
                "the read that raced a save was refused: {} {}",
                error.status, error.body.message
            )
        })?;
        assert_eq!(part.content, PNG);
        assert_eq!(part.sha256, PNG_SHA256);
        assert_eq!(part.size, 106);
        // The head did move, so a read that starts now is refused as stale.
        let conflict = refusal(
            runtime_assets::read(&state, &call, &read).await,
            StatusCode::CONFLICT,
        )?;
        assert_eq!(
            conflict.body.current_revision,
            Some(3),
            "the head must have moved to revision 3"
        );
        Ok::<(), Box<dyn Error>>(())
    })
}

#[test]
fn a_read_whose_asset_is_deleted_during_the_association_lookup_still_returns_its_bytes()
-> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let (view, _session, asset) = granted_view(&state, &actor).await?;
        let call = frame_call(&actor, &view);
        assert_unraced_read(&state, &call, &view, &asset).await?;
        let read = read_request(&view, &asset);
        let mut request = Box::pin(runtime_assets::read(&state, &call, &read));
        let (release, blocker) = pause_at_the_association_lookup(&mut request).await?;
        // The deletion waits behind the lookup, which has already read the asset,
        // so the read is ordered before the deletion and returns the bytes.
        let mut deletion = Box::pin(operations::delete_asset(
            &state,
            actor.clone(),
            "asset-race-delete".parse()?,
            &view.artifact_id,
            &asset,
            DeleteAssetRequest {
                base_asset_version: 1,
            },
        ));
        assert!(
            pends(&mut deletion).await,
            "the deletion must wait behind the association lookup"
        );
        release.send(())?;
        blocker.await?;
        let deleted = deletion.await.map_err(|error| error.body.message)?;
        assert!(deleted.deleted, "the fixture must delete the asset");
        let part = request.await.map_err(|error| {
            format!(
                "the read that raced a deletion was refused: {} {}",
                error.status, error.body.message
            )
        })?;
        assert_eq!(part.content, PNG);
        assert_eq!(part.sha256, PNG_SHA256);
        assert_eq!(part.size, 106);
        Ok::<(), Box<dyn Error>>(())
    })
}
