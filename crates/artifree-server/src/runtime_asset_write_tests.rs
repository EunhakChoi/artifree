//! Runtime asset writes that race a source save or a grant change (decision
//! 0045, rules 12 and 13).
//!
//! An upload or a deletion is judged when its frame call starts, again on the
//! blocking task that commits it just before the store is asked, and once more
//! after the commit. These tests hold the write on that blocking task, so the
//! race lands where the test wants it. A source save that moved the head, or a
//! grant withdrawn while the write waited, leaves no association, version,
//! receipt or asset identifier behind. A withdrawal after the commit withholds
//! only the reply, and the association stays.

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
    FileUpload, RuntimeAssetDeleteRequest, RuntimeAssetUploadRequest, RuntimeView,
    SaveRevisionRequest, SetRuntimeGrantRequest, UploadAssetRequest,
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
            "artifree-asset-write-{}-{nonce}-{count}",
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
        format!("asset-write-save-{base}").parse()?,
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

/// An artifact whose manifest declares `assets.write`. Its head is revision 2,
/// the one after the manifest, and it holds no assets. Returns its identifier.
async fn declared_artifact(state: &AppState, actor: &ActorId) -> Result<String, Box<dyn Error>> {
    let created = operations::create(
        state,
        actor.clone(),
        "asset-write-artifact".parse()?,
        CreateArtifactRequest {
            title: "Asset write".into(),
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
        r#"{"sdk":"artifree.app.v1","capabilities":["assets.write"]}"#,
    )
    .await?;
    Ok(artifact)
}

/// Uploads the stored PNG as the owner, at asset version 0, and returns the
/// identifier of the asset it became.
async fn owner_upload(
    state: &AppState,
    actor: &ActorId,
    artifact: &str,
) -> Result<String, Box<dyn Error>> {
    let uploaded = operations::upload_asset(
        state,
        actor.clone(),
        "asset-write-owner".parse()?,
        artifact,
        UploadAssetRequest {
            base_asset_version: 0,
            content_type: "image/png".into(),
            content_base64: PNG.into(),
        },
    )
    .await
    .map_err(|error| error.body.message)?;
    Ok(uploaded.asset.id)
}

/// Opens a connection on `artifact`, grants it `assets.write` and creates a
/// view pinned to revision 2. The view's bootstrap is served, as the frame
/// would serve it, so the view is live. Returns the view and the connection's
/// session identifier.
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
            capabilities: vec!["assets.write".into()],
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
            adopt: None,
        },
    )
    .await
    .map_err(|error| error.body.message)?;
    assert_eq!(view.granted, ["assets.write"]);
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

/// The frame's upload of the stored PNG under the retry identity `retry`, at
/// asset version `base`.
fn upload_request(view: &RuntimeView, retry: &str, base: u64) -> RuntimeAssetUploadRequest {
    RuntimeAssetUploadRequest {
        initialization: view.initialization.clone(),
        retry_id: retry.into(),
        base_asset_version: base,
        content_type: "image/png".into(),
        content_base64: PNG.into(),
    }
}

/// The frame's deletion of `asset` under the retry identity `retry`, at asset
/// version `base`.
fn delete_request(
    view: &RuntimeView,
    retry: &str,
    base: u64,
    asset: &str,
) -> RuntimeAssetDeleteRequest {
    RuntimeAssetDeleteRequest {
        initialization: view.initialization.clone(),
        retry_id: retry.into(),
        base_asset_version: base,
        asset_id: asset.into(),
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

/// Drives `request` past its artifact lookup and holds it where its commit
/// waits behind a blocking thread the test holds. The lookup waits behind a
/// first blocking thread, which the test releases once the lookup has run. The
/// caller releases the returned blocker, which lets the commit run.
async fn pause_before_the_commit<T>(
    request: &mut Pin<Box<impl Future<Output = T>>>,
) -> Result<(mpsc::Sender<()>, tokio::task::JoinHandle<()>), Box<dyn Error>> {
    let (release, blocker) = hold_blocking_thread().await?;
    assert!(
        pends(request).await,
        "the artifact lookup must wait behind the blocker"
    );
    release.send(())?;
    blocker.await?;
    settle().await?;
    let (release, blocker) = hold_blocking_thread().await?;
    assert!(
        pends(request).await,
        "the commit must wait behind the blocker"
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

/// Saves `note.txt` at revision `base` of `artifact` through a second
/// connection to the database file. The owner's store is not used, so the save
/// runs on the test thread while the blocking thread is held.
fn save_on_second_connection(
    directory: &TestDirectory,
    actor: &ActorId,
    artifact: &str,
    base: u64,
) -> Result<(), Box<dyn Error>> {
    let service = ArtifactService::new(SqliteStore::open(&directory.0.join("assets.sqlite3"))?);
    let artifact = operations::artifact_id(artifact).map_err(|error| error.body.message)?;
    service.save(
        actor,
        &format!("asset-write-race-{base}").parse()?,
        &artifact,
        SourceRevision(base),
        vec![artifree_domain::package::FileChange::Put {
            path: "note.txt".parse()?,
            content: b"moved".to_vec(),
        }],
    )?;
    Ok(())
}

/// The rows of the asset tables, counted on a connection of the test's own, so
/// that a write which committed anything changes one of them.
#[derive(Debug, PartialEq, Eq)]
struct AssetRows {
    associations: i64,
    receipts: i64,
    spaces: i64,
    ids: i64,
}

fn asset_rows(directory: &TestDirectory) -> Result<AssetRows, Box<dyn Error>> {
    let connection = rusqlite::Connection::open(directory.0.join("assets.sqlite3"))?;
    let count =
        |sql: &str| -> rusqlite::Result<i64> { connection.query_row(sql, [], |row| row.get(0)) };
    Ok(AssetRows {
        associations: count("SELECT COUNT(*) FROM assets")?,
        receipts: count("SELECT COUNT(*) FROM asset_requests")?,
        spaces: count("SELECT COUNT(*) FROM asset_spaces")?,
        ids: count("SELECT COUNT(*) FROM asset_ids")?,
    })
}

/// The owner's listing of `artifact` is at asset version `version` and lists
/// exactly the assets with the SHA-256 digests `digests`.
async fn assert_listing(
    state: &AppState,
    actor: &ActorId,
    artifact: &str,
    version: u64,
    digests: &[&str],
) -> Result<(), Box<dyn Error>> {
    let listing = operations::list_assets(state, actor.clone(), artifact)
        .await
        .map_err(|error| error.body.message)?;
    assert_eq!(
        listing.asset_version, version,
        "the asset version must be {version}"
    );
    let listed: Vec<&str> = listing
        .assets
        .iter()
        .map(|entry| entry.sha256.as_str())
        .collect();
    assert_eq!(listed, digests, "the listed digests must be {digests:?}");
    Ok(())
}

/// A source save that moves the head after an upload's checks, and before its
/// commit, refuses the commit: `409 revision_conflict` with the new head, and no
/// association, version, receipt or identifier is added.
#[test]
fn a_queued_upload_whose_source_moves_before_its_commit_commits_nothing()
-> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let artifact = declared_artifact(&state, &actor).await?;
        let (view, _session) = granted_view(&state, &actor, &artifact).await?;
        let call = frame_call(&actor, &view);
        let rows_before = asset_rows(&directory)?;
        let request = upload_request(&view, "upload-race-1", 0);
        let mut upload = Box::pin(runtime_assets::upload(&state, &call, &request));
        let (release, blocker) = pause_before_the_commit(&mut upload).await?;
        // The source moves through a second connection while the commit waits.
        save_on_second_connection(&directory, &actor, &artifact, 2)?;
        release.send(())?;
        blocker.await?;
        let error = refusal(upload.await, StatusCode::CONFLICT)?;
        assert_eq!(error.body.code, ErrorCode::RevisionConflict);
        assert_eq!(
            error.body.current_revision,
            Some(3),
            "the source head must have moved to revision 3"
        );
        assert_eq!(
            asset_rows(&directory)?,
            rows_before,
            "a refused upload must add no association, version, receipt or identifier"
        );
        assert_listing(&state, &actor, &artifact, 0, &[]).await?;
        Ok::<(), Box<dyn Error>>(())
    })
}

/// A grant withdrawn while an upload waits for its commit refuses the commit on
/// the blocking task: `403`, and nothing is committed. The withdrawal lands
/// after the upload's own checks, so only that check can refuse it.
#[test]
fn a_queued_upload_whose_grant_is_withdrawn_commits_nothing() -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let artifact = declared_artifact(&state, &actor).await?;
        let (view, session) = granted_view(&state, &actor, &artifact).await?;
        let call = frame_call(&actor, &view);
        let rows_before = asset_rows(&directory)?;
        let request = upload_request(&view, "upload-race-2", 0);
        let mut upload = Box::pin(runtime_assets::upload(&state, &call, &request));
        let mut withdrawal = Box::pin(withdraw_grant(&state, &actor, &session, &view));
        // The upload's lookup and then the withdrawal's lookup queue behind the
        // first blocker. Once both have run, the upload has passed its checks
        // while the grant still stands, and its commit queues behind the second
        // blocker. The withdrawal takes effect before that blocker is released.
        let (release, blocker) = hold_blocking_thread().await?;
        assert!(
            pends(&mut upload).await,
            "the upload must wait for its lookup"
        );
        assert!(
            pends(&mut withdrawal).await,
            "the withdrawal must wait behind the upload's lookup"
        );
        release.send(())?;
        blocker.await?;
        settle().await?;
        let (release, blocker) = hold_blocking_thread().await?;
        assert!(
            pends(&mut upload).await,
            "the commit must wait behind the second blocker"
        );
        withdrawal.await?;
        release.send(())?;
        blocker.await?;
        refusal(upload.await, StatusCode::FORBIDDEN)?;
        assert_eq!(
            asset_rows(&directory)?,
            rows_before,
            "a refused upload must add no association, version, receipt or identifier"
        );
        assert_listing(&state, &actor, &artifact, 0, &[]).await?;
        Ok::<(), Box<dyn Error>>(())
    })
}

/// A connection closed while an upload waits for its commit refuses the commit
/// on the blocking task: the binding is gone, so the answer is `404`, the same
/// as for any closed connection, and nothing is committed.
#[test]
fn a_queued_upload_whose_connection_is_revoked_commits_nothing() -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let artifact = declared_artifact(&state, &actor).await?;
        let (view, _session) = granted_view(&state, &actor, &artifact).await?;
        let call = frame_call(&actor, &view);
        let rows_before = asset_rows(&directory)?;
        let request = upload_request(&view, "upload-race-3", 0);
        let mut upload = Box::pin(runtime_assets::upload(&state, &call, &request));
        let (release, blocker) = pause_before_the_commit(&mut upload).await?;
        state.revoke_actor(&actor);
        release.send(())?;
        blocker.await?;
        refusal(upload.await, StatusCode::NOT_FOUND)?;
        assert_eq!(
            asset_rows(&directory)?,
            rows_before,
            "a refused upload must add no association, version, receipt or identifier"
        );
        assert_listing(&state, &actor, &artifact, 0, &[]).await?;
        Ok::<(), Box<dyn Error>>(())
    })
}

/// A grant withdrawn after an upload's commit withholds only the reply: `403`.
/// The commit is not undone, so the association, the version and the receipt
/// all remain.
#[test]
fn an_upload_whose_grant_is_withdrawn_after_its_commit_keeps_its_association()
-> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let artifact = declared_artifact(&state, &actor).await?;
        let (view, session) = granted_view(&state, &actor, &artifact).await?;
        let call = frame_call(&actor, &view);
        let request = upload_request(&view, "upload-race-4", 0);
        let mut upload = Box::pin(runtime_assets::upload(&state, &call, &request));
        let (release, blocker) = pause_before_the_commit(&mut upload).await?;
        // The commit runs first, while the grant stands; the withdrawal queues
        // behind it and lands before the upload's reply is built.
        let mut withdrawal = Box::pin(withdraw_grant(&state, &actor, &session, &view));
        assert!(
            pends(&mut withdrawal).await,
            "the withdrawal must wait behind the commit"
        );
        release.send(())?;
        blocker.await?;
        withdrawal.await?;
        refusal(upload.await, StatusCode::FORBIDDEN)?;
        assert_eq!(
            asset_rows(&directory)?,
            AssetRows {
                associations: 1,
                receipts: 1,
                spaces: 1,
                ids: 1,
            },
            "the commit must have landed once: one association, receipt, space and identifier"
        );
        assert_listing(&state, &actor, &artifact, 1, &[PNG_SHA256]).await?;
        Ok::<(), Box<dyn Error>>(())
    })
}

/// A connection closed after an upload's commit withholds only the reply: `404`,
/// the answer for any closed connection. The commit is not undone, so the
/// association, the version and the receipt all remain.
#[test]
fn an_upload_whose_connection_is_revoked_after_its_commit_keeps_its_association()
-> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let artifact = declared_artifact(&state, &actor).await?;
        let (view, _session) = granted_view(&state, &actor, &artifact).await?;
        let call = frame_call(&actor, &view);
        let request = upload_request(&view, "upload-race-5", 0);
        let mut upload = Box::pin(runtime_assets::upload(&state, &call, &request));
        let (release, blocker) = pause_before_the_commit(&mut upload).await?;
        release.send(())?;
        blocker.await?;
        // The commit is queued behind the blocker, so it has run once settle returns.
        settle().await?;
        state.revoke_actor(&actor);
        refusal(upload.await, StatusCode::NOT_FOUND)?;
        assert_eq!(
            asset_rows(&directory)?,
            AssetRows {
                associations: 1,
                receipts: 1,
                spaces: 1,
                ids: 1,
            },
            "the commit must have landed once: one association, receipt, space and identifier"
        );
        assert_listing(&state, &actor, &artifact, 1, &[PNG_SHA256]).await?;
        Ok::<(), Box<dyn Error>>(())
    })
}

/// A source save that moves the head before a queued deletion commits refuses
/// the deletion: `409 revision_conflict` with the new head. The asset stays
/// associated, the version does not move and no receipt is added.
#[test]
fn a_queued_delete_whose_source_moves_before_its_commit_deletes_nothing()
-> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let state = test_state(&directory)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?;
    runtime.block_on(async {
        let actor: ActorId = "local".parse()?;
        let artifact = declared_artifact(&state, &actor).await?;
        let asset = owner_upload(&state, &actor, &artifact).await?;
        let (view, _session) = granted_view(&state, &actor, &artifact).await?;
        let call = frame_call(&actor, &view);
        let rows_before = asset_rows(&directory)?;
        let request = delete_request(&view, "delete-race-1", 1, &asset);
        let mut deletion = Box::pin(runtime_assets::delete(&state, &call, &request));
        let (release, blocker) = pause_before_the_commit(&mut deletion).await?;
        save_on_second_connection(&directory, &actor, &artifact, 2)?;
        release.send(())?;
        blocker.await?;
        let error = refusal(deletion.await, StatusCode::CONFLICT)?;
        assert_eq!(error.body.code, ErrorCode::RevisionConflict);
        assert_eq!(
            error.body.current_revision,
            Some(3),
            "the source head must have moved to revision 3"
        );
        assert_eq!(
            asset_rows(&directory)?,
            rows_before,
            "a refused deletion must leave the association, version and receipts as they were"
        );
        assert_listing(&state, &actor, &artifact, 1, &[PNG_SHA256]).await?;
        Ok::<(), Box<dyn Error>>(())
    })
}
